// Filesystem layout helpers for per-mission event logs.
//
// Arch §7.2 pins the tree as:
//   $APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson
//
// Callers pass the resolved `app_data` dir (whatever `tauri::Manager::path()`
// returned at startup, or a tempdir in tests) so this module stays platform-
// agnostic.

use std::path::{Path, PathBuf};

pub fn crew_dir(app_data: &Path, crew_id: &str) -> PathBuf {
    app_data.join("crews").join(crew_id)
}

pub fn mission_dir(app_data: &Path, crew_id: &str, mission_id: &str) -> PathBuf {
    crew_dir(app_data, crew_id)
        .join("missions")
        .join(mission_id)
}

pub fn events_path(app_data: &Path, crew_id: &str, mission_id: &str) -> PathBuf {
    mission_dir(app_data, crew_id, mission_id).join(super::log::EVENTS_FILENAME)
}

pub fn signal_types_path(app_data: &Path, crew_id: &str) -> PathBuf {
    crew_dir(app_data, crew_id).join("signal_types.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn layout_matches_arch_section_7_2() {
        let root = PathBuf::from("/tmp/rtest");
        assert_eq!(
            events_path(&root, "C", "M"),
            PathBuf::from("/tmp/rtest/crews/C/missions/M/events.ndjson")
        );
        assert_eq!(
            signal_types_path(&root, "C"),
            PathBuf::from("/tmp/rtest/crews/C/signal_types.json")
        );
    }
}
