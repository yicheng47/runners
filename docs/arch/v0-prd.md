# Runners — v0 PRD

> Status: draft, open for feedback. Anything in **[OPEN]** is a decision we haven't taken yet.
>
> **Canonicity.** `docs/arch/v0-arch.md` is the source of truth for all protocol, schema, and event-model decisions. Where this PRD conflicts with the arch doc, the arch doc wins. Sections marked **⚠️ SUPERSEDED** below have been overtaken by arch updates and are kept only for historical context until the PRD is rewritten:
> - §5 (Golden path) — the `changes_requested → ask_human → inject Coder` flow has been replaced by lead-mediated HITL (arch §2.2, §5.5.0).
> - §6.8 (`runners` CLI) — the `--correlation-id` / `--causation-id` flags are dropped (arch §5.2).
> - §6.11.1 — "clicking a message highlights correlated signals" relies on the dropped correlation fields.

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
- **Message** — prose posted to the mission. Can be broadcast (to the mission) or directed (to a specific runner via `--to`).
- **Inbox** — each runner's projection of the mission: broadcast messages plus messages addressed directly to it. Read via `runners msg read`. Pull-based: runners check their inbox on convention; there is no automatic interrupt for arriving messages.
- **Orchestrator** — the rule-based router that reads signals and decides what happens next. Signals are the urgent wake-up channel; when the orchestrator injects stdin on a signal, it also appends a summary of the recipient's unread inbox so relevant messages ride along.

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
2. User spawns two runners on the crew (handles shown in backticks; display names in parens):
   - `coder` (Coder) — runtime `claude-code`, working dir `~/src/myproj`, brief "Implement feature X. When ready, signal `review_requested` and post a message explaining what changed."
   - `reviewer` (Reviewer) — runtime `claude-code`, same working dir, brief "When review is requested, read `coder`'s messages and the diff, then signal `approved` or `changes_requested` and post messages with specific feedback."
3. User clicks **Start Mission**. Both PTYs spawn. User sees two terminals, one per runner.
4. Coder writes code, runs tests, then:
   - `runners msg post "Branch feat/x is ready. I refactored auth and added session tests."`
   - `runners signal review_requested`
