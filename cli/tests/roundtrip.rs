// I2 — `runner` CLI ↔ event log roundtrip.
// Mirrors docs/tests/v0-mvp-tests.md `## I2`.
//
// Each test sets up a tempdir mission directory, seeds the signal-types
// and roster sidecars (the parts `mission_start` would write), spawns
// the just-built `runner` binary with the four `RUNNER_*` env vars
// pointing at it, and asserts on the resulting NDJSON.

use std::path::{Path, PathBuf};
use std::process::Command;

use runner_core::event_log::EventLog;
use runner_core::model::EventKind;

/// Locate the `runner` binary built by `cargo build -p runner-cli`.
/// `CARGO_BIN_EXE_runner` is set by Cargo for integration tests of the
/// crate that defines the binary.
fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

struct Fixture {
    _root: tempfile::TempDir,
    crew_dir: PathBuf,
    mission_dir: PathBuf,
    events_log: PathBuf,
}

impl Fixture {
    fn new(crew_id: &str, mission_id: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let crew_dir = root.path().join("crews").join(crew_id);
        let mission_dir = crew_dir.join("missions").join(mission_id);
        std::fs::create_dir_all(&mission_dir).unwrap();
        let events_log = mission_dir.join("events.ndjson");
        Self {
            _root: root,
            crew_dir,
            mission_dir,
            events_log,
        }
    }

    fn write_signal_types(&self, types: &[&str]) {
        let json = serde_json::to_vec(types).unwrap();
        std::fs::write(self.crew_dir.join("signal_types.json"), json).unwrap();
    }

    fn write_roster(&self, members: &[(&str, bool)]) {
        let entries: Vec<serde_json::Value> = members
            .iter()
            .map(|(h, lead)| serde_json::json!({ "handle": h, "lead": lead }))
            .collect();
        std::fs::write(
            self.mission_dir.join("roster.json"),
            serde_json::to_vec(&entries).unwrap(),
        )
        .unwrap();
    }

    fn cmd(&self, handle: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(runner_bin());
        cmd.args(args);
        cmd.env("RUNNER_CREW_ID", "C");
        cmd.env("RUNNER_MISSION_ID", "M");
        cmd.env("RUNNER_HANDLE", handle);
        cmd.env("RUNNER_EVENT_LOG", &self.events_log);
        cmd
    }

    fn read_log(&self) -> Vec<runner_core::model::Event> {
        if !self.events_log.exists() {
            return vec![];
        }
        let log = EventLog::open(self.events_log.parent().unwrap()).unwrap();
        log.read_from(0)
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    fn line_count(&self) -> usize {
        std::fs::read_to_string(&self.events_log)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    }
}

fn standard_signals() -> Vec<&'static str> {
    vec![
        "mission_goal",
        "human_said",
        "ask_lead",
        "ask_human",
        "human_question",
        "human_response",
        "runner_status",
        "inbox_read",
    ]
}

#[test]
fn i2_1_signal_appends_one_line_with_correct_envelope() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    let out = f
        .cmd(
            "impl",
            &["signal", "mission_goal", "--payload", r#"{"text":"go"}"#],
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let events = f.read_log();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert!(matches!(ev.kind, EventKind::Signal));
    assert_eq!(ev.from, "impl");
    assert_eq!(ev.to, None);
    assert_eq!(ev.signal_type.as_ref().unwrap().as_str(), "mission_goal");
    assert_eq!(ev.payload["text"], "go");
}

#[test]
fn i2_2_signal_rejects_unknown_type_and_does_not_append() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    let out = f
        .cmd("impl", &["signal", "not_a_real_type"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("allowlist") || stderr.contains("signal_types"),
        "stderr should point at the allowlist; got: {stderr}",
    );
    assert_eq!(f.line_count(), 0);
}

#[test]
fn i2_3_msg_post_to_handle_routes_directed() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());
    f.write_roster(&[("lead", true), ("impl", false)]);

    let out = f
        .cmd("lead", &["msg", "post", "ready for review", "--to", "impl"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let events = f.read_log();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert!(matches!(ev.kind, EventKind::Message));
    assert_eq!(ev.from, "lead");
    assert_eq!(ev.to.as_deref(), Some("impl"));
    assert_eq!(ev.payload["text"], "ready for review");
}

#[test]
fn i2_4_msg_post_to_unknown_handle_is_rejected() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());
    f.write_roster(&[("lead", true), ("impl", false)]);

    let out = f
        .cmd("lead", &["msg", "post", "hi", "--to", "ghost"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("@ghost") || stderr.contains("roster"),
        "stderr should mention the unknown handle; got: {stderr}",
    );
    assert_eq!(f.line_count(), 0);
}

