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
4. Let runners **coordinate** through a shared event log and a shared fact whiteboard.
5. Get pulled in by an **orchestrator** only when a decision needs a human.

v0 proves the loop works end-to-end with two runners on a single mission. v1+ scales it.

## 3. Vocabulary

- **Crew** — a named, persistent group of runners configured to work together.
- **Runner** — an individual CLI agent (one PTY, one role, one system prompt). Configured once as part of a crew; spawned fresh on each mission.
- **Mission** — one activation of the crew. Everyone spawns together, shares an event log and a fact whiteboard, ends together. A crew can have many missions over its lifetime; only one is live at a time.
- **Session** — the live PTY process for a single runner within a single mission. One runner × one mission = one session.
- **Event** — a structured NDJSON line runners emit to coordinate; routed by the orchestrator.
- **Fact** — a key/value entry in the mission's shared whiteboard, written by any runner via `runners ctx set`.
- **Orchestrator** — the rule-based router that reads events and decides what happens next (route to another runner, ask the human, etc.).

## 4. Non-goals for v0

- Multiple concurrent missions per crew
- Cross-mission memory (each mission starts with an empty fact whiteboard)
- Session replay, session history browsing
- LLM-based orchestrator (v0 is rule-based only)
- Remote / cloud / multi-host runners
- Cost tracking, observability dashboards, telemetry
- Runner templates, presets, or marketplace
- Multi-human collaboration (a crew is a crew of runners, not humans)
- Secrets management beyond plain env vars

## 5. User journey (the v0 demo)

The concrete loop v0 must support end-to-end:

1. User creates a crew called *Feature Ship*.
2. User spawns two runners on the crew:
   - **Coder** — runtime `claude-code`, working dir `~/src/myproj`, brief "Implement feature X. When done, emit `review_requested`."
   - **Reviewer** — runtime `claude-code`, same working dir, brief "Wait for `review_requested`. Read the diff. Emit `approved` or `changes_requested`."
3. User clicks **Start Mission**. A new mission is created; both PTYs spawn; user sees two terminals, one per runner.
4. Coder records a fact: `runners ctx set pr_branch feat/x`. Event appears on the mission timeline.
5. Coder writes code, runs tests, then calls `runners emit review_requested`.
6. Orchestrator policy routes the event: injects a message into Reviewer's stdin ("A review is pending, please proceed.").
7. Reviewer runs `runners ctx get pr_branch` to learn which branch to review, reads the diff, emits `changes_requested` with a payload listing issues.
8. Orchestrator policy for `changes_requested` says `ask_human`. The human-in-the-loop panel pops a card: *"Reviewer requested changes. Accept and forward to Coder, or override?"*
9. User clicks **Accept**. Orchestrator writes a `forward_to_coder` event with the reviewer's notes, which injects into Coder's stdin.
10. Coder fixes the issues, re-emits `review_requested`. Loop continues until `approved`.
11. User clicks **End Mission**. Sessions terminate, mission status flips to `completed`, the mission appears in the crew's history list.

If v0 doesn't ship this flow working end-to-end, it hasn't shipped.

## 6. Features

### 6.1 Crew CRUD
- Create, rename, delete crews.
- A crew has: `name`, `goal` (default mission brief), list of runners, an orchestrator policy, an allowlist of event types.
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
- **End Mission** stops all sessions in the mission and marks status `completed` (or `aborted` if stopped mid-flight).
- v0: one live mission per crew at a time. Starting a new mission requires ending the current one.
- A mission has: `id`, `crew_id`, `status` (running/completed/aborted), `goal_override` (optional per-mission brief overlay), `started_at`, `stopped_at`.
- Crew page shows mission history: list of past missions with start time, duration, outcome summary.

### 6.4 Live per-runner terminal (first-class)
- One PTY subprocess per runner per mission, spawned via `portable-pty`.
- Stdout streams to the frontend via a Tauri event (`session:{id}:out`).
- Frontend renders with **xterm.js** to preserve ANSI colors, cursor control, and TUI layouts. A dumb `<pre>` will look broken for claude/codex.
- Scrollback retained per session (cap at ~10k lines; overflow dumped to a per-session log file on disk).
- Status chip per runner: `idle | running | waiting_for_input | blocked_on_human | crashed`. Derived from PTY state + last event.
- Stdin input box so the human can type directly into any runner at any time.
- Terminals stay alive across tab/selection switches — we don't re-create the xterm instance on hide, or we lose the ANSI state machine and scrollback.

