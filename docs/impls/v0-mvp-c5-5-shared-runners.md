# v0 MVP — C5.5: shared runners + direct sessions

> Amendment to the umbrella plan (`v0-mvp.md`), inserted between C5
> (mission lifecycle, shipped) and C6 (PTY session runtime). C6 needs the
> new session schema, so this chunk must land before C6 starts.
>
> Three product changes driven by user feedback after C3:
>
> 1. **Runners are top-level and shared.** A runner is its own entity; a
>    crew is a composition of existing runners. One runner can belong to
>    many crews.
> 2. **Sessions without missions.** A runner can be spawned as a standalone
>    PTY session (direct chat) without going through the mission lifecycle.
> 3. **Per-runner activity.** The Runners page and Runner detail show how
>    many sessions / missions each runner is currently in.

This supersedes the §C3 scope note that said "no top-level Runners page in
MVP" and the §7.1 schema's `runners.crew_id NOT NULL` assumption.

## Schema — rewrite `0001_init.sql`

We're still in MVP with no production users, so instead of layering a
`0002_*.sql` on top we rewrite the one-and-only migration in place. Dev
bootstraps fresh; any local DB file (`$APPDATA/runners/runners.db`) gets
deleted once.

**`runners`** becomes global.
- Drop `crew_id`, `position`, `lead`. Drop `UNIQUE(crew_id, handle)` and the `one_lead_per_crew` index.
- Add `UNIQUE(handle)` — global handle namespace so `@architect` means the same runner everywhere it appears in the event log.

**`crew_runners`** — new join table for crew membership.
```sql
CREATE TABLE crew_runners (
    crew_id   TEXT NOT NULL REFERENCES crews(id)   ON DELETE CASCADE,
    runner_id TEXT NOT NULL REFERENCES runners(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    lead      INTEGER NOT NULL DEFAULT 0,
    added_at  TEXT NOT NULL,
    PRIMARY KEY (crew_id, runner_id),
    UNIQUE     (crew_id, position)
);
CREATE UNIQUE INDEX one_lead_per_crew ON crew_runners(crew_id) WHERE lead = 1;
```

**`sessions`** — loosen mission binding.
- `mission_id` becomes `NULL`-able (`ON DELETE SET NULL` instead of `CASCADE`).
- Add `cwd TEXT` (direct sessions need their own cwd — no mission to inherit from).

## Commands — changes and additions

**Runner CRUD (now global):**
- `runner_create/update/delete/list/get` — drop `crew_id` from inputs. `runner_list` returns every runner across the DB.
- `runner_activity(runner_id) -> { active_sessions, active_missions, last_active_at }` — new.

**Crew membership (the old lead-invariant logic moves here):**
- `crew_add_runner(crew_id, runner_id)` — appends at next position; auto-leads if first member.
- `crew_remove_runner(crew_id, runner_id)` — auto-promotes lowest-position member if the lead was removed.
- `crew_set_lead(crew_id, runner_id)` — atomic swap in `crew_runners`.
- `crew_reorder(crew_id, ordered_runner_ids)` — same contract as today, different table.
- `crew_list_runners(crew_id) -> Vec<Runner>` — join `crew_runners` + `runners` ordered by position.

**Sessions:**
- `session_start_direct(runner_id, cwd) -> Session` — C6 hook. Inserts a `sessions` row with `mission_id = NULL`, event log at `$APPDATA/runners/sessions/{session_id}/events.ndjson` (instead of `missions/{mission_id}/events.ndjson`).
- `session_stop(session_id)` — works for both mission-backed and direct sessions.
- `mission_start` (C5) — unchanged contract; internally reads `crew_list_runners` instead of `runner::list(crew_id)`.

## Frontend

**Sidebar:** add `Runner` between `Crew` and `Mission` (reverses the §C3 removal).

**`src/pages/Runners.tsx`** — new.
- Grid/list of all runners with: `@handle`, display name, runtime, command, **active sessions · active missions** badge, Edit / Chat / Delete.
- `+ New runner` opens the creation modal (the old `AddSlotModal` generalised).
- "Chat" action → calls `session_start_direct`, navigates to `/runners/:id/sessions/:sessionId` (C6 renders the PTY).

**`src/pages/CrewEditor.tsx`** — existing Add Slot flow changes.
- "+ Add slot" opens a picker: pick an existing runner or jump to "+ Create new runner" (inline shortcut that creates a runner and immediately joins the crew).
- Slot row now just identifies the referenced runner; edits to the runner itself happen on the Runners page (link through).

**`src/pages/RunnerDetail.tsx`** — stub/minimal.
- Displays the runner's default system prompt + activity + "Chat now" button. Full chat UI is C6/C10 territory.

## Chunk split

- **C5.5a — schema + backend.** Migration, runner/crew_runner/session command updates, tests. One PR. Unblocks C6.
- **C5.5b — Runners page + crew picker.** Frontend surfaces. Depends on C5.5a merged.

Downstream chunks to adjust once C5.5a lands:
- **C6** builds on the new `sessions` shape (nullable `mission_id`, explicit `cwd`); `session_start_direct` is wired to a PTY here.
- **C9** (`runners` CLI): `@handle` stays the addressing primitive but is now globally unique — the `signal_types` sidecar file layout gets simpler (one file per runner instead of per-crew lookup).

## Out of scope for C5.5

- Real chat rendering (that's C6 + C10).
- Cross-crew conflict resolution (if two crews reference the same runner and both run missions simultaneously — v0.x problem).
- Migration tooling. In MVP we rewrite DDL; pre-MVP users delete their local DB.

## Open questions

- **Handle immutability on a shared runner.** Today we forbid handle rename after creation (per arch §2.2). With shared runners, that's even more important — renaming breaks every crew referencing it. Contract stays: handle is immutable.
- **Orphan runners.** If every crew removes a runner, the runner row remains (orphaned). That's intentional — you can still chat with it directly.
