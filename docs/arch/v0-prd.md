# Runners — v0 PRD

> Status: draft, open for feedback. Anything in **[OPEN]** is a decision we haven't taken yet.

## 1. Problem

Coding agents like Claude Code, Codex, and aider are each powerful alone, but there's no good way to run several of them *together* on one machine with different roles, a shared view of what's happening, and a sane way to pull in the human when they disagree or hit a wall.

Today, coordinating two agents means juggling terminal windows, eyeballing logs, and manually relaying messages. It breaks down past one agent, and doesn't scale as people start combining specialists (a coder + a reviewer + a tester + a fixer).

## 2. Goal

A local desktop app where one person can:

1. Assemble a **crew** of CLI coding agents on their own machine.
2. Give each **runner** a role and a brief (system prompt / instructions).
3. **Launch a mission** — one activation of the whole crew — and watch every runner's live output in one window.
4. Let runners **coordinate** through two channels: **signals** (typed, orchestrator-routable) and **messages** (prose, runner-to-runner).
5. Get pulled in by an **orchestrator** only when a decision needs a human.

v0 proves the loop works end-to-end with two runners on a single mission. v1+ scales it.

## 3. Vocabulary

- **Crew** — a named, persistent group of runners configured to work together.
- **Runner** — an individual CLI agent (one PTY, one role, one system prompt).
- **Mission** — one activation of the crew. Everyone spawns together, shares a coordination bus, ends together.
- **Session** — the live PTY process for a single runner within a single mission.
- **Signal** — a typed notification runners emit for the orchestrator to route on. Verb grammar (`review_requested`, `approved`, `blocked`).
- **Message** — prose posted to the mission's flat stream. Runner-to-runner conversation.
- **Orchestrator** — the rule-based router that reads signals and decides what happens next.

## 4. v0 scope

Runners and the mission coordinate through two primitives — **signals** and **messages**. We deliberately defer three concepts that belong to the vision but aren't needed for v0:

- **Threads** (v0.x) — scoped sub-conversations. v0 has flat messages.
- **Facts** (v0.x) — KV whiteboard for mission state. v0 has no shared queryable state.
- **Mentions, reactions** (v1) — targeted pings, lightweight signals on messages.

All four are described in `v0-arch.md` §2.7 with milestone tags, so the vision is clear — v0 just ships less.

### Other non-goals for v0

- Multiple concurrent missions per crew
- Cross-mission memory
- Session replay, session history browsing
- LLM-based orchestrator (v0 is rule-based only)
- Remote / cloud / multi-host runners
- Cost tracking, observability dashboards
- Runner templates, presets, marketplace
- Multi-human collaboration
- Secrets management beyond plain env vars

## 5. User journey (the v0 demo)

The concrete loop v0 must support end-to-end:

1. User creates a crew called *Feature Ship*.
2. User spawns two runners on the crew:
   - **Coder** — runtime `claude-code`, working dir `~/src/myproj`, brief "Implement feature X. When ready, signal `review_requested` and post a message explaining what changed."
   - **Reviewer** — runtime `claude-code`, same working dir, brief "When review is requested, read the Coder's messages and the diff, then signal `approved` or `changes_requested` and post messages with specific feedback."
3. User clicks **Start Mission**. Both PTYs spawn. User sees two terminals, one per runner.
4. Coder writes code, runs tests, then:
   - `runners msg post "Branch feat/x is ready. I refactored auth and added session tests."`
   - `runners signal review_requested`
