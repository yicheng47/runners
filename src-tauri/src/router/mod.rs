// Signal router v0 — flat parent-process dispatcher.
//
// What this is. The lead runner is the agent that *thinks* about
// coordination — it plans, dispatches workers via directed messages,
// decides when to escalate. The router is the parent-process plumbing
// underneath: bootstrap (write the launch prompt to the lead's stdin on
// `mission_goal`), cross-process stdin push (`ask_lead`, `human_said`,
// `human_response`), the UI bridge (`ask_human` → `human_question` event),
// and the runner-availability map (`runner_status`). See arch §5.5 and
// docs/impls/v0-mvp.md `C8 — Signal router v0`.
//
// What this is not. There is no policy engine, no rule abstraction, no
// per-crew config in MVP. Handlers are a flat `match signal_type { … }`.
// `crews.orchestrator_policy` is reserved for v0.x and is not read here.
//
// Per arch §5.5.0 invariant: messages never trigger router actions. Only
// `EventKind::Signal` reaches the dispatcher; messages flow through the
// inbox projection in `event_bus`.

mod handlers;
pub mod prompt;
pub mod runtime;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use runner_core::event_log::EventLog;
use runner_core::model::{Event, EventKind, SignalType};

use crate::error::Result;
use crate::event_bus::{AppendedEvent, BusEmitter, InboxUpdate, WatermarkUpdate};
use crate::model::{CrewRunner, Runner};
use crate::session::manager::SessionManager;

/// What the router uses to push bytes into a child's PTY. The full
/// `SessionManager` impls it; tests use a recording fake. Lives behind a
/// trait so the router doesn't pull a PTY runtime into unit tests.
pub trait StdinInjector: Send + Sync + 'static {
    fn inject(&self, session_id: &str, bytes: &[u8]) -> Result<()>;
}

impl StdinInjector for SessionManager {
    fn inject(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        SessionManager::inject_stdin(self, session_id, bytes)
    }
}

/// Latest-known availability for a runner. Populated from `runner_status`
/// signals; never inferred from PTY bytes (arch §5.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerStatus {
    Busy,
    Idle,
}

/// Inputs to the launch-prompt composer, captured at mount so the
/// `mission_goal` handler doesn't have to round-trip the DB. The lead row
/// also doubles as the lead-resolved handle the dispatcher routes to.
pub(crate) struct LaunchInputs {
    crew_name: String,
    lead: Runner,
    roster: Vec<RosterRow>,
    allowed_signals: Vec<SignalType>,
}

pub(crate) struct RosterRow {
    handle: String,
    display_name: String,
    role: String,
    lead: bool,
}

/// Mutable per-mission state. Rebuilt on reopen by replaying the log into
/// `reconstruct_from_log` — no separate persistence layer.
#[derive(Default)]
struct RouterState {
    /// Resolved at mount from the spawned `SpawnedSession` rows. The map is
    /// authoritative for the mission's lifetime; if a child crashes the
    /// entry stays so subsequent injections fail visibly with a
    /// `mission_warning` (the desired behavior — better than silently
    /// dropping a `human_response`).
    session_by_handle: HashMap<String, String>,
    /// `human_question.id` → asker handle. Populated when an `ask_human`
    /// is dispatched (the appended card's id is the canonical question_id
    /// per arch §5.5.0) and consumed by the matching `human_response`.
    pending_asks: HashMap<String, String>,
    /// Latest `runner_status` per handle.
    status: HashMap<String, RunnerStatus>,
    /// Replay high-water ULID. Set by `reconstruct_from_log` on reopen;
    /// `handle_event` short-circuits any event whose `id` is `≤` this so
    /// the bus's initial replay doesn't re-inject historical stdin or
    /// re-emit `human_question` cards. `None` for fresh missions: the
    /// opening `mission_goal` event must reach the live dispatcher to
    /// bootstrap the lead.
    replay_high_water: Option<String>,
}

/// One mission's router. Mounted by `mission_start` after sessions spawn,
/// dropped by `mission_stop`. Wired into the event bus as a `BusEmitter`
/// subscriber so `handle_event` runs on every appended envelope.
pub struct Router {
    mission_id: String,
    crew_id: String,
    log: Arc<EventLog>,
    injector: Arc<dyn StdinInjector>,
    launch: LaunchInputs,
    state: Mutex<RouterState>,
}

