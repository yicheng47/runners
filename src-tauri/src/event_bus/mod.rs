// Event bus — tails a mission's append-only NDJSON log and broadcasts every
// new event to the rest of the process.
//
// Why this layer exists: C4 gave us durable append. C5 emits the opening
// events. C6 spawns the runners. None of those are enough for the UI or the
// orchestrator to *react* — they need a stream of envelopes as they land.
// Once C8 arrives, the orchestrator subscribes to this bus to inject stdin
// in response to signals; until then, the bus already powers replay + the
// per-runner inbox/watermark accounting that C10's workspace UI consumes.
//
// Design notes:
//
//   - One bus per live mission. The watcher tails exactly one file. We mount
//     in `mission_start` after the opening events are durable, and unmount in
//     `mission_stop`. Crashes during mount don't strand a watcher because the
//     bus owns its watcher and is dropped on error.
//
//   - notify can fire spurious modify events; we always re-read from the
//     stored byte offset. `EventLog::read_from` returns an empty Vec when
//     there's nothing new, so a duplicate notify is a cheap no-op. We never
//     trust notify's contents — only that "something might have changed".
//
//   - The watcher runs notify on its own thread and pipes events to a single
//     consumer thread that owns mutable bus state. We do NOT process inside
//     notify's callback, both to keep that thread fast and to serialize all
//     state updates through one channel. The consumer calls back into the
//     emitter (Tauri or test fake) so the bus is unit-testable without a
//     running app.
//
//   - Per-runner inbox projection is `events where to == null OR to == handle`.
//     We track every matching event id per handle so unread_count after a
//     watermark advance is just `len - read_idx` — no log rescans.
//
//   - Watermarks come exclusively from `inbox_read` signals (per arch §5.3 and
//     v0-mvp.md C7), never inferred from `--since` or wall time. The signal's
//     `from` identifies which runner is reading; `payload.up_to` is the ULID
//     they claim to have read through.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{
    Event as NotifyEvent, EventKind as NotifyKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use runner_core::event_log::EventLog;
use runner_core::model::{Event, EventKind};
use serde::Serialize;
use tauri::Emitter;

use crate::error::{Error, Result};

/// Decouples the bus from Tauri so the consumer thread can be unit-tested
/// without a running AppHandle. Production wraps `AppHandle::emit`; tests
/// use a recording fake. Mirrors the `SessionEvents` trait in `session::manager`.
pub trait BusEmitter: Send + Sync + 'static {
    fn appended(&self, ev: &AppendedEvent);
    fn inbox_updated(&self, ev: &InboxUpdate);
    fn watermark_advanced(&self, ev: &WatermarkUpdate);
}

/// Real Tauri emitter — fans out to `event/appended`, `inbox/updated`,
/// `watermark/advanced` so the React workspace can subscribe.
pub struct TauriBusEvents(pub tauri::AppHandle);

impl BusEmitter for TauriBusEvents {
    fn appended(&self, ev: &AppendedEvent) {
        let _ = self.0.emit("event/appended", ev);
    }
    fn inbox_updated(&self, ev: &InboxUpdate) {
        let _ = self.0.emit("inbox/updated", ev);
    }
    fn watermark_advanced(&self, ev: &WatermarkUpdate) {
        let _ = self.0.emit("watermark/advanced", ev);
    }
}

/// Payload for `event/appended`. The full envelope plus the mission id so the
/// frontend can route across multiple open missions.
#[derive(Debug, Clone, Serialize)]
pub struct AppendedEvent {
    pub mission_id: String,
    pub event: Event,
}

/// Payload for `inbox/updated`. Sent every time a runner's matching-event set
/// or unread count changes — that is, on every newly-projected event for that
/// handle, plus on every watermark advance that reduces unread_count.
#[derive(Debug, Clone, Serialize)]
pub struct InboxUpdate {
    pub mission_id: String,
    pub runner_handle: String,
    /// Highest ULID currently in the runner's projected inbox, or `None` if
    /// nothing has been routed to them yet.
    pub last_id: Option<String>,
    /// `payload.up_to` from the most recent `inbox_read` signal this runner
    /// emitted, or `None` if they haven't read anything.
    pub watermark: Option<String>,
    pub unread_count: usize,
}

