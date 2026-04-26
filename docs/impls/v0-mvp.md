# v0 MVP — Implementation Plan

> Umbrella plan for the first end-to-end vertical slice of Runner. Twelve ordered chunks (C1–C11 plus the C5.5a amendment), each its own PR merged directly into `main`. The original umbrella-branch model was dropped after C8.5 — see `## Branching` at the bottom for the rationale.
>
> Companion to `docs/arch/v0-arch.md` (architecture) and `docs/arch/v0-prd.md` (scope). For current build status see the latest snapshot under `docs/logs/`.

## Definition of done (demo path)

From a clean launch of the app, a user can:

1. Create a **Crew** on the Crews page, then add two runners to it (one `claude-code` lead, one `shell` worker) on the **Crew Detail** page. Per C5.5a, runners are top-level config and shared across crews — adding a runner to a crew creates a `crew_runners` membership row, not a new runner. The lead invariant is per-crew (one lead per crew, enforced via partial unique index on `crew_runners`) and is checked end-to-end.
2. Click **Start Mission**, fill the goal, and see the Mission workspace open with two live PTY sessions.
3. Watch the lead runner receive the goal via stdin, draft a plan, and post a directed message to the worker; see the worker pick it up on its next `runner msg read`.
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
    │           ├─► C6  PTY session runtime ─► C9    `runner` CLI
    │           │                          └─► C8.5  Runners page + Runner Detail + direct chat
    │           │
    │           └─► C7  event bus + notify watcher ─► C8  orchestrator v0
    │
    └─► C4  event log primitives  (feeds C5, C7, C9)

  C10  mission workspace UI   (depends on C3, C7, C8)
  C11  missions list + Start Mission modal   (depends on C3, C5, C10)
