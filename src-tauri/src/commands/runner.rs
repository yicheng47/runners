// Runner CRUD — global scope (C5.5).
//
// A runner is a reusable definition (handle, runtime, command, system
// prompt, ...) that can be referenced by zero or more crews via the
// `crew_runners` join table (see commands/crew_runner.rs). The handle is
// globally unique: @impl means the same runner everywhere it appears in
// the event log.
//
// Lead/position invariants are per-crew and live in crew_runner.rs. This
// module only owns the runner rows themselves.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use tauri::State;
use ulid::Ulid as UlidGen;

use crate::{
    error::{Error, Result},
    model::{Runner, Timestamp},
    AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunnerInput {
    pub handle: String,
    pub display_name: String,
    pub role: String,
    pub runtime: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// `handle` is intentionally excluded from updates: per arch §2.2 and §5.2
// the handle is the runner's identity in events, CLI addressing, and
// policy rules. Renaming after creation would break historical event
// attribution and any persisted policy references. Users who want a
// different handle delete the runner and create a new one.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRunnerInput {
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub runtime: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub working_dir: Option<Option<String>>,
    pub system_prompt: Option<Option<String>>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerActivity {
    pub runner_id: String,
    pub active_sessions: i64,
    pub active_missions: i64,
    pub crew_count: i64,
    pub last_started_at: Option<Timestamp>,
    /// Most recent running direct-chat session for this runner, if any.
    /// Lets the sidebar's SESSION list re-attach to a live PTY across page
    /// reloads — without this, the frontend `activeSessions` map starts
    /// empty on reload and we'd fall back to the runner detail page.
    pub direct_session_id: Option<String>,
}

/// Runner row plus its `RunnerActivity`. Returned by `runner_list_with_activity`
/// so the Runners list page can render every card's badges in one IPC round-
/// trip — without this the page would do N+1 calls (one `runner_list` and
/// one `runner_activity` per row), which also produces a flicker as
/// counters fill in.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerWithActivity {
    #[serde(flatten)]
    pub runner: Runner,
    #[serde(flatten)]
    pub activity: RunnerActivity,
}

fn new_id() -> String {
    UlidGen::new().to_string()
}

fn now() -> Timestamp {
    Utc::now()
}

// Handle validation: lowercase ASCII slug, 1..=32 chars, [a-z0-9] start,
// body [a-z0-9_-]. Matches PRD §4 handle rules.
pub(super) fn validate_handle(handle: &str) -> Result<()> {
    if handle.is_empty() || handle.len() > 32 {
        return Err(Error::msg("runner handle must be 1-32 chars"));
    }
    let bytes = handle.as_bytes();
    let first_ok = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    if !first_ok {
        return Err(Error::msg(
            "runner handle must start with a lowercase letter or digit",
        ));
    }
    for b in bytes {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_';
        if !ok {
            return Err(Error::msg(
                "runner handle must be lowercase letters, digits, '-' or '_'",
            ));
        }
    }
    Ok(())
}

pub(super) fn row_to_runner(row: &Row<'_>) -> rusqlite::Result<Runner> {
    let args_raw: Option<String> = row.get("args_json")?;
    let env_raw: Option<String> = row.get("env_json")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    Ok(Runner {
        id: row.get("id")?,
        handle: row.get("handle")?,
        display_name: row.get("display_name")?,
        role: row.get("role")?,
        runtime: row.get("runtime")?,
        command: row.get("command")?,
        args: match args_raw {
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            None => Vec::new(),
        },
        working_dir: row.get("working_dir")?,
        system_prompt: row.get("system_prompt")?,
        env: match env_raw {
            Some(s) => serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            None => HashMap::new(),
        },
        created_at: created_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: updated_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

pub(super) const SELECT_COLS: &str = "id, handle, display_name, role, runtime, command,
                                       args_json, working_dir, system_prompt, env_json,
                                       created_at, updated_at";

pub fn list(conn: &Connection) -> Result<Vec<Runner>> {
    let sql = format!("SELECT {SELECT_COLS} FROM runners ORDER BY handle ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_runner)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// `list()` + `activity()` for every runner, in one IPC call. The Runners
/// list page calls this on mount so each card's "N sessions / M missions"
/// badge renders without a second-pass flicker. Activity is computed
/// per row rather than via one giant JOIN — there are at most a few
/// dozen runners and the queries are indexed; a JOIN would obscure the
/// fact that `activity()` is the canonical aggregation and the two paths
/// would drift over time.
pub fn list_with_activity(conn: &Connection) -> Result<Vec<RunnerWithActivity>> {
    let runners = list(conn)?;
    let mut out = Vec::with_capacity(runners.len());
    for runner in runners {
        let activity = activity(conn, &runner.id)?;
        out.push(RunnerWithActivity { runner, activity });
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: &str) -> Result<Runner> {
    let sql = format!("SELECT {SELECT_COLS} FROM runners WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_runner)
        .optional()?
        .ok_or_else(|| Error::msg(format!("runner not found: {id}")))
}

/// Look up a runner by its `handle`. Used by `/runners/:handle` so the URL
/// stays stable across runner-id rotations (the user thinks in handles,
/// not ULIDs). Handles are globally unique by schema, so this is exactly
/// 0 or 1 rows.
pub fn get_by_handle(conn: &Connection, handle: &str) -> Result<Runner> {
    let sql = format!("SELECT {SELECT_COLS} FROM runners WHERE handle = ?1");
    conn.query_row(&sql, params![handle], row_to_runner)
        .optional()?
        .ok_or_else(|| Error::msg(format!("runner not found: @{handle}")))
}

pub fn create(conn: &Connection, input: CreateRunnerInput) -> Result<Runner> {
    validate_handle(&input.handle)?;
    if input.display_name.trim().is_empty() {
        return Err(Error::msg("display_name must not be empty"));
    }

    let id = new_id();
    let ts = now().to_rfc3339();
    let args_json = serde_json::to_string(&input.args)?;
    let env_json = serde_json::to_string(&input.env)?;

    conn.execute(
        "INSERT INTO runners (
            id, handle, display_name, role, runtime, command,
            args_json, working_dir, system_prompt, env_json,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            id,
            input.handle,
            input.display_name,
            input.role,
            input.runtime,
            input.command,
            args_json,
            input.working_dir,
            input.system_prompt,
            env_json,
            ts,
        ],
    )?;
    get(conn, &id)
}

pub fn update(conn: &Connection, id: &str, input: UpdateRunnerInput) -> Result<Runner> {
    let existing = get(conn, id)?;
    if let Some(ref n) = input.display_name {
        if n.trim().is_empty() {
            return Err(Error::msg("display_name must not be empty"));
        }
    }

    let display_name = input.display_name.unwrap_or(existing.display_name);
    let role = input.role.unwrap_or(existing.role);
    let runtime = input.runtime.unwrap_or(existing.runtime);
    let command = input.command.unwrap_or(existing.command);
    let args = input.args.unwrap_or(existing.args);
    let working_dir = input.working_dir.unwrap_or(existing.working_dir);
    let system_prompt = input.system_prompt.unwrap_or(existing.system_prompt);
    let env = input.env.unwrap_or(existing.env);

    let args_json = serde_json::to_string(&args)?;
    let env_json = serde_json::to_string(&env)?;
    let ts = now().to_rfc3339();

    conn.execute(
        "UPDATE runners
            SET display_name = ?1,
                role = ?2,
                runtime = ?3,
                command = ?4,
                args_json = ?5,
                working_dir = ?6,
                system_prompt = ?7,
                env_json = ?8,
                updated_at = ?9
          WHERE id = ?10",
        params![
            display_name,
            role,
            runtime,
            command,
            args_json,
            working_dir,
            system_prompt,
            env_json,
            ts,
            id,
        ],
    )?;
    get(conn, id)
}

// Global delete: removes the runner row and lets the `ON DELETE CASCADE`
// on crew_runners strip every slot the runner occupied. For any crew
// where the runner was lead, we auto-promote the lowest-position
// surviving member so that non-empty crews never end up leaderless.
pub fn delete(conn: &mut Connection, id: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Every crew the runner currently belongs to, collected BEFORE the
    // cascade so we still know who needs auto-promotion + position repack.
    // Lead flag captured inline so we don't need a second query.
    let affected_crews: Vec<(String, bool)> = {
        let mut stmt = tx.prepare("SELECT crew_id, lead FROM crew_runners WHERE runner_id = ?1")?;
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let affected = tx.execute("DELETE FROM runners WHERE id = ?1", params![id])?;
    if affected != 1 {
        return Err(Error::msg(format!("runner not found: {id}")));
    }
    // CASCADE fired: all crew_runners rows for this runner are gone.

    for (crew_id, was_lead) in affected_crews {
        if was_lead {
            let promote: Option<String> = tx
                .query_row(
                    "SELECT runner_id FROM crew_runners
                      WHERE crew_id = ?1
                      ORDER BY position ASC LIMIT 1",
                    params![crew_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(new_lead) = promote {
                tx.execute(
                    "UPDATE crew_runners SET lead = 1 WHERE crew_id = ?1 AND runner_id = ?2",
                    params![crew_id, new_lead],
                )?;
            }
        }
        // Close the position gap the cascade left for this crew, so
        // survivors stay dense (0..N-1) and the next `add_runner` lands
        // at a contiguous position instead of `MAX+1 = old_max + 1`.
        super::crew_runner::repack_positions(&tx, &crew_id)?;
    }

    tx.commit()?;
    Ok(())
}

/// Activity stats for a runner — how many sessions and missions it's
/// currently participating in, and when it last started a session. Used by
/// the Runners page to render "2 sessions · 1 mission" badges. Missions
/// are counted distinctly because a single runner might have multiple
/// sessions in the same mission historically; in MVP that never happens
/// but the COUNT(DISTINCT) keeps us honest if it ever does.
pub fn activity(conn: &Connection, runner_id: &str) -> Result<RunnerActivity> {
    // Runner must exist — fail loud so the caller's UI can render a proper
    // error rather than silently showing zero.
    get(conn, runner_id)?;

    let active_sessions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE runner_id = ?1 AND status = 'running'",
        params![runner_id],
        |r| r.get(0),
    )?;
    let active_missions: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT mission_id) FROM sessions
          WHERE runner_id = ?1 AND status = 'running' AND mission_id IS NOT NULL",
        params![runner_id],
        |r| r.get(0),
    )?;
    let crew_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM crew_runners WHERE runner_id = ?1",
        params![runner_id],
        |r| r.get(0),
    )?;
    let last_started_at_raw: Option<String> = conn
        .query_row(
            "SELECT MAX(started_at) FROM sessions WHERE runner_id = ?1",
            params![runner_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let last_started_at =
        match last_started_at_raw {
            Some(s) => Some(s.parse::<Timestamp>().map_err(|e| {
                Error::msg(format!("failed to parse last_started_at timestamp: {e}"))
            })?),
            None => None,
        };
    let direct_session_id: Option<String> = conn
        .query_row(
            "SELECT id FROM sessions
              WHERE runner_id = ?1 AND status = 'running' AND mission_id IS NULL
              ORDER BY started_at DESC
              LIMIT 1",
            params![runner_id],
            |r| r.get(0),
        )
        .optional()?;

    Ok(RunnerActivity {
        runner_id: runner_id.to_string(),
        active_sessions,
        active_missions,
        crew_count,
        last_started_at,
        direct_session_id,
    })
}

// ---------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn runner_list(state: State<'_, AppState>) -> Result<Vec<Runner>> {
    let conn = state.db.get()?;
    list(&conn)
}

#[tauri::command]
pub async fn runner_list_with_activity(
    state: State<'_, AppState>,
) -> Result<Vec<RunnerWithActivity>> {
    let conn = state.db.get()?;
    list_with_activity(&conn)
}

#[tauri::command]
pub async fn runner_get(state: State<'_, AppState>, id: String) -> Result<Runner> {
    let conn = state.db.get()?;
    get(&conn, &id)
}

#[tauri::command]
pub async fn runner_get_by_handle(state: State<'_, AppState>, handle: String) -> Result<Runner> {
    let conn = state.db.get()?;
    get_by_handle(&conn, &handle)
}

#[tauri::command]
pub async fn runner_create(state: State<'_, AppState>, input: CreateRunnerInput) -> Result<Runner> {
    let conn = state.db.get()?;
    create(&conn, input)
}

#[tauri::command]
pub async fn runner_update(
    state: State<'_, AppState>,
    id: String,
    input: UpdateRunnerInput,
) -> Result<Runner> {
    let conn = state.db.get()?;
    update(&conn, &id, input)
}

#[tauri::command]
pub async fn runner_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    // Reap every live PTY for this runner BEFORE the DB delete.
    // `sessions.runner_id` is `ON DELETE CASCADE`, so the row drop nukes
    // the session record — but the in-memory SessionManager still holds
    // the live child + reader thread. Without `kill_all_for_runner`, the
    // PTY lingers as a daemon attached to nothing and the Mac's TTY count
    // climbs every time the user deletes a runner with an open chat.
    state.sessions.kill_all_for_runner(&id)?;
    let mut conn = state.db.get()?;
    delete(&mut conn, &id)
}

#[tauri::command]
pub async fn runner_activity(state: State<'_, AppState>, id: String) -> Result<RunnerActivity> {
    let conn = state.db.get()?;
    activity(&conn, &id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn ctx() -> db::DbPool {
        db::open_in_memory().unwrap()
    }

    fn make(conn: &Connection, handle: &str) -> Runner {
        create(
            conn,
            CreateRunnerInput {
                handle: handle.into(),
                display_name: format!("{handle} display"),
                role: "impl".into(),
                runtime: "shell".into(),
                command: "sh".into(),
                args: vec![],
                working_dir: None,
                system_prompt: None,
                env: HashMap::new(),
            },
        )
        .unwrap()
    }

    #[test]
    fn create_inserts_global_runner_without_crew() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let r = make(&conn, "alpha");
        assert_eq!(r.handle, "alpha");
        assert_eq!(r.role, "impl");
    }

    #[test]
    fn list_returns_all_runners_alphabetical() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        make(&conn, "bravo");
        make(&conn, "alpha");
        let runners = list(&conn).unwrap();
        assert_eq!(runners.len(), 2);
        assert_eq!(runners[0].handle, "alpha");
        assert_eq!(runners[1].handle, "bravo");
    }

    #[test]
    fn unique_handle_globally() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        make(&conn, "shared");
        let err = create(
            &conn,
            CreateRunnerInput {
                handle: "shared".into(),
                display_name: "Dup".into(),
                role: "impl".into(),
                runtime: "shell".into(),
                command: "sh".into(),
                args: vec![],
                working_dir: None,
                system_prompt: None,
                env: HashMap::new(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unique"));
    }

    #[test]
    fn update_preserves_unset_fields() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let r = make(&conn, "alpha");
        let updated = update(
            &conn,
            &r.id,
            UpdateRunnerInput {
                role: Some("reviewer".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.role, "reviewer");
        assert_eq!(
            updated.display_name, r.display_name,
            "unchanged field preserved"
        );
    }

    #[test]
    fn delete_removes_row() {
        let pool = ctx();
        let mut conn = pool.get().unwrap();
        let r = make(&conn, "alpha");
        delete(&mut conn, &r.id).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM runners", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_on_missing_id_errors_cleanly() {
        let pool = ctx();
        let mut conn = pool.get().unwrap();
        let err = delete(&mut conn, "does-not-exist").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn handle_must_be_lowercase_slug() {
        assert!(validate_handle("lead").is_ok());
        assert!(validate_handle("impl-1").is_ok());
        assert!(validate_handle("worker_2").is_ok());
        assert!(validate_handle("0worker").is_ok());

        assert!(validate_handle("").is_err());
        assert!(validate_handle("Lead").is_err());
        assert!(validate_handle("lead bot").is_err());
        assert!(validate_handle("lead!").is_err());
        assert!(validate_handle("-lead").is_err());
        assert!(validate_handle(&"x".repeat(33)).is_err());
    }

    #[test]
    fn activity_counts_zero_for_brand_new_runner() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let r = make(&conn, "alpha");
        let a = activity(&conn, &r.id).unwrap();
        assert_eq!(a.active_sessions, 0);
        assert_eq!(a.active_missions, 0);
        assert_eq!(a.crew_count, 0);
        assert!(a.last_started_at.is_none());
    }

    #[test]
    fn activity_counts_running_sessions() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let r = make(&conn, "alpha");
        // Insert a running session by hand — C6 will own this path later.
        conn.execute(
            "INSERT INTO sessions (id, mission_id, runner_id, cwd, status, started_at)
             VALUES ('s1', NULL, ?1, '/tmp', 'running', '2026-04-23T00:00:00Z')",
            params![r.id],
        )
        .unwrap();
        let a = activity(&conn, &r.id).unwrap();
        assert_eq!(a.active_sessions, 1);
        assert_eq!(a.active_missions, 0, "direct session has no mission");
        assert!(a.last_started_at.is_some());
    }
}