impl Router {
    /// Build a router from the crew's roster and lead. `roster` is the same
    /// slice `mission_start` already loaded for the spawn loop.
    pub fn new(
        mission_id: String,
        crew_id: String,
        crew_name: String,
        roster: &[CrewRunner],
        allowed_signals: Vec<SignalType>,
        log: Arc<EventLog>,
        injector: Arc<dyn StdinInjector>,
    ) -> Result<Arc<Self>> {
        let lead = roster
            .iter()
            .find(|m| m.lead)
            .map(|m| m.runner.clone())
            .ok_or_else(|| {
                crate::error::Error::msg(format!(
                    "router mount: crew {crew_id} has no lead runner"
                ))
            })?;
        let roster_rows = roster
            .iter()
            .map(|m| RosterRow {
                handle: m.runner.handle.clone(),
                display_name: m.runner.display_name.clone(),
                role: m.runner.role.clone(),
                lead: m.lead,
            })
            .collect();

        Ok(Arc::new(Self {
            mission_id,
            crew_id,
            log,
            injector,
            launch: LaunchInputs {
                crew_name,
                lead,
                roster: roster_rows,
                allowed_signals,
            },
            state: Mutex::new(RouterState::default()),
        }))
    }

    /// Register the spawned session ids so handlers can find which PTY
    /// owns each handle. Called once after `mission_start`'s spawn loop
    /// succeeds. Live `mission_start` calls `register_sessions` *before*
    /// the bus mounts so the initial replay's `mission_goal` lands on a
    /// fully-wired router; reopen paths register against existing live
    /// PTYs (when reattach lands) or skip injection (the workspace
    /// surfaces `mission_warning` from `inject_to_handle` either way).
    pub fn register_sessions(&self, sessions: &[(String, String)]) {
        let mut state = self.state.lock().unwrap();
        for (handle, session_id) in sessions {
            state
                .session_by_handle
                .insert(handle.clone(), session_id.clone());
        }
    }

    /// Reopen path only — fold historical projection state from the log
    /// without firing handler side effects, and set the replay high-water
    /// mark so the subsequent bus mount's initial replay no-ops past it.
    ///
    /// What is rebuilt:
    /// - `pending_asks` from `ask_human` → `human_question` pairs (the
    ///   card id is the canonical `question_id`; we walk in append order
    ///   to match each ask with its following card via
    ///   `human_question.payload.triggered_by`). Asks already answered
    ///   by a `human_response` are removed.
    /// - `runner_status` from the latest `runner_status` row per handle.
    ///
    /// What is *not* rebuilt: stdin pushes. The launch prompt, ask_lead
    /// relays, human_said echoes, and idle nudges are all live-only side
    /// effects. Per the C8 plan, replay does not re-inject prompts into
    /// a sleeping LLM.
    ///
    /// **MUST NOT be called for fresh missions.** Setting the watermark
    /// over the just-written opening `mission_goal` would cause the bus
    /// initial replay to no-op the bootstrap injection, leaving the lead
    /// without its launch prompt.
    pub fn reconstruct_from_log(&self) -> Result<()> {
        let entries = self.log.read_from(0)?;

        // Walk once, building a transient ask_human.id → asker map so we
        // can pair the next human_question with the right asker. Once the
        // pairing lands in pending_asks, the ask_human.id is no longer
        // needed.
        let mut ask_human_asker: HashMap<String, String> = HashMap::new();
        let mut pending: HashMap<String, String> = HashMap::new();
        let mut status: HashMap<String, RunnerStatus> = HashMap::new();
        let mut last_id: Option<String> = None;

        for entry in &entries {
            let event = &entry.event;
            last_id = Some(event.id.clone());
            if !matches!(event.kind, EventKind::Signal) {
                continue;
            }
            let Some(t) = event.signal_type.as_ref() else {
                continue;
            };
            match t.as_str() {
                "ask_human" => {
                    ask_human_asker.insert(event.id.clone(), event.from.clone());
                }
                "human_question" => {
                    let triggered_by = event
                        .payload
                        .get("triggered_by")
                        .and_then(|v| v.as_str());
                    if let Some(ask_id) = triggered_by {
                        if let Some(asker) = ask_human_asker.remove(ask_id) {
                            pending.insert(event.id.clone(), asker);
                        }
                    }
                }
                "human_response" => {
                    if let Some(qid) = event
                        .payload
                        .get("question_id")
                        .and_then(|v| v.as_str())
                    {
                        pending.remove(qid);
                    }
                }
                "runner_status" => {
                    let s = match event.payload.get("state").and_then(|v| v.as_str()) {
                        Some("busy") => Some(RunnerStatus::Busy),
                        Some("idle") => Some(RunnerStatus::Idle),
                        _ => None,
                    };
                    if let Some(s) = s {
                        status.insert(event.from.clone(), s);
                    }
                }
                _ => {}
            }
        }

        let mut state = self.state.lock().unwrap();
        state.pending_asks = pending;
        state.status = status;
        state.replay_high_water = last_id;
        Ok(())
    }