5. Orchestrator routes `review_requested`: injects into Reviewer's stdin "A review is pending — check `runners msg read`."
6. Reviewer:
   - `runners msg read` (sees Coder's message)
   - reads the diff
   - `runners msg post "Line 47 auth.rs needs a null check."`
   - `runners msg post "session.rs timeout is 30s; our convention is 10s."`
   - `runners signal changes_requested`
7. Orchestrator policy for `changes_requested` says `ask_human`. The HITL panel pops a card: *"Reviewer requested changes. Accept and forward to Coder, or override?"*
8. User clicks **Accept**. Orchestrator injects into Coder's stdin: "Reviewer requested changes — check `runners msg read`."
9. Coder:
   - `runners msg read`
   - fixes the null check; defends the 30s timeout
   - `runners msg post "Added null check. Kept 30s timeout — provider is slow on cold start."`
   - `runners signal review_requested`
10. Reviewer reads, agrees, `runners signal approved`.
11. User clicks **End Mission**. Sessions terminate, mission status flips to `completed`, the mission appears in the crew's history list.

If v0 doesn't ship this flow working end-to-end, it hasn't shipped.

## 6. Features

### 6.1 Crew CRUD
- Create, rename, delete crews.
- A crew has: `name`, `goal` (default mission brief), list of runners, orchestrator policy, signal-type allowlist.
- Persisted in SQLite.

### 6.2 Runner CRUD (scoped to a crew)
- Spawn, edit, remove runners within a crew.
- A runner has:
  - `name` (display, e.g. "Coder")
  - `role` (short label, e.g. "implementation")
  - `runtime` — enum: `claude-code | codex | shell`. Adds the right default `command` + `args`.
  - `command` + `args` — concrete binary to spawn. Pre-filled from runtime, editable.
  - `working_dir`
  - `system_prompt` — the runner's role-specific brief.
  - `env` — key/value list, optional.

### 6.3 Missions (a crew's runtime activations)
- One-click **Start Mission** on a crew. Creates a new mission row, spawns a session per runner, opens the mission control screen.
- **End Mission** stops all sessions and marks status `completed` (or `aborted` if stopped mid-flight).
- v0: one live mission per crew. Starting a new mission requires ending the current one.
- A mission has: `id`, `crew_id`, `status` (running/completed/aborted), `goal_override` (optional per-mission brief overlay), `started_at`, `stopped_at`.
- Crew page shows mission history: list of past missions with start time, duration, outcome summary.

### 6.4 Live per-runner terminal
- One PTY subprocess per runner per mission, spawned via `portable-pty`.
- Stdout streams to the frontend via a Tauri event (`session:{id}:out`).
- Frontend renders with **xterm.js** to preserve ANSI colors, cursor control, and TUI layouts.
- Scrollback retained per session (cap at ~10k lines; overflow dumped to a per-session log file).
- Status chip per runner: `idle | running | waiting_for_input | blocked_on_human | crashed`.
- Stdin input box so the human can type directly into any runner.
- Terminals stay alive across tab/selection switches.

### 6.5 Signals — typed orchestrator-routable notifications
- Runners emit via `runners signal <type> [--payload <json>]`.
- Types are per-crew allowlisted (stored in `crews.signal_types`).
- Payload is optional JSON for the orchestrator's decision logic.
- Emitted signals appear as events in the coordination bus (see §6.7) and drive the orchestrator.

### 6.6 Messages — flat prose stream per mission
- Runners post via `runners msg post "<text>"`.
- Read via `runners msg read [--since <ts>]` — returns all messages in the mission in ULID order.
- No thread scoping in v0 — one flat stream per mission.
- Messages are human-readable and runner-readable. They are the "what I think" layer; signals are the "please act" layer.

### 6.7 Coordination bus
- Single append-only NDJSON file per mission at `$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson`.
- Both signals and messages are persisted as events with `kind: "signal"` or `kind: "message"`.
- File is watched with the `notify` crate. Orchestrator and UI both subscribe.
- Tailable with `tail -f` for debugging. Per-mission file solves log rotation implicitly.

### 6.8 `runners` CLI

```
runners signal <type> [--payload <json>] [--correlation-id <id>] [--causation-id <id>]
runners msg    post <text> [--correlation-id <id>] [--causation-id <id>]
runners msg    read [--since <ts>]
runners help
```

One binary, two verbs. Reads context (crew, mission, runner, log path) from env vars injected at PTY spawn. Bundled with the app; dropped at `$APPDATA/runners/bin/runners` on first run; PATH is prepended for each session so agents can invoke it unqualified.

See `v0-arch.md` §5.3 for the full three-layer emission mechanism (system prompt → CLI on PATH → role brief examples).

### 6.9 Orchestrator (rule-based)
- Policy is per-crew, stored as JSON on the crew row. Shared across all missions.
- In-memory state (pending asks, correlation tracking) is per-mission; cleared on mission end.
- Policy is an ordered list of `{ when, do }` rules. First match wins. Schema:
  ```json
  [
    { "when": { "signal": "review_requested" },
      "do": { "action": "inject_stdin", "target": "reviewer",
              "template": "A review is pending — run `runners msg read` for context." } },
    { "when": { "signal": "changes_requested" },
      "do": { "action": "ask_human",
              "prompt": "Reviewer requested changes. Accept or override?",
              "choices": ["accept", "override"] } },
    { "when": { "signal": "approved" },
      "do": { "action": "notify_human", "message": "Review approved." } }
  ]
  ```
- Supported actions in v0:
  - `inject_stdin` — write a message into the target runner's stdin.
  - `ask_human` — show a card in the HITL panel, wait for response.
  - `notify_human` — fire a toast.
  - `pause_runner` / `resume_runner` — SIGSTOP/SIGCONT the target PTY.
- Rules fire only on signals in v0. Messages don't drive routing (v0.x: mentions will).

### 6.10 Human-in-the-loop panel
- Right-rail panel showing all pending `ask_human` cards for the current mission.
- Each card shows: triggering signal, orchestrator prompt, choices.
- User clicks a choice → orchestrator emits a `human_response` signal with `correlation_id = triggering signal's id` which downstream rules can match.
- Cleared when the mission ends.

### 6.11 Mission control UI

Single screen per live mission. Layout:

```
┌──────────────────────────────────────────────────────────────────────┐
│ Feature Ship  •  Mission 2026-04-21 12:34    ▶ ⏸ ⏹    [End Mission]   │
├──────────┬──────────────────────────────────┬────────────────────────┤
│ Runners  │  ▌ Coder (running)               │ Pending asks           │
│          │  ┌───────────────────────────┐   │ ┌────────────────────┐ │
│ ● Coder  │  │ [xterm live output]       │   │ │ Reviewer requested │ │
│ ○ Reviewer│ │                           │   │ │ changes.           │ │
│          │  └───────────────────────────┘   │ │ [Accept] [Override]│ │
│ + Spawn  │  > _                             │ └────────────────────┘ │
│          │                                  │                        │
│          │                                  │ Messages               │
│          │  Signals (this mission)          │ coder  12:34           │
│          │  12:34 review_requested          │  Branch feat/x ready…  │
│          │  12:40 changes_requested         │ reviewer  12:38        │
│          │  ...                             │  Line 47 auth.rs…      │
└──────────┴──────────────────────────────────┴────────────────────────┘
```

- **Left rail**: runner list with status dots. Click to focus.
- **Main area**: focused runner's live terminal + this mission's signal log below.
- **Right rail**: HITL panel at the top, mission-wide messages stream below.
- **[OPEN]** side-by-side view of two runners' terminals. Deferrable to v0.x.

The **crew page** (not the mission page) lists past missions with status, start/stop times, and a one-line outcome summary pulled from the last few signals.

## 7. Data model

```sql
crews (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  goal TEXT,
  orchestrator_policy TEXT,           -- JSON
  signal_types TEXT,                  -- JSON array, CLI allowlist
  created_at TEXT, updated_at TEXT
);

runners (
  id TEXT PRIMARY KEY,
  crew_id TEXT REFERENCES crews(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  role TEXT NOT NULL,
  runtime TEXT NOT NULL,              -- claude-code | codex | shell
  command TEXT NOT NULL,
  args_json TEXT,
  working_dir TEXT,
  system_prompt TEXT,
  env_json TEXT,
  created_at TEXT, updated_at TEXT
);

missions (
  id TEXT PRIMARY KEY,
  crew_id TEXT REFERENCES crews(id) ON DELETE CASCADE,
  status TEXT NOT NULL,               -- running | completed | aborted
  goal_override TEXT,
  started_at TEXT NOT NULL,
  stopped_at TEXT
);

sessions (
  id TEXT PRIMARY KEY,
  mission_id TEXT REFERENCES missions(id) ON DELETE CASCADE,
  runner_id TEXT REFERENCES runners(id) ON DELETE CASCADE,
  status TEXT NOT NULL,               -- running | stopped | crashed
  started_at TEXT, stopped_at TEXT
);
```

Signals and messages live in the mission's NDJSON file, not in SQLite. SQLite is for config + session lifecycle only.

## 8. Tech boundaries

- **Backend:** Rust, Tauri 2. PTY via `portable-pty`. File watching via `notify`. Persistence via `rusqlite` (WAL).
- **Frontend:** React 19, TypeScript, Tailwind 4, xterm.js, React Router.
- **Coordination bus:** NDJSON file per mission.
- **Orchestrator:** Rust module in the Tauri backend, subscribes to the mission's event file via `notify`.
- **`runners` CLI:** small Rust binary, bundled with the app, dropped at `$APPDATA/runners/bin/runners`, PATH-prepended per session.

## 9. Open questions

1. **Side-by-side runner terminals** in v0, or defer to v0.x.
2. **How does the system prompt actually get passed** to each runtime? `claude-code` takes `--append-system-prompt`; `codex` has its own flag. Runtime enum owns the mapping.
3. **Restart semantics** — if a runner crashes, auto-restart? v0: no.
4. **Event ordering guarantees** — single-writer per line via `O_APPEND`. Document filesystem requirements (local POSIX).
5. **Does `msg read` paginate?** v0: return everything, client can filter by `--since`.
6. **Should the orchestrator include recent messages as context** when injecting stdin on a signal? Leaning yes — helpful for runners that don't immediately think to call `msg read`.

## 10. Risks

- **PTY flakiness across platforms** — especially Windows. v0 targets macOS only; Linux best-effort; Windows deferred.
- **TUI rendering in xterm.js** — claude/codex use rich TUIs. Budget time for tuning.
- **Runners that don't know the `runners signal` / `runners msg` conventions** — they can't coordinate. Ship starter briefs / system-prompt snippets per runtime.

## 11. Done criteria

v0 ships when:
- [ ] A user can create a crew, spawn two runners, click Start Mission, and see two live terminals.
- [ ] Runners can emit signals via `runners signal` and the UI shows them in the signal log.
- [ ] Runners can post and read messages via `runners msg`, and the UI shows the live messages pane.
- [ ] A rule-based policy can route a signal into another runner's stdin.
- [ ] A rule-based policy can pause the mission and ask the human a question, then resume based on the answer.
- [ ] End Mission terminates all sessions, marks the mission completed, and the crew page shows it in history.
- [ ] The Coder + Reviewer demo loop from §5 works end-to-end without the user touching a terminal outside the app.
