// Router unit tests. The list mirrors docs/tests/v0-mvp-tests.md C8.
//
// We bypass the event bus entirely here — the router exposes
// `handle_event(&Event)` synchronously so we can drive it with hand-crafted
// envelopes and assert what landed in the recording injector + the log.
// Bus integration is covered separately (mission lifecycle + mission_e2e).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use runner_core::event_log::EventLog;
use runner_core::model::{Event, EventDraft, EventKind, SignalType};

use super::{Router, RouterRegistry, StdinInjector};
use crate::error::Result;
use crate::model::{CrewRunner, Runner};

/// Records every `inject` call so handler outputs can be asserted.
#[derive(Default)]
struct RecordingInjector {
    pushes: Mutex<Vec<(String, Vec<u8>)>>,
    /// Optional `dead_session` set — `inject` errors when called with one
    /// of these ids, simulating a crashed PTY for `mission_warning` tests.
    dead: Mutex<Vec<String>>,
}

impl RecordingInjector {
    fn pushes_for(&self, session_id: &str) -> Vec<String> {
        self.pushes
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s == session_id)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .collect()
    }

    fn all_pushes(&self) -> Vec<(String, String)> {
        self.pushes
            .lock()
            .unwrap()
            .iter()
            .map(|(s, b)| (s.clone(), String::from_utf8_lossy(b).into_owned()))
            .collect()
    }

    fn mark_dead(&self, session_id: &str) {
        self.dead.lock().unwrap().push(session_id.to_string());
    }
}

impl StdinInjector for RecordingInjector {
    fn inject(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        if self.dead.lock().unwrap().iter().any(|d| d == session_id) {
            return Err(crate::error::Error::msg(format!(
                "test: session {session_id} is dead"
            )));
        }
        self.pushes
            .lock()
            .unwrap()
            .push((session_id.to_string(), bytes.to_vec()));
        Ok(())
    }
}

fn runner(handle: &str, runtime: &str) -> Runner {
    Runner {
        id: format!("rid-{handle}"),
        handle: handle.into(),
        display_name: handle.to_uppercase(),
        role: "test".into(),
        runtime: runtime.into(),
        command: "/bin/sh".into(),
        args: vec![],
        working_dir: None,
        system_prompt: Some(format!("brief for {handle}")),
        env: HashMap::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn crew_runner(handle: &str, lead: bool) -> CrewRunner {
    CrewRunner {
        runner: runner(handle, "claude-code"),
        position: 0,
        lead,
        added_at: Utc::now(),
    }
}

/// Build a router around a fresh tempdir log + recording injector. Returns
/// `(router, injector, log, dir)` so tests can inspect everything without
/// re-opening the file. The dir is returned so tempdir cleanup is delayed
/// to test-end (otherwise the log path would be invalidated immediately).
fn fixture(
    roster: Vec<CrewRunner>,
    sessions: &[(&str, &str)],
) -> (
    Arc<Router>,
    Arc<RecordingInjector>,
    Arc<EventLog>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EventLog::open(dir.path()).unwrap());
    let injector = Arc::new(RecordingInjector::default());
    let injector_dyn: Arc<dyn StdinInjector> = injector.clone();
    let router = Router::new(
        "mission-1".into(),
        "crew-1".into(),
        "Crew One".into(),
        &roster,
        vec![SignalType::new("mission_goal"), SignalType::new("ask_lead")],
        log.clone(),
        injector_dyn,
    )
    .unwrap();
    let session_pairs: Vec<(String, String)> = sessions
        .iter()
        .map(|(h, s)| (h.to_string(), s.to_string()))
        .collect();
    router.register_sessions(&session_pairs);
    (router, injector, log, dir)
}

fn signal(from: &str, ty: &str, payload: serde_json::Value) -> EventDraft {
    EventDraft::signal("crew-1", "mission-1", from, SignalType::new(ty), payload)
}

fn message(from: &str, to: Option<&str>, text: &str) -> EventDraft {
    EventDraft::message("crew-1", "mission-1", from, to.map(String::from), text)
}

fn read_signals(log: &EventLog) -> Vec<Event> {
    log.read_from(0)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .filter(|e| matches!(e.kind, EventKind::Signal))
        .collect()
}

#[test]
fn messages_do_not_trigger_router_actions() {
    // Arch §5.5.0: messages flow through the inbox projection only; the
    // router's dispatcher must early-return on EventKind::Message. A
    // `mission_warning` from a missing handler would also surface here, so
    // an empty pushes Vec proves both that the dispatcher matched on kind
    // and that no handler ran.
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );
    let bcast = log.append(message("lead", None, "broadcast")).unwrap();
    let direct = log.append(message("lead", Some("impl"), "go")).unwrap();
    router.handle_event(&bcast);
    router.handle_event(&direct);
    assert!(injector.all_pushes().is_empty());
}

