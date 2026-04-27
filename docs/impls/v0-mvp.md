# v0 MVP — Implementation Plan and Status

> Umbrella plan for the first end-to-end vertical slice of Runner. Twelve ordered chunks (C1–C11 plus the C5.5a amendment), each its own PR merged directly into `main`. The original umbrella-branch model was dropped after C8.5 — see `## Branching` at the bottom for the rationale.
>
> Companion to `docs/arch/v0-arch.md` (architecture) and `docs/arch/v0-prd.md` (scope). This file is the single source for both the MVP implementation plan and current build status.

## Current status — 2026-04-27

> **2026-04-27 update.** C8 (signal router v0) and C9 (`runner` CLI binary) are both merged. The router observes the bus, dispatches built-in signals to handlers (bootstrap, ask_lead/human_said relays, ask_human → human_question card, human_response routing, runner_status idle nudges), and reconstructs pending-ask + status state from the log on reopen via a high-water mark. The CLI exposes `signal`, `msg post`, `msg read`, `status`, and `help`; emits `inbox_read` correctly (skipped on `--from` filtered reads to avoid global-watermark corruption); validates against per-crew signal_types and per-mission roster sidecars. The bundled CLI is built by Tauri's `beforeDev`/`beforeBuild` hooks and installed into `$APPDATA/runner/bin/runner` at app startup. Remaining MVP work is the mission workspace UI (C10) and the Missions list + Start Mission modal (C11).
>
> **2026-04-26 plan revision.** C8 was reframed from "orchestrator v0" to "signal router v0" — a flat parent-process dispatcher, not a rule engine. The dispatch ledger, replay idempotence, inbox-summary enrichment, and policy loader were all explicitly descoped because the lead runner already owns coordination judgment; C8 only owns the plumbing (bootstrap, cross-process stdin push, UI bridge) the lead can't do from inside a child PTY. See **C8 — Signal router v0** below for the rationale and the descoped list. The cross-cutting prompt/runtime adapter is now part of C8 instead of a separate prerequisite.


The persistence, configuration, PTY runtime, event log, event bus, signal router, and `runner` CLI are all in place. The remaining MVP work is the mission workspace UI and the Start Mission entrypoint.

### Implemented

| Area | PR | What is live |
|------|----|--------------|
| C1 Schema + shared types | #4 | SQLite schema, Rust/TS domain types, default signal-type allowlist seeded per crew. |
| C2 Config commands | #7 | Global runner CRUD, crew CRUD, crew membership commands, and per-crew lead invariant enforced in Rust plus the `one_lead_per_crew` partial unique index. |
| C3 Config UI | #9 | Crews page, Crew Detail, Add Slot modal, runner edit drawer. |
| C4 Event log | #10 | `runner-core` event-log primitives: `flock`-scoped NDJSON append, monotonic ULIDs with an on-disk floor, crash-tail repair, lossy reads for malformed tails. |
| C5 Mission lifecycle | #11 | Transactional `mission_start` / `mission_stop`, one-live-mission-per-crew invariant, opening/terminal log events, atomic `signal_types.json` sidecar. |
| C5.5a Shared runners | #13 | Runners are top-level rows reused through `crew_runners`; `sessions.mission_id` is nullable for direct-chat sessions. |
| C6 PTY runtime | #12 | `portable-pty` session manager, reader threads, stdin injection, kill/reap semantics, all-or-nothing mission spawn rollback, `$APPDATA/runner/bin` on child `PATH`. |
| C7 Event bus | #14 | `notify` watcher per live mission, replay-on-mount, messages-only inbox projection, `inbox_read` watermarks, `event/appended`, `inbox/updated`, `watermark/advanced` events. |
| C8.5 Runner surfaces | #15 | `/runners`, `/runners/:handle`, direct-chat session backend, `runner_list_with_activity`, `runner/activity` live counters. |
| Rename / namespace cleanup | #16 + follow-up | Project/crate/app namespace is singular `runner`; env vars are `RUNNER_*`; app data is under `$APPDATA/runner`; planned CLI binary is `runner`. SQL table names stay plural where they represent row collections. |
| Direct-chat frontend hardening | #17 in review | xterm.js direct-chat pane, persistent sidebar SESSION list, PTY resize handshake, base64 raw PTY output for TUI fidelity. Two review follow-ups are open: reload reattach on the chat route itself, and waiting for output/exit listener registration before spawning. |
| C8 Signal router v0 + runtime adapter | #18 | Flat parent-process dispatcher (`src-tauri/src/router/`): handlers for `mission_goal`, `human_said`, `ask_lead`, `ask_human`, `human_response`, `runner_status`. Pending-ask + status maps reconstruct on reopen via a replay high-water ULID; live tail short-circuits at-or-below the watermark. Runtime adapter wires `runner.system_prompt` into both mission and direct-chat spawn paths (claude-code → `--append-system-prompt`; codex deferred until a verified flag exists). |
| C9 `runner` CLI binary | #19 | New `cli/` workspace member produces `runner-cli`, installed at app startup as `$APPDATA/runner/bin/runner` (rename on copy). Verbs: `signal`, `msg post`, `msg read`, `status`, `help`. Validates against per-crew `signal_types.json` and per-mission `roster.json` sidecars (frozen at mission_start). `inbox_read` is suppressed on `--from` filtered reads to avoid corrupting the global per-runner watermark. Tauri's `beforeDev`/`beforeBuild` build the CLI alongside the app so the dev install path needs no manual cargo step. |

