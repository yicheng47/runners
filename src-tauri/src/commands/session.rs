// Session Tauri commands — thin wrappers over `session::SessionManager`.
//
// Spawn happens inside `mission_start` (see `commands::mission`), so there's
// no `session_spawn` here. The commands below let the frontend:
//   - list persisted sessions for a mission (including ones that have exited)
//   - inject bytes into a live session's stdin
//   - kill a live session
//
// `session/output` and `session/exit` events flow from the PTY reader threads
// directly via `AppHandle::emit`; the frontend subscribes without going
// through a command.

use std::sync::Arc;

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    commands::runner,
    error::{Error, Result},
    model::{Session, SessionStatus, Timestamp},
    session::manager::{SessionEvents, SpawnedSession, TauriSessionEvents},
    AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    #[serde(flatten)]
    pub session: Session,
    /// Handle of the runner this session instantiates — denormalized so the
    /// frontend can render `@coder`-style labels without a second lookup.
    pub handle: String,
}

fn row_to_session(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    let status: String = row.get("status")?;
    let started_at: Option<String> = row.get("started_at")?;
    let stopped_at: Option<String> = row.get("stopped_at")?;

    let status = match status.as_str() {
        "running" => SessionStatus::Running,
        "stopped" => SessionStatus::Stopped,
        "crashed" => SessionStatus::Crashed,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown session status {other:?}").into(),
            ))
        }
    };
    let parse_ts = |s: String| -> rusqlite::Result<Timestamp> {
        s.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    Ok(SessionRow {
        session: Session {
            id: row.get("id")?,
            mission_id: row.get("mission_id")?,
            runner_id: row.get("runner_id")?,
            cwd: row.get("cwd")?,
            status,
            pid: row.get("pid")?,
            started_at: started_at.map(parse_ts).transpose()?,
            stopped_at: stopped_at.map(parse_ts).transpose()?,
        },
        handle: row.get("handle")?,
    })
}

#[tauri::command]
pub async fn session_list(
    state: State<'_, AppState>,
    mission_id: String,
) -> Result<Vec<SessionRow>> {
    // Order by the crew-scoped position of the runner within this mission's
    // crew, so the UI renders sessions in the same slot order as the Crew
    // Detail roster. `runners` is globally scoped post-C5.5a so we join
    // through `missions` + `crew_runners` to get the crew-local position.
    let conn = state.db.get()?;
    let mut stmt = conn.prepare(
        "SELECT s.id, s.mission_id, s.runner_id, s.cwd, s.status, s.pid,
                s.started_at, s.stopped_at, r.handle
           FROM sessions s
           JOIN runners r ON r.id = s.runner_id
           JOIN missions m ON m.id = s.mission_id
           LEFT JOIN crew_runners cr
                  ON cr.crew_id = m.crew_id AND cr.runner_id = s.runner_id
          WHERE s.mission_id = ?1
          ORDER BY cr.position ASC, s.started_at ASC",
    )?;
    let rows = stmt.query_map(params![mission_id], row_to_session)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[tauri::command]
pub async fn session_inject_stdin(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> Result<()> {
    state.sessions.inject_stdin(&session_id, text.as_bytes())
}

#[tauri::command]
pub async fn session_kill(state: State<'_, AppState>, session_id: String) -> Result<()> {
    state.sessions.kill(&session_id)
}

#[tauri::command]
pub async fn session_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<()> {
    state.sessions.resize(&session_id, cols, rows)
}

/// Spawn a "direct chat" session for a runner — a PTY with no parent
/// mission, no orchestrator, no event log (C8.5). Used by the Runner
/// Detail page's "Chat now" button: the user picks a working directory
/// and gets a one-on-one terminal with the agent's CLI.
///
/// `cwd` defaults to the runner's own `working_dir` when None — that's
/// what the spawn path resolves anyway, but exposing it on the row gives
/// future UI surfaces (session list, recent chats) something to show
/// without a second lookup against the runner config.
#[tauri::command]
pub async fn session_start_direct(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    runner_id: String,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<SpawnedSession> {
    // Look up the runner under a short-lived connection so we don't hold
    // a pool slot across the spawn (which itself grabs a connection to
    // insert the `sessions` row).
    let runner = {
        let conn = state.db.get()?;
        runner::get(&conn, &runner_id)?
    };
    let emitter: Arc<dyn SessionEvents> = Arc::new(TauriSessionEvents(app));
    let spawned = state
        .sessions
        .spawn_direct(
            &runner,
            cwd.as_deref(),
            cols,
            rows,
            &state.app_data_dir,
            state.db.clone(),
            emitter,
        )
        .map_err(|e| Error::msg(format!("session_start_direct: {e}")))?;
    Ok(spawned)
}