/// Payload for `watermark/advanced`. Emitted only when an `inbox_read` signal
/// pushes the watermark forward — never for redundant reads. The frontend can
/// use this to clear unread badges without re-reading inbox state.
#[derive(Debug, Clone, Serialize)]
pub struct WatermarkUpdate {
    pub mission_id: String,
    pub runner_handle: String,
    pub watermark: String,
    pub unread_count: usize,
}

/// One mission's tail-and-project loop. Holding this value alive keeps the
/// notify watcher and consumer thread running; dropping it tears both down.
pub struct EventBus {
    mission_id: String,
    shutdown: Arc<AtomicBool>,
    consumer: Mutex<Option<JoinHandle<()>>>,
    /// Notify watcher — kept alive for the bus's lifetime. It feeds the
    /// channel; we never read from it directly.
    _watcher: RecommendedWatcher,
}

impl EventBus {
    /// Mount a bus on `mission_dir`'s `events.ndjson`, replay everything
    /// that's already on disk through the emitter, then keep tailing.
    ///
    /// `roster` is the list of runner handles whose inboxes we'll project.
    /// Adding handles after mount is not supported in MVP — crews can't grow
    /// mid-mission.
    pub fn for_mission(
        mission_id: String,
        mission_dir: &Path,
        roster: &[String],
        emitter: Arc<dyn BusEmitter>,
    ) -> Result<Arc<Self>> {
        let log = EventLog::open(mission_dir)?;
        let log_path_for_filter = log.path().to_path_buf();

        // The watcher's notify thread sends `()` pings on every fs event;
        // the consumer drains the channel and re-reads from the stored
        // offset. A bounded channel isn't needed — pings collapse anyway,
        // and notify can drop its own queue if we fall too far behind.
        let (tx_for_watcher, rx) = channel::<WatchPing>();

        // Watch the parent directory rather than the file itself. On macOS
        // `FSEvents` only delivers reliable updates for the file you watched
        // by exact inode; if a writer truncates+renames (which our event log
        // never does, but defense in depth) the original watch is silently
        // useless. Watching the dir + filtering on path is cheap and robust.
        let watch_root = mission_dir.to_path_buf();
        let mut watcher = notify::recommended_watcher(
            move |res: std::result::Result<NotifyEvent, notify::Error>| {
                let Ok(ev) = res else { return };
                if !ev.paths.iter().any(|p| p == &log_path_for_filter) {
                    return;
                }
                if matches!(
                    ev.kind,
                    NotifyKind::Modify(_) | NotifyKind::Create(_) | NotifyKind::Any
                ) {
                    let _ = tx_for_watcher.send(WatchPing::FsEvent);
                }
            },
        )
        .map_err(|e| Error::msg(format!("notify watcher: {e}")))?;
        watcher
            .watch(&watch_root, RecursiveMode::NonRecursive)
            .map_err(|e| Error::msg(format!("notify watch {watch_root:?}: {e}")))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let mission_id_for_thread = mission_id.clone();
        let roster_for_thread: Vec<String> = roster.to_vec();
        let shutdown_for_thread = Arc::clone(&shutdown);
        let emitter_for_thread = Arc::clone(&emitter);
        let consumer = thread::Builder::new()
            .name(format!("event-bus-{mission_id}"))
            .spawn(move || {
                let mut state = BusState::new(mission_id_for_thread.clone(), roster_for_thread);
                // Initial replay: read everything already on disk so the UI
                // can rehydrate after a reopen and so any backlog the writer
                // produced before the watcher attached is delivered.
                if let Err(e) = state.tick(&log, emitter_for_thread.as_ref()) {
                    eprintln!("event_bus[{mission_id_for_thread}]: initial tick failed: {e}");
                }
                loop {
                    // recv_timeout lets us notice shutdown without a notify
                    // event arriving; also serves as a slow-poll safety net
                    // in case notify drops something.
                    let _ = rx.recv_timeout(Duration::from_millis(500));
                    let shutting = shutdown_for_thread.load(Ordering::SeqCst);
                    // Always tick, even when shutting down: `mission_stop`
                    // appends the terminal `mission_stopped` event *before*
                    // calling unmount, and clients need to see it via
                    // `event/appended` before the bus tears down. Without
                    // this final drain, the consumer can wake on the
                    // shutdown flag and exit before notify delivered the
                    // terminal write, dropping the event silently.
                    if let Err(e) = state.tick(&log, emitter_for_thread.as_ref()) {
                        eprintln!("event_bus[{mission_id_for_thread}]: tick failed: {e}");
                    }
                    if shutting {
                        return;
                    }
                }
            })
            .map_err(|e| Error::msg(format!("spawn event-bus consumer: {e}")))?;

        Ok(Arc::new(Self {
            mission_id,
            shutdown,
            consumer: Mutex::new(Some(consumer)),
            _watcher: watcher,
        }))
    }