### 6.5 Shared context (per mission)

Three layers, each with different semantics:

#### 6.5.1 Mission brief (read-only)
The goal for this mission. Defaults to `crew.goal`; user can override at mission start (`goal_override` on the mission row). Injected into each runner's system prompt at spawn.

#### 6.5.2 Roster (read-only)
Who else is on the crew and what they do. Auto-assembled at mission start from `crew.runners`. Each runner sees a rendered list of crewmates with their roles and a summary of their briefs, injected into its system prompt at spawn. This is how the Reviewer knows there's a Coder.

#### 6.5.3 Facts — the shared whiteboard (mutable)
A key-value store any runner can read/write during the mission.

- Write: `runners ctx set <key> <value>`
- Read: `runners ctx get <key>` or `runners ctx list`
- Delete: `runners ctx unset <key>`
- Last-writer-wins per key.
- Flat namespace in v0 — no `ctx set foo.bar` convention yet; just `ctx set foo_bar`.

Backed by the same NDJSON event log. `ctx set` emits a `fact_recorded` event. The orchestrator maintains an in-memory projection; `ctx get` reads through a local endpoint on the orchestrator (or re-scans the log). No second store. See §6.8.1 in `v0-arch.md` for mechanics.

### 6.6 Event bus (inter-runner comm)
- Single append-only **NDJSON** file per mission, at `$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson`.
- One line per event. Event schema:
  ```json
  {
    "id": "ULID",
    "ts": "2026-04-21T12:34:56.123Z",
    "crew_id": "...",
    "mission_id": "...",
    "from": "coder",
    "to": null,
    "type": "review_requested",
    "payload": { "...": "..." },
    "correlation_id": null,
    "causation_id": null
  }
  ```
- File is watched with the `notify` crate. UI and orchestrator both subscribe.
- File is tailable with `tail -f` for debugging. Deliberate.
- One file per mission solves log rotation implicitly — each mission gets a fresh file.

#### 6.6.1 How runners emit events — **[DECIDED]**

CLI wrapper: a `runners` binary on PATH. When an agent executes `runners emit <type> --payload <json>`, it reads its context from env vars (`RUNNERS_CREW_ID`, `RUNNERS_MISSION_ID`, `RUNNERS_RUNNER_NAME`, `RUNNERS_EVENT_LOG`) and appends one JSON line to the current mission's event log.

The agent learns to use this via its system prompt (which is composed by the backend to include the CLI's usage and the crew's allowed event types). See `v0-arch.md` §3.3 for the full three-layer emission mechanism.

Alternatives considered and deferred:
- MCP tool — revisit in v1 once MCP is ubiquitous in CLI agents.
- Stdout parsing — fragile, interferes with TUI output.

### 6.7 Orchestrator (rule-based)
- Policy is per-crew, stored as JSON on the crew row. Shared across all missions of that crew.
- In-memory state (pending asks, correlation tracking, fact projection) is per-mission; cleared on mission end.
- Policy is an ordered list of `{ when, do }` rules. First match wins. Schema:
  ```json
  [
    { "when": { "type": "review_requested" },
      "do": { "action": "inject_stdin", "target": "reviewer",
              "template": "A review is pending. Please proceed." } },
    { "when": { "type": "changes_requested" },
      "do": { "action": "ask_human",
              "prompt": "Reviewer requested changes. Accept or override?",
              "choices": ["accept", "override"] } },
    { "when": { "type": "approved" },
      "do": { "action": "notify_human", "message": "PR approved by reviewer." } }
  ]
  ```
- Supported actions in v0:
  - `inject_stdin` — write a message into the target runner's stdin.
  - `ask_human` — show a card in the HITL panel, wait for response, emit a follow-up event with the answer.
  - `notify_human` — fire a toast, don't block.
  - `pause_runner` / `resume_runner` — send SIGSTOP/SIGCONT to the target PTY.
- No expressions, no scripting, no LLM. v0 is a lookup table.

