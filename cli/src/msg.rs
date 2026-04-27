// `runner msg post` and `runner msg read`.
//
// Post: append one `EventDraft::message` (broadcast if `--to` is None;
// directed otherwise). The recipient must be in the per-mission roster
// sidecar so typos can't silently land in nobody's inbox.
//
// Read: project the caller's inbox — `kind = "message" AND (to == null
// OR to == handle)` — apply the optional `--since` / `--from` filters,
// print in append order, and (only on success with at least one message
// printed) emit a single `signal inbox_read` with `payload.up_to = max
// printed ULID`. The watermark advance is what C7's `EventBus` consumes
// to clear unread badges.

use runner_core::event_log::EventLog;
use runner_core::model::{Event, EventDraft, EventKind, SignalType};

use crate::{env, roster};

pub fn post(text: &str, to: Option<&str>) -> i32 {
    let Some(env) = env::require_mission_or_handle_offbus("msg post") else {
        return 0;
    };

    if let Some(handle) = to {
        if !roster::is_known(&env.event_log, handle) {
            eprintln!(
                "runner msg post: --to @{handle} is not in this mission's roster. \
                 Use one of the handles printed by `runner msg read --from <handle>` \
                 or check the mission workspace's runner rail."
            );
            return 1;
        }
    }

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
        kind: EventKind::Message,
        from: env.handle.clone(),
        to: to.map(String::from),
        signal_type: None,
        payload: serde_json::json!({ "text": text }),
    };
    match log.append(draft) {
        Ok(_ev) => 0,
        Err(e) => {
            eprintln!("runner: append failed: {e}");
            1
        }
    }
}

pub fn read(since: Option<&str>, from: Option<&str>) -> i32 {
    let Some(env) = env::require_mission_or_handle_offbus("msg read") else {
        return 0;
    };

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

    // Lossy read so a single bad NDJSON line doesn't make `runner msg
    // read` return non-zero in the middle of an otherwise healthy
    // session. Mirrors `event_bus` and `Router::reconstruct_from_log`.
    let entries = match log.read_from_lossy(0) {
        Ok((entries, skipped)) => {
            for skip in &skipped {
                eprintln!(
                    "runner msg read: skipping malformed line at offset {} ({})",
                    skip.offset, skip.error
                );
            }
            entries
        }
        Err(e) => {
            eprintln!("runner msg read: failed to read event log: {e}");
            return 1;
        }
    };

    let mut printed: Vec<&Event> = Vec::new();
    for entry in &entries {
        let ev = &entry.event;
        if !is_inbox(ev, &env.handle) {
            continue;
        }
        if let Some(s) = since {
            if ev.id.as_bytes() <= s.as_bytes() {
                continue;
            }
        }
        if let Some(sender) = from {
            if ev.from != sender {
                continue;
            }
        }
        printed.push(ev);
    }

    for ev in &printed {
        print_message(ev);
    }

    // Emit `inbox_read` only when something was actually shown — an empty
    // inbox shouldn't generate noise (or a redundant watermark) in the
    // log. The bus de-dupes redundant up_to values anyway, but skipping
    // here keeps the log readable.
    if let Some(last) = printed.last() {
        let draft = EventDraft {
            crew_id: env.crew_id.clone(),
            mission_id: env.mission_id.clone(),
            kind: EventKind::Signal,
            from: env.handle.clone(),
            to: None,
            signal_type: Some(SignalType::new("inbox_read")),
            payload: serde_json::json!({ "up_to": last.id }),
        };
        if let Err(e) = log.append(draft) {
            eprintln!("runner msg read: failed to emit inbox_read: {e}");
            // Still exit 0 — the user got their messages; failing to
            // mark them read is a UI-only annoyance.
        }
    }
    0
}

fn is_inbox(ev: &Event, handle: &str) -> bool {
    if !matches!(ev.kind, EventKind::Message) {
        return false;
    }
    match ev.to.as_deref() {
        None => true,
        Some(target) => target == handle,
    }
}

fn print_message(ev: &Event) {
    let to = ev.to.as_deref().unwrap_or("*");
    let text = ev
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    println!(
        "[{ts}] @{from} -> @{to}: {text}",
        ts = ev.ts.to_rfc3339(),
        from = ev.from,
    );
    println!("  id: {}", ev.id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use runner_core::model::Event;

    fn msg(id: &str, from: &str, to: Option<&str>) -> Event {
        Event {
            id: id.to_string(),
            ts: chrono::Utc::now(),
            crew_id: "c".into(),
            mission_id: "m".into(),
            kind: EventKind::Message,
            from: from.into(),
            to: to.map(String::from),
            signal_type: None,
            payload: serde_json::json!({ "text": "hi" }),
        }
    }

    #[test]
    fn inbox_includes_broadcast_and_directed_to_self_only() {
        assert!(is_inbox(&msg("1", "lead", None), "impl"));
        assert!(is_inbox(&msg("1", "lead", Some("impl")), "impl"));
        assert!(!is_inbox(&msg("1", "lead", Some("reviewer")), "impl"));
    }

    #[test]
    fn signals_never_appear_in_inbox() {
        let mut ev = msg("1", "lead", None);
        ev.kind = EventKind::Signal;
        assert!(!is_inbox(&ev, "impl"));
    }
}