    /// Stop the consumer thread and drop the watcher. Idempotent — safe to
    /// call from `mission_stop` even if the bus is already shutting down.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.consumer.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }
}

impl Drop for EventBus {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Don't join from Drop — if the consumer is blocked on the emitter,
        // we'd deadlock the caller. The watcher dropping below closes the
        // notify side; the consumer wakes up via recv_timeout and exits.
    }
}

enum WatchPing {
    FsEvent,
}

/// Per-mission projection state. Owns nothing thread-shared — the consumer
/// thread is the only writer. `Vec<Ulid>` per handle holds matching event ids
/// in append order; `read_idx` advances on `inbox_read` signals so unread
/// count is `matched.len() - read_idx` without a log rescan.
struct BusState {
    mission_id: String,
    next_offset: u64,
    /// Roster of runner handles whose inboxes we project. Stable for the
    /// mission's lifetime.
    handles: Vec<String>,
    inbox: BTreeMap<String, RunnerInbox>,
}

#[derive(Default)]
struct RunnerInbox {
    matched: Vec<String>, // ULIDs in append order
    /// Index of the first unread entry in `matched`. `matched[read_idx..]`
    /// is the unread tail; `matched[..read_idx]` has been ack'd via
    /// inbox_read.
    read_idx: usize,
    /// Last `payload.up_to` from inbox_read. `None` means "never read".
    /// Stored separately from read_idx so we can emit it without scanning.
    watermark: Option<String>,
}

impl BusState {
    fn new(mission_id: String, handles: Vec<String>) -> Self {
        let mut inbox = BTreeMap::new();
        for h in &handles {
            inbox.insert(h.clone(), RunnerInbox::default());
        }
        Self {
            mission_id,
            next_offset: 0,
            handles,
            inbox,
        }
    }

