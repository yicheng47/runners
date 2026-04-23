// Crew membership — manages the `crew_runners` join table (C5.5).
//
// Invariants (moved from the old crew-scoped runner.rs):
//   - A crew with ≥1 member has exactly one `lead = 1` row. DB partial
//     unique index `one_lead_per_crew` is the backstop; the functions
//     here are the user-facing contract.
//   - First member added to a crew is auto-lead.
//   - Removing the lead while other members remain auto-promotes the
//     runner at the lowest `position`.
//   - `position` is dense within a crew (0, 1, 2, ...) and enforced
//     unique by the schema.
//
// Runner CRUD is in commands/runner.rs — a runner exists globally and can
// be referenced by zero or more crews.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tauri::State;

use crate::{
    commands::runner,
    error::{Error, Result},
    model::{CrewRunner, Timestamp},
    AppState,
};

fn now() -> Timestamp {
    Utc::now()
}

fn crew_exists(conn: &Connection, crew_id: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM crews WHERE id = ?1", params![crew_id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(found.is_some())
}

fn runner_exists(conn: &Connection, runner_id: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM runners WHERE id = ?1",
            params![runner_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Return the runners that belong to a crew, ordered by position, each
/// annotated with its `lead` flag and membership timestamp. Joins
/// `crew_runners` against `runners` in one shot so the UI can render a
/// crew roster without N+1 lookups.
pub fn list(conn: &Connection, crew_id: &str) -> Result<Vec<CrewRunner>> {
    let sql = format!(
        "SELECT r.{cols}, cr.position AS cr_position, cr.lead AS cr_lead,
                cr.added_at AS cr_added_at
           FROM crew_runners cr
           JOIN runners r ON r.id = cr.runner_id
          WHERE cr.crew_id = ?1
          ORDER BY cr.position ASC",
        cols = runner::SELECT_COLS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![crew_id], |row| {
        let r = runner::row_to_runner(row)?;
        let position: i64 = row.get("cr_position")?;
        let lead_int: i64 = row.get("cr_lead")?;
        let added_at_raw: String = row.get("cr_added_at")?;
        let added_at: Timestamp = added_at_raw.parse().map_err(|e: chrono::ParseError| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(CrewRunner {
            runner: r,
            position,
            lead: lead_int != 0,
            added_at,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Append `runner_id` to `crew_id`'s roster at the next position.
/// Auto-assigns lead if the crew was empty.
pub fn add_runner(conn: &mut Connection, crew_id: &str, runner_id: &str) -> Result<CrewRunner> {
    if !crew_exists(conn, crew_id)? {
        return Err(Error::msg(format!("crew not found: {crew_id}")));
    }
    if !runner_exists(conn, runner_id)? {
        return Err(Error::msg(format!("runner not found: {runner_id}")));
    }

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Reject duplicate membership up front rather than letting the PK
    // violation surface a cryptic SQLite error. Composite PK (crew_id,
    // runner_id) is the backstop.
    let already: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM crew_runners WHERE crew_id = ?1 AND runner_id = ?2",
            params![crew_id, runner_id],
            |r| r.get(0),
        )
        .optional()?;
    if already.is_some() {
        return Err(Error::msg(format!(
            "runner {runner_id} already belongs to crew {crew_id}"
        )));
    }

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM crew_runners WHERE crew_id = ?1",
        params![crew_id],
        |r| r.get(0),
    )?;
    let next_position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM crew_runners WHERE crew_id = ?1",
        params![crew_id],
        |r| r.get(0),
    )?;
    let is_first = count == 0;
    let lead: i64 = if is_first { 1 } else { 0 };
    let ts = now().to_rfc3339();

    tx.execute(
        "INSERT INTO crew_runners (crew_id, runner_id, position, lead, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![crew_id, runner_id, next_position, lead, ts],
    )?;

    tx.commit()?;

    // Re-read via list() to return the joined shape the UI wants.
    list(conn, crew_id)?
        .into_iter()
        .find(|r| r.runner.id == runner_id)
        .ok_or_else(|| Error::msg("crew_add_runner: inserted row vanished"))
}

/// Remove `runner_id` from `crew_id`. Promotes the lowest-position
/// surviving member to lead if we just removed the lead.
pub fn remove_runner(conn: &mut Connection, crew_id: &str, runner_id: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let was_lead: Option<i64> = tx
        .query_row(
            "SELECT lead FROM crew_runners WHERE crew_id = ?1 AND runner_id = ?2",
            params![crew_id, runner_id],
            |r| r.get(0),
        )
        .optional()?;
    let was_lead = was_lead.ok_or_else(|| {
        Error::msg(format!(
            "runner {runner_id} is not a member of crew {crew_id}"
        ))
    })?;

    let affected = tx.execute(
        "DELETE FROM crew_runners WHERE crew_id = ?1 AND runner_id = ?2",
        params![crew_id, runner_id],
    )?;
    if affected != 1 {
        return Err(Error::msg(format!(
            "runner {runner_id} is not a member of crew {crew_id}"
        )));
    }

    if was_lead != 0 {
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

    tx.commit()?;
    Ok(())
}

/// Atomically transfer leadership within a crew. No-op if the target is
/// already lead. Errors if the runner isn't a member.
pub fn set_lead(conn: &mut Connection, crew_id: &str, runner_id: &str) -> Result<CrewRunner> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let is_lead: Option<i64> = tx
        .query_row(
            "SELECT lead FROM crew_runners WHERE crew_id = ?1 AND runner_id = ?2",
            params![crew_id, runner_id],
            |r| r.get(0),
        )
        .optional()?;
    let is_lead = is_lead.ok_or_else(|| {
        Error::msg(format!(
            "runner {runner_id} is not a member of crew {crew_id}"
        ))
    })?;

    if is_lead != 0 {
        tx.commit()?;
        return list(conn, crew_id)?
            .into_iter()
            .find(|r| r.runner.id == runner_id)
            .ok_or_else(|| Error::msg("crew_set_lead: member vanished mid-call"));
    }

    // Clear the old lead first so the partial unique index never sees two
    // lead=1 rows mid-transaction on this crew.
    tx.execute(
        "UPDATE crew_runners SET lead = 0 WHERE crew_id = ?1 AND lead = 1",
        params![crew_id],
    )?;
    let affected = tx.execute(
        "UPDATE crew_runners SET lead = 1 WHERE crew_id = ?1 AND runner_id = ?2",
        params![crew_id, runner_id],
    )?;
    if affected != 1 {
        return Err(Error::msg(format!(
            "runner {runner_id} is not a member of crew {crew_id}"
        )));
    }

    tx.commit()?;
    list(conn, crew_id)?
        .into_iter()
        .find(|r| r.runner.id == runner_id)
        .ok_or_else(|| Error::msg("crew_set_lead: member vanished mid-call"))
}

/// Reorder a crew's membership. `ordered_ids` must be a permutation of
/// the crew's current members — no adds or removes allowed. Positions
/// are rewritten 0..N in the given order.
pub fn reorder(
    conn: &mut Connection,
    crew_id: &str,
    ordered_ids: Vec<String>,
) -> Result<Vec<CrewRunner>> {
    let mut seen = std::collections::HashSet::new();
    for id in &ordered_ids {
        if !seen.insert(id.clone()) {
            return Err(Error::msg("crew_reorder: ordered_ids contains duplicates"));
        }
    }

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let current: Vec<String> = {
        let mut stmt = tx.prepare("SELECT runner_id FROM crew_runners WHERE crew_id = ?1")?;
        let rows = stmt.query_map(params![crew_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if current.len() != ordered_ids.len() {
        return Err(Error::msg(
            "crew_reorder: ordered_ids must contain every member exactly once",
        ));
    }
    for id in &current {
        if !seen.contains(id) {
            return Err(Error::msg(format!(
                "crew_reorder: ordered_ids missing runner {id}"
            )));
        }
    }

    // SQLite enforces `UNIQUE(crew_id, position)`, and two rows swapping
    // positions would transiently violate the constraint even if the final
    // state is fine. Park everyone at negative positions first, then write
    // the target positions.
    for (i, id) in current.iter().enumerate() {
        tx.execute(
            "UPDATE crew_runners SET position = ?1
               WHERE crew_id = ?2 AND runner_id = ?3",
            params![-(i as i64) - 1, crew_id, id],
        )?;
    }
    for (position, id) in ordered_ids.iter().enumerate() {
        let affected = tx.execute(
            "UPDATE crew_runners SET position = ?1
               WHERE crew_id = ?2 AND runner_id = ?3",
            params![position as i64, crew_id, id],
        )?;
        if affected != 1 {
            return Err(Error::msg(format!(
                "crew_reorder: runner {id} not in crew {crew_id}"
            )));
        }
    }

    tx.commit()?;
    list(conn, crew_id)
}

// ---------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn crew_list_runners(
    state: State<'_, AppState>,
    crew_id: String,
) -> Result<Vec<CrewRunner>> {
    let conn = state.db.get()?;
    list(&conn, &crew_id)
}

#[tauri::command]
pub async fn crew_add_runner(
    state: State<'_, AppState>,
    crew_id: String,
    runner_id: String,
) -> Result<CrewRunner> {
    let mut conn = state.db.get()?;
    add_runner(&mut conn, &crew_id, &runner_id)
}

#[tauri::command]
pub async fn crew_remove_runner(
    state: State<'_, AppState>,
    crew_id: String,
    runner_id: String,
) -> Result<()> {
    let mut conn = state.db.get()?;
    remove_runner(&mut conn, &crew_id, &runner_id)
}

#[tauri::command]
pub async fn crew_set_lead(
    state: State<'_, AppState>,
    crew_id: String,
    runner_id: String,
) -> Result<CrewRunner> {
    let mut conn = state.db.get()?;
    set_lead(&mut conn, &crew_id, &runner_id)
}

#[tauri::command]
pub async fn crew_reorder(
    state: State<'_, AppState>,
    crew_id: String,
    ordered_ids: Vec<String>,
) -> Result<Vec<CrewRunner>> {
    let mut conn = state.db.get()?;
    reorder(&mut conn, &crew_id, ordered_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{commands::crew, db};
    use std::collections::HashMap;

    fn pool() -> db::DbPool {
        db::open_in_memory().unwrap()
    }

    fn seed_crew(conn: &Connection, name: &str) -> String {
        crew::create(
            conn,
            crew::CreateCrewInput {
                name: name.into(),
                purpose: None,
                goal: None,
            },
        )
        .unwrap()
        .id
    }

    fn seed_runner(conn: &Connection, handle: &str) -> String {
        runner::create(
            conn,
            runner::CreateRunnerInput {
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
        .id
    }

    #[test]
    fn first_runner_added_becomes_lead() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "Alpha");
        let r = seed_runner(&conn, "lead");
        let added = add_runner(&mut conn, &c, &r).unwrap();
        assert!(added.lead);
        assert_eq!(added.position, 0);
    }

    #[test]
    fn second_runner_is_not_lead() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "Alpha");
        let r1 = seed_runner(&conn, "alpha");
        let r2 = seed_runner(&conn, "beta");
        add_runner(&mut conn, &c, &r1).unwrap();
        let second = add_runner(&mut conn, &c, &r2).unwrap();
        assert!(!second.lead);
        assert_eq!(second.position, 1);
    }

    #[test]
    fn shared_runner_can_belong_to_multiple_crews() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c1 = seed_crew(&conn, "A");
        let c2 = seed_crew(&conn, "B");
        let r = seed_runner(&conn, "shared");
        add_runner(&mut conn, &c1, &r).unwrap();
        add_runner(&mut conn, &c2, &r).unwrap();

        let in_c1 = list(&conn, &c1).unwrap();
        let in_c2 = list(&conn, &c2).unwrap();
        assert_eq!(in_c1.len(), 1);
        assert_eq!(in_c2.len(), 1);
        // Same global runner row, different slot info per crew.
        assert_eq!(in_c1[0].runner.id, in_c2[0].runner.id);
        assert!(in_c1[0].lead);
        assert!(in_c2[0].lead);
    }

    #[test]
    fn adding_same_runner_twice_to_same_crew_errors() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r = seed_runner(&conn, "alpha");
        add_runner(&mut conn, &c, &r).unwrap();
        let err = add_runner(&mut conn, &c, &r).unwrap_err();
        assert!(err.to_string().contains("already belongs"));
    }

    #[test]
    fn set_lead_reassigns_atomically() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r1 = seed_runner(&conn, "one");
        let r2 = seed_runner(&conn, "two");
        add_runner(&mut conn, &c, &r1).unwrap();
        add_runner(&mut conn, &c, &r2).unwrap();

        let promoted = set_lead(&mut conn, &c, &r2).unwrap();
        assert!(promoted.lead);

        let roster = list(&conn, &c).unwrap();
        let leads = roster.iter().filter(|m| m.lead).count();
        assert_eq!(leads, 1, "exactly one lead per crew");
        assert!(roster.iter().find(|m| m.runner.id == r2).unwrap().lead);
    }

    #[test]
    fn remove_lead_auto_promotes_lowest_position() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r1 = seed_runner(&conn, "alpha"); // pos 0, auto-lead
        let r2 = seed_runner(&conn, "beta"); // pos 1
        let r3 = seed_runner(&conn, "gamma"); // pos 2
        add_runner(&mut conn, &c, &r1).unwrap();
        add_runner(&mut conn, &c, &r2).unwrap();
        add_runner(&mut conn, &c, &r3).unwrap();
        set_lead(&mut conn, &c, &r3).unwrap();

        remove_runner(&mut conn, &c, &r3).unwrap();
        let roster = list(&conn, &c).unwrap();
        // r1 (pos 0) should win promotion.
        assert!(roster.iter().find(|m| m.runner.id == r1).unwrap().lead);
        assert!(!roster.iter().find(|m| m.runner.id == r2).unwrap().lead);
    }

    #[test]
    fn removing_last_member_leaves_empty_crew() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r = seed_runner(&conn, "only");
        add_runner(&mut conn, &c, &r).unwrap();
        remove_runner(&mut conn, &c, &r).unwrap();
        assert!(list(&conn, &c).unwrap().is_empty());
    }

    #[test]
    fn reorder_rewrites_positions_and_preserves_lead() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r1 = seed_runner(&conn, "alpha");
        let r2 = seed_runner(&conn, "beta");
        let r3 = seed_runner(&conn, "gamma");
        add_runner(&mut conn, &c, &r1).unwrap();
        add_runner(&mut conn, &c, &r2).unwrap();
        add_runner(&mut conn, &c, &r3).unwrap();

        let roster = reorder(&mut conn, &c, vec![r3.clone(), r1.clone(), r2.clone()]).unwrap();
        assert_eq!(roster[0].runner.id, r3);
        assert_eq!(roster[0].position, 0);
        assert_eq!(roster[1].runner.id, r1);
        assert_eq!(roster[1].position, 1);
        assert_eq!(roster[2].runner.id, r2);
        assert_eq!(roster[2].position, 2);

        // r1 was the original lead — position changes, but lead doesn't.
        assert!(roster.iter().find(|m| m.runner.id == r1).unwrap().lead);
    }

    #[test]
    fn reorder_rejects_missing_members() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c = seed_crew(&conn, "A");
        let r1 = seed_runner(&conn, "alpha");
        let r2 = seed_runner(&conn, "beta");
        add_runner(&mut conn, &c, &r1).unwrap();
        add_runner(&mut conn, &c, &r2).unwrap();

        let err = reorder(&mut conn, &c, vec![r1.clone()]).unwrap_err();
        assert!(err.to_string().contains("exactly once"));

        // Untouched on rejection.
        let roster = list(&conn, &c).unwrap();
        assert_eq!(roster[0].runner.id, r1);
        assert_eq!(roster[1].runner.id, r2);
    }

    #[test]
    fn deleting_shared_runner_auto_promotes_in_each_crew_where_it_leads() {
        let pool = pool();
        let mut conn = pool.get().unwrap();
        let c1 = seed_crew(&conn, "A");
        let c2 = seed_crew(&conn, "B");
        let shared = seed_runner(&conn, "shared");
        let r2_a = seed_runner(&conn, "second-a");
        let r2_b = seed_runner(&conn, "second-b");
        add_runner(&mut conn, &c1, &shared).unwrap(); // lead in A
        add_runner(&mut conn, &c1, &r2_a).unwrap();
        add_runner(&mut conn, &c2, &shared).unwrap(); // lead in B
        add_runner(&mut conn, &c2, &r2_b).unwrap();

        runner::delete(&mut conn, &shared).unwrap();

        let in_a = list(&conn, &c1).unwrap();
        let in_b = list(&conn, &c2).unwrap();
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_b.len(), 1);
        assert!(in_a[0].lead, "auto-promoted lead in crew A");
        assert!(in_b[0].lead, "auto-promoted lead in crew B");
    }
}
