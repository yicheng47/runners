# Runners — v0 Architecture

> Companion to `v0-prd.md`. The PRD defines *what* v0 ships; this doc defines *how* it works. Mostly about four things: **missions**, **PTY runner sessions**, **the event bus**, and **shared context**.

## 1. System overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Tauri process (runners desktop app)                                         │
│                                                                             │
│  ┌──────────────────────┐   ┌──────────────────────┐   ┌─────────────────┐  │
│  │ MissionManager       │   │ SessionManager       │   │ EventBus        │  │
│  │  - lifecycle of the  │   │  - spawns PTYs       │   │  - tail NDJSON  │  │
│  │    live mission      │   │  - reader threads    │   │  - notify watch │  │
│  │  - roster + brief    │   │  - writer handles    │   │  - ring buffer  │  │
│  └────────┬─────────────┘   └────────┬─────────────┘   └────────┬────────┘  │
│           │                          │                          │           │
│           │                          │                          ▼           │
│           │                          │               ┌──────────────────┐   │
│           │                          │               │ Orchestrator     │   │
│           │                          │               │  - policy rules  │   │
│           │                          │               │  - action dispch │   │
│           │                          │               │  - fact project. │   │
│           │                          │               └────────┬─────────┘   │
│           │                          │                        │             │
│           │         inject_stdin / ask_human / ...            │             │
│           └─────────────────────────►│◄───────────────────────┘             │
│                                      ▼                                      │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │ Runner session (one per runner × mission)                            │   │
│  │   ┌──────────┐   PTY   ┌─────────────────────────────────────────┐   │   │
│  │   │  master  │ ◄────►  │  child: claude-code / codex / shell     │   │   │
│  │   └──────────┘         │  env: RUNNERS_CREW_ID,                  │   │   │
│  │                        │       RUNNERS_MISSION_ID,               │   │   │
│  │                        │       RUNNERS_RUNNER_NAME,              │   │   │
│  │                        │       RUNNERS_EVENT_LOG, PATH=/bin:...  │   │   │
│  │                        └─────┬───────────────────────────────────┘   │   │
│  └─────────────────────────────┼──────────────────────────────────────┘   │
│                                │                                          │
│                                │ runs `runners emit ...` / `ctx set ...`  │
│                                ▼                                          │
│                  ┌─────────────────────────────┐                          │
│                  │  events.ndjson (per mission)│                          │
│                  └──────────────┬──────────────┘                          │
│                                 │  notify → EventBus → Orchestrator + UI  │
└─────────────────────────────────┼─────────────────────────────────────────┘
                                  ▼
                         Tauri events to webview
                         ┌───────────────────────┐
                         │ React + xterm.js      │
                         │  - terminal panes     │
                         │  - event timeline     │
                         │  - HITL cards         │
                         │  - facts view         │
                         └───────────────────────┘
```

Five in-process components (MissionManager, SessionManager, EventBus, Orchestrator, Tauri commands), one on-disk artifact (events.ndjson), one webview.

## 2. Missions

### 2.1 Why missions exist

"Crew" is a config. "Mission" is a run. Without this split, the event log is a flat history of everything that ever happened to the crew, and there's no clean scope for:
- The current HITL queue
- The current fact whiteboard
- Orchestrator in-memory state
- Event log files (otherwise grow unbounded)
- UI history ("show me what happened on mission 3")

With missions, everything scopes to one run, and starting a new mission is a clean reset.

### 2.2 Lifecycle

```
user clicks Start Mission on a crew
  └─► MissionManager.start(crew_id):
        ├─ insert row into `missions` with status=running
        ├─ mkdir $APPDATA/runners/crews/{crew_id}/missions/{mission_id}/
        ├─ touch events.ndjson
        ├─ for each runner in crew:
        │     compose system prompt (brief + roster + coordination notes)
        │     SessionManager.spawn(mission_id, runner, composed_prompt)
        ├─ Orchestrator.start(mission_id):  ← fresh state
        │     open events.ndjson, read history (empty at boot), tail via notify
        └─ emit Tauri event: mission:{id}:started
```

```
user clicks End Mission (or all sessions have exited)
  └─► MissionManager.end(mission_id, status):
        ├─ SessionManager.kill_all_in_mission(mission_id)
        ├─ Orchestrator.stop(mission_id)
        ├─ update `missions` row: status, stopped_at
        └─ emit Tauri event: mission:{id}:ended
