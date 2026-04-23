// Crew CRUD — the top-level container for a team of runners.
//
// `crews.signal_types` is seeded by SQL DEFAULT (see migrations/0001_init.sql),
// so crew_create leaves that column unset and lets the DB populate it. See
// docs/impls/v0-mvp.md §C2 and docs/arch/v0-arch.md §5.3 Layer 2.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use tauri::State;
use ulid::Ulid as UlidGen;

use crate::{
    error::{Error, Result},
    model::{Crew, SignalType, Timestamp},
    AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCrewInput {
    pub name: String,
    pub purpose: Option<String>,
    pub goal: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateCrewInput {
    pub name: Option<String>,
    pub purpose: Option<Option<String>>,
    pub goal: Option<Option<String>>,
    pub orchestrator_policy: Option<Option<serde_json::Value>>,
    pub signal_types: Option<Vec<SignalType>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewListItem {
    #[serde(flatten)]
    pub crew: Crew,
    pub runner_count: i64,
}

fn new_id() -> String {
    UlidGen::new().to_string()
}

fn now() -> Timestamp {
    Utc::now()
}

fn row_to_crew(row: &Row<'_>) -> rusqlite::Result<Crew> {
    let orchestrator_policy: Option<String> = row.get("orchestrator_policy")?;
    let signal_types_raw: String = row.get("signal_types")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    Ok(Crew {
        id: row.get("id")?,
        name: row.get("name")?,
        purpose: row.get("purpose")?,
        goal: row.get("goal")?,
        orchestrator_policy: match orchestrator_policy {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?),
            None => None,
        },
        signal_types: serde_json::from_str(&signal_types_raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_at: created_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: updated_at.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<CrewListItem>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.purpose, c.goal, c.orchestrator_policy,
                c.signal_types, c.created_at, c.updated_at,
                (SELECT COUNT(*) FROM crew_runners cr WHERE cr.crew_id = c.id) AS runner_count
           FROM crews c
         ORDER BY c.created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let crew = row_to_crew(row)?;
        let runner_count: i64 = row.get("runner_count")?;
        Ok(CrewListItem { crew, runner_count })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn get(conn: &Connection, id: &str) -> Result<Crew> {
    conn.query_row(
        "SELECT id, name, purpose, goal, orchestrator_policy,
                signal_types, created_at, updated_at
           FROM crews WHERE id = ?1",
        params![id],
        row_to_crew,
    )
    .optional()?
    .ok_or_else(|| Error::msg(format!("crew not found: {id}")))
}

pub fn create(conn: &Connection, input: CreateCrewInput) -> Result<Crew> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("crew name must not be empty"));
    }
    let id = new_id();
    let ts = now().to_rfc3339();
    conn.execute(
        "INSERT INTO crews (id, name, purpose, goal, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, name, input.purpose, input.goal, ts],
    )?;
    get(conn, &id)
}

pub fn update(conn: &Connection, id: &str, input: UpdateCrewInput) -> Result<Crew> {
    let existing = get(conn, id)?;

    let name = match input.name.as_ref() {
        Some(n) => {
            let trimmed = n.trim();
            if trimmed.is_empty() {
                return Err(Error::msg("crew name must not be empty"));
            }
            trimmed.to_string()
        }
        None => existing.name,
    };
    let purpose = input.purpose.unwrap_or(existing.purpose);
    let goal = input.goal.unwrap_or(existing.goal);
    let orchestrator_policy = input
        .orchestrator_policy
        .unwrap_or(existing.orchestrator_policy);
    let signal_types = input.signal_types.unwrap_or(existing.signal_types);

    let policy_raw = match orchestrator_policy.as_ref() {
        Some(v) => Some(serde_json::to_string(v)?),
        None => None,
    };
    let signals_raw = serde_json::to_string(&signal_types)?;
    let ts = now().to_rfc3339();

    conn.execute(
        "UPDATE crews
            SET name = ?1,
                purpose = ?2,
                goal = ?3,
                orchestrator_policy = ?4,
                signal_types = ?5,
                updated_at = ?6
          WHERE id = ?7",
        params![name, purpose, goal, policy_raw, signals_raw, ts, id],
    )?;
    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let affected = conn.execute("DELETE FROM crews WHERE id = ?1", params![id])?;
    if affected == 0 {
        return Err(Error::msg(format!("crew not found: {id}")));
    }
    Ok(())
}

#[tauri::command]
pub async fn crew_list(state: State<'_, AppState>) -> Result<Vec<CrewListItem>> {
    let conn = state.db.get()?;
    list(&conn)
}

#[tauri::command]
pub async fn crew_get(state: State<'_, AppState>, id: String) -> Result<Crew> {
    let conn = state.db.get()?;
    get(&conn, &id)
}

#[tauri::command]
pub async fn crew_create(state: State<'_, AppState>, input: CreateCrewInput) -> Result<Crew> {
    let conn = state.db.get()?;
    create(&conn, input)
}

#[tauri::command]
pub async fn crew_update(
    state: State<'_, AppState>,
    id: String,
    input: UpdateCrewInput,
) -> Result<Crew> {
    let conn = state.db.get()?;
    update(&conn, &id, input)
}

#[tauri::command]
pub async fn crew_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let conn = state.db.get()?;
    delete(&conn, &id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn ctx() -> db::DbPool {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn create_seeds_default_signal_types() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let crew = create(
            &conn,
            CreateCrewInput {
                name: "Alpha".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap();
        assert_eq!(
            crew.signal_types
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            db::DEFAULT_SIGNAL_TYPES.to_vec()
        );
    }

    #[test]
    fn list_returns_crews_with_runner_counts() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let a = create(
            &conn,
            CreateCrewInput {
                name: "A".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateCrewInput {
                name: "B".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runners (
                id, handle, display_name, role, runtime, command,
                created_at, updated_at
             ) VALUES ('r1', 'lead', 'Lead', 'impl', 'shell', 'sh',
                       '2026-04-22T00:00:00Z', '2026-04-22T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO crew_runners (crew_id, runner_id, position, lead, added_at)
             VALUES (?1, 'r1', 0, 1, '2026-04-22T00:00:00Z')",
            params![a.id],
        )
        .unwrap();

        let items = list(&conn).unwrap();
        assert_eq!(items.len(), 2);
        let a_item = items.iter().find(|i| i.crew.id == a.id).unwrap();
        assert_eq!(a_item.runner_count, 1);
        let b_item = items.iter().find(|i| i.crew.name == "B").unwrap();
        assert_eq!(b_item.runner_count, 0);
    }

    #[test]
    fn update_preserves_unset_fields() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let crew = create(
            &conn,
            CreateCrewInput {
                name: "Original".into(),
                purpose: Some("keep me".into()),
                goal: None,
            },
        )
        .unwrap();

        let updated = update(
            &conn,
            &crew.id,
            UpdateCrewInput {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.purpose.as_deref(), Some("keep me"));
    }

    #[test]
    fn delete_cascades_to_crew_runners_but_spares_runner_row() {
        // Runners are global (C5.5). Deleting a crew should strip the
        // join rows but leave the runner intact for other crews (or a
        // future direct chat).
        let pool = ctx();
        let conn = pool.get().unwrap();
        let crew = create(
            &conn,
            CreateCrewInput {
                name: "Doomed".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runners (
                id, handle, display_name, role, runtime, command,
                created_at, updated_at
             ) VALUES ('r1', 'lead', 'Lead', 'impl', 'shell', 'sh',
                       '2026-04-22T00:00:00Z', '2026-04-22T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO crew_runners (crew_id, runner_id, position, lead, added_at)
             VALUES (?1, 'r1', 0, 1, '2026-04-22T00:00:00Z')",
            params![crew.id],
        )
        .unwrap();

        delete(&conn, &crew.id).unwrap();
        let slot_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM crew_runners WHERE crew_id = ?1",
                params![crew.id],
                |r| r.get(0),
            )
            .unwrap();
        let runner_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM runners WHERE id = 'r1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(slot_count, 0);
        assert_eq!(runner_count, 1, "runner outlives the crew");
    }

    #[test]
    fn empty_name_is_rejected() {
        let pool = ctx();
        let conn = pool.get().unwrap();
        let err = create(
            &conn,
            CreateCrewInput {
                name: "   ".into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }
}
