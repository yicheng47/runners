# v0 MVP — Implementation Plan

> Umbrella plan for the first end-to-end vertical slice of Runners. Ships as one feature branch (`feature/v0-mvp`) with **eleven** ordered chunks, each its own commit/PR merged into the feature branch. Once the slice is demo-able end to end, the feature branch squash-merges to `main`.
>
> Companion to `docs/arch/v0-arch.md` (architecture) and `docs/arch/v0-prd.md` (scope).

## Definition of done (demo path)

From a clean launch of the app, a user can:

1. Create two runner templates (one `claude-code`, one `shell`) on the **Runners** page.
2. Create a **Crew** of two slots (lead + worker) on the **Crews** / **Crew Detail** pages; the lead invariant is enforced end-to-end.
3. Click **Start Mission**, fill the goal, and see the Mission workspace open with two live PTY sessions.
4. Watch the lead runner receive the goal via stdin, draft a plan, and post a directed message to the worker; see the worker's reply in the feed.
5. Receive an `ask_human` card from any runner, click **Approve**, and see the response injected back into that runner's stdin.
6. Post a broadcast `@human` message and have it land on the lead by default.
7. Close and reopen the mission; the feed replays and the orchestrator's in-memory state reconstructs.

Anything beyond this is explicitly v0.x or later.

## Out of scope for MVP

- Windows support (macOS + Linux only for v0).
- Threads / reactions / reply-to semantics beyond `--to <handle>`.
- LLM-in-the-loop orchestrator rules (v0 is rule-based only; see arch §2.3).
- Envelope-level `correlation_id` / `causation_id` fields (see arch §5.2).
- Mission branching / forking / rewind.
- Multi-device sync, auth, cloud persistence.
- Per-slot system-prompt overrides in Crew Detail (the UI surface exists, but the prompt-override field is a stub that renders without effect until v0.x).

## Chunking principles

- Each chunk lands independently: `cargo check`, `cargo test`, and `pnpm tsc --noEmit` all pass after every merge into the feature branch.
- A chunk is ~1 day of focused work, one coherent review.
- Dependencies flow downstream only; no circular re-opens of earlier chunks.
- Rust chunks ship with unit tests; UI chunks ship with a manual test checklist in the PR description.
- Commit message format: `feat(<chunk-area>): <imperative summary>`. E.g. `feat(db): schema + shared types for v0`.

## Dependency graph

```
  C1  schema + shared types
    │
    ├─► C2  config CRUD (runners, crews, lead invariant)
    │     │
    │     ├─► C3  config UI (Runners, Crews, Crew Detail, Add Slot)
    │     │
    │     └─► C5  mission lifecycle commands
    │           │
    │           ├─► C6  PTY session runtime ─► C9  `runners` CLI
    │           │
    │           └─► C7  event bus + notify watcher ─► C8  orchestrator v0
    │
    └─► C4  event log primitives  (feeds C5, C7, C9)

  C10  mission workspace UI   (depends on C3, C7, C8)
  C11  missions list + Start Mission modal   (depends on C3, C5, C10)
```

C3 and C4 can run in parallel after C2 lands. C6 and C7 can run in parallel after C5. Everything else is serial.

---

## C1 — Schema + shared types

**Goal.** Lay down the SQLite schema and the Rust/TS type surface that every later chunk consumes.

**Deliverables.**
- `src-tauri/src/db.rs` — connection pool with WAL mode, `rusqlite` migrations runner, bootstrapped at app start.
- Migration `0001_init.sql` creating:
  - `crews(id, name, purpose, created_at, updated_at)`
  - `runners(id, crew_id, handle, display_name, role, binary, args, cwd, system_prompt, lead, position, created_at, updated_at)` with:
    - `UNIQUE(crew_id, handle)`
    - `UNIQUE(crew_id) WHERE lead = 1` — the lead invariant from arch §2.3.
    - `FOREIGN KEY(crew_id) REFERENCES crews(id) ON DELETE CASCADE`
  - `missions(id, crew_id, title, goal, cwd, status, started_at, stopped_at)`
