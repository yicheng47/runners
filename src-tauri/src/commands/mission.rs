// Mission lifecycle — start, stop, list, get.
//
// A mission is the runtime container: it owns a directory, an NDJSON event
// log, and a set of sessions (spawned in C6). This module only does the
// bookkeeping layer — no PTYs yet.
//
// `mission_start` is the point where config crystallizes into runtime:
// validate the crew has ≥1 runner and exactly one lead, create the mission
// row, create the mission directory, export the crew's `signal_types`
// allowlist to a sidecar file for the CLI to read (arch §5.3 Layer 2), and
// emit the two opening events — `mission_start` (system announces the run)
// and `mission_goal` (the human's intent, which the orchestrator routes to
// the lead via the built-in rule in C8).

use std::path::{Path, PathBuf};

use chrono::Utc;
use runners_core::event_log::{self, EventLog};
use runners_core::model::{EventDraft, EventKind, SignalType};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use tauri::State;
use ulid::Ulid as UlidGen;

use crate::{
    commands::{crew, runner},
    error::{Error, Result},
    model::{Mission, MissionStatus, Timestamp},
    AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct StartMissionInput {
    pub crew_id: String,
    pub title: String,
    /// Optional override of the crew's default goal. When `None`, the crew's
    /// `goal` column is used; if that is also unset the mission starts with
    /// an empty-goal event (valid — the human may post a `human_said` signal
    /// later instead of setting a goal up front).
    #[serde(default)]
    pub goal_override: Option<String>,
    /// Working directory exposed to every session as `$MISSION_CWD`.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartMissionOutput {
    pub mission: Mission,
    /// Effective goal (override if present, else crew default, else empty).
    /// The frontend uses this to render the first event in the workspace
    /// without making a second round-trip.
    pub goal: String,
}

fn new_id() -> String {
    UlidGen::new().to_string()
}

fn now() -> Timestamp {
    Utc::now()
}

fn row_to_mission(row: &Row<'_>) -> rusqlite::Result<Mission> {
    let status: String = row.get("status")?;
    let started_at: String = row.get("started_at")?;
    let stopped_at: Option<String> = row.get("stopped_at")?;

    let status = match status.as_str() {
        "running" => MissionStatus::Running,
        "completed" => MissionStatus::Completed,
        "aborted" => MissionStatus::Aborted,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown mission status {other:?}").into(),
            ))
        }
    };
    let parse_ts = |s: String| -> rusqlite::Result<Timestamp> {
        s.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    };

    Ok(Mission {
        id: row.get("id")?,
        crew_id: row.get("crew_id")?,
        title: row.get("title")?,
        status,
        goal_override: row.get("goal_override")?,
        cwd: row.get("cwd")?,
        started_at: parse_ts(started_at)?,
        stopped_at: stopped_at.map(parse_ts).transpose()?,
    })
}

pub fn list(conn: &Connection, crew_id: Option<&str>) -> Result<Vec<Mission>> {
    let sql = "SELECT id, crew_id, title, status, goal_override, cwd,
                      started_at, stopped_at
                 FROM missions
                 WHERE (?1 IS NULL OR crew_id = ?1)
                 ORDER BY started_at DESC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![crew_id], row_to_mission)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn get(conn: &Connection, id: &str) -> Result<Mission> {
    conn.query_row(
        "SELECT id, crew_id, title, status, goal_override, cwd,
                started_at, stopped_at
           FROM missions WHERE id = ?1",
        params![id],
        row_to_mission,
    )
    .optional()?
    .ok_or_else(|| Error::msg(format!("mission not found: {id}")))
}

pub fn start(
    conn: &Connection,
    app_data_dir: &Path,
    input: StartMissionInput,
) -> Result<StartMissionOutput> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(Error::msg("mission title must not be empty"));
    }

    // Validate crew exists and is launchable.
    let crew = crew::get(conn, &input.crew_id)?;
    let runners = runner::list(conn, &input.crew_id)?;
    if runners.is_empty() {
        return Err(Error::msg(format!(
            "crew {} has no runners; cannot start mission",
            crew.name
        )));
    }
    // DB enforces `UNIQUE(crew_id) WHERE lead = 1` so we only need to check
    // that at least one runner carries the flag.
    if !runners.iter().any(|r| r.lead) {
        return Err(Error::msg(format!(
            "crew {} has no lead runner; cannot start mission",
            crew.name
        )));
    }

    let id = new_id();
    let started_at = now();
    conn.execute(
        "INSERT INTO missions
            (id, crew_id, title, status, goal_override, cwd, started_at)
         VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
        params![
            id,
            crew.id,
            title,
            input.goal_override,
            input.cwd,
            started_at.to_rfc3339(),
        ],
    )?;

    // Create the mission directory and export the signal-types allowlist
    // sidecar. The CLI (C9) reads this file to validate signal types.
    let mission_dir = event_log::mission_dir(app_data_dir, &crew.id, &id);
    std::fs::create_dir_all(&mission_dir)?;
    write_signal_types_sidecar(app_data_dir, &crew.id, &crew.signal_types)?;

    // Effective goal = override || crew default || "".
    let goal_text = input
        .goal_override
        .as_deref()
        .or(crew.goal.as_deref())
        .unwrap_or("")
        .to_string();

    // Open the event log and emit the two opening events.
    let log = EventLog::open(&mission_dir)?;
    log.append(EventDraft {
        crew_id: crew.id.clone(),
        mission_id: id.clone(),
        kind: EventKind::Signal,
        from: "system".into(),
        to: None,
        signal_type: Some(SignalType::new("mission_start")),
        payload: serde_json::json!({
            "title": title,
            "cwd": input.cwd,
        }),
    })?;
    log.append(EventDraft {
        crew_id: crew.id.clone(),
        mission_id: id.clone(),
        kind: EventKind::Signal,
        from: "human".into(),
        to: None,
        signal_type: Some(SignalType::new("mission_goal")),
        payload: serde_json::json!({ "text": goal_text }),
    })?;

    let mission = get(conn, &id)?;
    Ok(StartMissionOutput {
        mission,
        goal: goal_text,
    })
}