    pub fn lead_handle(&self) -> &str {
        &self.launch.lead.handle
    }

    /// Single dispatcher entry point. Bus calls this for every appended
    /// event in arrival order. Messages return early per arch §5.5.0.
    /// On reopen, events at-or-below the replay high-water mark are
    /// short-circuited so the bus's initial replay doesn't re-inject
    /// historical stdin or re-emit cards (arch §5.5: "stdin pushes are
    /// deliberately silent" + plan's projection-only replay).
    pub fn handle_event(&self, event: &Event) {
        if !matches!(event.kind, EventKind::Signal) {
            return;
        }
        let Some(signal) = event.signal_type.as_ref() else {
            return;
        };
        // Watermark check before signal-type match: covers every handler
        // (mission_goal, human_said, ask_lead, ask_human, human_response,
        // runner_status) in one place. Lex-compare on bytes; ULIDs sort
        // lex-correct.
        if let Some(w) = self.state.lock().unwrap().replay_high_water.as_deref() {
            if event.id.as_bytes() <= w.as_bytes() {
                return;
            }
        }
        match signal.as_str() {
            "mission_goal" => handlers::mission_goal(self, event),
            "human_said" => handlers::human_said(self, event),
            "ask_lead" => handlers::ask_lead(self, event),
            "ask_human" => handlers::ask_human(self, event),
            "human_response" => handlers::human_response(self, event),
            "runner_status" => handlers::runner_status(self, event),
            // mission_start, mission_stopped, inbox_read, human_question,
            // mission_warning — observed but not routed here. inbox_read is
            // owned by the bus's projection layer; mission_warning /
            // human_question are events the router itself emits.
            _ => {}
        }
    }

    // ---- helpers used by handlers --------------------------------------

    pub(crate) fn inject_to_handle(&self, handle: &str, bytes: &[u8]) -> Result<()> {
        let session_id = {
            let state = self.state.lock().unwrap();
            state.session_by_handle.get(handle).cloned()
        };
        let Some(session_id) = session_id else {
            return Err(crate::error::Error::msg(format!(
                "router: no live session for handle @{handle}"
            )));
        };
        self.injector.inject(&session_id, bytes)
    }

    pub(crate) fn launch(&self) -> &LaunchInputs {
        &self.launch
    }

    pub(crate) fn record_pending_ask(&self, question_id: String, asker: String) {
        self.state
            .lock()
            .unwrap()
            .pending_asks
            .insert(question_id, asker);
    }

    pub(crate) fn take_pending_ask(&self, question_id: &str) -> Option<String> {
        self.state.lock().unwrap().pending_asks.remove(question_id)
    }

    pub(crate) fn set_status(&self, handle: String, status: RunnerStatus) {
        self.state.lock().unwrap().status.insert(handle, status);
    }

    /// Append a `mission_warning` event when a handler hits an unexpected
    /// state (dead session, unmatched `human_response`, malformed payload).
    /// Best-effort: a log-write failure here is logged but never panics
    /// the router thread.
    pub(crate) fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        let draft = runner_core::model::EventDraft::signal(
            self.crew_id.clone(),
            self.mission_id.clone(),
            "router",
            SignalType::new("mission_warning"),
            serde_json::json!({ "message": message }),
        );
        if let Err(e) = self.log.append(draft) {
            eprintln!(
                "router[{}]: failed to append mission_warning ({}): {e}",
                self.mission_id, message,
            );
        }
    }

    /// Append a `human_question` event for the workspace UI and return its
    /// id. Per arch §5.5.0 the canonical `question_id` is the appended
    /// event's own `id`; `human_response.payload.question_id` references
    /// that. We deliberately do *not* echo `question_id` into the payload —
    /// the spec calls that "echoed here for convenience" and constructing
    /// it would require knowing the id before append. Consumers should
    /// read `event.id`. `triggered_by` ties the card back to the
    /// originating `ask_human` for replay reconstruction and audit.
    pub(crate) fn append_human_question(
        &self,
        ask_human_id: &str,
        prompt: &str,
        choices: &serde_json::Value,
        on_behalf_of: Option<&str>,
    ) -> Option<String> {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "triggered_by".into(),
            serde_json::Value::String(ask_human_id.to_string()),
        );
        payload.insert(
            "prompt".into(),
            serde_json::Value::String(prompt.to_string()),
        );
        payload.insert("choices".into(), choices.clone());
        if let Some(on_behalf_of) = on_behalf_of {
            payload.insert(
                "on_behalf_of".into(),
                serde_json::Value::String(on_behalf_of.to_string()),
            );
        }
        let draft = runner_core::model::EventDraft::signal(
            self.crew_id.clone(),
            self.mission_id.clone(),
            "router",
            SignalType::new("human_question"),
            serde_json::Value::Object(payload),
        );
        match self.log.append(draft) {
            Ok(ev) => Some(ev.id),
            Err(e) => {
                eprintln!(
                    "router[{}]: failed to append human_question: {e}",
                    self.mission_id
                );
                None
            }
        }
    }
}

