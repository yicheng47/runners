# v0 MVP — Implementation Plan

> Umbrella plan for the first end-to-end vertical slice of Runners. Ships as one feature branch (`feature/v0-mvp`) with **eleven** ordered chunks, each its own commit/PR merged into the feature branch. Once the slice is demo-able end to end, the feature branch squash-merges to `main`.
>
> Companion to `docs/arch/v0-arch.md` (architecture) and `docs/arch/v0-prd.md` (scope).

## Definition of done (demo path)

From a clean launch of the app, a user can:

1. Create a **Crew** on the Crews page, then add two runners to it (one `claude-code` lead, one `shell` worker) on the **Crew Detail** page. Runners are crew-scoped — there is no shared "template" concept in v0 (per PRD §3). The lead invariant is enforced end-to-end.
2. Click **Start Mission**, fill the goal, and see the Mission workspace open with two live PTY sessions.
3. Watch the lead runner receive the goal via stdin, draft a plan, and post a directed message to the worker; see the worker pick it up on its next `runners msg read`.
4. See a worker emit an `ask_lead` signal; watch the lead decide to escalate via `ask_human`; click **Approve** on the resulting card; see the lead receive the response and forward it to the worker.
5. Post a broadcast human signal from the workspace input and have it land on the lead by default.
6. Close and reopen the mission; the feed replays and the orchestrator's in-memory state reconstructs.

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
- Migration `0001_init.sql` creating (field names match arch §5.2 / §6 data model exactly):
  - `crews(id, name, purpose, goal, orchestrator_policy, signal_types, created_at, updated_at)`. `purpose` is short prose; `goal` is the default mission goal; `orchestrator_policy` is a JSON blob (nullable / empty for MVP — C8 only uses built-ins but the column is reserved); `signal_types` is a JSON array of allowed signal type strings.
  - `runners(id, crew_id, handle, display_name, role, runtime, command, args_json, working_dir, env_json, system_prompt, lead, position, created_at, updated_at)` with:
    - `runtime` = one of `claude-code`, `codex`, `aider`, `shell` (enum stored as TEXT).
    - `command` + `args_json` are the spawn form; `env_json` is a JSON map merged onto the session env at spawn time; `working_dir` is the runner's PTY cwd override (null = use mission cwd).
    - `UNIQUE(crew_id, handle)`.
    - `FOREIGN KEY(crew_id) REFERENCES crews(id) ON DELETE CASCADE`.
  - `missions(id, crew_id, title, goal_override, cwd, status, started_at, stopped_at)`. `goal_override` is nullable; when null, the mission inherits `crews.goal`.
  - `sessions(id, mission_id, runner_id, handle, pid, status, started_at, stopped_at)` — persisted so the reopen path (see C7/C8 replay) can identify which runners were active. PTYs themselves are not restored across app restarts.
- Separate partial index for the lead invariant (SQLite requires a standalone statement for partial uniqueness, not inline in `CREATE TABLE`):
  ```sql
  CREATE UNIQUE INDEX one_lead_per_crew ON runners(crew_id) WHERE lead = 1;
  ```