```

v0 constraint: one live mission per crew. Starting a new one when one is live is blocked in the UI. (v1 relaxation: concurrent missions per crew.)

### 2.3 What the MissionManager owns

- The `mission_id` of the currently live mission (if any) per crew.
- References to the orchestrator instance and session manager entries for that mission.
- The composed system prompt for each runner, built once at mission start.

## 3. PTY runner sessions

### 3.1 Why PTY at all

Claude Code and Codex are TUIs. They check `isatty()`; if false, they degrade (no colors, no spinner, sometimes outright refuse). Their output is a stream of terminal escape sequences (`\x1b[2K`, alt-screen toggles, cursor moves) that only make sense to a terminal emulator.

A pseudo-terminal solves both:
- The child sees a real terminal on its stdin/stdout/stderr → full TUI mode.
- We hold the master end, capture the raw byte stream, and feed it to **xterm.js** in the webview, which knows how to render escape sequences.

Anything less (plain pipes, stdout-only capture) will look broken.

### 3.2 Session lifecycle

A session is one run of one runner, within one mission.

```
  spawn (called by MissionManager)
    └─► portable_pty::openpty(rows, cols)
          ├─ master handle  → kept by SessionManager
          └─ slave handle   → given to child via spawn_command()
    └─► child inherits env + composed system prompt (passed via runtime-specific flag)
    └─► reader thread:
          loop { read(master) → emit session:{id}:out event, push to ring buffer }
          on EOF: wait(child) → emit session:{id}:exit → update sessions row
    └─► ring buffer (last ~10k lines) for scrollback
```

On the frontend, on first view:
1. Ask backend for session's current ring buffer → write into xterm.js to restore scrollback.
2. Subscribe to `session:{id}:out` → stream live output.
3. xterm.js `onData` (typing) → invoke `send_input(session_id, bytes)` → `master.writer.write_all(bytes)`.

On resize (webview resizes, or xterm fit addon fires):
1. Frontend sends new (rows, cols) to backend, debounced ~100ms.
2. `master.resize(rows, cols)` — the kernel delivers SIGWINCH to the child; the TUI redraws at the right size.

Without resize handling, the TUI renders to the wrong width and looks mangled. This is non-optional.

### 3.3 The env the child inherits

```
PATH                = $APPDATA/runners/bin:<original PATH>
RUNNERS_CREW_ID     = <ulid>
RUNNERS_MISSION_ID  = <ulid>
RUNNERS_RUNNER_NAME = coder
RUNNERS_EVENT_LOG   = $APPDATA/runners/crews/<crew>/missions/<mission>/events.ndjson
```

These env vars are how the child — and anything it spawns — finds the `runners` CLI and knows which crew, mission, and runner it's acting as.

### 3.4 The composed system prompt

On mission start, the MissionManager builds each runner's system prompt by concatenating:

1. **The user-authored brief** (`runners.system_prompt`).
2. **The mission brief** (`missions.goal_override` or falls back to `crews.goal`).
3. **The roster** — rendered list of crewmates with their names, roles, and a one-line brief summary.
4. **Coordination notes** — how to use `runners emit`, `runners ctx`, allowed event types.

Example composed prompt for the Reviewer:

```
You are Reviewer, a runner in crew "Feature Ship".
Your role: code review.

== Your brief ==
Wait for review_requested events. Read the diff on the branch recorded
in the `pr_branch` fact. Emit `approved` or `changes_requested`.

== Mission ==
Goal: Implement feature X with tests and a clean PR.

== Your crewmates ==
- Coder (implementation): Writes code. Emits review_requested when ready.

== Coordination ==
Use `runners emit <type> [--payload '{...}']` to signal milestones.
Use `runners ctx get <key>` to read facts, `runners ctx set <key> <value>` to record them.
Event types in this crew: review_requested, changes_requested, approved, blocked.
```

The prompt is passed to the runtime via its native flag: `--append-system-prompt` for claude-code; equivalent for each runtime. The runtime enum in the runners table owns the flag mapping.

### 3.5 Threads, not async

`portable-pty`'s reader is blocking. Don't wrap it in tokio — spawn an OS thread per session. Writers can stay on the Tauri async runtime; writes are short.

### 3.6 Scrollback — in Rust, not xterm.js

xterm.js has scrollback built in, but we want it to survive tab-switches, crashes, and restarts. Keep a `VecDeque<String>` ring (line-granular, ~10k cap) in the SessionManager. Overflow lines append to `$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/sessions/{session_id}.log`.

The ring sees raw bytes including alt-screen toggles — acceptable scuff for v0.

### 3.7 Process death + kill semantics

Reader thread owns the child handle. On `read()` → 0 bytes, call `wait(child)`, emit `session:{id}:exit { code }`, update sessions row. No auto-restart in v0.

Kill: drop master handle; `portable-pty` sends SIGHUP; escalate to SIGKILL if the child lingers. v0 targets macOS; Linux best-effort; Windows deferred.

## 4. Inter-runner communication (A2A)

### 4.1 Transport: append-only NDJSON file per mission

```
$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson
```

One line, one event. Source of truth for everything downstream — orchestrator, UI timeline, fact projection, future analytics all read the same file.

Why per-mission:
- Log rotation is implicit (new mission = new file).
- Scoping: crash-replay only needs to read the live mission's file.
- Delete mission = delete directory.

Why a file:
- **Debuggable**: `tail -f events.ndjson | jq .`.
- **Crash-durable**: whatever's on disk survived the crash.
- **Atomic appends**: writes < `PIPE_BUF` (4KB on macOS) to a file opened `O_APPEND` are atomic at the OS level. Multiple runners running `runners emit` concurrently interleave correctly.
- **Replay for free**: restart the orchestrator, re-scan, resume.

### 4.2 Event schema

```jsonc
{
  "id": "01HG3K1YRG7RQ3N9...",     // ULID: time-sortable, monotonic within ms
  "ts": "2026-04-21T12:34:56.123Z",
  "crew_id": "01HG...",
  "mission_id": "01HG...",
  "from": "coder",                  // runner name | "human" | "orchestrator"
  "to": null,                       // null = broadcast, or target runner name
  "type": "review_requested",       // free-form v0; allowlisted per crew
  "payload": { "...": "..." },      // type-specific JSON
  "correlation_id": null,           // ties related events into a conversation
  "causation_id": null              // which event caused this one
}
```

**ULID for `id`** — sortable, embeds a ms timestamp. Two events in the same millisecond still have a defined order.

**`correlation_id`** — shared by all events in one "conversation" (e.g. every event in one review cycle). Set by the first emitter and propagated by the orchestrator to events it generates in response.

**`causation_id`** — the immediate trigger. Every event has a parent (or null if root). Together, correlation + causation reconstruct the event graph as a DAG.

### 4.3 How runners emit events

The answer is three layers, and answers the question "how does the agent know it should append to the event log?"

#### Layer 1 — system prompt tells the agent the convention

When MissionManager spawns a runner (§3.4), the composed prompt includes a "Coordination" section that describes the `runners emit` CLI and lists the crew's allowed event types. Claude Code / Codex / any LLM agent already knows how to read CLI tool documentation and invoke tools; this is the same capability it uses for `git`, `gh`, `npm`, `pytest`. We just provide one more tool + docs.

#### Layer 2 — the CLI exists on PATH with context in env

The Tauri backend prepends `$APPDATA/runners/bin/` to PATH and drops the `runners` binary there at install/first-run. At session spawn we set `RUNNERS_CREW_ID`, `RUNNERS_MISSION_ID`, `RUNNERS_RUNNER_NAME`, `RUNNERS_EVENT_LOG`.

On invocation:
1. Reads env vars; errors if missing ("not inside a runners PTY").
2. Builds an event object (generates ULID, stamps `from`, `ts`, `crew_id`, `mission_id`).
3. Validates `type` against the allowlist loaded via a sidecar file `$APPDATA/runners/crews/{crew_id}/event_types.json` (written by the backend from `crews.event_types`).
4. Appends one JSON line to `$RUNNERS_EVENT_LOG` via `open(O_APPEND | O_WRONLY)` + `write_all` + close.
5. Exits 0 on success, non-zero with clear stderr otherwise.

Single-writer atomicity holds because each invocation writes ≤ one 4KB line in one `write` syscall.

#### Layer 3 — role briefs reinforce usage

The user-authored brief (`runners.system_prompt`) should include examples at the points where emission matters. We ship defaults per-runtime so first-time users get good behavior without thinking about it.

Example default brief for a reviewer role:
> After reading the diff, emit either `approved` or `changes_requested` with a payload listing issues. Do not write code yourself.

#### Why this setup is robust

- **No out-of-band protocol in the PTY stream.** Not parsing `[[runners:event:...]]` out of stdout.
- **Works for any CLI agent**, with or without MCP. Only requirement: "can execute shell commands."
- **Fails visibly.** If the agent never emits, the orchestrator fires no rules and the runner sits idle. The user sees it in the UI and can adjust the brief or inject stdin manually.
- **Fully observable.** The literal string `$ runners emit review_requested` appears in the terminal pane; the event it produced appears in the timeline pane. One-to-one, no magic.

#### The one real failure mode

Hallucinated event types. v0 mitigation: the CLI validates against the allowlist. Unknown type → exit non-zero with `unknown event type "foo"; expected one of: ...`. The agent reads the error from its shell history and self-corrects. v1: stricter JSON schema per event type.

### 4.4 Consumers — orchestrator and UI via `notify`

The NDJSON file has exactly two subscribers, both using the `notify` crate:

- **Orchestrator** — Rust task. On each new line, deserializes, runs policy, dispatches actions. Also updates the fact projection for any `fact_recorded` event.
- **UI** — Tauri backend re-emits each line as a `mission:{id}:event` event. Frontend renders the timeline pane and fact view.

#### Startup replay

On orchestrator start (triggered by mission start):
1. Open the mission's `events.ndjson`.
2. Read all events (empty at mission boot; non-empty if the app restarted mid-mission).
3. Rebuild in-memory state: fact projection, pending `ask_human` cards, correlation tracking.
4. Switch to tailing mode via `notify`.

This is how we get crash-safety for free — the file *is* the state.

### 4.5 Orchestrator actions

| Action | Effect | Emits event? |
|---|---|---|
| `inject_stdin` | write template + `\r` to target runner's PTY master writer | `stdin_injected` |
| `ask_human` | add card to HITL panel; waits for user click | `human_question`, then `human_response` on answer |
| `notify_human` | fire a toast in the UI | `human_notified` |
| `pause_runner` | SIGSTOP to target PTY | `runner_paused` |
| `resume_runner` | SIGCONT to target PTY | `runner_resumed` |

Emitted events have `causation_id` = the triggering event's `id`, so the chain is fully reconstructable.

#### Crash correctness

Emit the event *before* taking the action. Worst case on crash+replay: a duplicate action. Better than silent loss. For `inject_stdin`, a duplicate injection is recoverable (the agent sees the prompt twice). For `ask_human`, dedupe cards by event id.

### 4.6 Who does delivery

Runners never address other runners. They emit; the orchestrator routes.

- **Decoupled runners** — the Coder doesn't know the Reviewer exists. Swap the Reviewer without touching the Coder's brief.
- **Single policy location** — every "when X happens, do Y" lives on the crew row.
- **Orchestrator is the only thing with side-effects** outside runner processes. Easy to test, easy to reason about.

### 4.7 Failure modes and v0 mitigations

| Failure | Mitigation |
|---|---|
| Orchestrator crashes mid-action | Emit event before action; replay on boot; accept duplicate actions |
| Two runners ask human at once | HITL panel queues both; user answers in order |
| Event storm (runner bug-looping) | Surface events-per-second warning in UI; no rate limit in v0 |
| Malformed NDJSON line | Skip line, log warning; file stays valid |
| NDJSON file grows large in a long mission | End the mission. New mission = new file. |
| Hallucinated event type | CLI validates against allowlist; clear stderr for self-correction |

## 5. Shared context (mission-scoped)

Three layers, each with different mechanics.

### 5.1 Mission brief (read-only, prompt-injected)

`missions.goal_override` or falls back to `crews.goal`. Appears in the composed prompt at spawn (§3.4). Never changes during a mission.

### 5.2 Roster (read-only, prompt-injected)

Assembled from the crew's runners at mission start. Rendered into each runner's prompt as "== Your crewmates ==" with name, role, one-line brief. Never changes during a mission (v0 constraint: no mid-mission crew changes).

### 5.3 Facts — the shared whiteboard (mutable, event-backed)

A key-value store any runner can read/write during the mission. Implemented on top of the event log; no second store.

#### Write path

```
runners ctx set <key> <value>
  └─► CLI emits a synthetic event:
        { type: "fact_recorded", payload: { key, value } }
      into the same events.ndjson as any other event.

runners ctx unset <key>
  └─► CLI emits: { type: "fact_recorded", payload: { key, value: null } }
```

#### Read path

```
runners ctx get <key>       → single value
runners ctx list            → all current key/value pairs
```

Implementation options for reads:
- **(A) CLI re-scans the log** (fold fact_recorded events, last-writer-wins). Simple, slightly wasteful on large logs.
- **(B) Orchestrator serves an HTTP endpoint on localhost**; CLI makes one GET per query. Faster, more moving parts.

**v0: option (A).** Missions are short; logs are small. Upgrade to (B) if profiling shows it matters.

#### Projection

The orchestrator keeps an in-memory `HashMap<String, serde_json::Value>`, updated on each `fact_recorded` event during normal tailing, and rebuilt from scratch on boot replay. The UI's fact view is driven by this projection via a Tauri event emitted on each change.

#### Why log-structured (not a separate table/file)

- **Single source of truth** — facts are events.
- **Auditable** — you can see who set what and when.
- **Replayable** — projection rebuilds from the log.
- **Atomic** — one append per write, no read-modify-write races.
- **Observable** — a fact update appears in the timeline like any event.

#### Optional snapshot file

The orchestrator may periodically write `missions/{mission_id}/context.json` as a `cat`-friendly snapshot of the current fact projection. Not load-bearing; purely debugger ergonomics.

### 5.4 What the `runners` CLI actually looks like

```
runners emit   <type> [--payload <json>] [--correlation-id <id>] [--causation-id <id>]
runners ctx    get <key> | set <key> <value> | unset <key> | list
runners help
```

One binary, two verbs, bundled with the app. Context always read from env vars.

## 6. Data model

### 6.1 SQLite (config + session lifecycle only)

```sql
crews (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  goal TEXT,
  orchestrator_policy TEXT,           -- JSON: [{ when, do }]
  event_types TEXT,                   -- JSON array: allowlist
  created_at TEXT, updated_at TEXT
);

runners (
  id TEXT PRIMARY KEY,
  crew_id TEXT REFERENCES crews(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  role TEXT NOT NULL,
  runtime TEXT NOT NULL,
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

### 6.2 Filesystem

```
$APPDATA/runners/
├── bin/
│   └── runners                              # the CLI (emit + ctx)
├── runners.db                               # SQLite
└── crews/
    └── {crew_id}/
        ├── event_types.json                 # CLI allowlist sidecar
        └── missions/
            └── {mission_id}/
                ├── events.ndjson            # per-mission event log
                ├── context.json             # optional snapshot of facts
                └── sessions/
                    └── {session_id}.log     # scrollback overflow
```

On macOS `$APPDATA` = `~/Library/Application Support/com.wycstudios.runners`. Dev builds use a `-dev` suffix.

## 7. Process and thread model

```
Tauri main thread
  ├── Tauri async runtime (tokio)
  │     ├── MissionManager (async)
  │     ├── Orchestrator task per live mission (notify + policy dispatch)
  │     └── Command handlers (crew/runner/mission/session/event CRUD)
  ├── Thread per active session (blocking PTY reader)
  └── Webview process (React + xterm.js)
```

For v0 scale (one live mission, ≤ ~10 sessions), this is fine.

## 8. What's out of scope for v0 architecture

- Concurrent live missions per crew
- Cross-mission memory (fact carryover)
- Remote runners / SSH
- Secure sandboxing beyond the child's own permissions
- MCP-based event emission
- Auto-restart on crash
- Event log rotation (solved implicitly by per-mission files)
- LLM-based orchestrator rules
- Typed event schemas (JSON Schema per type)
- Multi-user / multi-machine event bus

## 9. Key architectural bets

1. **Mission is the runtime unit.** Crew is config; mission is a run. Scopes event log, HITL queue, fact whiteboard, orchestrator state.
2. **PTY, not pipes.** Required for TUI fidelity.
3. **NDJSON file per mission, not broker.** Debuggability and crash-durability.
4. **CLI wrapper, not MCP.** Works with every agent today.
5. **Orchestrator is the only router.** Runners stay decoupled.
6. **Facts via event log, not separate store.** Single source of truth.
7. **Prompt composition at spawn time** (brief + roster + coordination notes). Replaces complicated runtime handshakes.
8. **xterm.js for rendering.** Don't reinvent the terminal emulator.
9. **ULID for event IDs.** Sortable + monotonic within ms.

## 10. Open questions

1. **CLI installation** — bundled with the `.app`, copied to `$APPDATA/runners/bin/` on first run. Ok?
2. **Fact read fast path** — (A) CLI re-scans log vs (B) orchestrator HTTP endpoint. v0: (A).
3. **Resize debounce interval** — 100ms is a guess. Tune with a real TUI.
4. **Event type allowlist source** — per-crew only (current plan), or global defaults + per-crew additions? Leaning latter.
5. **`from` field in CLI** — locked to `RUNNERS_RUNNER_NAME` (v0), or overridable via `--from`? v0: locked.
6. **Mid-mission fact injection** — should the orchestrator ever push "fact X changed" notifications into runners' stdin? v0: no, pull-only. Revisit if it proves awkward.

## 11. What would make this architecture fail

- A runtime that doesn't support injecting a system prompt at spawn (we'd have to type it into stdin after spawn, ugly but doable).
- An agent that won't learn to call CLI tools (hasn't happened with any modern coding agent, theoretical risk).
- NDJSON append atomicity breaking on an exotic filesystem (NFS, iCloud-synced volume). v0: document that app data must be on a local POSIX filesystem.