### What runs today

- **Crews and runners:** Users can create crews and runners, add runners to crews, reorder slots, set/remove the lead, edit runner fields, and delete crews/runners with cascade/promotion behavior.
- **Direct chat:** Users can open a top-level runner, start a direct PTY session, type through xterm.js, resize the terminal, and stop the session. Direct chats do not join any mission event bus.
- **Mission start/stop backend:** `mission_start` creates a mission row, writes opening events, mounts the event bus, and spawns one PTY child per crew member. `mission_stop` appends the terminal event, kills/reaps sessions, and unmounts the bus.
- **Event transport:** Mission logs are durable NDJSON files at `$APPDATA/runner/crews/{crew_id}/missions/{mission_id}/events.ndjson`; the in-process bus replays and tails them into Tauri events.
- **Coordination loop:** Spawned mission runners get the composed launch prompt injected into the lead's stdin on `mission_goal`; workers escalate via `ask_lead`; the lead can `ask_human` for HITL cards; `human_response` routes back to the asker; non-lead `runner_status idle` reports nudge the lead.
- **CLI:** The bundled `runner` binary is dropped into `$APPDATA/runner/bin/` at app startup. Spawned agents can `runner signal`, `runner msg post`, `runner msg read`, `runner status`. Direct-chat sessions (no env vars) no-op cleanly.
- **Tests:** `pnpm exec tsc --noEmit`, `pnpm run lint`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` all pass. Backend coverage is 87 Tauri-app tests + 24 CLI tests (11 unit + 13 integration) + 20 runner-core tests.

### Known gaps in implemented surfaces

- **No mission workspace UI or Start Mission UI yet.** The backend commands and the router are wired end-to-end; what's missing is the polished `/missions` entrypoint and the workspace pane that renders the event feed, ask-human cards, runner rail, and embedded xterms. Tracked as C10 + C11.
- **Production sidecar packaging.** The dev path now installs the bundled CLI into `$APPDATA/runner/bin/` via Tauri's `beforeDev` hook. Production installer builds (`tauri build`) do not yet ship the CLI as a `bundle.externalBin` sidecar — needs a build-step that stages `runner-cli-<target-triple>` for the bundler. v0 ships in dev; this is a release-engineering follow-up that doesn't affect the demo path.
- **Codex `system_prompt` flag.** The runtime adapter currently emits `--append-system-prompt` only for the `claude-code` runtime; `codex --instructions` was tried first but the installed Codex CLI rejected it. Codex runners spawn without their `system_prompt` until a verified flag is identified.

### Integrated C5.5 amendment context

C5.5 was originally a standalone amendment after the first config UI shipped. It is now part of the main MVP plan and live schema, but the rationale matters for future work:

1. **Runners are top-level and shared.** A runner is a reusable config row. A crew composes existing runners through `crew_runners`, so one runner can sit in multiple crews.
2. **Sessions can exist without missions.** `sessions.mission_id` is nullable and direct-chat sessions store their own `cwd`; this powers the Runners page's Chat now flow.
3. **Runner activity is first-class.** The UI needs per-runner session/mission counters, so the backend exposes `runner_activity`, `runner_list_with_activity`, and live `runner/activity` events.

Schema consequences:
- `runners` has no `crew_id`, `position`, or `lead`; handles are globally unique.
- `crew_runners` owns membership, per-crew `position`, and per-crew `lead`.
- `sessions.mission_id` is nullable with `ON DELETE SET NULL`; direct sessions use `cwd` on the session row.

Command consequences:
- Runner CRUD is global (`runner_create`, `runner_update`, `runner_delete`, `runner_list`, `runner_get`).
- Crew membership lives in `crew_add_runner`, `crew_remove_runner`, `crew_set_lead`, `crew_reorder`, and `crew_list_runners`.
- `session_start_direct` spawns a PTY without `RUNNER_CREW_ID`, `RUNNER_MISSION_ID`, or `RUNNER_EVENT_LOG`.

Product consequences:
- Handles are immutable. They are the addressing primitive in event envelopes, CLI commands, and historical logs.
- Orphan runners are intentional. Removing a runner from every crew leaves it available for reuse and direct chat.
- Cross-crew conflict resolution is deferred. If two live crews reference the same runner config at once, v0 treats those as separate sessions of the same runner.

## Remaining v0 work

The launch/prompt adapter that was previously listed as a separate cross-cutting prerequisite is now folded into C8 — see the "Cross-cutting prerequisite" block under C8.

### C8 — Signal router v0

**Reframing.** The earlier plan called this an "orchestrator" and described it as a deterministic policy state machine with a dispatch ledger and inbox-summary enrichment. That framing oversold what's actually needed. The lead runner is the agent that *thinks* about coordination — it plans, dispatches workers via directed messages, decides when to escalate. C8 is the parent-process plumbing under that lead, doing four things the lead can't do from inside a child PTY:

1. **Bootstrap.** Write the composed launch prompt (`runner.system_prompt + mission goal + roster + coordination instructions + signal allowlist`) to the lead's stdin on `mission_start`. The lead can't bootstrap itself — there's no LLM yet when the mission opens.
2. **Cross-process stdin push.** A worker's stdin is owned by the parent. So `ask_lead` (worker → lead's stdin) and `human_said` (UI → lead's or worker's stdin) require a parent-side router.
3. **UI bridge.** `ask_human` becomes a card on the workspace. The lead emits the signal; only the parent can render the card and capture the click. `human_response` then routes the answer back to the original asker.
4. **Availability bridge.** Workers self-report `runner_status` (`busy` / `idle`) as signals. The router keeps the latest status map and tells the lead when a worker becomes idle so the lead can assign the next task. The router does not infer status from terminal bytes and does not decide what the worker should do next.

That's it. There are no policy rules to evolve — these are a few hardcoded mechanisms. v0.x can revisit policy/LLM-in-the-loop framing if user-defined signal types ever ship; MVP has no place for it.

**Where:** `src-tauri/src/orchestrator/mod.rs` is a stub; rename to `src-tauri/src/router/` with this chunk so the next reader doesn't expect a framework.

**Required behavior:**
- Mount per live mission when `mission_start` succeeds; unmount when `mission_stop` completes or spawn rollback aborts.
- Subscribe to the existing `EventBus` (which already replays-then-tails). Handle each event in arrival order through one flat dispatcher.
- Hardcoded signal handlers (signal-driven only — per arch §5.5.0, messages never trigger router actions; per arch §5.2, signals always carry `to: null` and any target lives in `payload.target`):
  - `mission_goal` → inject the composed launch prompt to the crew lead's stdin.
  - `human_said` → inject `payload.text` to `payload.target` if present, otherwise to the lead.
  - `ask_lead` → inject the worker's question/context to the lead.
  - `ask_human` → append a `human_question` event preserving `on_behalf_of`; the workspace UI (C10) renders the card from that event.
  - `human_response` → look up the matching `question_id` in an in-memory pending-ask map and inject the answer to the original asker (the lead in the lead-mediated flow, the worker in the direct flow).
  - `runner_status` → update the latest-status map from `payload.state` (`busy` / `idle`) and inject a short availability update to the lead when a non-lead runner reports `idle`.
- Pending-ask map: in-memory `HashMap<question_id, asker_handle>` populated when an `ask_human` event is observed (live or during replay). No persistence; on reopen the map is reconstructed by re-reading the log through the same handler.
- Runner-status map: in-memory `HashMap<runner_handle, RunnerStatus>` populated from `runner_status` events and session lifecycle (`crashed` / `stopped` still come from the session row). Reopen reconstructs it from the log before live tail begins.
- Dead-session errors produce a visible mission-warning event in the log, not a silent drop. The mission workspace surfaces these.

**Explicitly descoped (was in the original C8 doc, deferred to v0.x):**
- **Dispatch ledger / replay idempotence.** The router is not re-run against historical events on reopen; live tail starts from the current end of the log. `mission_goal` only fires once per mission anyway, `human_question` is rendered from log replay by the UI (not re-emitted), and re-injecting old prompts into a sleeping LLM is bizarre UX. The pending-ask map is the only state that needs reopen reconstruction — we get it for free by re-reading `ask_human`/`human_response` rows in order, no ledger required.
- **Inbox-summary enrichment in injected stdin templates.** Originally the router was going to prepend the recipient's unread message summary onto every injection and advance the watermark via a synthesized `stdin_injected` event. MVP drops this — the lead can call `runner msg read` itself when it wants its inbox, and that's the documented contract. Keeping enrichment out of the injection path means the router does not have to write log events, only consume them.
- **Rule abstraction / policy loader.** No `Rule` trait, no policy JSON loaded from `crews.orchestrator_policy`. The handlers are a `match signal_type { … }` and that's the whole router.

**Cross-cutting prerequisite — launch/prompt adapter.**
- `mission_goal`'s injected prompt is `runner.system_prompt + mission goal + roster + coordination instructions + signal allowlist`. There's no composer today.
- `runner.system_prompt` is also dropped on the floor by `SessionManager::spawn` and `spawn_direct`. C8 must add a runtime adapter (`claude-code` → `--append-system-prompt`, `codex` → its equivalent, fallback → documented behavior) and apply it on both the mission and direct-chat spawn paths.
- Direct chats receive the runner's default `system_prompt` only; no roster, no goal.
- Tests assert resolved command/env contains the prompt for claude-code on both paths.

**Risks to settle:**
- Stdin writes are a mutex-protected write path, not a queued command stream. MVP keeps one handler output per triggering event; anything more would need per-session sequencing.
- The pending-ask map is in-memory only. If the app crashes between a worker's `ask_human` and the user's response, the map is lost. v0 accepts this — the next reopen rebuilds the map from log replay before any new events tail in.

### C9 — `runner` CLI binary

**Where:** there is no `cli/` crate in the workspace yet.

Required behavior:
- Add a `cli/` workspace member producing the `runner` binary.
- Resolve envelope fields from `RUNNER_CREW_ID`, `RUNNER_MISSION_ID`, `RUNNER_HANDLE`, `RUNNER_EVENT_LOG`.
- Implement:
  - `runner signal <type> [--payload <json>]`;
  - `runner msg post <text> [--to <handle>]`;
  - `runner msg read [--since <ulid>] [--from <handle>]`;
  - `runner status <busy|idle> [--note <text>]` as a convenience wrapper that emits `signal runner_status`;
  - `runner help`.
- `msg read` must project the caller's inbox and emit `inbox_read` with `payload.up_to = max ULID` for messages shown.
- Reuse `runner_core::event_log` for append/read; do not duplicate log writer semantics.
- Install or copy the binary to `$APPDATA/runner/bin` so the existing PATH prepend wins inside spawned sessions.

Risks to settle:
- Direct-chat sessions intentionally do not set mission/event-log env vars. CLI commands in that context must print a clear no-bus message or no-op cleanly, not crash the agent process.
- Packaging needs executable bits on macOS/Linux and a predictable update path when the app ships a newer CLI.
- The CLI's signal-type validation must match the eight built-ins seeded in C1 before user-defined signal types are opened up.

### C10 — Mission workspace UI

**Where:** no `MissionWorkspace.tsx` page or `/missions/:id` route exists yet.

Required behavior:
- Add `src/pages/MissionWorkspace.tsx` and route `/missions/:id`.
- Subscribe to `event/appended`, `inbox/updated`, and `watermark/advanced`, filtering by `mission_id`.
- Render:
  - event feed with message, signal, system, and terminal event variants;
  - `AskHumanCard` for pending `human_question` events, including attribution chains like `@impl -> @architect -> you`;
  - mission input that emits `signal human_said` with envelope `to: null` and optional `payload.target`;
  - runner rail with sessions, lead badge, status dots, and open-terminal action;
  - xterm-backed runner terminal using the same raw-byte `session/output` contract as direct chat.
- Reopen behavior: loading `/missions/:id` must fetch mission/session metadata, replay visible feed state from the event log or bus snapshot, and reattach terminals to live sessions where available.

Risks to settle:
- Feed backpressure: agent output and event volume must not make React render unbounded rows on every chunk.
- The workspace needs a clear distinction between chat/feed events and raw PTY output; raw terminal output should stay in xterm, not the event feed.
- Ask-human cards must dedupe across replay and live tail.

### C11 — Missions list + Start Mission modal

**Where:** no `Missions.tsx` page or `StartMissionModal.tsx` exists. The backend commands are already exposed.

Required behavior:
- Add `src/pages/Missions.tsx` with Active/Past tabs, mission rows, status, started/stopped timestamps, crew name, and open/stop actions.
- Add `StartMissionModal` with crew picker, title, goal textarea, cwd picker, and an Advanced section stub.
- Start flow: call `mission_start`, then route to `/missions/:id`.
- Reopen flow: selecting an active mission routes to C10's workspace and reconstructs feed + router state.
- Pending ask indicator: derive from the router's pending-ask map once C8 exposes it (or, if the router isn't mounted yet for that mission, scan the log for unmatched `ask_human` rows — see the risks block).

Risks to settle:
- The pending-ask flag either reads from the live router's pending-ask map or runs an on-demand log scan for unmatched `ask_human` rows. Live-map read is better for list performance; log scan is acceptable for MVP-sized data and is the only option for missions whose router isn't mounted (e.g., before the user opens the workspace).
- `/debug` should be removed or hidden behind a dev flag once this lands, because it currently bypasses the intended user flow.

## Definition of done (demo path)

From a clean launch of the app, a user can:

1. Create a **Crew** on the Crews page, then add two runners to it (one `claude-code` lead, one `shell` worker) on the **Crew Detail** page. Per C5.5a, runners are top-level config and shared across crews — adding a runner to a crew creates a `crew_runners` membership row, not a new runner. The lead invariant is per-crew (one lead per crew, enforced via partial unique index on `crew_runners`) and is checked end-to-end.
2. Click **Start Mission**, fill the goal, and see the Mission workspace open with two live PTY sessions.
3. Watch the lead runner receive the goal via stdin, draft a plan, and post a directed message to the worker; see the worker pick it up on its next `runner msg read`.
4. See a worker emit an `ask_lead` signal; watch the lead decide to escalate via `ask_human`; click **Approve** on the resulting card; see the lead receive the response and forward it to the worker.
5. Post a broadcast human signal from the workspace input and have it land on the lead by default.
6. Close and reopen the mission; the feed replays and the router's in-memory pending-ask map reconstructs from the log.

Anything beyond this is explicitly v0.x or later.

## Out of scope for MVP

- Windows support (macOS + Linux only for v0).
- Threads / reactions / reply-to semantics beyond `--to <handle>`.
- LLM-in-the-loop signal routing (v0's router is a hardcoded dispatcher; see arch §2.3).
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
    │           └─► C7  event bus + notify watcher ─► C8  signal router v0
    │
    └─► C4  event log primitives  (feeds C5, C7, C9)

  C10  mission workspace UI   (depends on C3, C7, C8)
  C11  missions list + Start Mission modal   (depends on C3, C5, C10)
```