### 6.8 Human-in-the-loop panel
- Right-rail panel showing all pending `ask_human` cards for the current mission.
- Each card shows: triggering event, orchestrator prompt, choices.
- User clicks a choice → orchestrator writes a response event `{type: "human_response", correlation_id: <triggering event id>, choice: "accept"}` which any downstream rule can match.
- Visible across all views so the user never misses one.
- Cleared when the mission ends.

### 6.9 Mission control UI

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
│          │  Events (this runner)            │ Facts                  │
│          │  12:34 fact_recorded pr_branch   │ pr_branch = feat/x     │
│          │  12:35 emit review_requested     │ main_branch = main-dev │
│          │  12:36 stdin injected            │                        │
│          │                                  │ Event stream (all)     │
└──────────┴──────────────────────────────────┴────────────────────────┘
```

- **Left rail**: runner list with status dots. Click to focus.
- **Main area**: focused runner's live terminal + that runner's event log below.
- **Right rail**: HITL panel, live facts view (the shared whiteboard), and the global event timeline.
- **[OPEN]**: interleaved terminal + events, or split? **Recommendation: split.** Events below the terminal, timestamp-aligned.
- **[OPEN]**: side-by-side view of two runners' terminals. Deferrable to v0.x.

The **crew page** (not the mission page) lists past missions with status, start/stop times, and a one-line outcome summary pulled from the last few events.

## 7. Data model

```sql
crews (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  goal TEXT,                          -- default mission brief
  orchestrator_policy TEXT,           -- JSON
  event_types TEXT,                   -- JSON array, CLI allowlist
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
  goal_override TEXT,                 -- optional per-mission brief
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

Events and facts live in the mission's NDJSON file, not in SQLite. SQLite is for config + session lifecycle only.

## 8. Tech boundaries

- **Backend:** Rust, Tauri 2. PTY via `portable-pty`. File watching via `notify`. Persistence via `rusqlite` (WAL).
- **Frontend:** React 19, TypeScript, Tailwind 4, xterm.js, React Router.
- **Event log + facts:** NDJSON file per mission. No message broker, no WebSockets, no DB row stream.
- **Orchestrator:** Rust module, runs in the Tauri backend, subscribes to the mission's event file via `notify`.
- **`runners` CLI:** small Rust binary, bundled with the app, dropped at `$APPDATA/runners/bin/runners` on first run, PATH-prepended for each session.

## 9. Open questions

1. **Terminal + events visual layout** — interleaved or split. Recommendation: split.
2. **Side-by-side runner terminals** in v0, or defer to v0.x.
3. **How does the system prompt actually get passed** to each runtime? `claude-code` takes `--append-system-prompt`; `codex` has its own flag. The runtime enum in §6.2 should own this mapping.
4. **Restart semantics** — if a runner crashes, auto-restart? v0 answer: no, surface the crash and let the human click Restart.
5. **Event ordering guarantees** — single-writer per file should give us total order via append. Confirm `fs::OpenOptions::new().append(true)` writes <`PIPE_BUF` are atomic on macOS (they are, but document).
6. **Fact namespacing** — flat or prefixed. v0 answer: flat.
7. **Cross-mission memory** — v0 answer: no. Each mission starts empty.

## 10. Risks

- **PTY flakiness across platforms** — especially Windows. v0 targets macOS only; Linux best-effort; Windows deferred.
- **TUI rendering in xterm.js** — claude/codex use rich TUIs. xterm.js is mature but some escape sequences may still render oddly. Budget time for tuning.
- **Runners that don't know the `runners emit` / `runners ctx` conventions** — they can't coordinate. We need starter briefs / system-prompt snippets per runtime.

## 11. Done criteria

v0 ships when:
- [ ] A user can create a crew, spawn two runners, click Start Mission, and see two live terminals.
- [ ] Runners can emit events via `runners emit` and the UI shows them in real time.
- [ ] Runners can read/write facts via `runners ctx` and the UI shows the live fact whiteboard.
- [ ] A rule-based policy can route an event into another runner's stdin.
- [ ] A rule-based policy can pause the mission and ask the human a question, then resume based on the answer.
- [ ] End Mission terminates all sessions, marks the mission completed, and the crew page shows it in history.
- [ ] The Coder + Reviewer demo loop from §5 works end-to-end without the user touching a terminal outside the app.