- **Default signal-type allowlist.** Every new crew row is seeded with `signal_types = ["mission_goal", "human_said", "ask_lead", "ask_human", "human_question", "human_response", "inbox_read"]` — the full set of built-in types the MVP needs. Users can extend this list in v0.x; in MVP it is write-only from the DB layer. Without this seeding the CLI will reject the built-in signals at spawn time per arch §5.3 Layer 2.
- Rust types in `src-tauri/src/model.rs`: `Crew`, `Runner`, `Mission`, `Session`, `Event`, `EventKind`, `SignalType`, serde-derived. Serde field attributes map Rust snake_case fields like `args`, `working_dir`, `env` to the DB/JSON column names `args_json`, `working_dir`, `env_json` consistently.
- TS types in `src/lib/types.ts` hand-synced with Rust (we're not pulling in `ts-rs` yet — too much ceremony for the MVP).

**Tests.** Constraint tests for the partial unique index: inserting two leads in one crew fails; inserting leads across crews succeeds. Round-trip tests for the JSON-blob columns (`orchestrator_policy`, `signal_types`, `env`).

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

## C3 — Config UI (Crews, Crew Detail, Runner Detail, Add Slot)

**Goal.** Wire the config CRUD to the wireframes in `design/runners-design.pen`. This is the first chunk a non-engineer can interact with.

**Scope note — no top-level Runners page in MVP.** Runners are crew-scoped per PRD §3. The design file's standalone "Runners" list and "Runner Detail" frames are kept for a future v0.x surface (a cross-crew runner browser) but are *not* built in MVP. Runner CRUD happens inside Crew Detail via Add Slot and an inline edit drawer.

**Deliverables.**
- `src/pages/Crews.tsx` — crew cards (create, list, delete).
- `src/pages/CrewEditor.tsx` — Crew Detail: ordered runner list within the crew, `LEAD` badge, `Set as lead` action, drag-reorder, delete-runner.
- `src/components/AddSlotModal.tsx` — modal form: handle, runtime, command/args, cwd, system prompt. First runner in a crew is auto-lead (per C2).
- `src/components/RunnerEditDrawer.tsx` — slide-over to edit an existing runner's fields in place. Reuses the Runner Detail frame's layout but inside Crew Detail context.
- All pages call Tauri commands via a tiny `src/lib/api.ts` wrapper.

**Manual test plan.** Create a crew, add two runners, reassign lead, delete lead, confirm auto-promotion to the next runner by `position`.

**Out of scope.** Mission workspace, Start Mission modal. Standalone Runners list/detail pages (design exists, MVP does not build them).

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
  - `mission_start(crew_id, title, goal_override, cwd)` — validates the crew has ≥1 runner and exactly one lead, creates the mission row, creates the mission dir, exports the crew's `signal_types` column to `$APPDATA/runners/crews/{crew_id}/signal_types.json` (per arch §5.3 Layer 2 — the CLI reads this sidecar to validate emitted signal types), then appends `mission_start` and `mission_goal` events to the log.
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
  - Built-in rules — **all signal-driven** (per arch §5.5.0, messages never trigger orchestrator actions). Per arch §5.2, signals always carry `to: null` in v0; any target lives in `payload.target`.
    - `signal mission_goal → inject_stdin @lead` with a composed prompt including the goal, the crew roster, and coordination instructions (see arch §4 for the template).
    - `signal human_said (from: "human", payload: { text, target? }) → inject_stdin @payload.target if set, else @lead`. One signal type covers both broadcast and directed human input; routing is payload-driven, not envelope-driven. The workspace input emits this signal on Post.
    - `signal ask_lead (from: <worker>, payload: { question, context }) → inject_stdin @lead` with the payload rendered into the injection template. The worker-asks-lead half of the lead-mediated HITL flow.
    - `signal ask_human (from: <runner>, payload: { prompt, choices, on_behalf_of? }) → emit human_question event + open card in UI`. If `payload.on_behalf_of` is present (the lead-mediated case), carry it into the `human_question` payload so the UI can render the attribution chain.
    - `signal human_response → inject_stdin to the runner that emitted the matching ask_human` — the lead in the lead-mediated flow, the worker in the fallback direct flow. Orchestrator looks up the original asker by `question_id`.
  - Lead-forwards-answer back to worker and any runner-to-runner exchange are **directed messages**, not orchestrator actions — recipients see them on their next `runners msg read`. No `directed message → inject` rule in MVP.
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
- `src/components/AskHumanCard.tsx` — buttons emit a `human_response` signal. If the underlying `human_question` carries `on_behalf_of`, render the attribution chain (e.g. *@impl → @architect → you*).
- `src/components/MissionInput.tsx` — the Slack-channel input. Default recipient in the UI is `@<lead>`. Submitting always emits a `signal human_said` (not a message event) so the orchestrator can wake the recipient, per arch §5.5.0. Signal envelope keeps `to: null` per arch §5.2; the picked recipient lives in `payload.target` (omitted for broadcast, set to the handle for directed). The UI label can still say "message" for user-facing clarity; the underlying event kind is `signal`.
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