C3 and C4 can run in parallel after C2 lands. C6 and C7 can run in parallel after C5. C8 (signal router) and C8.5 (Runners page) are peers — both depend on C6, neither depends on the other, so they can ship in either order.

---

## C1 — Schema + shared types

**Goal.** Lay down the SQLite schema and the Rust/TS type surface that every later chunk consumes.

**Deliverables.**
- `src-tauri/src/db.rs` — connection pool with WAL mode, `rusqlite` migrations runner, bootstrapped at app start.
- Migration `0001_init.sql` — implements **arch §7.1 verbatim**, including the four tables (`crews`, `runners`, `missions`, `sessions`) and the `one_lead_per_crew` partial unique index. No additions, no renames. The plan used to list the columns inline; that list has been deleted to remove the two-source-of-truth risk the earlier review called out. Implementers copy §7.1 directly into `0001_init.sql`.
- **Default signal-type allowlist.** Every new crew row is seeded with `signal_types = ["mission_goal", "human_said", "ask_lead", "ask_human", "human_question", "human_response", "runner_status", "inbox_read"]` — the full set of built-in types the MVP needs. Users can extend this list in v0.x; in MVP it is write-only from the DB layer. Without this seeding the CLI will reject the built-in signals at spawn time per arch §5.3 Layer 2.
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