    /// Drain whatever new lines are on disk and project them through the
    /// emitter. Called both on initial mount (from offset 0) and on every
    /// notify ping.
    ///
    /// Uses `read_from_lossy` so a single malformed complete line is logged
    /// and skipped, never re-tried on the next tick. Without this, one bad
    /// JSON write — say from a buggy CLI release — would freeze the bus on
    /// the same offset forever and silently swallow every later event.
    fn tick(&mut self, log: &EventLog, emitter: &dyn BusEmitter) -> Result<()> {
        let (entries, skipped) = log.read_from_lossy(self.next_offset)?;
        for skip in &skipped {
            eprintln!(
                "event_bus[{}]: skipping malformed line at offset {} ({})",
                self.mission_id, skip.offset, skip.error
            );
        }
        // Compute the new `next_offset` from the max of every line seen this
        // tick — entries AND skips. If we used only entries we'd re-read
        // skipped bytes whenever a bad line came *after* the last good one;
        // if we used only skips we'd lose track when bad lines came first.
        let max_skip_next = skipped.iter().map(|s| s.next_offset).max().unwrap_or(0);
        let max_entry_next = entries.iter().map(|e| e.next_offset).max().unwrap_or(0);
        let new_offset = self.next_offset.max(max_skip_next).max(max_entry_next);
        for entry in entries {
            let event = entry.event;

            emitter.appended(&AppendedEvent {
                mission_id: self.mission_id.clone(),
                event: event.clone(),
            });

            // inbox_read signals advance watermarks; handle them and move on.
            // Other signals (mission_start, mission_goal, ask_lead, …) never
            // project into inboxes — per arch §2.7 the inbox is strictly
            // `kind = "message" AND (to = null OR to = h)`.
            if Self::is_inbox_read(&event) {
                self.handle_inbox_read(&event, emitter);
                continue;
            }
            if !matches!(event.kind, EventKind::Message) {
                continue;
            }

            // Project into matching inboxes. Broadcasts (`to == null`) land in
            // every roster member's inbox; directs land in exactly one.
            for handle in self.handles.clone() {
                if event_targets(&event, &handle) {
                    let inbox = self.inbox.entry(handle.clone()).or_default();
                    inbox.matched.push(event.id.clone());
                    let unread = inbox.matched.len() - inbox.read_idx;
                    emitter.inbox_updated(&InboxUpdate {
                        mission_id: self.mission_id.clone(),
                        runner_handle: handle,
                        last_id: Some(event.id.clone()),
                        watermark: inbox.watermark.clone(),
                        unread_count: unread,
                    });
                }
            }
        }
        self.next_offset = new_offset;
        Ok(())
    }

    fn is_inbox_read(event: &Event) -> bool {
        matches!(event.kind, EventKind::Signal)
            && event
                .signal_type
                .as_ref()
                .map(|t| t.as_str() == "inbox_read")
                .unwrap_or(false)
    }

    /// Process an `inbox_read` signal: bump `read_idx` for the runner that
    /// emitted it (taken from `event.from`) past every matched id ≤ up_to,
    /// store the new watermark, and fire `watermark/advanced` if it actually
    /// moved. A duplicate or stale `up_to` is silently no-op'd.
    fn handle_inbox_read(&mut self, event: &Event, emitter: &dyn BusEmitter) {
        let Some(up_to) = event.payload.get("up_to").and_then(|v| v.as_str()) else {
            // Malformed inbox_read — log nothing, just drop. The CLI is the
            // only authorized writer of these and it always sets up_to.
            return;
        };
        // Reject up_to values that aren't real ULIDs. Without this guard a
        // junk value like "zzzz" sorts after every real ULID lexically, so
        // the comparisons below would silently mark every existing inbox
        // entry as read and hide every future entry whose ULID came before
        // "zzzz" — which, given Crockford's alphabet, is all of them.
        if up_to.parse::<ulid::Ulid>().is_err() {
            eprintln!(
                "event_bus[{}]: dropping inbox_read with non-ULID up_to {:?}",
                self.mission_id, up_to
            );
            return;
        }
        let handle = event.from.clone();
        let inbox = self.inbox.entry(handle.clone()).or_default();

        // Has the watermark moved? Compare lexically: ULIDs sort lex-correct.
        let already_at_or_past = inbox
            .watermark
            .as_deref()
            .map(|w| w.as_bytes() >= up_to.as_bytes())
            .unwrap_or(false);
        if already_at_or_past {
            return;
        }

        inbox.watermark = Some(up_to.to_string());
        // Advance read_idx past every matched id ≤ up_to. matched is already
        // append-ordered (ULIDs are monotonic), so this is just a forward
        // walk from the current read_idx.
        while inbox.read_idx < inbox.matched.len()
            && inbox.matched[inbox.read_idx].as_bytes() <= up_to.as_bytes()
        {
            inbox.read_idx += 1;
        }
        let unread = inbox.matched.len() - inbox.read_idx;
        emitter.watermark_advanced(&WatermarkUpdate {
            mission_id: self.mission_id.clone(),
            runner_handle: handle.clone(),
            watermark: up_to.to_string(),
            unread_count: unread,
        });
        // Also fire inbox/updated so consumers that only listen for that
        // event still see the new unread count.
        emitter.inbox_updated(&InboxUpdate {
            mission_id: self.mission_id.clone(),
            runner_handle: handle,
            last_id: inbox.matched.last().cloned(),
            watermark: inbox.watermark.clone(),
            unread_count: unread,
        });
    }
}