impl LaunchInputs {
    pub(crate) fn crew_name(&self) -> &str {
        &self.crew_name
    }
    pub(crate) fn lead(&self) -> &Runner {
        &self.lead
    }
    pub(crate) fn roster(&self) -> &[RosterRow] {
        &self.roster
    }
    pub(crate) fn allowed_signals(&self) -> &[SignalType] {
        &self.allowed_signals
    }
}

impl RosterRow {
    pub(crate) fn handle(&self) -> &str {
        &self.handle
    }
    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }
    pub(crate) fn role(&self) -> &str {
        &self.role
    }
    pub(crate) fn is_lead(&self) -> bool {
        self.lead
    }
}

/// `BusEmitter` adapter so the existing `BusRegistry::mount` machinery can
/// drive the router. Only `appended` carries the work; the inbox/watermark
/// methods are no-ops because those are projections owned by the bus.
pub struct RouterSubscriber(pub Arc<Router>);

impl BusEmitter for RouterSubscriber {
    fn appended(&self, ev: &AppendedEvent) {
        self.0.handle_event(&ev.event);
    }
    fn inbox_updated(&self, _ev: &InboxUpdate) {}
    fn watermark_advanced(&self, _ev: &WatermarkUpdate) {}
}

/// Fan a single bus emission to multiple subscribers. The bus accepts only
/// one emitter, so `mission_start` wraps the Tauri emitter and the router
/// in this composite. Each sub-emitter is called in registration order.
pub struct CompositeBusEmitter {
    subs: Vec<Arc<dyn BusEmitter>>,
}

impl CompositeBusEmitter {
    pub fn new(subs: Vec<Arc<dyn BusEmitter>>) -> Self {
        Self { subs }
    }
}

impl BusEmitter for CompositeBusEmitter {
    fn appended(&self, ev: &AppendedEvent) {
        for s in &self.subs {
            s.appended(ev);
        }
    }
    fn inbox_updated(&self, ev: &InboxUpdate) {
        for s in &self.subs {
            s.inbox_updated(ev);
        }
    }
    fn watermark_advanced(&self, ev: &WatermarkUpdate) {
        for s in &self.subs {
            s.watermark_advanced(ev);
        }
    }
}

/// Process-wide registry of live routers, keyed by mission id. Mirrors
/// `event_bus::BusRegistry`.
pub struct RouterRegistry {
    routers: Mutex<HashMap<String, Arc<Router>>>,
}

impl RouterRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            routers: Mutex::new(HashMap::new()),
        })
    }

    pub fn register(&self, mission_id: String, router: Arc<Router>) {
        self.routers.lock().unwrap().insert(mission_id, router);
    }

    pub fn unregister(&self, mission_id: &str) {
        self.routers.lock().unwrap().remove(mission_id);
    }

    #[allow(dead_code)] // Exposed for the future workspace UI bridge.
    pub fn get(&self, mission_id: &str) -> Option<Arc<Router>> {
        self.routers.lock().unwrap().get(mission_id).cloned()
    }
}

/// Convenience for `mission_start`: open the events log Arc once. Both the
/// router (for log appends) and `mission_start`'s opening writes use the
/// same flock-guarded path, so multiple `EventLog` instances are safe.
pub fn open_log_for_mission(mission_dir: &Path) -> Result<Arc<EventLog>> {
    Ok(Arc::new(EventLog::open(mission_dir)?))
}

#[cfg(test)]
mod tests;