**Scope note — Runners are top-level in MVP, but the dedicated Runners pages land in C8.5.** C5.5a already moved runners out from under crews and made the same runner shareable across crews; the data model has no notion of "crew-scoped runner" anymore. C3 still does runner CRUD inside Crew Detail (Add Slot + edit drawer) because that's the path the demo flow needs. The standalone Runners list and Runner Detail frames in `design/runners-design.pen` (`2Oecf`, `ocAFJ`) are built in C8.5 (sibling chunk of C8 signal router).

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

**Out of scope.** Router reactions to events — that's C8.

---

## C8 — Signal router v0

**Goal.** A flat parent-process dispatcher that wires built-in signal types into stdin pushes, runner availability projection, and a UI-card event. Not a framework — a hardcoded `match` plus the prompt composer it needs for `mission_goal`. See "C8 — Signal router v0" in the **Remaining v0 work** section above for the full reframing rationale and explicit descoping.

**Deliverables.**
- `src-tauri/src/router/mod.rs` (renamed from the existing `orchestrator/` stub):
  - `Router::for_mission(mission, crew_roster, sessions, log)` — mounted by `mission_start`, unmounted by `mission_stop` and the spawn-rollback path.
  - `Router::handle_event(&Event)` — single entry point invoked by the bus on each appended event. `EventKind::Message` is a no-op (per arch §5.5.0); `EventKind::Signal` matches on `signal_type` against the built-ins.
  - In-memory pending-ask map keyed by `question_id`. Reconstructed on reopen by replaying `ask_human` / `human_response` rows through the same handler before live tail begins.
  - In-memory runner-status map keyed by handle. Reconstructed on reopen by replaying `runner_status` rows and session lifecycle state before live tail begins.
  - Dead-session writes append a `mission_warning` event to the log instead of silently dropping. The workspace UI surfaces these.
