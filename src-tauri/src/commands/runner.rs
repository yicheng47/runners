// Runner CRUD — crew-scoped. Enforces the lead invariant at the Rust layer
// (docs/arch/v0-arch.md §2.2): exactly one `lead=1` runner per non-empty
// crew. The partial unique index in migrations/0001_init.sql is the
// defense-in-depth backstop; the rules here are the user-facing contract.
//
// Invariants per docs/impls/v0-mvp.md §C2:
//   - First runner added to a crew is auto-lead.
//   - `runner_set_lead` atomically transfers leadership in one transaction.
//   - Deleting the lead while other runners remain auto-promotes the runner
//     at the lowest `position`.
//   - Deleting the last runner is allowed (crew becomes empty, unstartable).

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Deserialize;
use tauri::State;
use ulid::Ulid as UlidGen;

use crate::{
    error::{Error, Result},
    model::{Runner, Timestamp},
    AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunnerInput {
    pub crew_id: String,
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

// `handle` is deliberately excluded: per arch §2.2 and §5.2 it is the
// runner's identity in events, CLI addressing, and policy rules. Renaming
// after creation would break historical event attribution and any
// persisted policy references. Users who want a different handle must
// delete the runner and re-add it.
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

fn new_id() -> String {
    UlidGen::new().to_string()
}

fn now() -> Timestamp {
    Utc::now()
}

// Handle validation: lowercase ASCII slug, 1..=32 chars, [a-z0-9] start,
// followed by [a-z0-9_-]. Matches PRD §4 handle rules.
fn validate_handle(handle: &str) -> Result<()> {
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

fn row_to_runner(row: &Row<'_>) -> rusqlite::Result<Runner> {
    let args_raw: Option<String> = row.get("args_json")?;
    let env_raw: Option<String> = row.get("env_json")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    let lead_int: i64 = row.get("lead")?;
    Ok(Runner {
        id: row.get("id")?,
        crew_id: row.get("crew_id")?,
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
        lead: lead_int != 0,
        position: row.get("position")?,
        created_at: created_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: updated_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

const SELECT_COLS: &str = "id, crew_id, handle, display_name, role, runtime, command,
                            args_json, working_dir, system_prompt, env_json,
                            lead, position, created_at, updated_at";

pub fn list(conn: &Connection, crew_id: &str) -> Result<Vec<Runner>> {
    let sql = format!("SELECT {SELECT_COLS} FROM runners WHERE crew_id = ?1 ORDER BY position ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![crew_id], row_to_runner)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn get(conn: &Connection, id: &str) -> Result<Runner> {
    let sql = format!("SELECT {SELECT_COLS} FROM runners WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_runner)
        .optional()?
        .ok_or_else(|| Error::msg(format!("runner not found: {id}")))
}

fn crew_exists(conn: &Connection, crew_id: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM crews WHERE id = ?1", params![crew_id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(found.is_some())
}

pub fn create(conn: &mut Connection, input: CreateRunnerInput) -> Result<Runner> {
    validate_handle(&input.handle)?;
    if input.display_name.trim().is_empty() {
        return Err(Error::msg("display_name must not be empty"));
    }
    if !crew_exists(conn, &input.crew_id)? {
        return Err(Error::msg(format!("crew not found: {}", input.crew_id)));
    }

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM runners WHERE crew_id = ?1",
        params![input.crew_id],
        |r| r.get(0),
    )?;
    let is_first = count == 0;
    let next_position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM runners WHERE crew_id = ?1",
        params![input.crew_id],
        |r| r.get(0),
    )?;

    let id = new_id();
    let ts = now().to_rfc3339();
    let args_json = serde_json::to_string(&input.args)?;
    let env_json = serde_json::to_string(&input.env)?;
    let lead: i64 = if is_first { 1 } else { 0 };

    tx.execute(
        "INSERT INTO runners (
            id, crew_id, handle, display_name, role, runtime, command,
            args_json, working_dir, system_prompt, env_json,
            lead, position, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        params![
            id,
            input.crew_id,
            input.handle,
            input.display_name,
            input.role,
            input.runtime,
            input.command,
            args_json,
            input.working_dir,
            input.system_prompt,
            env_json,
            lead,
            next_position,
            ts,
        ],
    )?;
    tx.commit()?;
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

pub fn delete(conn: &mut Connection, id: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Read lead/crew_id inside the tx so a concurrent set_lead or create
    // can't race between the read and the delete — otherwise we could
    // leave a non-empty crew with zero leads (e.g. delete sees lead=0,
    // another tx promotes this runner, then we delete without promoting).
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT crew_id, lead FROM runners WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (crew_id, was_lead) = row.ok_or_else(|| Error::msg(format!("runner not found: {id}")))?;

    let affected = tx.execute("DELETE FROM runners WHERE id = ?1", params![id])?;
    if affected != 1 {
        return Err(Error::msg(format!("runner not found: {id}")));
    }

    if was_lead != 0 {
        // Auto-promote the lowest-position surviving runner in the crew.
        // Returns None when the crew is now empty — that's a valid state.
        let promote: Option<String> = tx
            .query_row(
                "SELECT id FROM runners WHERE crew_id = ?1 ORDER BY position ASC LIMIT 1",
                params![crew_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(new_lead) = promote {
            let ts = now().to_rfc3339();
            tx.execute(
                "UPDATE runners SET lead = 1, updated_at = ?1 WHERE id = ?2",
                params![ts, new_lead],
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

pub fn set_lead(conn: &mut Connection, runner_id: &str) -> Result<Runner> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Read inside the tx so the runner can't be deleted between check and
    // write. A stale read would silently no-op against a DELETE'd id.
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT crew_id, lead FROM runners WHERE id = ?1",
            params![runner_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (crew_id, is_lead) =
        row.ok_or_else(|| Error::msg(format!("runner not found: {runner_id}")))?;

    if is_lead != 0 {
        tx.commit()?;
        return get(conn, runner_id);
    }

    let ts = now().to_rfc3339();
    // Clear the old lead first so the partial unique index never sees two
    // lead=1 rows mid-transaction on this crew.
    tx.execute(
        "UPDATE runners SET lead = 0, updated_at = ?1 WHERE crew_id = ?2 AND lead = 1",
        params![ts, crew_id],
    )?;
    let affected = tx.execute(
        "UPDATE runners SET lead = 1, updated_at = ?1 WHERE id = ?2",
        params![ts, runner_id],
    )?;
    if affected != 1 {
        return Err(Error::msg(format!("runner not found: {runner_id}")));
    }

    tx.commit()?;
    get(conn, runner_id)
}

pub fn reorder(
    conn: &mut Connection,
    crew_id: &str,
    ordered_ids: Vec<String>,
) -> Result<Vec<Runner>> {
    // Pure duplicate check runs outside the tx — no DB state needed.
    let mut seen = std::collections::HashSet::new();
    for id in &ordered_ids {
        if !seen.insert(id.clone()) {
            return Err(Error::msg(
                "runner_reorder: ordered_ids contains duplicates",
            ));
        }
    }

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Current ids and permutation check live inside the tx so a concurrent
    // create/delete between validation and the UPDATE loop can't commit
    // against a different runner set than the one validated. The
    // affected-row assertion below is the redundant backstop.
    let current: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM runners WHERE crew_id = ?1")?;
        let rows = stmt.query_map(params![crew_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if current.len() != ordered_ids.len() {
        return Err(Error::msg(
            "runner_reorder: ordered_ids must contain every runner in the crew exactly once",
        ));
    }
    for id in &current {
        if !seen.contains(id) {
            return Err(Error::msg(format!(
                "runner_reorder: ordered_ids missing runner {id}"
            )));
        }
    }

    let ts = now().to_rfc3339();
    for (position, id) in ordered_ids.iter().enumerate() {
        let affected = tx.execute(
            "UPDATE runners SET position = ?1, updated_at = ?2 WHERE id = ?3 AND crew_id = ?4",
            params![position as i64, ts, id, crew_id],
        )?;
        if affected != 1 {
            return Err(Error::msg(format!(
                "runner_reorder: runner {id} not in crew {crew_id}"
            )));
        }
    }
    tx.commit()?;
    list(conn, crew_id)
}

#[tauri::command]
pub async fn runner_list(state: State<'_, AppState>, crew_id: String) -> Result<Vec<Runner>> {
    let conn = state.db.get()?;
    list(&conn, &crew_id)
}

#[tauri::command]
pub async fn runner_get(state: State<'_, AppState>, id: String) -> Result<Runner> {
    let conn = state.db.get()?;
    get(&conn, &id)
}

#[tauri::command]
pub async fn runner_create(state: State<'_, AppState>, input: CreateRunnerInput) -> Result<Runner> {
    let mut conn = state.db.get()?;
    create(&mut conn, input)
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
    let mut conn = state.db.get()?;
    delete(&mut conn, &id)
}

#[tauri::command]
pub async fn runner_set_lead(state: State<'_, AppState>, id: String) -> Result<Runner> {
    let mut conn = state.db.get()?;
    set_lead(&mut conn, &id)
}

#[tauri::command]
pub async fn runner_reorder(
    state: State<'_, AppState>,
    crew_id: String,
    ordered_ids: Vec<String>,
) -> Result<Vec<Runner>> {
    let mut conn = state.db.get()?;
    reorder(&mut conn, &crew_id, ordered_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{commands::crew, db};

    fn seed_crew(pool: &db::DbPool) -> String {
        let conn = pool.get().unwrap();
        crew::create(
            &conn,
            crew::CreateCrewInput {
                name: "Test".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap()
        .id
    }

    fn add(pool: &db::DbPool, crew_id: &str, handle: &str) -> Runner {
        let mut conn = pool.get().unwrap();
        create(
            &mut conn,
            CreateRunnerInput {
                crew_id: crew_id.into(),
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
    fn first_runner_added_to_crew_is_auto_lead() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "lead");
        assert!(r1.lead, "first runner must auto-lead");
        assert_eq!(r1.position, 0);

        let r2 = add(&pool, &crew_id, "impl");
        assert!(!r2.lead, "second runner must not be lead");
        assert_eq!(r2.position, 1);
    }

    #[test]
    fn runner_set_lead_reassigns_atomically() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "one");
        let r2 = add(&pool, &crew_id, "two");
        assert!(r1.lead && !r2.lead);

        let mut conn = pool.get().unwrap();
        set_lead(&mut conn, &r2.id).unwrap();

        let r1_after = get(&conn, &r1.id).unwrap();
        let r2_after = get(&conn, &r2.id).unwrap();
        assert!(!r1_after.lead);
        assert!(r2_after.lead);

        let lead_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runners WHERE crew_id = ?1 AND lead = 1",
                params![crew_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lead_count, 1, "invariant: exactly one lead per crew");
    }

    #[test]
    fn set_lead_on_current_lead_is_noop() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "one");
        add(&pool, &crew_id, "two");

        let mut conn = pool.get().unwrap();
        let after = set_lead(&mut conn, &r1.id).unwrap();
        assert!(after.lead);
    }

    #[test]
    fn deleting_lead_auto_promotes_lowest_position() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "alpha"); // position 0, auto-lead
        let r2 = add(&pool, &crew_id, "beta"); // position 1
        let r3 = add(&pool, &crew_id, "gamma"); // position 2

        // Promote r3 to lead, then delete r3. r1 (position 0) should win.
        let mut conn = pool.get().unwrap();
        set_lead(&mut conn, &r3.id).unwrap();
        delete(&mut conn, &r3.id).unwrap();

        let r1_after = get(&conn, &r1.id).unwrap();
        let r2_after = get(&conn, &r2.id).unwrap();
        assert!(r1_after.lead, "lowest-position runner gets promoted");
        assert!(!r2_after.lead);
    }

    #[test]
    fn deleting_last_runner_leaves_empty_crew() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "only");

        let mut conn = pool.get().unwrap();
        delete(&mut conn, &r1.id).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runners WHERE crew_id = ?1",
                params![crew_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        // The crew row itself must remain — empty crews are valid, just
        // unstartable. C5 enforces that.
        let crew_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM crews WHERE id = ?1",
                params![crew_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(crew_count, 1);
    }

    #[test]
    fn runner_reorder_rejects_missing_ids() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "alpha");
        let r2 = add(&pool, &crew_id, "beta");
        add(&pool, &crew_id, "gamma");

        let mut conn = pool.get().unwrap();
        let err = reorder(&mut conn, &crew_id, vec![r1.id.clone(), r2.id.clone()]).unwrap_err();
        assert!(err.to_string().contains("every runner"));

        // Positions untouched on rejection (no partial writes).
        let runners = list(&conn, &crew_id).unwrap();
        assert_eq!(runners[0].handle, "alpha");
        assert_eq!(runners[1].handle, "beta");
        assert_eq!(runners[2].handle, "gamma");
    }

    #[test]
    fn runner_reorder_rejects_duplicates() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "alpha");
        let _r2 = add(&pool, &crew_id, "beta");

        let mut conn = pool.get().unwrap();
        let err = reorder(&mut conn, &crew_id, vec![r1.id.clone(), r1.id.clone()]).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn runner_reorder_applies_new_positions() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let r1 = add(&pool, &crew_id, "alpha");
        let r2 = add(&pool, &crew_id, "beta");
        let r3 = add(&pool, &crew_id, "gamma");

        let mut conn = pool.get().unwrap();
        let reordered = reorder(
            &mut conn,
            &crew_id,
            vec![r3.id.clone(), r1.id.clone(), r2.id.clone()],
        )
        .unwrap();

        assert_eq!(reordered[0].handle, "gamma");
        assert_eq!(reordered[0].position, 0);
        assert_eq!(reordered[1].handle, "alpha");
        assert_eq!(reordered[1].position, 1);
        assert_eq!(reordered[2].handle, "beta");
        assert_eq!(reordered[2].position, 2);

        // Reorder preserves lead (still whoever was lead before).
        let r1_after = get(&conn, &r1.id).unwrap();
        assert!(r1_after.lead, "lead moves with the runner, not the slot");
    }

    #[test]
    fn crew_delete_cascades_to_runners_via_command() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        add(&pool, &crew_id, "alpha");
        add(&pool, &crew_id, "beta");

        let conn = pool.get().unwrap();
        crew::delete(&conn, &crew_id).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runners WHERE crew_id = ?1",
                params![crew_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
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
    fn create_rejects_invalid_handles_before_touching_db() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        let mut conn = pool.get().unwrap();
        let err = create(
            &mut conn,
            CreateRunnerInput {
                crew_id: crew_id.clone(),
                handle: "BadHandle".into(),
                display_name: "Bad".into(),
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
        assert!(err.to_string().contains("lowercase"));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runners WHERE crew_id = ?1",
                params![crew_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no partial write on validation failure");
    }

    #[test]
    fn delete_on_missing_id_errors_cleanly() {
        let pool = db::open_in_memory().unwrap();
        let mut conn = pool.get().unwrap();
        let err = delete(&mut conn, "does-not-exist").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn set_lead_on_missing_id_errors_cleanly() {
        let pool = db::open_in_memory().unwrap();
        let mut conn = pool.get().unwrap();
        let err = set_lead(&mut conn, "does-not-exist").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn unique_handle_within_crew_surfaces_from_command() {
        let pool = db::open_in_memory().unwrap();
        let crew_id = seed_crew(&pool);
        add(&pool, &crew_id, "alpha");

        let mut conn = pool.get().unwrap();
        let err = create(
            &mut conn,
            CreateRunnerInput {
                crew_id: crew_id.clone(),
                handle: "alpha".into(),
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
        // Surfaces as a SQLite UNIQUE constraint from (crew_id, handle).
        assert!(err.to_string().to_lowercase().contains("unique"));
    }
}