#[test]
fn mission_goal_injects_composed_prompt_to_lead() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );
    let ev = log
        .append(signal(
            "human",
            "mission_goal",
            serde_json::json!({ "text": "ship v0" }),
        ))
        .unwrap();
    router.handle_event(&ev);

    let lead_pushes = injector.pushes_for("S-LEAD");
    assert_eq!(lead_pushes.len(), 1, "lead receives one prompt push");
    let prompt = &lead_pushes[0];
    assert!(prompt.contains("Goal: ship v0"));
    assert!(prompt.contains("`impl`"));
    assert!(prompt.contains("Allowed signal types"));
    // Workers do not receive the launch prompt.
    assert!(injector.pushes_for("S-IMPL").is_empty());
}

#[test]
fn human_said_routes_to_target_or_lead() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );

    // Targeted: lands on the worker.
    let direct = log
        .append(signal(
            "human",
            "human_said",
            serde_json::json!({ "text": "look at line 42", "target": "impl" }),
        ))
        .unwrap();
    router.handle_event(&direct);
    let impl_pushes = injector.pushes_for("S-IMPL");
    assert_eq!(impl_pushes.len(), 1);
    assert!(impl_pushes[0].contains("look at line 42"));
    assert!(injector.pushes_for("S-LEAD").is_empty());

    // Untargeted: defaults to the lead.
    let bcast = log
        .append(signal(
            "human",
            "human_said",
            serde_json::json!({ "text": "status?" }),
        ))
        .unwrap();
    router.handle_event(&bcast);
    let lead_pushes = injector.pushes_for("S-LEAD");
    assert_eq!(lead_pushes.len(), 1);
    assert!(lead_pushes[0].contains("status?"));
}

#[test]
fn ask_lead_injects_question_and_context_to_lead() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );
    let ev = log
        .append(signal(
            "impl",
            "ask_lead",
            serde_json::json!({ "question": "use notify-debouncer-full?", "context": "Pros: …\nCons: …" }),
        ))
        .unwrap();
    router.handle_event(&ev);

    let pushes = injector.pushes_for("S-LEAD");
    assert_eq!(pushes.len(), 1);
    let text = &pushes[0];
    assert!(text.contains("[ask_lead from @impl]"));
    assert!(text.contains("use notify-debouncer-full?"));
    assert!(text.contains("Pros:"));
    // Worker stdin must not see the relayed question.
    assert!(injector.pushes_for("S-IMPL").is_empty());
}

#[test]
fn ask_human_appends_human_question_card_and_records_pending_ask() {
    let (router, _injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );
    let ev = log
        .append(signal(
            "lead",
            "ask_human",
            serde_json::json!({
                "prompt": "Approve?",
                "choices": ["yes", "no"],
                "on_behalf_of": "impl",
            }),
        ))
        .unwrap();
    router.handle_event(&ev);

    // Append a `human_question` event referencing the original ask. Per
    // arch §5.5.0, the canonical `question_id` is the card event's own
    // `id`; `triggered_by` ties it back to the originating `ask_human`.
    // The convenience-echo `payload.question_id` is intentionally absent
    // because the id isn't known until after append.
    let signals = read_signals(&log);
    let card = signals
        .iter()
        .find(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "human_question")
                .unwrap_or(false)
        })
        .expect("router must append human_question");
    assert_eq!(card.from, "router");
    assert_eq!(card.payload["prompt"], "Approve?");
    assert_eq!(card.payload["choices"], serde_json::json!(["yes", "no"]));
    assert_eq!(card.payload["on_behalf_of"], "impl");
    assert_eq!(card.payload["triggered_by"], ev.id);
    assert!(
        card.payload.get("question_id").is_none(),
        "question_id is the event's own id; not echoed in payload"
    );
}

