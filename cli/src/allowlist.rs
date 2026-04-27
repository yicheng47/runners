// Read the per-crew signal-types allowlist that `mission_start` writes
// to `$APPDATA/runner/crews/{crew_id}/signal_types.json`. The CLI uses
// this to reject `runner signal <type>` invocations whose type isn't on
// the list (arch §5.3 Layer 2).
//
// Where it lives: two directories above the mission's events log. The
// CLI receives the events-log path via `RUNNER_EVENT_LOG`, which is
// `$APPDATA/runner/crews/{crew_id}/missions/{mission_id}/events.ndjson`.
// `signal_types.json` lives at the crew level (one per crew, shared
// across that crew's missions), so we walk up:
//   events.ndjson → mission_dir → crew_dir → signal_types.json

use std::path::{Path, PathBuf};

/// Locate the allowlist sidecar from the events-log path.
pub fn sidecar_path(event_log: &Path) -> Option<PathBuf> {
    // events.ndjson → missions/{id}/ → crews/{crew_id}/
    let crew_dir = event_log.parent()?.parent()?.parent()?;
    Some(crew_dir.join("signal_types.json"))
}

/// Load the allowlist, or `None` if the sidecar is missing/unreadable.
/// Missing-sidecar is treated permissively (the CLI prints a warning
/// and accepts any signal type) only because losing the sidecar should
/// not silently strand a running mission. The mission_start path always
/// writes it; a missing file means something else is broken.
pub fn load(event_log: &Path) -> Option<Vec<String>> {
    let path = sidecar_path(event_log)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<Vec<String>>(&bytes).ok()
}

/// Returns true if `ty` is allowed. If the allowlist can't be read, we
/// permit (best-effort) but log to stderr so the operator notices.
pub fn is_allowed(event_log: &Path, ty: &str) -> bool {
    match load(event_log) {
        Some(list) => list.iter().any(|t| t == ty),
        None => {
            eprintln!(
                "runner: signal_types sidecar missing/unreadable; allowing {ty:?} permissively. \
                 Re-run mission_start to restore the sidecar."
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_walks_up_three_levels() {
        let log = Path::new("/data/runner/crews/CREW01/missions/MISS01/events.ndjson");
        assert_eq!(
            sidecar_path(log).unwrap(),
            PathBuf::from("/data/runner/crews/CREW01/signal_types.json"),
        );
    }

    #[test]
    fn load_returns_some_for_valid_json_array() {
        let dir = tempfile::tempdir().unwrap();
        let crew = dir.path().join("crews/C");
        let mission = crew.join("missions/M");
        std::fs::create_dir_all(&mission).unwrap();
        std::fs::write(
            crew.join("signal_types.json"),
            br#"["mission_goal","ask_lead"]"#,
        )
        .unwrap();
        let log = mission.join("events.ndjson");
        let list = load(&log).unwrap();
        assert_eq!(
            list,
            vec!["mission_goal".to_string(), "ask_lead".to_string()]
        );
    }

    #[test]
    fn is_allowed_permits_when_sidecar_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.ndjson");
        // No sidecar at all — permissive fallback. Stderr noise is fine
        // for a buggy parent setup.
        assert!(is_allowed(&log, "anything"));
    }
}