- `src-tauri/src/router/handlers.rs` (or inline in `mod.rs` if it fits):
  - `mission_goal` → compose launch prompt, inject to the lead's stdin via `SessionManager::inject_stdin`.
  - `human_said` → resolve recipient (`payload.target` or lead), inject `payload.text`.
  - `ask_lead` → render worker's `{question, context}` into a short stdin template, inject to the lead.
  - `ask_human` → append a `human_question` event carrying `on_behalf_of` (if present) and the original `ask_human` id as `triggered_by`. UI renders the card from the appended event in C10.
  - `human_response` → look up `question_id` in the pending-ask map; inject `payload.text` to the original asker. Unmatched `human_response` logs a warning event, no panic.
  - `runner_status` → accept `payload.state = "busy" | "idle"` and optional `payload.note`; update the status map; when a non-lead reports `idle`, inject a short availability update to the lead.
- `src-tauri/src/router/prompt.rs` — composes `runner.system_prompt + mission goal + roster + coordination instructions + signal allowlist` into the `mission_goal` injection. Pure function over inputs; no I/O; easy to unit-test.
- Cross-cutting **launch/prompt adapter** (must land in this chunk):
  - `src-tauri/src/runtime.rs` — adapter trait + per-runtime impls. `claude-code` injects `runner.system_prompt` via `--append-system-prompt`. `codex` ships with a TODO until its CLI flag is verified. Fallback runtimes get a documented no-op + a warning log.
  - Apply on both `SessionManager::spawn` (mission) and `spawn_direct` (direct chat). Direct chat gets the runner's `system_prompt` only — no roster, no goal.