#[test]
fn human_response_routes_back_to_asker() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );
    let ask = log
        .append(signal(
            "lead",
            "ask_human",
            serde_json::json!({
                "prompt": "Approve?",
                "choices": ["yes", "no"],
                "on_behalf_of": "impl",
            }),
        ))
        .unwrap();
    router.handle_event(&ask);

    // human_response.payload.question_id is the human_question.id (arch
    // §5.5.0), not the ask_human.id. Find the card the router appended.
    let card_id = read_signals(&log)
        .into_iter()
        .find(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "human_question")
                .unwrap_or(false)
        })
        .expect("router must append human_question")
        .id;

    let resp = log
        .append(signal(
            "human",
            "human_response",
            serde_json::json!({ "question_id": card_id, "choice": "yes" }),
        ))
        .unwrap();
    router.handle_event(&resp);

    let lead_pushes = injector.pushes_for("S-LEAD");
    assert!(
        lead_pushes.iter().any(|p| p.contains("[human_response] yes")),
        "lead must receive the routed answer; got {lead_pushes:?}",
    );
    // The pending-ask map is consumed; a duplicate response surfaces a
    // warning rather than re-injecting.
    let dup = log
        .append(signal(
            "human",
            "human_response",
            serde_json::json!({ "question_id": card_id, "choice": "no" }),
        ))
        .unwrap();
    router.handle_event(&dup);
    let warnings: Vec<_> = read_signals(&log)
        .into_iter()
        .filter(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "mission_warning")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.payload["message"].as_str().unwrap().contains("unknown question_id")),
        "duplicate response must produce mission_warning; got {warnings:?}",
    );
}

#[test]
fn human_response_without_matching_question_emits_mission_warning() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true)],
        &[("lead", "S-LEAD")],
    );
    let resp = log
        .append(signal(
            "human",
            "human_response",
            serde_json::json!({ "question_id": "01HUNKNOWN", "choice": "yes" }),
        ))
        .unwrap();
    router.handle_event(&resp);

    assert!(injector.all_pushes().is_empty());
    let warnings: Vec<_> = read_signals(&log)
        .into_iter()
        .filter(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "mission_warning")
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(warnings.len(), 1);
}

#[test]
fn runner_status_idle_for_worker_notifies_lead_and_busy_does_not() {
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true), crew_runner("impl", false)],
        &[("lead", "S-LEAD"), ("impl", "S-IMPL")],
    );

    // busy from a worker — silent (no push to lead).
    let busy = log
        .append(signal(
            "impl",
            "runner_status",
            serde_json::json!({ "state": "busy" }),
        ))
        .unwrap();
    router.handle_event(&busy);
    assert!(injector.pushes_for("S-LEAD").is_empty());

    // idle from a worker — push to the lead, mentioning the worker.
    let idle = log
        .append(signal(
            "impl",
            "runner_status",
            serde_json::json!({ "state": "idle", "note": "ready for next task" }),
        ))
        .unwrap();
    router.handle_event(&idle);
    let pushes = injector.pushes_for("S-LEAD");
    assert_eq!(pushes.len(), 1);
    assert!(pushes[0].contains("@impl is idle"));
    assert!(pushes[0].contains("ready for next task"));

    // idle from the lead itself — does not self-notify.
    let lead_idle = log
        .append(signal(
            "lead",
            "runner_status",
            serde_json::json!({ "state": "idle" }),
        ))
        .unwrap();
    router.handle_event(&lead_idle);
    assert_eq!(
        injector.pushes_for("S-LEAD").len(),
        1,
        "lead going idle must not push to lead",
    );
}