```

C3 and C4 can run in parallel after C2 lands. C6 and C7 can run in parallel after C5. C8 (orchestrator) and C8.5 (Runners page) are peers — both depend on C6, neither depends on the other, so they can ship in either order.

---

## C1 — Schema + shared types

**Goal.** Lay down the SQLite schema and the Rust/TS type surface that every later chunk consumes.

**Deliverables.**
- `src-tauri/src/db.rs` — connection pool with WAL mode, `rusqlite` migrations runner, bootstrapped at app start.
- Migration `0001_init.sql` — implements **arch §7.1 verbatim**, including the four tables (`crews`, `runners`, `missions`, `sessions`) and the `one_lead_per_crew` partial unique index. No additions, no renames. The plan used to list the columns inline; that list has been deleted to remove the two-source-of-truth risk the earlier review called out. Implementers copy §7.1 directly into `0001_init.sql`.
- **Default signal-type allowlist.** Every new crew row is seeded with `signal_types = ["mission_goal", "human_said", "ask_lead", "ask_human", "human_question", "human_response", "inbox_read"]` — the full set of built-in types the MVP needs. Users can extend this list in v0.x; in MVP it is write-only from the DB layer. Without this seeding the CLI will reject the built-in signals at spawn time per arch §5.3 Layer 2.
- Rust types in `src-tauri/src/model.rs`: `Crew`, `Runner`, `Mission`, `Session`, `Event`, `EventKind`, `SignalType`, serde-derived. Serde field attributes map Rust-idiomatic snake_case (`args`, `env`) to the DB column names (`args_json`, `env_json`) where they differ.
- TS types in `src/lib/types.ts` hand-synced with Rust (we're not pulling in `ts-rs` yet — too much ceremony for the MVP).

**Tests.** Constraint tests for the partial unique index: inserting two leads in one crew fails; inserting leads across crews succeeds. Round-trip tests for the JSON-blob columns (`orchestrator_policy`, `signal_types`, `env`).

**Out of scope.** No Tauri commands yet — that's C2.

---

## C2 — Config CRUD (runners, crews, lead invariant)

**Goal.** Tauri commands for managing crews and (top-level, sharable) runners, with the per-crew lead invariant enforced at the Rust layer in addition to the DB.

**Note (post-C5.5a).** This section was originally written for the per-crew runner model. C5.5a moved runner CRUD onto the global `runners` table and put crew membership on `crew_runners`; the live commands match that shape. The descriptions below reflect what actually shipped.

**Deliverables.**
- `src-tauri/src/commands/crew.rs` — `crew_list`, `crew_create`, `crew_update`, `crew_delete`.
- `src-tauri/src/commands/runner.rs` — `runner_list` (global, no crew arg), `runner_get`, `runner_create`, `runner_update`, `runner_delete`, `runner_activity`. Runners exist independently of any crew.
- `src-tauri/src/commands/crew_runner.rs` — membership commands: `crew_list_runners(crew_id)`, `crew_add_runner(crew_id, runner_id)`, `crew_remove_runner(crew_id, runner_id)`, `crew_set_lead(crew_id, runner_id)`, `crew_reorder(crew_id, ordered_runner_ids)`.
- Invariant rules encoded in Rust:
  - First runner added to a crew is auto-lead (membership-level, not runner-level).
  - `crew_set_lead` runs in a transaction: unset old lead, set new lead, single commit.
  - Removing the lead from a crew while other members remain auto-promotes the runner at the lowest `position`.
  - Removing the last member of a crew is allowed (crew becomes empty, unstartable).
  - Deleting a runner globally cascades through `crew_runners` (`ON DELETE CASCADE`); deleting a crew cascades through `crew_runners` but **does not** delete the runner row itself.

**Tests.** `cargo test` covers: auto-lead on first membership insert, forbidden second lead per crew, lead auto-promotion on remove, atomic reassign, runner survives crew delete, same runner can join multiple crews and be lead in each independently.

**Out of scope.** UI, mission, PTY.

---

## C3 — Config UI (Crews, Crew Detail, Runner Detail, Add Slot)

**Goal.** Wire the config CRUD to the wireframes in `design/runners-design.pen`. This is the first chunk a non-engineer can interact with.

**Scope note — Runners are top-level in MVP, but the dedicated Runners pages land in C8.5.** C5.5a (`v0-mvp-c5-5-shared-runners.md`) already moved runners out from under crews and made the same runner shareable across crews; the data model has no notion of "crew-scoped runner" anymore. C3 still does runner CRUD inside Crew Detail (Add Slot + edit drawer) because that's the path the demo flow needs. The standalone Runners list and Runner Detail frames in `design/runners-design.pen` (`2Oecf`, `ocAFJ`) are built in C8.5 (sibling chunk of C8 orchestrator).

**Deliverables.**
- `src/pages/Crews.tsx` — crew cards (create, list, delete).
- `src/pages/CrewEditor.tsx` — Crew Detail: ordered runner list within the crew, `LEAD` badge, `Set as lead` action, drag-reorder, delete-runner.
- `src/components/AddSlotModal.tsx` — modal form: handle, runtime, command/args, cwd, system prompt. First runner in a crew is auto-lead (per C2).
- `src/components/RunnerEditDrawer.tsx` — slide-over to edit an existing runner's fields in place. Reuses the Runner Detail frame's layout but inside Crew Detail context.
- All pages call Tauri commands via a tiny `src/lib/api.ts` wrapper.

**Manual test plan.** Create a crew, add two runners, reassign lead, delete lead, confirm auto-promotion to the next runner by `position`.

**Out of scope.** Mission workspace, Start Mission modal. The standalone Runners list / Runner Detail pages (frames `2Oecf` and `ocAFJ` in the design) ship in **C8.5**, not C3.

---

## C4 — Event log primitives

**Goal.** Low-level NDJSON event log that every later chunk reads from or writes to.

**Deliverables.**
- `src-tauri/src/event_log/mod.rs`:
  - `EventLog::open(mission_dir)` — opens `events.ndjson` with `O_APPEND | O_WRONLY | O_CREAT`.
  - `EventLog::append(event)` — acquires `flock(LOCK_EX)`, writes a single `write(2)`, unlocks. One event = one line.
  - `EventLog::read_from(offset)` — streaming parser used by the watcher.
- `src-tauri/src/event_log/ulid.rs` — monotonic ULID generator (millisecond-sortable, collision-safe within the same ms).
- Path helper: `$APPDATA/runner/crews/{crew_id}/missions/{mission_id}/events.ndjson`.

**Tests.** Concurrent append from N threads never interleaves, ULID ordering is stable, parser round-trips all `EventKind` variants.

**Out of scope.** The notify watcher itself (C7) — this chunk only ships the append + parse.

---

## C5 — Mission lifecycle commands

**Goal.** Start and stop missions. No PTYs yet — this chunk is the bookkeeping layer.

**Deliverables.**
- `src-tauri/src/commands/mission.rs`:
  - `mission_start(crew_id, title, goal_override, cwd)` — validates the crew has ≥1 runner and exactly one lead, creates the mission row, creates the mission dir, exports the crew's `signal_types` column to `$APPDATA/runner/crews/{crew_id}/signal_types.json` (per arch §5.3 Layer 2 — the CLI reads this sidecar to validate emitted signal types), then appends `mission_start` and `mission_goal` events to the log.
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
  - `spawn(mission, runner)` — uses `portable-pty`, sets env (`RUNNER_CREW_ID`, `RUNNER_MISSION_ID`, `RUNNER_HANDLE`, `RUNNER_EVENT_LOG`, augmented `PATH` that puts the bundled `runner` CLI first).
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
  - Lead-forwards-answer back to worker and any runner-to-runner exchange are **directed messages**, not orchestrator actions — recipients see them on their next `runner msg read`. No `directed message → inject` rule in MVP.
  - Dispatch ledger (in-memory map of `triggering_event_id → handled`) so replay is idempotent.
  - Pending-ask map keyed by `question_id`.

**Tests.** Each built-in rule fires exactly once. Replay after reopen reconstructs state from the log. `human_response` without a matching `human_question` is dropped with a log warning, not panic.

**Out of scope.** LLM policy, user-authored rules. MVP ships only the built-ins plus a no-op policy slot.

---

## C8.5 — Runners page + Runner Detail + direct chat

**Goal.** Promote runners to a top-level UI surface, mirroring the design's `2Oecf` (Runners list) and `ocAFJ` (Runner Detail) frames, plus a "Chat now" path that exercises the direct-chat session shape C5.5a already baked into the schema. Without this chunk, runners that aren't in any crew are invisible and the user can never spawn a runner without going through a full mission.

**Why it sits at C8.5.** Sibling/parallel to C8 (orchestrator) — both depend on C6 and neither depends on the other, following the C5.5a precedent for inserted chunks. The orchestrator and the Runners page can ship in either order; this just records that the work is part of v0-mvp, not deferred.

**Scope-shift context.** Originally cut from MVP under the C3 "no top-level Runners page" scope note, now restored: the C5.5a schema work is wasted UI-side until this lands.

**Deliverables.**
- **Backend.**
  - `commands/runner.rs::runner_list_with_activity()` — extends the existing `runner_list` to include `running_session_count` (from `sessions WHERE status = 'running'`) and `open_mission_count` (from `crew_runners ⨝ missions WHERE status = 'running'`). The Runners list cards need both counters.
  - `commands/runner.rs::runner_get_by_handle(handle)` — used by `/runners/:handle` so the URL is stable across runner-id rotations.
  - `commands/session.rs::session_start_direct(runner_id, cwd)` — inserts a `sessions` row with `mission_id = NULL` and the chosen `cwd`, then spawns through the existing `SessionManager::spawn` path. Differences from the mission flavor: no `RUNNER_MISSION_ID`, `RUNNER_EVENT_LOG`, or `RUNNER_CREW_ID` env vars are set, and the runner does not join any event bus or orchestrator. The `runner` CLI must no-op gracefully when those vars are absent (small change in C9-land — the CLI errors today on `RUNNER_EVENT_LOG`-not-set, which would crash a direct-chat agent the moment it tries to emit an event).
  - Live activity events: `SessionManager` emits `runner/activity { runner_id, running_sessions, open_missions }` on every spawn, reap, and kill so the Runners list and Runner Detail can update without polling.
- **Frontend.**
  - `src/components/Sidebar.tsx` — flip the placeholder Runner item to an enabled `NavLink to="/runners"`. Order in the design is Runner / Crew / Mission, top to bottom.
  - `src/pages/Runners.tsx` — vertical stack of `RunnerCard`s, header with `+ New runner`, dashed empty-state card. Same visual vocabulary as `Crews.tsx`. Subscribes to `runner/activity` for live counters.
  - `src/components/CreateRunnerModal.tsx` — extracted from `CrewEditor.tsx`'s anonymous "Add Slot" modal so both surfaces reuse one component. The Crew Detail flow keeps adding *existing* runners through Add Slot, plus this same modal as a "create new" affordance.
  - `src/pages/RunnerDetail.tsx` (`/runners/:handle`) — two columns matching `ocAFJ`: left has `Default system prompt` (with the same edit-drawer behavior C3 ships) and `Crews using this runner` (LEAD badge per row, deep-link into Crew Detail); right has `Activity` (counts + clickable list of open sessions) and `Details` (handle, runtime, created, ID). Header shows breadcrumb `Runners › @handle`, role badge, and two actions: `Edit` (opens `RunnerEditDrawer`) and `Chat now`.
  - **Chat now flow.** Opens a small dialog asking for working directory (defaulting to the runner's own `working_dir` if set), calls `session_start_direct`, then routes to a new pane modeled on the C6 debug page minus the mission/runners-rail concepts. Route shape `/runners/:handle/chat/:sessionId` so multiple direct chats can stay open across runners.

**Tests.**
- Backend: `runner_list_with_activity` returns zero counters for a brand-new runner; reflects running mission sessions; reflects direct-chat sessions independently. Deleting a mission must leave its session row counted under the runner (per `sessions.mission_id ON DELETE SET NULL`) until the session itself is reaped.
- Backend: `session_start_direct` against `/bin/cat` → row has `mission_id IS NULL`, stdin injection round-trips, kill reaps cleanly, status reaches `stopped`. Concurrent direct chats on the same runner work and don't fight for the runner's `working_dir`.
- Backend: starting a direct session does **not** affect mission invariants — a crew that already has a live mission can still be inspected, and a direct chat does not block its lead's other crew from starting a new mission.
- Frontend: `/runners` renders with mocked activity payloads; sidebar routing; opening Runner Detail; round-trip the edit drawer; clicking a crew row navigates to `/crews/{id}`; Chat now opens the chat pane and bytes flow.

**Out of scope.**
- A persistent transcript log per direct-chat session. Direct chats are ephemeral by design — the C6 scrollback ring is the only memory. A real transcript would need its own append-only store and is deferred to v0.x.
- Renaming a `handle`. Globally-unique handles + cross-crew membership make immutability load-bearing — renaming would silently change `from`/`to` semantics on every historical event. Runner Detail surfaces only `display_name` as editable; a tooltip on `handle` explains why it's locked.
- Cross-window sync for activity counters. We don't ship multi-window in MVP.

**Manual test plan.** From the sidebar's Runner item, land on Runners list; verify activity badges; create a fresh runner from the Runners page (not from inside a crew); open its detail; verify the empty Crews-using-this-runner section; click Chat now; type a command into the runner's CLI and see output; close the chat; check that the activity counter on the list page dropped back to zero.

---

## C9 — `runner` CLI binary

**Goal.** The binary each runner's PTY calls to post events. Without this, runners can't talk to the log.

**Deliverables.**
- `cli/` crate in the workspace: `runner` binary.
- Resolves envelope fields from env vars set by C6.
- Commands:
  - `runner signal <type> [--payload <json>]`
  - `runner msg post <text> [--to <handle>]`
  - `runner msg read [--since <ts>] [--from <handle>]` — emits `inbox_read` signal with `payload.up_to = max ULID`.
  - `runner help`.
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

- Each chunk lives on its own branch off `main` (e.g. `feat/c8-orchestrator-v0`).
- Chunk PRs target `main` directly, merged with `--squash --delete-branch`.
- The original plan stacked chunks on a `feature/v0-mvp` umbrella branch
  that batched-merged into `main` once C11 landed. That added a layer of
  ceremony without buying anything: chunks already ship in
  feature-flagged-by-absence increments (an unfinished orchestrator just
  means the relevant signals don't get routed yet) and the umbrella was
  one extra rebase target. Dropped after C8.5; stale references to
  `feature/v0-mvp` elsewhere in the docs predate this change.