- Rust types in `src-tauri/src/model.rs`: `Crew`, `Runner`, `Mission`, `Event`, `EventKind`, serde-derived.
- TS types in `src/lib/types.ts` hand-synced with Rust (we're not pulling in `ts-rs` yet — too much ceremony for the MVP).

**Tests.** Constraint tests for the unique partial index: inserting two leads in one crew fails; inserting leads across crews succeeds.

**Out of scope.** No Tauri commands yet — that's C2.

---

## C2 — Config CRUD (runners, crews, lead invariant)

**Goal.** Tauri commands for managing runner templates and crews with the lead invariant enforced at the Rust layer in addition to the DB.

**Deliverables.**
- `src-tauri/src/commands/crew.rs` — `crew_list`, `crew_create`, `crew_update`, `crew_delete`.
- `src-tauri/src/commands/runner.rs` — `runner_list(crew_id)`, `runner_create`, `runner_update`, `runner_delete`, `runner_reorder(crew_id, ordered_ids)`, `runner_set_lead(runner_id)`.
- Invariant rules encoded in Rust:
  - First runner added to a crew is auto-lead.
  - `runner_set_lead` runs in a transaction: unset old lead, set new lead, single commit.
  - Deleting the lead while other runners remain auto-promotes the runner at the lowest `position`.
  - Deleting the last runner of a crew is allowed (crew becomes empty, unstartable).

**Tests.** `cargo test` covers: auto-lead on first insert, forbidden second lead, lead auto-promotion on delete, atomic reassign.

**Out of scope.** UI, mission, PTY.

---

## C3 — Config UI (Runners, Crews, Crew Detail, Add Slot)

**Goal.** Wire the config CRUD to the wireframes in `design/runners-design.pen`. This is the first chunk a non-engineer can interact with.

**Deliverables.**
- `src/pages/Runners.tsx` — list, create, edit runner templates (maps to the "Runners" frame).
- `src/pages/RunnerDetail.tsx` — edit a runner's handle / binary / system prompt.
- `src/pages/Crews.tsx` — crew cards.
- `src/pages/CrewEditor.tsx` — Crew Detail with ordered slot list, `LEAD` badge, `Set as lead` action, drag-reorder.
- `src/components/AddSlotModal.tsx` — modal form.
- All pages call Tauri commands via a tiny `src/lib/api.ts` wrapper.

**Manual test plan.** Create two runner templates, create a crew, add two slots, reassign lead, delete lead, confirm auto-promotion.

**Out of scope.** Mission workspace, Start Mission modal. Slots' system-prompt override field renders but is a write-only stub until v0.x.

---

## C4 — Event log primitives

**Goal.** Low-level NDJSON event log that every later chunk reads from or writes to.

**Deliverables.**
- `src-tauri/src/event_log/mod.rs`:
  - `EventLog::open(mission_dir)` — opens `events.ndjson` with `O_APPEND | O_WRONLY | O_CREAT`.
  - `EventLog::append(event)` — acquires `flock(LOCK_EX)`, writes a single `write(2)`, unlocks. One event = one line.
  - `EventLog::read_from(offset)` — streaming parser used by the watcher.
- `src-tauri/src/event_log/ulid.rs` — monotonic ULID generator (millisecond-sortable, collision-safe within the same ms).
- Path helper: `$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson`.

**Tests.** Concurrent append from N threads never interleaves, ULID ordering is stable, parser round-trips all `EventKind` variants.

**Out of scope.** The notify watcher itself (C7) — this chunk only ships the append + parse.

---

## C5 — Mission lifecycle commands

**Goal.** Start and stop missions. No PTYs yet — this chunk is the bookkeeping layer.

**Deliverables.**
- `src-tauri/src/commands/mission.rs`:
  - `mission_start(crew_id, title, goal, cwd)` — validates the crew has ≥1 runner and exactly one lead, creates the mission row, creates the mission dir, appends `mission_start` and `mission_goal` events to the log.
  - `mission_stop(mission_id)` — marks the mission stopped, appends `mission_stopped`.
  - `mission_list`, `mission_get`.
- Returns enough context to the frontend that C10 can navigate to the workspace.

**Tests.** Starting a crewless / leadless crew errors cleanly. `mission_start` writes the expected two opening events.

**Out of scope.** Actually spawning runner processes — that's C6.

---

## C6 — PTY session runtime

**Goal.** Spawn one PTY-backed session per runner for a running mission.

**Deliverables.**
- `src-tauri/src/session/manager.rs`:
  - `SessionManager` owns `HashMap<SessionId, Session>`.
  - `spawn(mission, runner)` — uses `portable-pty`, sets env (`RUNNERS_CREW_ID`, `RUNNERS_MISSION_ID`, `RUNNERS_RUNNER_HANDLE`, `RUNNERS_EVENT_LOG`, augmented `PATH` that puts the bundled `runners` CLI first).
  - `inject_stdin(session_id, text)` — through a write channel.
  - `pause(session_id)` (SIGSTOP on Unix), `resume(session_id)` (SIGCONT), `kill(session_id)`.
- Reader thread per session: stdout/stderr → ring buffer (last N KB) → Tauri event `session/output`.
- Hooks into C5 so `mission_start` spawns all sessions and `mission_stop` kills them.

**Tests.** Spawn a `sh`, inject `echo hi`, read `hi` back. Pause/resume changes process state.

**Out of scope.** xterm.js frontend — that's part of C10.

---

## C7 — Event bus + notify watcher

**Goal.** Tail the mission's `events.ndjson` and broadcast events to the rest of the process.

**Deliverables.**
- `src-tauri/src/event_bus/mod.rs`:
  - `EventBus::for_mission(mission)` — starts a `notify` watcher on the log file; on `Modify`, reads from last offset, parses new lines.
  - Per-runner inbox projection: `events where to = null OR to = runner.handle`.
  - Per-runner read watermark, driven by `inbox_read` signals (never inferred from `--since`).
  - Emits Tauri events: `event/appended`, `inbox/updated`, `watermark/advanced`.

**Tests.** Append events, watcher sees them, projections include/exclude the right rows.

**Out of scope.** Orchestrator reactions to events — that's C8.

---

## C8 — Orchestrator v0

**Goal.** The deterministic rule-based router that turns events into actions.

**Deliverables.**
- `src-tauri/src/orchestrator/mod.rs`:
  - Policy loader (reads the crew's policy JSON).
  - Built-in rules always loaded:
    - `mission_goal → inject_stdin @lead` with a composed prompt including the goal, the crew roster, and coordination instructions (see arch §4 for the template).
    - `broadcast message from human → inject_stdin @lead` (lead-routing invariant).
    - `directed message → inject_stdin @<to>` (any runner, any sender).
    - `ask_human signal → emit human_question event + open card in UI`.
    - `human_response event → inject_stdin to the original asker`.
  - Dispatch ledger (in-memory map of `triggering_event_id → handled`) so replay is idempotent.
  - Pending-ask map keyed by `question_id`.

**Tests.** Each built-in rule fires exactly once. Replay after reopen reconstructs state from the log. `human_response` without a matching `human_question` is dropped with a log warning, not panic.

**Out of scope.** LLM policy, user-authored rules. MVP ships only the built-ins plus a no-op policy slot.

---

## C9 — `runners` CLI binary

**Goal.** The binary each runner's PTY calls to post events. Without this, runners can't talk to the log.

**Deliverables.**
- `cli/` crate in the workspace: `runners` binary.
- Resolves envelope fields from env vars set by C6.
- Commands:
  - `runners signal <type> [--payload <json>]`
  - `runners msg post <text> [--to <handle>]`
  - `runners msg read [--since <ts>] [--from <handle>]` — emits `inbox_read` signal with `payload.up_to = max ULID`.
  - `runners help`.
- Reuses `event_log` crate from C4 directly (shared crate, not duplicated code).

**Tests.** Integration test: spawn a shell with the env a real session would have, run CLI commands, assert events land in the ndjson.

**Out of scope.** Any form of direct-to-orchestrator RPC — everything goes through the log.

---

## C10 — Mission workspace UI

**Goal.** Render the live mission in the design's "Mission workspace" frame.

**Deliverables.**
- `src/pages/MissionWorkspace.tsx` — subscribes to `event/appended`, renders the feed.
- `src/components/EventFeed.tsx` — message / signal / `ask_human` card variants.
- `src/components/AskHumanCard.tsx` — buttons emit a `human_response` signal.
- `src/components/MissionInput.tsx` — the Slack-channel input. Default `to: @<lead>`. `message` / `signal` mode toggle. Submitting calls a Tauri `human_post_message` command that writes to the log.
- `src/components/RunnersRail.tsx` — list of sessions with status dot, `LEAD` badge, "open pty" action.
- `src/components/RunnerTerminal.tsx` — xterm.js bound to the session output stream (popped out of the rail).

**Manual test plan.** End-to-end demo path from the "Definition of done" section.

**Out of scope.** The Start Mission modal itself — that's C11.

---

## C11 — Missions list + Start Mission modal

**Goal.** The entrypoint to everything C10 renders. The final chunk that closes the loop.

**Deliverables.**
- `src/pages/Missions.tsx` — Active / Past tabs, mission rows, status dot, "pending ask" flag derived from orchestrator state.
- `src/components/StartMissionModal.tsx` — crew picker, title, goal textarea, cwd with `Browse…`, Advanced collapse (stubbed).
- Navigation: Start → call `mission_start` → route to `/missions/:id`.

**Manual test plan.** From Home, pick a crew, start a mission, land on the workspace, interact, close, reopen from Missions list.

**Out of scope.** Mission archive / search / filter — deferred.

---

## Commit message convention (all chunks)

```
feat(<area>): <imperative summary>

<body — what changed and why, mentioning the chunk letter>

Part of the v0 MVP umbrella. See docs/impls/v0-mvp.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

Areas: `db`, `commands`, `ui`, `event-log`, `session`, `event-bus`, `orchestrator`, `cli`, `mission`.

## Branching

- Feature branch: `feature/v0-mvp`, branched from `main`.
- Each chunk PR targets `feature/v0-mvp`, merged with `--squash --delete-branch`.
- Once C11 lands and the demo path passes, `feature/v0-mvp` squash-merges to `main` with a summary commit message.