**Tests.**
- Each handler fires exactly once per triggering event under live tail.
- Reopen reconstructs the pending-ask map: append `ask_human` then `mission_stop`, reopen, append `human_response`, assert the right asker's stdin received the answer.
- Reopen reconstructs the runner-status map from `runner_status` rows; a worker `idle` signal updates the map and reaches the lead.
- `human_response` without a matching `human_question` emits a `mission_warning`, not a panic.
- `messages_do_not_trigger_router_actions` — appending an `EventKind::Message` produces no `inject_stdin` call.
- Runtime adapter resolves `--append-system-prompt` for claude-code on both `spawn` and `spawn_direct`. Missing `system_prompt` is fine (no flag added).

**Out of scope.**
- Dispatch ledger / replay idempotence — descoped, see reframing section.
- Inbox-summary enrichment in injection templates — descoped, the lead calls `runner msg read` itself.
- LLM policy, user-authored rules, `crews.orchestrator_policy` schema usage — deferred to v0.x. The column stays in the schema for forward compatibility but is unread in MVP.

---

## C8.5 — Runners page + Runner Detail + direct chat

**Goal.** Promote runners to a top-level UI surface, mirroring the design's `2Oecf` (Runners list) and `ocAFJ` (Runner Detail) frames, plus a "Chat now" path that exercises the direct-chat session shape C5.5a already baked into the schema. Without this chunk, runners that aren't in any crew are invisible and the user can never spawn a runner without going through a full mission.