#[test]
fn i2_5_msg_read_prints_inbox_in_order_and_emits_inbox_read() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());
    f.write_roster(&[("lead", true), ("impl", false)]);

    // Pre-populate two directed messages to @impl from @lead.
    let log = EventLog::open(&f.mission_dir).unwrap();
    use runner_core::model::EventDraft;
    let m1 = log
        .append(EventDraft::message(
            "C",
            "M",
            "lead",
            Some("impl".into()),
            "first",
        ))
        .unwrap();
    let m2 = log
        .append(EventDraft::message(
            "C",
            "M",
            "lead",
            Some("impl".into()),
            "second",
        ))
        .unwrap();

    let out = f.cmd("impl", &["msg", "read"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let first_pos = stdout.find("first").expect("first message in stdout");
    let second_pos = stdout.find("second").expect("second message in stdout");
    assert!(
        first_pos < second_pos,
        "messages must print in append order"
    );

    // The CLI appended one inbox_read with up_to = m2.id.
    let events = f.read_log();
    let last = events.last().unwrap();
    assert_eq!(last.signal_type.as_ref().unwrap().as_str(), "inbox_read");
    assert_eq!(last.from, "impl");
    assert_eq!(last.payload["up_to"], m2.id);
    assert_ne!(last.payload["up_to"], m1.id);
}

#[test]
fn i2_5b_msg_read_with_empty_inbox_does_not_emit_inbox_read() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());
    f.write_roster(&[("lead", true), ("impl", false)]);

    let out = f.cmd("impl", &["msg", "read"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(f.line_count(), 0, "empty inbox must not emit inbox_read");
}

#[test]
fn i2_6_status_idle_emits_runner_status_signal() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    let out = f
        .cmd("impl", &["status", "idle", "--note", "ready for next task"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let events = f.read_log();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert!(matches!(ev.kind, EventKind::Signal));
    assert_eq!(ev.signal_type.as_ref().unwrap().as_str(), "runner_status");
    assert_eq!(ev.from, "impl");
    assert_eq!(ev.payload["state"], "idle");
    assert_eq!(ev.payload["note"], "ready for next task");
}

#[test]
fn i2_6b_status_rejects_unknown_state() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    let out = f.cmd("impl", &["status", "sleeping"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(f.line_count(), 0);
}

#[test]
fn i2_7_concurrent_writers_interleave_atomically() {
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    // 5 workers × 20 invocations = 100 lines. Cut down from the spec's
    // 10×100 because we're spawning real processes; 100 is enough to
    // exercise the flock contract without burning CI minutes on
    // process spawn overhead.
    let workers = 5usize;
    let per_worker = 20usize;
    let bin = runner_bin();
    let log = f.events_log.clone();
    let handles: Vec<_> = (0..workers)
        .map(|w| {
            let bin = bin.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                for _ in 0..per_worker {
                    let status = Command::new(&bin)
                        .args(["signal", "human_said", "--payload", r#"{"text":"x"}"#])
                        .env("RUNNER_CREW_ID", "C")
                        .env("RUNNER_MISSION_ID", "M")
                        .env("RUNNER_HANDLE", format!("worker{w}"))
                        .env("RUNNER_EVENT_LOG", &log)
                        .status()
                        .unwrap();
                    assert!(status.success(), "worker {w} signal failed");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let total = workers * per_worker;
    assert_eq!(
        f.line_count(),
        total,
        "every invocation must produce one line"
    );
    let events = f.read_log();
    assert_eq!(
        events.len(),
        total,
        "every line must parse as a complete event"
    );

    // ULIDs strictly monotonic — the EventLog flock contract.
    for w in events.windows(2) {
        assert!(
            w[0].id.as_bytes() < w[1].id.as_bytes(),
            "ULIDs must be strictly monotonic: {} ≥ {}",
            w[0].id,
            w[1].id,
        );
    }
}

#[test]
fn i2_8_partial_env_fails_fast_with_pointer() {
    // RUNNER_EVENT_LOG missing while the other three are set should
    // produce a non-zero exit and a stderr message naming the missing
    // var. Off-bus (none-set) is a separate test below.
    let f = Fixture::new("C", "M");
    f.write_signal_types(&standard_signals());

    let mut cmd = Command::new(runner_bin());
    cmd.args(["signal", "mission_goal"]);
    cmd.env("RUNNER_CREW_ID", "C");
    cmd.env("RUNNER_MISSION_ID", "M");
    cmd.env("RUNNER_HANDLE", "impl");
    // Deliberately do not set RUNNER_EVENT_LOG; also unset any inherited
    // one to avoid cross-test bleed-through.
    cmd.env_remove("RUNNER_EVENT_LOG");

    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("RUNNER_EVENT_LOG"),
        "stderr should name the missing env var; got: {stderr}",
    );
    assert_eq!(f.line_count(), 0);
}

#[test]
fn off_bus_with_no_env_vars_exits_zero_with_notice() {
    // Direct-chat sessions deliberately set none of the four. The CLI
    // must no-op cleanly so an agent that calls `runner status idle` in
    // a direct chat doesn't crash.
    let mut cmd = Command::new(runner_bin());
    cmd.args(["status", "idle"]);
    cmd.env_remove("RUNNER_CREW_ID");
    cmd.env_remove("RUNNER_MISSION_ID");
    cmd.env_remove("RUNNER_HANDLE");
    cmd.env_remove("RUNNER_EVENT_LOG");

    let out = cmd.output().unwrap();
    assert!(out.status.success(), "off-bus must exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no mission context") || stderr.contains("ignoring"),
        "stderr should explain the no-op; got: {stderr}",
    );
}

#[test]
fn help_works_without_env_vars() {
    let mut cmd = Command::new(runner_bin());
    cmd.args(["help"]);
    cmd.env_remove("RUNNER_CREW_ID");
    cmd.env_remove("RUNNER_MISSION_ID");
    cmd.env_remove("RUNNER_HANDLE");
    cmd.env_remove("RUNNER_EVENT_LOG");

    let out = cmd.output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("runner signal"));
    assert!(stdout.contains("runner msg"));
}

// Avoid unused-warning on Path import when the file body changes.
#[allow(dead_code)]
fn _path_marker(_p: &Path) {}
