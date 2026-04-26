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

use std::path::Path;

use chrono::Utc;
use runner_core::event_log::{self, EventLog};
use runner_core::model::{EventDraft, EventKind, SignalType};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use tauri::State;
use ulid::Ulid as UlidGen;

use crate::{
    commands::{crew, crew_runner},
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
    conn: &mut Connection,
    app_data_dir: &Path,
    input: StartMissionInput,
) -> Result<StartMissionOutput> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(Error::msg("mission title must not be empty"));
    }

    // Validate crew exists and is launchable.
    let crew = crew::get(conn, &input.crew_id)?;
    let roster = crew_runner::list(conn, &input.crew_id)?;
    if roster.is_empty() {
        return Err(Error::msg(format!(
            "crew {} has no runners; cannot start mission",
            crew.name
        )));
    }
    // DB enforces `one_lead_per_crew` so at most one member is lead; we
    // still check at least one carries the flag (defense in depth for any
    // future path that could leave a crew leaderless).
    if !roster.iter().any(|m| m.lead) {
        return Err(Error::msg(format!(
            "crew {} has no lead runner; cannot start mission",
            crew.name
        )));
    }

    // Everything below is done under a DB transaction so that if any of the
    // filesystem or event-log writes fail, the mission row is rolled back
    // and the operator doesn't see a phantom `running` mission (review
    // finding #1). The sole piece of state that can linger on failure is
    // an empty mission directory — harmless because the ULID is never
    // reused, and the next `mission_start` gets a fresh ID + dir.
    let tx = conn.transaction()?;

    // Arch §2.5: a crew can have at most one live mission at a time
    // (review finding #2). Enforce here inside the tx to avoid a
    // race between the check and the insert.
    let running_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM missions WHERE crew_id = ?1 AND status = 'running'",
        params![crew.id],
        |row| row.get(0),
    )?;
    if running_count > 0 {
        return Err(Error::msg(format!(
            "crew {} already has a live mission; stop it before starting another",
            crew.name
        )));
    }

    let id = new_id();
    let started_at = now();
    tx.execute(
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

    // All log writes succeeded — commit the DB row so the mission becomes
    // visible to list/get only after its startup events are durable.
    let mission = get(&tx, &id)?;
    tx.commit()?;
    Ok(StartMissionOutput {
        mission,
        goal: goal_text,
    })
}

pub fn stop(conn: &mut Connection, app_data_dir: &Path, id: &str) -> Result<Mission> {
    // Mirror `start`: flip status inside a tx and only commit once the
    // terminal `mission_stopped` event has been appended. If the log write
    // fails, the mission stays `running` and the operator can retry.
    let tx = conn.transaction()?;

    // Conditional UPDATE binds the status check and the transition into one
    // atomic SQL statement. Without this, two racing `mission_stop` calls
    // could each observe `running`, both commit `completed`, and both append
    // a `mission_stopped` event (duplicate terminal). With `WHERE status =
    // 'running'`, the slower of the two updates 0 rows and is rejected
    // below, so only one writer ever reaches the log append.
    let stopped_at = now();
    let affected = tx.execute(
        "UPDATE missions
            SET status = 'completed', stopped_at = ?1
          WHERE id = ?2 AND status = 'running'",
        params![stopped_at.to_rfc3339(), id],
    )?;
    if affected == 0 {
        // Either the id doesn't exist or the mission isn't running anymore
        // (a concurrent stop won the race). Fetch for a precise error.
        let mission = get(&tx, id)?;
        return Err(Error::msg(format!(
            "mission {id} is not running; status = {:?}",
            mission.status
        )));
    }

    // Fetch crew_id now that we know the row exists and we own the
    // transition; used for the mission-dir path below.
    let mission = get(&tx, id)?;

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

    tx.commit()?;
    Ok(mission)
}