/// Returns true when `event` should appear in `handle`'s inbox.
fn event_targets(event: &Event, handle: &str) -> bool {
    match event.to.as_deref() {
        None => true,
        Some(target) => target == handle,
    }
}

/// Process-wide registry of live buses, keyed by mission id. Mounted by
/// `mission_start`, drained by `mission_stop` and at app shutdown.
pub struct BusRegistry {
    buses: Mutex<HashMap<String, Arc<EventBus>>>,
}

impl BusRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buses: Mutex::new(HashMap::new()),
        })
    }

    /// Mount a bus for `mission_id` if one isn't already registered.
    /// Returning the existing bus on a duplicate mount keeps the contract
    /// idempotent — `mission_start`'s rollback path can call `unmount` even
    /// if mount partially failed.
    pub fn mount(
        &self,
        mission_id: String,
        mission_dir: &Path,
        roster: &[String],
        emitter: Arc<dyn BusEmitter>,
    ) -> Result<Arc<EventBus>> {
        let mut buses = self.buses.lock().unwrap();
        if let Some(existing) = buses.get(&mission_id) {
            return Ok(Arc::clone(existing));
        }
        let bus = EventBus::for_mission(mission_id.clone(), mission_dir, roster, emitter)?;
        buses.insert(mission_id, Arc::clone(&bus));
        Ok(bus)
    }

    pub fn unmount(&self, mission_id: &str) {
        let bus = self.buses.lock().unwrap().remove(mission_id);
        if let Some(bus) = bus {
            bus.stop();
        }
    }

    #[allow(dead_code)] // Used by tests + future shutdown path.
    pub fn get(&self, mission_id: &str) -> Option<Arc<EventBus>> {
        self.buses.lock().unwrap().get(mission_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runner_core::model::{EventDraft, EventKind, SignalType};
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    /// Recording emitter for unit tests.
    #[derive(Default)]
    struct Capture {
        appended: StdMutex<Vec<AppendedEvent>>,
        inbox: StdMutex<Vec<InboxUpdate>>,
        watermark: StdMutex<Vec<WatermarkUpdate>>,
    }

    impl BusEmitter for Capture {
        fn appended(&self, ev: &AppendedEvent) {
            self.appended.lock().unwrap().push(ev.clone());
        }
        fn inbox_updated(&self, ev: &InboxUpdate) {
            self.inbox.lock().unwrap().push(ev.clone());
        }
        fn watermark_advanced(&self, ev: &WatermarkUpdate) {
            self.watermark.lock().unwrap().push(ev.clone());
        }
    }

    fn signal(from: &str, ty: &str, payload: serde_json::Value) -> EventDraft {
        EventDraft {
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Signal,
            from: from.into(),
            to: None,
            signal_type: Some(SignalType::new(ty)),
            payload,
        }
    }

    fn message(from: &str, to: Option<&str>, text: &str) -> EventDraft {
        EventDraft {
            crew_id: "crew".into(),
            mission_id: "mission".into(),
            kind: EventKind::Message,
            from: from.into(),
            to: to.map(String::from),
            signal_type: None,
            payload: serde_json::json!({ "text": text }),
        }
    }

    /// Wait until `pred` returns true or `deadline_ms` elapses. Returns true
    /// on success, false on timeout. Used to give notify time to deliver.
    fn wait_until<F: FnMut() -> bool>(deadline_ms: u64, mut pred: F) -> bool {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        pred()
    }

    fn fresh_mission_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn cap_dyn(cap: &Arc<Capture>) -> Arc<dyn BusEmitter> {
        Arc::clone(cap) as Arc<dyn BusEmitter>
    }

    #[test]
    fn replay_existing_events_on_mount() {
        // Pre-seed the log, then mount the bus. Every existing event must
        // surface as an `appended` emission so the UI can rehydrate after
        // a reopen.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(signal("system", "mission_start", serde_json::json!({})))
            .unwrap();
        log.append(signal(
            "human",
            "mission_goal",
            serde_json::json!({ "text": "ship it" }),
        ))
        .unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();

        wait_until(1000, || cap.appended.lock().unwrap().len() == 2);
        let appended = cap.appended.lock().unwrap();
        assert_eq!(appended.len(), 2);
        assert_eq!(
            appended[0].event.signal_type.as_ref().unwrap().as_str(),
            "mission_start"
        );
        assert_eq!(
            appended[1].event.signal_type.as_ref().unwrap().as_str(),
            "mission_goal"
        );
    }

    #[test]
    fn watcher_observes_appends_after_mount() {
        // Mount on an empty log, then append. The notify watcher should
        // wake the consumer and emit `appended`. notify is timing-sensitive
        // so we poll within a generous window.
        let dir = fresh_mission_dir();
        std::fs::create_dir_all(dir.path()).unwrap();
        // Touch the log so the watcher has something to watch from the start.
        let log = EventLog::open(dir.path()).unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();
        // Wait for the empty initial replay to settle.
        wait_until(200, || true);

        log.append(message("lead", None, "broadcast hi")).unwrap();
        log.append(message("lead", Some("impl"), "directed to impl"))
            .unwrap();

        // Recv-timeout in the consumer is 500ms; allow up to 3s for slow CI.
        let arrived = wait_until(3000, || cap.appended.lock().unwrap().len() == 2);
        assert!(arrived, "watcher never observed the appended events");
    }

    #[test]
    fn projection_includes_broadcasts_and_directed_only_for_target() {
        // Two runners on roster. A broadcast event must inbox into both;
        // a directed event must inbox into only the addressee. The other
        // runner sees nothing for the directed event.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(message("lead", None, "broadcast")).unwrap(); // both
        log.append(message("lead", Some("impl"), "to impl"))
            .unwrap(); // impl only

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string(), "impl".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();

        wait_until(1000, || cap.inbox.lock().unwrap().len() >= 3);
        let inbox = cap.inbox.lock().unwrap().clone();

        let lead_updates: Vec<_> = inbox.iter().filter(|u| u.runner_handle == "lead").collect();
        let impl_updates: Vec<_> = inbox.iter().filter(|u| u.runner_handle == "impl").collect();

        // Lead sees only the broadcast. Impl sees broadcast + directed.
        assert_eq!(
            lead_updates.len(),
            1,
            "lead should only inbox the broadcast"
        );
        assert_eq!(
            impl_updates.len(),
            2,
            "impl should inbox both broadcast and directed"
        );
        assert_eq!(impl_updates.last().unwrap().unread_count, 2);
    }

    #[test]
    fn inbox_read_advances_watermark_and_clears_unread() {
        // Append a broadcast event, then an `inbox_read` from "lead" with
        // up_to = that event's id. The watermark must move forward and
        // unread_count must drop to zero.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        let bcast = log.append(message("lead", None, "broadcast")).unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();
        wait_until(1000, || !cap.inbox.lock().unwrap().is_empty());

        // Now post the inbox_read signal.
        log.append(signal(
            "lead",
            "inbox_read",
            serde_json::json!({ "up_to": bcast.id }),
        ))
        .unwrap();

        wait_until(3000, || !cap.watermark.lock().unwrap().is_empty());
        let wm = cap.watermark.lock().unwrap().clone();
        assert_eq!(wm.len(), 1, "exactly one watermark/advanced");
        assert_eq!(wm[0].runner_handle, "lead");
        assert_eq!(wm[0].watermark, bcast.id);
        assert_eq!(wm[0].unread_count, 0);

        // The corresponding inbox/updated should also reflect zero unread.
        let inbox = cap.inbox.lock().unwrap();
        let last = inbox.last().unwrap();
        assert_eq!(last.runner_handle, "lead");
        assert_eq!(last.unread_count, 0);
    }

    #[test]
    fn redundant_inbox_read_emits_no_watermark_event() {
        // Reading up to the same ULID twice should be a no-op the second
        // time. Without this guard, every CLI heartbeat would spam the UI.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        let m = log.append(message("lead", None, "x")).unwrap();
        log.append(signal(
            "lead",
            "inbox_read",
            serde_json::json!({ "up_to": m.id }),
        ))
        .unwrap();
        log.append(signal(
            "lead",
            "inbox_read",
            serde_json::json!({ "up_to": m.id }),
        ))
        .unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();

        wait_until(1000, || cap.appended.lock().unwrap().len() == 3);
        // Even though we appended three events, only one watermark advance
        // should have occurred (the second inbox_read is a no-op).
        let wm = cap.watermark.lock().unwrap();
        assert_eq!(wm.len(), 1, "duplicate inbox_read must not re-emit");
    }

    #[test]
    fn registry_mount_unmount_drops_consumer_thread() {
        // Mount, append, observe; then unmount. After unmount, further
        // appends must NOT produce more emissions because the consumer is
        // gone.
        let dir = fresh_mission_dir();
        let _log_init = EventLog::open(dir.path()).unwrap();

        let cap: Arc<Capture> = Arc::new(Capture::default());
        let registry = BusRegistry::new();
        registry
            .mount(
                "mission".into(),
                dir.path(),
                &["lead".to_string()],
                cap_dyn(&cap),
            )
            .unwrap();

        let log = EventLog::open(dir.path()).unwrap();
        log.append(message("lead", None, "first")).unwrap();
        wait_until(3000, || cap.appended.lock().unwrap().len() == 1);
        assert_eq!(cap.appended.lock().unwrap().len(), 1);

        registry.unmount("mission");
        // The consumer must have stopped; new appends should not surface.
        log.append(message("lead", None, "second")).unwrap();
        // Wait long enough that any in-flight notify event would have been
        // consumed if the bus were still alive.
        thread::sleep(Duration::from_millis(800));
        assert_eq!(
            cap.appended.lock().unwrap().len(),
            1,
            "no events should arrive after unmount"
        );
    }

    #[test]
    fn signals_never_enter_inbox_projection() {
        // Regression for review finding #1: inbox is messages-only per arch
        // §2.7 (`kind = "message" AND (to = null OR to = h)`). Signals with
        // no `to` would otherwise show up as unread for every roster member,
        // so a brand-new mission would start with a bogus unread count from
        // its own `mission_start` + `mission_goal` opening events.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(signal("system", "mission_start", serde_json::json!({})))
            .unwrap();
        log.append(signal(
            "human",
            "mission_goal",
            serde_json::json!({ "text": "ship it" }),
        ))
        .unwrap();
        log.append(signal(
            "coder",
            "ask_lead",
            serde_json::json!({ "question": "?" }),
        ))
        .unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string(), "coder".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();

        wait_until(1000, || cap.appended.lock().unwrap().len() == 3);
        assert_eq!(
            cap.appended.lock().unwrap().len(),
            3,
            "all signals appended"
        );
        // Critical: no inbox_updated emissions for these signals.
        assert!(
            cap.inbox.lock().unwrap().is_empty(),
            "signals must not project into inboxes; got {:?}",
            cap.inbox.lock().unwrap()
        );

        // Now post a real message — it must inbox normally for both runners.
        log.append(message("lead", None, "broadcast")).unwrap();
        wait_until(3000, || cap.inbox.lock().unwrap().len() == 2);
        assert_eq!(
            cap.inbox.lock().unwrap().len(),
            2,
            "broadcast message should inbox for both lead and coder"
        );
    }

    #[test]
    fn inbox_read_with_non_ulid_up_to_is_dropped() {
        // Regression: a junk `up_to` like "zzzz" sorts past every real
        // ULID lexically; without ULID validation, the bus would mark
        // every existing inbox entry as read and hide every future one.
        // The signal must be silently dropped so the watermark stays put.
        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(message("lead", None, "real broadcast")).unwrap();
        log.append(signal(
            "lead",
            "inbox_read",
            serde_json::json!({ "up_to": "zzzz" }),
        ))
        .unwrap();

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();

        wait_until(1000, || cap.appended.lock().unwrap().len() == 2);
        // The bad inbox_read must not produce a watermark advance.
        let wm = cap.watermark.lock().unwrap();
        assert!(
            wm.is_empty(),
            "junk up_to must not advance the watermark; got {wm:?}"
        );
        // The real broadcast still got inboxed; unread_count should be 1.
        let inbox = cap.inbox.lock().unwrap();
        assert_eq!(inbox.last().unwrap().unread_count, 1);
    }

    #[test]
    fn malformed_line_is_skipped_with_warning() {
        // Regression for the v0-mvp-tests.md C7 contract: one bad line
        // doesn't poison the bus. Without `read_from_lossy`, the consumer
        // would re-tick the same offset forever and never deliver the good
        // event after the corruption.
        use std::io::Write;

        let dir = fresh_mission_dir();
        let log = EventLog::open(dir.path()).unwrap();
        log.append(message("lead", None, "first")).unwrap();

        // Hand-write a malformed line directly. A real writer would never
        // produce this, but a buggy CLI release on PATH could.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.path().join("events.ndjson"))
                .unwrap();
            f.write_all(b"this is not json\n").unwrap();
        }

        let cap = Arc::new(Capture::default());
        let _bus = EventBus::for_mission(
            "mission".into(),
            dir.path(),
            &["lead".to_string()],
            cap_dyn(&cap),
        )
        .unwrap();
        // Wait for initial replay to flush.
        wait_until(1000, || cap.appended.lock().unwrap().len() == 1);

        // Append another good event AFTER the bad line. The bus must
        // surface it — proving the corruption didn't freeze the offset.
        log.append(message("lead", None, "second")).unwrap();

        let arrived = wait_until(3000, || cap.appended.lock().unwrap().len() == 2);
        assert!(
            arrived,
            "bus must skip past the bad line and deliver later events"
        );
        let appended = cap.appended.lock().unwrap();
        assert_eq!(appended[0].event.payload["text"], "first");
        assert_eq!(appended[1].event.payload["text"], "second");
    }

    #[test]
    fn unmount_drains_pending_writes_before_exiting() {
        // Regression for review finding #2: writes that landed just before
        // `unmount` must surface via `event/appended` even if the consumer
        // hadn't ticked them yet. Simulates `mission_stop`'s real ordering:
        // append the terminal event, then call unmount immediately.
        let dir = fresh_mission_dir();
        let _log_init = EventLog::open(dir.path()).unwrap();

        let cap = Arc::new(Capture::default());
        let registry = BusRegistry::new();
        registry
            .mount(
                "mission".into(),
                dir.path(),
                &["lead".to_string()],
                cap_dyn(&cap),
            )
            .unwrap();
        // Let the initial replay settle.
        wait_until(200, || true);

        let log = EventLog::open(dir.path()).unwrap();
        log.append(signal("system", "mission_stopped", serde_json::json!({})))
            .unwrap();
        // Mirror mission_stop: unmount immediately after the append. The
        // consumer must do one final tick on its way out.
        registry.unmount("mission");

        let appended = cap.appended.lock().unwrap();
        assert!(
            appended.iter().any(|a| a
                .event
                .signal_type
                .as_ref()
                .map(|t| t.as_str() == "mission_stopped")
                .unwrap_or(false)),
            "terminal event must surface before bus tears down; got {:?}",
            appended
                .iter()
                .map(|a| a.event.signal_type.as_ref().map(|t| t.as_str().to_string()))
                .collect::<Vec<_>>()
        );
    }
}
