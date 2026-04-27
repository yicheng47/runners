// Read the per-mission roster sidecar that `mission_start` writes to
// `$APPDATA/runner/crews/{crew_id}/missions/{mission_id}/roster.json`.
// The CLI uses it to validate `runner msg post --to <handle>` (I2.4).
//
// Per-mission, not per-crew, so historical missions validate `--to`
// against the roster frozen at mission_start — even if crew membership
// later changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterEntry {
    pub handle: String,
    pub lead: bool,
}

/// Locate the sidecar from the events-log path. The roster.json sits in
/// the same mission directory as events.ndjson.
pub fn sidecar_path(event_log: &Path) -> Option<PathBuf> {
    let mission_dir = event_log.parent()?;
    Some(mission_dir.join("roster.json"))
}

pub fn load(event_log: &Path) -> Option<Vec<RosterEntry>> {
    let path = sidecar_path(event_log)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<Vec<RosterEntry>>(&bytes).ok()
}

/// Returns true if `handle` is in the roster. Missing-sidecar is
/// permissive for the same reason as the allowlist (a missing roster
/// sidecar means something is broken upstream — don't strand the
/// mission). Stderr warning surfaces the gap.
pub fn is_known(event_log: &Path, handle: &str) -> bool {
    match load(event_log) {
        Some(list) => list.iter().any(|r| r.handle == handle),
        None => {
            eprintln!(
                "runner: roster sidecar missing/unreadable; allowing --to @{handle} permissively. \
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
    fn sidecar_path_is_in_same_dir_as_events() {
        let log = Path::new("/data/runner/crews/C/missions/M/events.ndjson");
        assert_eq!(
            sidecar_path(log).unwrap(),
            PathBuf::from("/data/runner/crews/C/missions/M/roster.json"),
        );
    }

    #[test]
    fn load_parses_lead_flag() {
        let dir = tempfile::tempdir().unwrap();
        let mission = dir.path().join("crews/C/missions/M");
        std::fs::create_dir_all(&mission).unwrap();
        std::fs::write(
            mission.join("roster.json"),
            br#"[{"handle":"lead","lead":true},{"handle":"impl","lead":false}]"#,
        )
        .unwrap();
        let log = mission.join("events.ndjson");
        let r = load(&log).unwrap();
        assert_eq!(r.len(), 2);
        assert!(r.iter().any(|e| e.handle == "lead" && e.lead));
        assert!(r.iter().any(|e| e.handle == "impl" && !e.lead));
    }

    #[test]
    fn is_known_permits_when_sidecar_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.ndjson");
        assert!(is_known(&log, "anyone"));
    }
}