**Why it sits at C8.5.** Sibling/parallel to C8 (signal router) — both depend on C6 and neither depends on the other, following the C5.5a precedent for inserted chunks. The router and the Runners page can ship in either order; this just records that the work is part of v0-mvp, not deferred.

**Scope-shift context.** Originally cut from MVP under the C3 "no top-level Runners page" scope note, now restored: the C5.5a schema work is wasted UI-side until this lands.

**Deliverables.**
- **Backend.**
  - `commands/runner.rs::runner_list_with_activity()` — extends the existing `runner_list` to include `running_session_count` (from `sessions WHERE status = 'running'`) and `open_mission_count` (from `crew_runners ⨝ missions WHERE status = 'running'`). The Runners list cards need both counters.
  - `commands/runner.rs::runner_get_by_handle(handle)` — used by `/runners/:handle` so the URL is stable across runner-id rotations.
  - `commands/session.rs::session_start_direct(runner_id, cwd)` — inserts a `sessions` row with `mission_id = NULL` and the chosen `cwd`, then spawns through the existing `SessionManager::spawn` path. Differences from the mission flavor: no `RUNNER_MISSION_ID`, `RUNNER_EVENT_LOG`, or `RUNNER_CREW_ID` env vars are set, and the runner does not join any event bus or signal router. The `runner` CLI must no-op gracefully when those vars are absent (small change in C9-land — the CLI errors today on `RUNNER_EVENT_LOG`-not-set, which would crash a direct-chat agent the moment it tries to emit an event).
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