pub fn stop(conn: &Connection, app_data_dir: &Path, id: &str) -> Result<Mission> {
    let mission = get(conn, id)?;
    if !matches!(mission.status, MissionStatus::Running) {
        return Err(Error::msg(format!(
            "mission {id} is not running; status = {:?}",
            mission.status
        )));
    }

    let stopped_at = now();
    conn.execute(
        "UPDATE missions
            SET status = 'completed', stopped_at = ?1
          WHERE id = ?2",
        params![stopped_at.to_rfc3339(), id],
    )?;

    let mission_dir = event_log::mission_dir(app_data_dir, &mission.crew_id, id);
    let log = EventLog::open(&mission_dir)?;
    log.append(EventDraft {
        crew_id: mission.crew_id.clone(),
        mission_id: id.to_string(),
        kind: EventKind::Signal,
        from: "system".into(),
        to: None,
        signal_type: Some(SignalType::new("mission_stopped")),
        payload: serde_json::json!({}),
    })?;

    get(conn, id)
}

/// Write the crew's signal-type allowlist to
/// `$APPDATA/runners/crews/{crew_id}/signal_types.json` atomically (tmp +
/// rename) so a crash during write never leaves a half-written file that
/// the CLI would read and reject valid types on.
fn write_signal_types_sidecar(
    app_data_dir: &Path,
    crew_id: &str,
    allowlist: &[SignalType],
) -> Result<()> {
    let target = event_log::signal_types_path(app_data_dir, crew_id);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp: PathBuf = {
        let mut t = target.clone();
        let name = t
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_else(|| "signal_types.json".into());
        let mut owned = name;
        owned.push(".tmp");
        t.set_file_name(owned);
        t
    };
    let json = serde_json::to_string(allowlist)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

#[tauri::command]
pub async fn mission_start(
    state: State<'_, AppState>,
    input: StartMissionInput,
) -> Result<StartMissionOutput> {
    let conn = state.db.get()?;
    start(&conn, &state.app_data_dir, input)
}

#[tauri::command]
pub async fn mission_stop(state: State<'_, AppState>, id: String) -> Result<Mission> {
    let conn = state.db.get()?;
    stop(&conn, &state.app_data_dir, &id)
}

#[tauri::command]
pub async fn mission_list(
    state: State<'_, AppState>,
    crew_id: Option<String>,
) -> Result<Vec<Mission>> {
    let conn = state.db.get()?;
    list(&conn, crew_id.as_deref())
}

#[tauri::command]
pub async fn mission_get(state: State<'_, AppState>, id: String) -> Result<Mission> {
    let conn = state.db.get()?;
    get(&conn, &id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::crew::CreateCrewInput;
    use crate::commands::runner::CreateRunnerInput;
    use crate::db;

    fn pool() -> db::DbPool {
        db::open_in_memory().unwrap()
    }

    fn seed_crew(conn: &Connection, name: &str, goal: Option<&str>) -> String {
        let crew = crew::create(
            conn,
            CreateCrewInput {
                name: name.into(),
                purpose: None,
                goal: goal.map(String::from),
            },
        )
        .unwrap();
        crew.id
    }

    fn add_runner(conn: &mut Connection, crew_id: &str, handle: &str) {
        runner::create(
            conn,
            CreateRunnerInput {
                crew_id: crew_id.into(),
                handle: handle.into(),
                display_name: handle.into(),
                role: "test".into(),
                runtime: "shell".into(),
                command: "/bin/sh".into(),
                args: vec![],
                working_dir: None,
                system_prompt: None,
                env: std::collections::HashMap::new(),
            },
        )
        .unwrap();
    }

    #[test]
    fn start_rejects_crew_with_no_runners() {
        let pool = pool();
        let conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "Empty", None);
        let tmp = tempfile::tempdir().unwrap();

        let err = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "Try".into(),
                goal_override: None,
                cwd: None,
            },
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("no runners"),
            "expected 'no runners' error, got {msg}"
        );
    }

    #[test]
    fn start_rejects_empty_title() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "coder");
        let tmp = tempfile::tempdir().unwrap();

        let err = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "   ".into(),
                goal_override: None,
                cwd: None,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains("title must not be empty"));
    }

    #[test]
    fn start_writes_two_opening_events_and_sidecar() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "Alpha", Some("Ship v0"));
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let out = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "first mission".into(),
                goal_override: None,
                cwd: Some("/tmp/work".into()),
            },
        )
        .unwrap();

        assert_eq!(out.mission.title, "first mission");
        assert_eq!(out.mission.status, MissionStatus::Running);
        assert_eq!(out.goal, "Ship v0");

        // Event log has mission_start + mission_goal.
        let mission_dir = event_log::mission_dir(tmp.path(), &crew_id, &out.mission.id);
        let log = EventLog::open(&mission_dir).unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 2, "expected two opening events");

        let first = &entries[0].event;
        assert_eq!(first.kind, EventKind::Signal);
        assert_eq!(first.from, "system");
        assert_eq!(
            first.signal_type.as_ref().unwrap().as_str(),
            "mission_start"
        );
        assert_eq!(first.payload["title"], "first mission");
        assert_eq!(first.payload["cwd"], "/tmp/work");

        let second = &entries[1].event;
        assert_eq!(second.kind, EventKind::Signal);
        assert_eq!(second.from, "human");
        assert_eq!(
            second.signal_type.as_ref().unwrap().as_str(),
            "mission_goal"
        );
        assert_eq!(second.payload["text"], "Ship v0");
        // mission_goal must sort strictly after mission_start.
        assert!(second.id > first.id);

        // Signal-types sidecar exists with the crew's allowlist.
        let sidecar = event_log::signal_types_path(tmp.path(), &crew_id);
        assert!(sidecar.exists());
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        let types: Vec<String> = serde_json::from_str(&raw).unwrap();
        assert!(types.contains(&"mission_goal".to_string()));
        assert!(types.contains(&"ask_lead".to_string()));
    }

    #[test]
    fn start_override_beats_crew_default_goal() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", Some("default goal"));
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let out = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "m".into(),
                goal_override: Some("override goal".into()),
                cwd: None,
            },
        )
        .unwrap();

        assert_eq!(out.goal, "override goal");
    }

    #[test]
    fn stop_marks_completed_and_appends_event() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let out = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();

        let stopped = stop(&conn, tmp.path(), &out.mission.id).unwrap();
        assert_eq!(stopped.status, MissionStatus::Completed);
        assert!(stopped.stopped_at.is_some());

        let log = EventLog::open(&event_log::mission_dir(
            tmp.path(),
            &crew_id,
            &out.mission.id,
        ))
        .unwrap();
        let entries = log.read_from(0).unwrap();
        assert_eq!(entries.len(), 3, "start + goal + stopped");
        let last = &entries[2].event;
        assert_eq!(
            last.signal_type.as_ref().unwrap().as_str(),
            "mission_stopped"
        );
        assert_eq!(last.from, "system");
    }

    #[test]
    fn stop_rejects_already_stopped_mission() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let out = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();
        stop(&conn, tmp.path(), &out.mission.id).unwrap();

        let err = stop(&conn, tmp.path(), &out.mission.id).unwrap_err();
        assert!(format!("{err}").contains("not running"));
    }

    #[test]
    fn list_filters_by_crew_and_orders_by_started_at_desc() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let a = seed_crew(&conn, "A", None);
        let b = seed_crew(&conn, "B", None);
        add_runner(&mut conn, &a, "lead");
        add_runner(&mut conn, &b, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let m1 = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id: a.clone(),
                title: "first".into(),
                goal_override: Some("x".into()),
                cwd: None,
            },
        )
        .unwrap()
        .mission;
        // Force a distinct started_at.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let m2 = start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id: a.clone(),
                title: "second".into(),
                goal_override: Some("y".into()),
                cwd: None,
            },
        )
        .unwrap()
        .mission;
        start(
            &conn,
            tmp.path(),
            StartMissionInput {
                crew_id: b,
                title: "other crew".into(),
                goal_override: Some("z".into()),
                cwd: None,
            },
        )
        .unwrap();

        let for_a = list(&conn, Some(&a)).unwrap();
        assert_eq!(for_a.len(), 2);
        assert_eq!(for_a[0].id, m2.id, "newest first");
        assert_eq!(for_a[1].id, m1.id);

        let all = list(&conn, None).unwrap();
        assert_eq!(all.len(), 3);
    }
}
