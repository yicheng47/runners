// SQLite persistence for crews, runners, missions, and sessions.
//
// Schema lives in migrations/0001_init.sql and mirrors arch §7.1 verbatim.
// The pool is opened once at app start with WAL mode + foreign keys; later
// chunks pull connections from it via Tauri state.

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};

use crate::error::Result;

pub type DbPool = Pool<SqliteConnectionManager>;

// The v0 built-in signal allowlist seeded onto every new crew row. See
// arch §5.3 Layer 2 — the CLI reads this list (exported to a sidecar in C5)
// and rejects unknown `type`s. In MVP this list is write-only from the DB
// layer; users extend it in v0.x.
pub const DEFAULT_SIGNAL_TYPES: &[&str] = &[
    "mission_goal",
    "human_said",
    "ask_lead",
    "ask_human",
    "human_question",
    "human_response",
    "inbox_read",
];

#[allow(dead_code)] // Consumed by C5 when it writes the sidecar at $APPDATA/.../signal_types.json.
pub fn default_signal_types_json() -> String {
    serde_json::to_string(DEFAULT_SIGNAL_TYPES).expect("static allowlist must serialize")
}

pub fn open_pool(db_path: &Path) -> Result<DbPool> {
    let manager = SqliteConnectionManager::file(db_path).with_init(init_connection);
    build_pool(manager, 8)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<DbPool> {
    let manager = SqliteConnectionManager::memory().with_init(init_connection);
    build_pool(manager, 1)
}

fn build_pool(manager: SqliteConnectionManager, max_size: u32) -> Result<DbPool> {
    let pool = Pool::builder().max_size(max_size).build(manager)?;
    let mut conn = pool.get()?;
    run_migrations(&mut conn)?;
    Ok(pool)
}

fn init_connection(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA foreign_keys = ON;\n\
         PRAGMA busy_timeout = 5000;",
    )
}

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