/// Write the crew's signal-type allowlist to
/// `$APPDATA/runners/crews/{crew_id}/signal_types.json` atomically so a
/// crash during write never leaves a half-written file that the CLI would
/// read and reject valid types on.
///
/// Uses `tempfile::NamedTempFile::persist` for the replace — plain
/// `std::fs::rename` fails on Windows when the destination exists, which
/// would break every mission start after the first for a given crew.
fn write_signal_types_sidecar(
    app_data_dir: &Path,
    crew_id: &str,
    allowlist: &[SignalType],
) -> Result<()> {
    use std::io::Write;

    let target = event_log::signal_types_path(app_data_dir, crew_id);
    let parent = target
        .parent()
        .ok_or_else(|| Error::msg("signal_types.json path has no parent"))?;
    std::fs::create_dir_all(parent)?;

    // tempfile places the temp file in the same directory so the rename is
    // intra-filesystem (required for atomicity on Unix) and uses
    // `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` under the hood on Windows.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    let json = serde_json::to_vec(allowlist)?;
    tmp.write_all(&json)?;
    tmp.flush()?;
    tmp.persist(&target).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

#[tauri::command]
pub async fn mission_start(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    input: StartMissionInput,
) -> Result<StartMissionOutput> {
    use crate::event_bus::{BusEmitter, TauriBusEvents};
    use crate::session::manager::{SessionEvents, TauriSessionEvents};
    use std::sync::Arc;

    let out = {
        let mut conn = state.db.get()?;
        start(&mut conn, &state.app_data_dir, input)?
    };

    // Mission row + opening events are durable. Now spawn one PTY per
    // runner. This loop is **all-or-nothing**: if any spawn fails we kill
    // the sessions we already created, flip the mission to `aborted`, and
    // return the error. Without this the caller could see "err" while the
    // crew still has half a live mission that blocks future starts via
    // the one-live-mission-per-crew invariant.
    //
    // Post-C5.5a the roster lives in `crew_runners`, so we join through it
    // instead of listing global runners.
    let roster = {
        let conn = state.db.get()?;
        crew_runner::list(&conn, &out.mission.crew_id)?
    };
    let events_log_path =
        event_log::events_path(&state.app_data_dir, &out.mission.crew_id, &out.mission.id);

    // Mount the event-bus watcher *before* spawning sessions. The opening
    // events are already on disk (start() emitted them under the same DB
    // tx), so the bus's initial replay will pick up `mission_start` and
    // `mission_goal` and surface them to the UI. Mounting before spawn
    // also means anything a runner writes to the log on startup is
    // observed without a race against the watcher attaching.
    let mission_dir =
        event_log::mission_dir(&state.app_data_dir, &out.mission.crew_id, &out.mission.id);
    let roster_handles: Vec<String> = roster.iter().map(|m| m.runner.handle.clone()).collect();
    let bus_emitter: Arc<dyn BusEmitter> = Arc::new(TauriBusEvents(app.clone()));
    if let Err(e) = state.buses.mount(
        out.mission.id.clone(),
        &mission_dir,
        &roster_handles,
        bus_emitter,
    ) {
        // Roll back the mission row so the crew isn't stuck behind a
        // phantom `running` if the watcher couldn't attach.
        if let Ok(conn) = state.db.get() {
            let _ = conn.execute(
                "UPDATE missions
                    SET status = 'aborted', stopped_at = ?1
                  WHERE id = ?2",
                rusqlite::params![Utc::now().to_rfc3339(), out.mission.id],
            );
        }
        return Err(e);
    }

    let emitter: Arc<dyn SessionEvents> = Arc::new(TauriSessionEvents(app.clone()));
    for member in roster {
        let spawn_res = state.sessions.spawn(
            &out.mission,
            &member.runner,
            &state.app_data_dir,
            events_log_path.clone(),
            state.db.clone(),
            Arc::clone(&emitter),
        );
        if let Err(e) = spawn_res {
            // Rollback: kill the sessions that did start, drop the bus,
            // mark the mission aborted so the crew isn't stuck behind a
            // phantom `running`, then surface the original spawn error.
            let _ = state.sessions.kill_all_for_mission(&out.mission.id);
            state.buses.unmount(&out.mission.id);
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute(
                    "UPDATE missions
                        SET status = 'aborted', stopped_at = ?1
                      WHERE id = ?2",
                    rusqlite::params![Utc::now().to_rfc3339(), out.mission.id],
                );
            }
            return Err(e);
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn mission_stop(state: State<'_, AppState>, id: String) -> Result<Mission> {
    // Kill first, then flip the mission row. `kill_all_for_mission` blocks
    // until every reader thread has joined — which means every child has
    // been reaped and every `sessions` row has reached a terminal status.
    // Only then is it honest to call the mission `completed`.
    state.sessions.kill_all_for_mission(&id)?;
    let mut conn = state.db.get()?;
    let mission = stop(&mut conn, &state.app_data_dir, &id)?;
    // Drop the bus *after* the terminal `mission_stopped` event is on
    // disk, so the watcher gets one last tick and clients see it before
    // the bus tears down. unmount() is idempotent and never fails.
    state.buses.unmount(&id);
    Ok(mission)
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
    use crate::commands::runner::{self as runner_cmd, CreateRunnerInput};
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
        // C5.5: runners are global; membership goes through crew_runners.
        let r = runner_cmd::create(
            conn,
            CreateRunnerInput {
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
        crew_runner::add_runner(conn, crew_id, &r.id).unwrap();
    }

    #[test]
    fn start_rejects_crew_with_no_runners() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "Empty", None);
        let tmp = tempfile::tempdir().unwrap();

        let err = start(
            &mut conn,
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
            &mut conn,
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
            &mut conn,
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
            &mut conn,
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
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();

        let stopped = stop(&mut conn, tmp.path(), &out.mission.id).unwrap();
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
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();
        stop(&mut conn, tmp.path(), &out.mission.id).unwrap();

        let err = stop(&mut conn, tmp.path(), &out.mission.id).unwrap_err();
        assert!(format!("{err}").contains("not running"));
    }

    #[test]
    fn list_filters_by_crew_and_orders_by_started_at_desc() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let a = seed_crew(&conn, "A", None);
        let b = seed_crew(&conn, "B", None);
        // C5.5: handles are globally unique — give each crew a distinct one.
        add_runner(&mut conn, &a, "lead-a");
        add_runner(&mut conn, &b, "lead-b");
        let tmp = tempfile::tempdir().unwrap();

        let m1 = start(
            &mut conn,
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
        // One-live-mission-per-crew rule: stop the first before starting the second.
        stop(&mut conn, tmp.path(), &m1.id).unwrap();
        // Force a distinct started_at.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let m2 = start(
            &mut conn,
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
            &mut conn,
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

    #[test]
    fn start_rejects_second_live_mission_on_same_crew() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "first".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();

        let err = start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id,
                title: "second".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("already has a live mission"),
            "expected one-live-mission error, got {err}"
        );
    }

    #[test]
    fn sidecar_is_rewritten_on_second_start_for_same_crew() {
        // Regression for the Windows rename-over-existing issue. On Unix the
        // test passes trivially; on Windows it previously failed because
        // `std::fs::rename` errors when the destination exists.
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = tempfile::tempdir().unwrap();

        let out = start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m1".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();
        stop(&mut conn, tmp.path(), &out.mission.id).unwrap();

        // Sidecar now exists — starting the next mission must overwrite it.
        start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m2".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();

        let sidecar = event_log::signal_types_path(tmp.path(), &crew_id);
        assert!(sidecar.exists());
        let types: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert!(types.contains(&"mission_goal".to_string()));
    }

    #[test]
    fn concurrent_stop_appends_exactly_one_terminal_event() {
        // Two threads race to stop the same running mission. Without the
        // conditional UPDATE, both would see `running`, both would flip the
        // row, and both would append `mission_stopped`. With it, exactly one
        // UPDATE affects a row and exactly one log append happens.
        use std::sync::Arc;
        use std::thread;

        // The default `pool()` helper caps at 1 connection + :memory: which
        // gives each connection its own isolated DB — unusable for a race.
        // Use a file-backed DB on disk so multiple pool connections share state.
        let db_tmp = tempfile::tempdir().unwrap();
        let db_path = db_tmp.path().join("race.db");
        let pool = db::open_pool(&db_path).unwrap();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");
        let tmp = Arc::new(tempfile::tempdir().unwrap());

        let out = start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap();
        drop(conn); // release our pool handle so both threads can grab one

        let pool_a = pool.clone();
        let pool_b = pool.clone();
        let tmp_a = Arc::clone(&tmp);
        let tmp_b = Arc::clone(&tmp);
        let id = out.mission.id.clone();
        let id_a = id.clone();
        let id_b = id.clone();
        let h1 = thread::spawn(move || {
            let mut conn = pool_a.get().unwrap();
            stop(&mut conn, tmp_a.path(), &id_a)
        });
        let h2 = thread::spawn(move || {
            let mut conn = pool_b.get().unwrap();
            stop(&mut conn, tmp_b.path(), &id_b)
        });
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // Exactly one succeeded and exactly one failed with "not running".
        let (ok_count, err_count) = [&r1, &r2].iter().fold((0, 0), |(o, e), r| match r {
            Ok(_) => (o + 1, e),
            Err(err) => {
                assert!(
                    format!("{err}").contains("not running"),
                    "loser should report not-running, got {err}"
                );
                (o, e + 1)
            }
        });
        assert_eq!((ok_count, err_count), (1, 1));

        // Log has exactly one `mission_stopped` event.
        let log = EventLog::open(&event_log::mission_dir(tmp.path(), &crew_id, &id)).unwrap();
        let stopped_events = log
            .read_from(0)
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.event
                    .signal_type
                    .as_ref()
                    .map(|t| t.as_str() == "mission_stopped")
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(stopped_events, 1, "exactly one terminal event must land");
    }

    #[test]
    fn start_rolls_back_row_when_log_append_fails() {
        // Force `EventLog::open` to fail by giving it an `app_data_dir` that
        // can't be created (we preemptively occupy the path with a regular
        // file so `create_dir_all` bails). The mission row must not survive
        // the failure.
        use std::fs;

        let pool = pool();
        let mut conn = pool.get().unwrap();
        let crew_id = seed_crew(&conn, "A", None);
        add_runner(&mut conn, &crew_id, "lead");

        let tmp = tempfile::tempdir().unwrap();
        // Block the `crews/` subtree by making it a file instead of a dir.
        fs::write(tmp.path().join("crews"), b"blocked").unwrap();

        let err = start(
            &mut conn,
            tmp.path(),
            StartMissionInput {
                crew_id: crew_id.clone(),
                title: "m".into(),
                goal_override: Some("go".into()),
                cwd: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Io(_)),
            "expected IO failure from FS, got {err:?}"
        );

        // No phantom mission.
        let missions = list(&conn, Some(&crew_id)).unwrap();
        assert!(
            missions.is_empty(),
            "mission row must be rolled back; found {missions:?}"
        );
    }
}