#[test]
fn pending_ask_map_reconstructs_from_log_on_reopen() {
    // Mount router #1, dispatch ask_human (which appends human_question),
    // drop. Mount router #2, call reconstruct_from_log (the reopen entry
    // point), then route human_response. The answer must still reach the
    // original asker — no separate persistence layer.
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EventLog::open(dir.path()).unwrap());
    let roster = vec![crew_runner("lead", true), crew_runner("impl", false)];

    let ask = log
        .append(signal(
            "lead",
            "ask_human",
            serde_json::json!({
                "prompt": "Approve?",
                "choices": ["yes", "no"],
                "on_behalf_of": "impl",
            }),
        ))
        .unwrap();

    // First mount handles the ask live (appends human_question).
    {
        let injector = Arc::new(RecordingInjector::default());
        let injector_dyn: Arc<dyn StdinInjector> = injector.clone();
        let router = Router::new(
            "mission-1".into(),
            "crew-1".into(),
            "Crew One".into(),
            &roster,
            vec![],
            log.clone(),
            injector_dyn,
        )
        .unwrap();
        router.register_sessions(&[
            ("lead".into(), "S-LEAD".into()),
            ("impl".into(), "S-IMPL".into()),
        ]);
        router.handle_event(&ask);
    }
    // Capture the card id router #1 produced; we'll use it as
    // human_response.payload.question_id below.
    let card_id = read_signals(&log)
        .into_iter()
        .find(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "human_question")
                .unwrap_or(false)
        })
        .expect("router #1 must have appended human_question")
        .id;

    // Reopen: build router #2, fold projection state from history. This
    // is the path mission_resume / mount-on-app-restart will follow.
    let injector = Arc::new(RecordingInjector::default());
    let injector_dyn: Arc<dyn StdinInjector> = injector.clone();
    let router2 = Router::new(
        "mission-1".into(),
        "crew-1".into(),
        "Crew One".into(),
        &roster,
        vec![],
        log.clone(),
        injector_dyn,
    )
    .unwrap();
    router2.register_sessions(&[
        ("lead".into(), "S-LEAD".into()),
        ("impl".into(), "S-IMPL".into()),
    ]);
    router2.reconstruct_from_log().unwrap();

    // Replay the historical events through handle_event the way the bus's
    // initial replay would. The watermark must short-circuit them so the
    // ask_human is NOT re-handled (no second human_question card in the
    // log) and the lead is NOT re-injected with anything.
    let card_count_before = read_signals(&log)
        .iter()
        .filter(|s| s.signal_type.as_ref().map(|t| t.as_str() == "human_question").unwrap_or(false))
        .count();
    for entry in log.read_from(0).unwrap() {
        router2.handle_event(&entry.event);
    }
    let card_count_after = read_signals(&log)
        .iter()
        .filter(|s| s.signal_type.as_ref().map(|t| t.as_str() == "human_question").unwrap_or(false))
        .count();
    assert_eq!(
        card_count_before, card_count_after,
        "replay must NOT re-emit human_question cards",
    );
    assert!(
        injector.all_pushes().is_empty(),
        "replay must NOT re-inject historical stdin; got {:?}",
        injector.all_pushes(),
    );

    // Now post a *new* response (id strictly greater than the watermark)
    // and assert it routes to the asker the reconstruct path recovered.
    let resp = log
        .append(signal(
            "human",
            "human_response",
            serde_json::json!({ "question_id": card_id, "choice": "yes" }),
        ))
        .unwrap();
    router2.handle_event(&resp);

    let lead_pushes = injector.pushes_for("S-LEAD");
    assert!(
        lead_pushes.iter().any(|p| p.contains("[human_response] yes")),
        "after reopen + reconstruct, response must route to original asker; got {lead_pushes:?}",
    );
}

