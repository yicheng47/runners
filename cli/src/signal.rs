// `runner signal <type> [--payload <json>]` and the `runner status` sugar
// wrapper. Both append a single `EventDraft::signal` line to the
// mission's NDJSON event log via `runner_core::event_log::EventLog`.
//
// Per arch §5.2, signals always carry `to: null`; per-target routing
// lives in `payload.target` (only `human_said` uses this in v0). The CLI
// preserves this — `--to` is intentionally not exposed for `signal`.

use runner_core::event_log::EventLog;
use runner_core::model::{EventDraft, EventKind, SignalType};

use crate::{allowlist, env};

pub fn run(ty: &str, payload: Option<&str>) -> i32 {
    let Some(env) = env::require_mission_or_handle_offbus("signal") else {
        return 0; // unreachable in practice; the helper exits or returns Some.
    };

    if !allowlist::is_allowed(&env.event_log, ty) {
        eprintln!(
            "runner signal: {ty:?} is not in the crew's signal_types allowlist. \
             Edit the crew's signal types in the app, then re-run mission_start \
             to refresh the sidecar."
        );
        return 1;
    }

    let payload_value = match payload {
        Some(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("runner signal: --payload is not valid JSON: {e}");
                return 1;
            }
        },
        None => serde_json::json!({}),
    };

    append(&env, ty, payload_value)
}

/// `runner status busy|idle [--note <text>]` — emits a `runner_status`
/// signal with the validated state. Validation lives here so the CLI can
/// reject typos like `runner status sleeping` before they hit the log.
pub fn run_status(state: &str, note: Option<&str>) -> i32 {
    let normalized = match state {
        "busy" | "idle" => state,
        other => {
            eprintln!("runner status: state must be \"busy\" or \"idle\"; got {other:?}");
            return 1;
        }
    };
    let mut payload = serde_json::Map::new();
    payload.insert(
        "state".into(),
        serde_json::Value::String(normalized.to_string()),
    );
    if let Some(n) = note {
        payload.insert("note".into(), serde_json::Value::String(n.to_string()));
    }
    let value = serde_json::Value::Object(payload);

    let Some(env) = env::require_mission_or_handle_offbus("status") else {
        return 0;
    };
    // `runner_status` is in the default allowlist (see C1 seed) but we
    // still go through the same check so a user who removed it sees the
    // same rejection path as any other signal.
    if !allowlist::is_allowed(&env.event_log, "runner_status") {
        eprintln!("runner status: runner_status is not in the crew's signal_types allowlist.");
        return 1;
    }
    append(&env, "runner_status", value)
}

fn append(env: &env::MissionEnv, ty: &str, payload: serde_json::Value) -> i32 {
    // `EventLog::open` recreates the dir if needed; here it just resolves
    // the existing mission_dir. The flock + ULID floor logic lives there.
    let Some(mission_dir) = env.event_log.parent() else {
        eprintln!(
            "runner: RUNNER_EVENT_LOG has no parent directory: {}",
            env.event_log.display()
        );
        return 2;
    };
    let log = match EventLog::open(mission_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("runner: failed to open event log: {e}");
            return 1;
        }
    };

    let draft = EventDraft {
        crew_id: env.crew_id.clone(),
        mission_id: env.mission_id.clone(),
        kind: EventKind::Signal,
        from: env.handle.clone(),
        to: None,
        signal_type: Some(SignalType::new(ty)),
        payload,
    };
    match log.append(draft) {
        Ok(_ev) => 0,
        Err(e) => {
            eprintln!("runner: append failed: {e}");
            1
        }
    }
}