fn run_migrations(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
         )",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM _migrations",
        [],
        |row| row.get(0),
    )?;
    // Each migration + its `_migrations` bookkeeping row runs in a single
    // IMMEDIATE transaction: a crash mid-apply rolls back the DDL so the next
    // startup retries the same version instead of replaying it onto a
    // partially-migrated schema (which would fail on `CREATE TABLE crews`).
    for (version, sql) in MIGRATIONS {
        if *version > current {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO _migrations (version, applied_at) VALUES (?1, ?2)",
                params![version, chrono::Utc::now().to_rfc3339()],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ErrorCode;

    fn insert_crew(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO crews (id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![id, format!("crew-{id}"), "2026-04-22T00:00:00Z"],
        )
        .unwrap();
    }

    fn insert_runner(conn: &Connection, id: &str, handle: &str) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO runners (
                id, handle, display_name, role, runtime, command,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'impl', 'shell', 'sh', ?4, ?4)",
            params![
                id,
                handle,
                format!("{handle} display"),
                "2026-04-22T00:00:00Z"
            ],
        )
    }

    fn insert_crew_runner(
        conn: &Connection,
        crew_id: &str,
        runner_id: &str,
        position: i64,
        lead: i64,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO crew_runners (crew_id, runner_id, position, lead, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![crew_id, runner_id, position, lead, "2026-04-22T00:00:00Z"],
        )
    }

    #[test]
    fn migrations_bootstrap_all_tables() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN
                     ('crews','runners','crew_runners','missions','sessions')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn new_crew_is_seeded_with_default_signal_types() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        let raw: String = conn
            .query_row("SELECT signal_types FROM crews WHERE id = 'c1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let parsed: Vec<String> = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed,
            DEFAULT_SIGNAL_TYPES
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn one_lead_per_crew_index_rejects_second_lead() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        insert_runner(&conn, "r1", "alpha").unwrap();
        insert_runner(&conn, "r2", "beta").unwrap();

        insert_crew_runner(&conn, "c1", "r1", 0, 1).unwrap();
        let err = insert_crew_runner(&conn, "c1", "r2", 1, 1).unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
    }

    #[test]
    fn one_lead_per_crew_allows_leads_across_crews() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        insert_crew(&conn, "c2");
        insert_runner(&conn, "r1", "alpha").unwrap();
        insert_runner(&conn, "r2", "beta").unwrap();

        insert_crew_runner(&conn, "c1", "r1", 0, 1).unwrap();
        insert_crew_runner(&conn, "c2", "r2", 0, 1).unwrap();
    }

    #[test]
    fn runner_handle_is_globally_unique() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_runner(&conn, "r1", "shared").unwrap();
        let err = insert_runner(&conn, "r2", "shared").unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
    }

    #[test]
    fn same_runner_can_join_multiple_crews() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        insert_crew(&conn, "c2");
        insert_runner(&conn, "r1", "shared").unwrap();

        insert_crew_runner(&conn, "c1", "r1", 0, 1).unwrap();
        insert_crew_runner(&conn, "c2", "r1", 0, 1).unwrap();
    }

    #[test]
    fn position_is_unique_per_crew() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        insert_runner(&conn, "r1", "alpha").unwrap();
        insert_runner(&conn, "r2", "beta").unwrap();

        insert_crew_runner(&conn, "c1", "r1", 0, 1).unwrap();
        let err = insert_crew_runner(&conn, "c1", "r2", 0, 0).unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
    }

    #[test]
    fn json_blob_columns_roundtrip() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");

        let policy = serde_json::json!([{"when": {"signal": "ask_lead"}, "do": "inject_stdin"}]);
        let signals = serde_json::json!(["custom_a", "custom_b"]);
        conn.execute(
            "UPDATE crews SET orchestrator_policy = ?1, signal_types = ?2 WHERE id = 'c1'",
            params![policy.to_string(), signals.to_string()],
        )
        .unwrap();

        let env = serde_json::json!({"FOO": "bar", "BAZ": "qux"});
        let args = serde_json::json!(["--flag", "--val=1"]);
        conn.execute(
            "INSERT INTO runners (
                id, handle, display_name, role, runtime, command,
                args_json, env_json, created_at, updated_at
             ) VALUES ('r1','impl','Impl','impl','shell','sh',?1,?2,?3,?3)",
            params![args.to_string(), env.to_string(), "2026-04-22T00:00:00Z"],
        )
        .unwrap();

        let (policy_raw, signals_raw): (String, String) = conn
            .query_row(
                "SELECT orchestrator_policy, signal_types FROM crews WHERE id = 'c1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&policy_raw).unwrap(),
            policy
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&signals_raw).unwrap(),
            signals
        );

        let (args_raw, env_raw): (String, String) = conn
            .query_row(
                "SELECT args_json, env_json FROM runners WHERE id = 'r1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&args_raw).unwrap(),
            args
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&env_raw).unwrap(),
            env
        );
    }

    #[test]
    fn deleting_crew_cascades_crew_runner_rows_only() {
        // C5.5: runners are global — deleting a crew should strip its join
        // rows but leave the runner itself intact so other crews (or a
        // direct chat) can keep using it.
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        insert_crew(&conn, "c1");
        insert_runner(&conn, "r1", "alpha").unwrap();
        insert_crew_runner(&conn, "c1", "r1", 0, 1).unwrap();

        conn.execute("DELETE FROM crews WHERE id = 'c1'", [])
            .unwrap();
        let runner_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM runners WHERE id = 'r1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let slot_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM crew_runners WHERE runner_id = 'r1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(runner_count, 1, "runner row must survive crew delete");
        assert_eq!(slot_count, 0, "membership row cascades away");
    }

    #[test]
    fn migrations_are_idempotent_on_reopen() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("runner.db");

        {
            let _pool = open_pool(&path).unwrap();
        }
        let pool = open_pool(&path).unwrap();
        let conn = pool.get().unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 1, "each migration should apply exactly once");
    }
}