5. Orchestrator routes `review_requested`: injects into Reviewer's stdin "A review is pending — check `runners msg read`."
6. Reviewer:
   - `runners msg read` (sees Coder's message)
   - reads the diff
   - `runners msg post --to coder "Line 47 auth.rs needs a null check."`
   - `runners msg post --to coder "session.rs timeout is 30s; our convention is 10s."`
   - `runners signal changes_requested`
       *(orchestrator rule fires: injects into Coder's stdin — "Reviewer requested changes — check msg read." — and automatically appends a summary of Coder's 2 unread direct messages.)*
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

### 6.2 Runner CRUD (top-level, shared across crews)
- Create, edit, delete runners as standalone config; add or remove a runner's membership in a given crew via `crew_runners`. The same runner can sit in multiple crews simultaneously (post-C5.5a).
- A runner has:
  - `handle` — lowercase slug (e.g. `coder`). Immutable once set; **globally unique** across the app. Used everywhere addressing is needed (`from`/`to` in events, `--to <handle>` on the CLI, policy rules) so `@coder` names the same runner everywhere it appears.
  - `display_name` — free-form UI label (e.g. "Coder", "Lead Reviewer"). Editable; presentation-only.
  - `role` — short label, e.g. "implementation".
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

### 6.4 Live per-runner terminal (with human takeover)
- One PTY subprocess per runner per mission, spawned via `portable-pty`.
- Stdout streams to the frontend via a Tauri event (`session:{id}:out`).
- Frontend renders with **xterm.js** to preserve ANSI colors, cursor control, and TUI layouts.
- Scrollback retained per session (cap at ~10k lines; overflow dumped to a per-session log file).
- Status chip per runner: `idle | running | waiting_for_input | blocked_on_human | crashed`.
- **Human takeover, first-class.** The xterm pane is a real terminal, not a log viewer. At any moment the human can type directly into a runner's stdin — to answer a prompt, correct a bad plan, kill a runaway tool, or just chat with the agent mid-flight. Keystrokes including arrows, Enter, and Ctrl-C pass through untouched. Same writer path as the orchestrator's `inject_stdin`, so human and orchestrator are symmetric.
- **Sessions outlive the UI.** Closing the mission control window does not kill sessions — agents keep running, events keep flowing, the orchestrator keeps routing. Re-opening re-attaches by fetching each session's scrollback ring. A session ends only on End Mission, child exit, or app quit. This means the human can close the monitor and still rely on the orchestrator + rules without cutting anyone out of the loop.

### 6.5 Signals — typed orchestrator-routable notifications
- Runners emit via `runners signal <type> [--payload <json>]`.
- Types are per-crew allowlisted (stored in `crews.signal_types`).
- Payload is optional JSON for the orchestrator's decision logic.
- Emitted signals appear as events in the coordination bus (see §6.7) and drive the orchestrator.

### 6.6 Messages — prose with broadcast or direct addressing
- Broadcast: `runners msg post "<text>"` — visible to everyone in the mission.
- Direct: `runners msg post --to <runner> "<text>"` — lands only in that runner's inbox.
- Read the inbox: `runners msg read [--since <ts>] [--from <runner>]` — returns the calling runner's inbox (broadcasts + directs addressed to me), sorted by ULID.
- No thread scoping in v0 — one flat stream per mission.
- Messages are human-readable and runner-readable. They are the "what I think" layer; signals are the "please act" layer.

**Inbox as a concept.** Every runner has an inbox — a projection over the mission's message events where `to = null OR to = my_handle`. This is not a separate store; it's a filtered view of the event log. "Inbox" names the view so users and agents share a clear mental model: *I have an inbox; I can read mine; I can address others.*

**Messages are pull-based.** The system does not automatically interrupt a busy runner every time a message arrives. Two reasons: (1) not every direct message is urgent, and auto-interrupting would blur the signal/message split; (2) stdin injection on every DM risks corrupting in-flight tool calls.

Runners learn to check their inboxes through two mechanisms:

1. **Convention.** Each runner's composed prompt instructs it to check `runners msg read` at natural task boundaries (before a new task, before emitting a signal, while waiting).
2. **Signals as the wake-up channel.** If a sender needs immediate attention, they emit a signal in addition to (or instead of) the message. Signal routing through `inject_stdin` automatically enriches the injection with the recipient's unread inbox summary — so urgent wake-ups carry the related conversation with them. See `v0-arch.md` §5.5.1.

### 6.7 Coordination bus
- Single append-only NDJSON file per mission at `$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson`.
- Both signals and messages are persisted as events with `kind: "signal"` or `kind: "message"`.
- File is watched with the `notify` crate. Orchestrator and UI both subscribe.
- Tailable with `tail -f` for debugging. Per-mission file solves log rotation implicitly.

### 6.8 `runners` CLI

```
runners signal <type> [--payload <json>] [--correlation-id <id>] [--causation-id <id>]
runners msg    post <text> [--to <runner>] [--correlation-id <id>] [--causation-id <id>]
runners msg    read [--since <ts>] [--from <runner>]
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
  - `inject_stdin` — write a message into the target runner's stdin. Automatically enriched with the recipient's unread inbox summary; watermark advances on the resulting `stdin_injected` event (see `v0-arch.md` §5.5.1).
  - `ask_human` — show a card in the HITL panel; emits a `human_question` signal. On click, emits a `human_response` signal with payload `{ question_id, choice }` and `correlation_id` = the original triggering signal's id. Downstream rules match on `payload.choice`.
  - `notify_human` — fire a toast.
  - `pause_runner` / `resume_runner` — SIGSTOP/SIGCONT the target PTY.
- Rules fire on signals only. Messages do not trigger orchestrator actions in v0 — the inbox is pull-based. Senders escalate via signals when immediate attention is needed. (v0.x: mentions inside messages will trigger routing.)

### 6.10 Human-in-the-loop panel
- Right-rail panel showing all pending `ask_human` cards for the current mission.
- Each card shows: triggering signal, orchestrator prompt, choices.
- User clicks a choice → orchestrator emits a `human_response` signal with `correlation_id = triggering signal's id` which downstream rules can match.
- Cleared when the mission ends.

### 6.11 Mission control UI

One screen per live mission. Surfaces and their responsibilities:

- **Runner list** — every runner in the crew with a status indicator (`idle | running | waiting_for_input | blocked_on_human | crashed`). Selecting one focuses the main terminal area on it. Includes a control to spawn additional runners.
- **Focused terminal** — the xterm.js view for the selected runner. Live PTY output with full TUI fidelity, stdin input for human takeover, and scrollback. Terminals stay alive across focus switches (§6.4).
- **Signals pane** — chronological list of all signals emitted in this mission: timestamp, emitter, type. Scoped to the mission.
- **HITL panel** — pending `ask_human` cards. Each card shows the triggering signal, the orchestrator's prompt, and choice buttons. Always visible so the operator never misses a pending decision.
- **Messages pane** — every message in the mission (see §6.11.1 for visibility semantics and view modes).
- **Mission header** — crew name, mission start time, global controls (Start/Pause/Stop, End Mission).

Side-by-side view of two runners' terminals is **[OPEN]** for v0 — deferrable to v0.x if it adds scope pressure.

#### 6.11.1 Message visibility — the human sees everything

The operator is omniscient — the messages pane shows **every message in the mission**, regardless of addressing. Inbox scoping (broadcast + directed to me) applies to runners, not to the human. The human needs the full picture for oversight and debugging.

Each message is labeled with direction: sender and recipient (or "all" for broadcast). Typical rows:

- `coder → all` — a broadcast from the Coder.
- `reviewer → coder` — a directed message from the Reviewer to the Coder.
- `human → coder` — a message the operator sent via the UI (v0.x).

The pane has two view modes, toggled at the pane header:

- **All** (default) — flat chronological list of every message event in the mission.
- **Inbox** — scoped to the currently-focused runner — shows only what that runner would get from `runners msg read` (broadcasts + directs addressed to it). Useful for debugging "did agent X actually see this message?"

Clicking a message highlights any signal it correlates with (via `correlation_id` / `causation_id`) in the signals pane, so the operator can follow cause-and-effect threads across the mission.

#### 6.11.2 Crew page

The **crew page** (separate from the mission control screen) lists past missions with status, start/stop times, and a one-line outcome summary pulled from the last few signals.

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
  handle TEXT NOT NULL,               -- lowercase slug, immutable, unique within crew
  display_name TEXT NOT NULL,
  role TEXT NOT NULL,
  runtime TEXT NOT NULL,              -- claude-code | codex | shell
  command TEXT NOT NULL,
  args_json TEXT,
  working_dir TEXT,
  system_prompt TEXT,
  env_json TEXT,
  created_at TEXT, updated_at TEXT,
  UNIQUE (crew_id, handle)
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