#[test]
fn reconstruct_recovers_latest_runner_status_only() {
    // Reopen-path test for arch §5.5.1: latest reported state per handle.
    // busy → idle → busy must leave status[impl] = Busy after reconstruct,
    // and no historical idle-notice should re-inject into the lead.
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EventLog::open(dir.path()).unwrap());
    let roster = vec![crew_runner("lead", true), crew_runner("impl", false)];

    log.append(signal(
        "impl",
        "runner_status",
        serde_json::json!({ "state": "busy" }),
    ))
    .unwrap();
    log.append(signal(
        "impl",
        "runner_status",
        serde_json::json!({ "state": "idle", "note": "first idle" }),
    ))
    .unwrap();
    log.append(signal(
        "impl",
        "runner_status",
        serde_json::json!({ "state": "busy" }),
    ))
    .unwrap();

    let injector = Arc::new(RecordingInjector::default());
    let injector_dyn: Arc<dyn StdinInjector> = injector.clone();
    let router = Router::new(
        "mission-1".into(),
        "crew-1".into(),
        "Crew One".into(),
        &roster,
        vec![],
        log.clone(),
        injector_dyn,
    )
    .unwrap();
    router.register_sessions(&[
        ("lead".into(), "S-LEAD".into()),
        ("impl".into(), "S-IMPL".into()),
    ]);
    router.reconstruct_from_log().unwrap();

    // Bus replay of the historical events must short-circuit; no idle
    // notice is pushed to the lead, even though one of them is `idle`.
    for entry in log.read_from(0).unwrap() {
        router.handle_event(&entry.event);
    }
    assert!(
        injector.all_pushes().is_empty(),
        "historical idle must not push to lead on replay; got {:?}",
        injector.all_pushes(),
    );

    // A *new* idle event after the watermark must push normally — proves
    // the watermark only suppresses history, not live tail.
    let live_idle = log
        .append(signal(
            "impl",
            "runner_status",
            serde_json::json!({ "state": "idle", "note": "live" }),
        ))
        .unwrap();
    router.handle_event(&live_idle);
    let lead_pushes = injector.pushes_for("S-LEAD");
    assert_eq!(lead_pushes.len(), 1);
    assert!(lead_pushes[0].contains("@impl is idle"));
    assert!(lead_pushes[0].contains("live"));
}

#[test]
fn fresh_mission_start_does_not_call_reconstruct_so_mission_goal_fires() {
    // Regression on the reviewer's caveat: if a fresh-start mount called
    // reconstruct_from_log() over the just-written opening events, the
    // watermark would cover mission_goal and the lead would never receive
    // its launch prompt. mission_start must skip reconstruct entirely.
    // This test mirrors that path: pre-write opening events, build a
    // router WITHOUT calling reconstruct, then replay through handle_event
    // (what the bus does). The mission_goal handler must fire.
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EventLog::open(dir.path()).unwrap());
    let roster = vec![crew_runner("lead", true)];

    log.append(signal(
        "system",
        "mission_start",
        serde_json::json!({ "title": "fresh" }),
    ))
    .unwrap();
    log.append(signal(
        "human",
        "mission_goal",
        serde_json::json!({ "text": "go" }),
    ))
    .unwrap();

    let injector = Arc::new(RecordingInjector::default());
    let injector_dyn: Arc<dyn StdinInjector> = injector.clone();
    let router = Router::new(
        "mission-1".into(),
        "crew-1".into(),
        "Crew One".into(),
        &roster,
        vec![],
        log.clone(),
        injector_dyn,
    )
    .unwrap();
    router.register_sessions(&[("lead".into(), "S-LEAD".into())]);
    // NB: no reconstruct call. The bus's initial replay drives the
    // bootstrap.
    for entry in log.read_from(0).unwrap() {
        router.handle_event(&entry.event);
    }
    let lead_pushes = injector.pushes_for("S-LEAD");
    assert_eq!(
        lead_pushes.len(),
        1,
        "mission_goal must fire on fresh start; got {lead_pushes:?}",
    );
    assert!(lead_pushes[0].contains("Goal: go"));
}

#[test]
fn dead_session_for_handler_target_emits_mission_warning() {
    // The pending-ask map persists past a session crash by design — better
    // to surface the missed wake-up than to silently drop it. The router
    // attempts the inject, fails, and writes a mission_warning.
    let (router, injector, log, _dir) = fixture(
        vec![crew_runner("lead", true)],
        &[("lead", "S-LEAD")],
    );
    injector.mark_dead("S-LEAD");
    let ev = log
        .append(signal(
            "human",
            "human_said",
            serde_json::json!({ "text": "hi" }),
        ))
        .unwrap();
    router.handle_event(&ev);

    let warnings: Vec<_> = read_signals(&log)
        .into_iter()
        .filter(|s| {
            s.signal_type
                .as_ref()
                .map(|t| t.as_str() == "mission_warning")
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].payload["message"]
        .as_str()
        .unwrap()
        .contains("human_said injection"));
}

#[test]
fn registry_register_get_unregister() {
    let (router, _i, _l, _d) = fixture(
        vec![crew_runner("lead", true)],
        &[("lead", "S-LEAD")],
    );
    let reg = RouterRegistry::new();
    reg.register("mission-1".into(), router.clone());
    assert!(reg.get("mission-1").is_some());
    reg.unregister("mission-1");
    assert!(reg.get("mission-1").is_none());
}
