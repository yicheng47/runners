// Resolve the four `RUNNER_*` env vars that locate the caller in the
// coordination bus. Mission sessions set all four (see
// `SessionManager::spawn`); direct-chat sessions set none
// (`spawn_direct`). Anything in between is a bug.
//
// We don't read any other env. PATH manipulation, signal handling, etc.
// are out of scope — the parent process owns those.

use std::path::PathBuf;

const VAR_CREW: &str = "RUNNER_CREW_ID";
const VAR_MISSION: &str = "RUNNER_MISSION_ID";
const VAR_HANDLE: &str = "RUNNER_HANDLE";
const VAR_LOG: &str = "RUNNER_EVENT_LOG";

/// Per-process env after presence resolution.
pub enum BusContext {
    /// All four vars present — proceed normally.
    Mission(MissionEnv),
    /// None of the four set — direct-chat / off-bus session. The verb
    /// caller should print a soft notice on stderr and exit 0 so the
    /// agent process doesn't bail out.
    OffBus,
    /// Some-but-not-all set — the parent process is buggy. Bail with a
    /// pointer at the missing names.
    Partial { missing: Vec<&'static str> },
}

#[derive(Clone, Debug)]
pub struct MissionEnv {
    pub crew_id: String,
    pub mission_id: String,
    pub handle: String,
    pub event_log: PathBuf,
}

pub fn resolve() -> BusContext {
    resolve_from(|name| std::env::var(name).ok())
}

/// Test-friendly variant. Production goes through `resolve()`; tests
/// inject their own lookup so they don't have to mutate the live env.
pub fn resolve_from<F: Fn(&str) -> Option<String>>(get: F) -> BusContext {
    let crew = get(VAR_CREW);
    let mission = get(VAR_MISSION);
    let handle = get(VAR_HANDLE);
    let log = get(VAR_LOG);

    let present = [
        (VAR_CREW, crew.is_some()),
        (VAR_MISSION, mission.is_some()),
        (VAR_HANDLE, handle.is_some()),
        (VAR_LOG, log.is_some()),
    ];
    let count = present.iter().filter(|(_, p)| *p).count();
    if count == 0 {
        return BusContext::OffBus;
    }
    if count < 4 {
        let missing: Vec<&'static str> = present
            .iter()
            .filter(|(_, p)| !*p)
            .map(|(n, _)| *n)
            .collect();
        return BusContext::Partial { missing };
    }

    BusContext::Mission(MissionEnv {
        crew_id: crew.unwrap(),
        mission_id: mission.unwrap(),
        handle: handle.unwrap(),
        event_log: PathBuf::from(log.unwrap()),
    })
}

/// Top-level helper used by every verb except `help`. Encapsulates the
/// "off-bus → notice + exit 0" / "partial → exit 2" boilerplate so the
/// verb implementations stay focused on their actual work.
///
/// Returns `Some(MissionEnv)` to proceed, or `None` if the caller has
/// already exited via the side-channels above.
pub fn require_mission_or_handle_offbus(verb: &str) -> Option<MissionEnv> {
    match resolve() {
        BusContext::Mission(m) => Some(m),
        BusContext::OffBus => {
            // Soft no-op. arch §2.6 + the C8.5 risk: direct-chat sessions
            // are off-bus by design; an agent that didn't read the system
            // prompt and tried `runner status idle` here shouldn't crash.
            eprintln!("runner {verb}: no mission context (RUNNER_* env vars unset); ignoring.");
            std::process::exit(0);
        }
        BusContext::Partial { missing } => {
            eprintln!(
                "runner {verb}: missing required env var(s): {}",
                missing.join(", ")
            );
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |name| map.get(name).map(|s| s.to_string())
    }

    #[test]
    fn all_four_present_resolves_mission() {
        let map = HashMap::from([
            (VAR_CREW, "c"),
            (VAR_MISSION, "m"),
            (VAR_HANDLE, "impl"),
            (VAR_LOG, "/tmp/events.ndjson"),
        ]);
        let BusContext::Mission(env) = resolve_from(lookup(map)) else {
            panic!("expected Mission");
        };
        assert_eq!(env.crew_id, "c");
        assert_eq!(env.mission_id, "m");
        assert_eq!(env.handle, "impl");
        assert_eq!(env.event_log, PathBuf::from("/tmp/events.ndjson"));
    }

    #[test]
    fn none_set_is_off_bus() {
        let map = HashMap::new();
        assert!(matches!(resolve_from(lookup(map)), BusContext::OffBus));
    }

    #[test]
    fn partial_set_lists_missing_vars() {
        let map = HashMap::from([(VAR_CREW, "c"), (VAR_MISSION, "m")]);
        let BusContext::Partial { missing } = resolve_from(lookup(map)) else {
            panic!("expected Partial");
        };
        assert!(missing.contains(&VAR_HANDLE));
        assert!(missing.contains(&VAR_LOG));
        assert_eq!(missing.len(), 2);
    }
}