**Out of scope.** Any form of direct-to-router RPC — everything goes through the log.

---

## C10 — Mission workspace UI

**Goal.** Render the live mission in the design's "Mission workspace" frame.

**Deliverables.**
- `src/pages/MissionWorkspace.tsx` — subscribes to `event/appended`, renders the feed.
- `src/components/EventFeed.tsx` — message / signal / `ask_human` card variants.
- `src/components/AskHumanCard.tsx` — buttons emit a `human_response` signal. If the underlying `human_question` carries `on_behalf_of`, render the attribution chain (e.g. *@impl → @architect → you*).
- `src/components/MissionInput.tsx` — the Slack-channel input. Default recipient in the UI is `@<lead>`. Submitting always emits a `signal human_said` (not a message event) so the router can wake the recipient, per arch §5.5.0. Signal envelope keeps `to: null` per arch §5.2; the picked recipient lives in `payload.target` (omitted for broadcast, set to the handle for directed). The UI label can still say "message" for user-facing clarity; the underlying event kind is `signal`.
- `src/components/RunnersRail.tsx` — list of sessions with status dot, `LEAD` badge, "open pty" action.
- `src/components/RunnerTerminal.tsx` — xterm.js bound to the session output stream (popped out of the rail).

**Manual test plan.** End-to-end demo path from the "Definition of done" section.

**Out of scope.** The Start Mission modal itself — that's C11.

---

## C11 — Missions list + Start Mission modal

**Goal.** The entrypoint to everything C10 renders. The final chunk that closes the loop.

**Deliverables.**
- `src/pages/Missions.tsx` — Active / Past tabs, mission rows, status dot, "pending ask" flag derived from the router's pending-ask map (or a log scan for unmounted missions).
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

Areas: `db`, `commands`, `ui`, `event-log`, `session`, `event-bus`, `router`, `cli`, `mission`.

## Branching

- Each chunk lives on its own branch off `main` (e.g. `feat/c8-router-v0`).
- Chunk PRs target `main` directly, merged with `--squash --delete-branch`.
- The original plan stacked chunks on a `feature/v0-mvp` umbrella branch
  that batched-merged into `main` once C11 landed. That added a layer of
  ceremony without buying anything: chunks already ship in
  feature-flagged-by-absence increments (an unfinished router just
  means the relevant signals don't get routed yet) and the umbrella was
  one extra rebase target. Dropped after C8.5; stale references to
  `feature/v0-mvp` elsewhere in the docs predate this change.
