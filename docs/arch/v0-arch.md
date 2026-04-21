# Runners — v0 Architecture

> Companion to `v0-prd.md`. The PRD defines *what* v0 ships; this doc defines *how* it works.

## 1. Overview

Runners is a local desktop app. A user configures a **crew** of CLI coding agents, launches a **mission** to activate it, and watches the crew coordinate in real time. The app is a Tauri 2 binary: Rust backend, React webview, SQLite for config, and a per-mission NDJSON file for live state.

### 1.1 Runtime picture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Tauri process (runners desktop app)                                         │
│                                                                             │
│  ┌──────────────────────┐   ┌──────────────────────┐   ┌─────────────────┐  │
│  │ MissionManager       │   │ SessionManager       │   │ EventBus        │  │
│  │  - mission lifecycle │   │  - PTY spawn/kill    │   │  - tail NDJSON  │  │
│  │  - compose prompts   │   │  - reader threads    │   │  - notify watch │  │
│  │  - roster + brief    │   │  - scrollback rings  │   │  - projections  │  │
│  └────────┬─────────────┘   └────────┬─────────────┘   └────────┬────────┘  │
│           │                          │                          │           │
│           │                          │                          ▼           │
│           │                          │               ┌──────────────────┐   │
│           │                          │               │ Orchestrator     │   │
│           │                          │               │  - policy rules  │   │
│           │                          │               │  - action dispch │   │
│           │                          │               └────────┬─────────┘   │
│           │                          │                        │             │
│           │      inject_stdin / ask_human / pause / ...       │             │
│           └─────────────────────────►│◄───────────────────────┘             │
│                                      ▼                                      │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │ Runner session (one per runner × mission)                            │   │
│  │   ┌──────────┐   PTY   ┌─────────────────────────────────────────┐   │   │
│  │   │  master  │ ◄────►  │  child: claude-code / codex / shell     │   │   │
│  │   └──────────┘         │  env: RUNNERS_CREW_ID,                  │   │   │
│  │                        │       RUNNERS_MISSION_ID,               │   │   │
│  │                        │       RUNNERS_RUNNER_NAME,              │   │   │
│  │                        │       RUNNERS_EVENT_LOG, PATH=…         │   │   │
│  │                        └─────┬───────────────────────────────────┘   │   │
│  └─────────────────────────────┼──────────────────────────────────────┘   │
│                                │ runs `runners signal` / `runners msg`    │
│                                ▼                                          │
│                  ┌─────────────────────────────┐                          │
│                  │  events.ndjson (per mission)│                          │
│                  └──────────────┬──────────────┘                          │
│                                 │ notify → EventBus → Orchestrator + UI   │
└─────────────────────────────────┼─────────────────────────────────────────┘
                                  ▼
                         ┌───────────────────────┐
                         │ React + xterm.js      │
                         │  terminals, messages, │
                         │  signals, HITL        │
                         └───────────────────────┘
```

### 1.2 The one-paragraph story

The user defines a **crew** (configuration: runners + policy). They click **Start Mission**, which creates a **mission** (runtime container), spawns one PTY-backed **session** per runner, and composes each runner's system prompt with the mission brief, the crew roster, and coordination instructions. Runners run real CLI binaries inside PTYs. They coordinate through two primitives — **signals** (typed events for the orchestrator to route on) and **messages** (flat prose stream for runner-to-runner conversation) — both carried through a bundled `runners` CLI that appends to the mission's NDJSON file. The **orchestrator** tails that file, applies a rule-based policy, and dispatches actions (inject stdin, ask human, pause, etc.). The UI is a read-only tail that renders terminals, messages, signals, and HITL prompts.

## 2. Concepts

Domain objects split cleanly into two layers:

- **Configuration** — persistent, user-edited. Outlives missions. Crew, Runner, Orchestrator Policy.
- **Runtime** — created at mission start, torn down at mission end. Everything here is scoped to a mission: the Mission itself, its Sessions, its coordination primitives (Signals, Messages), and the orchestrator's in-memory state.

The key insight: **Runner is config; Session is its runtime instance** — the same pattern as Crew (config) → Mission (runtime). A runner never runs on its own. A runner runs *inside a mission* as a session. The session is born when the mission starts, lives while the mission runs, and dies when the mission ends.

### 2.1 Relationship diagram

```
┌─ Configuration (persistent) ─────────┐    ┌─ Runtime (mission-scoped) ──────────────┐
│                                      │    │                                         │
│   Crew ─┬── Runner ──────────────────┼────┼──► Session ─► PTY process               │
│         │      (describes a role,    │    │     (one instance per runner per         │
│         │       binary, brief)       │    │      mission; lives & dies with the      │
│         │                            │    │      mission)                            │
│         │                            │    │                                         │
│         └── Orchestrator Policy      │    │     ▲                                    │
│               │                      │    │     │  spawned & owned by                │
│               └── attached to ───────┼────┼──► Mission ─── events.ndjson             │
│                                      │    │     │              │                    │
│                                      │    │     │              ├─► Signal   [v0]    │
│                                      │    │     │              ├─► Message  [v0]    │
│                                      │    │     │              ├─► Thread   [v0.x]  │
│                                      │    │     │              └─► Fact     [v0.x]  │
│                                      │    │     │                                   │
│                                      │    │     ├─► Orchestrator in-memory state    │
│                                      │    │     │    (pending asks, correlations)   │
│                                      │    │     │                                   │
│                                      │    │     └─► Shared context:                 │
│                                      │    │           brief + roster (v0)           │
│                                      │    │           + facts (v0.x)                │
└──────────────────────────────────────┘    └─────────────────────────────────────────┘
```

A mission is a container. Everything in the runtime column is either the container itself (Mission) or an object whose lifecycle is scoped by it. **Sessions are first-class members of this container** alongside the coordination bus and the orchestrator state — not a side effect of spawning runners.

### 2.2 Crew — *a configured team*

The persistent "who's on the team and how they work together" record. A crew has a name, a default mission goal, a list of runners, an orchestrator policy, and a signal-type allowlist. It does not run. It is blueprint.

Lifecycle: created by the user, edited freely, deleted when no longer needed. Persisted in SQLite.

### 2.3 Runner — *one configured agent*

An individual CLI agent within a crew: what binary to run, with what args, in what working directory, with what system prompt (the role's brief). Persistent config. A runner doesn't run either; it describes a process that will be spawned when a mission starts.

A runner belongs to exactly one crew. Examples: "Coder (claude-code)", "Reviewer (claude-code)", "Tester (shell)".

### 2.4 Orchestrator Policy — *the crew's decision rules*

A JSON list of `{when, do}` rules attached to the crew. This is where all routing and human-in-the-loop behavior is expressed. Shared across every mission the crew runs (in-memory state like pending asks is per-mission, but the rule set is per-crew).

There is no code here — just a lookup table. No scripting, no LLM. v0 is deliberately dumb.

### 2.5 Mission — *one activation of the crew, and the runtime container*

A mission is the only runtime container in the system. Everything alive at runtime lives *inside* a mission and dies with it:

- A **Session** per runner (the PTY processes — see §2.6).
- The **coordination bus** — the NDJSON event log carrying signals and messages.
- The **orchestrator's in-memory state** — pending HITL asks, correlation tracking, (later) fact projection.
- The **shared context** injected into each runner's composed prompt — the mission brief and the roster.

Lifecycle:
- **Start**: user clicks Start Mission on a crew. A mission row is created, one session is spawned per runner in the crew, the orchestrator boots with fresh state, and an NDJSON file is opened.
- **End**: explicit stop, or all sessions exited. Every session is killed, the orchestrator stops, the mission row is closed out.

This framing matters: when we say "the coordination bus is mission-scoped" or "the fact whiteboard is mission-scoped," we're saying the same thing as "sessions are mission-scoped." They all share one lifecycle because they all belong to the same container.

v0 constraint: a crew can have at most one live mission at a time. A crew can have many historical missions.

### 2.6 Session — *one runner's PTY process, running inside a mission*

The runtime instance of a Runner. A Session is to a Runner what a Mission is to a Crew: the *run* of a *configuration*.

A session exists if and only if a mission exists. One runner × one mission = one session. When the mission starts, each runner in the crew gets a session spawned for it. When the mission ends, every session in that mission is killed. A session cannot outlive its mission; a session cannot exist without one.

A session owns:
- A PTY master handle (the only object in the system with a file descriptor to a running child process).
- A blocking reader thread that drains the PTY and pushes to the scrollback ring.
- A writer for stdin injection (used by the human and by the orchestrator's `inject_stdin` action).
- A ring buffer (~10k lines) for scrollback that survives frontend tab-switches and app restarts within the mission.
- An exit status once the child has terminated.

A session is the only object in the system that actually *executes* code — everything else is metadata, a coordination channel, or a projection over the event log.

### 2.7 Coordination primitives — *what flows between runners*

Runners don't share a programming model; they share an IM-like surface. The same way Slack/Lark/Teams gave humans a small vocabulary of coordination (messages, threads, pings, pinned canvas), Runners gives agents a parallel vocabulary. We ship a subset in each milestone.

| Primitive | Role | v0 | v0.x | v1+ |
|---|---|:---:|:---:|:---:|
| **Signal** | Typed notification; orchestrator routes on these. Verb grammar. | ✅ | | |
| **Message** | Prose, broadcast or directed to a specific runner. | ✅ | | |
| **Inbox** | Per-runner projection: broadcasts + messages addressed to me. | ✅ | | |
| **Thread** | Scoped sub-conversation within a mission. | | ✅ | |
| **Fact** | KV whiteboard; "what is currently true in this mission." | | ✅ | |
| **Mention** | Targeted `@name` inside a message's prose (lighter-weight than `--to`). | | | ✅ |
| **Reaction** | Lightweight signal attached to a message (`👍`, `🔍`, `blocking`). | | | ✅ |

#### 2.7.1 Signal — *"something happened, please decide"*

Short, typed, orchestrator-routable. Grammar: past-tense verb.

Examples: `review_requested`, `changes_requested`, `approved`, `blocked`.

Signals are machine-readable by design. The orchestrator has rules keyed to signal types. Runners emit them when they want the mission to move to its next state.

A signal carries an optional `payload` (JSON) but the payload is meant for the orchestrator's decision logic, not for runners to read as prose.

#### 2.7.2 Message — *"here's what I think"*

Prose, addressed either to the mission (broadcast) or to a specific crewmate (direct). Runner-to-runner (and human-readable). Grammar: sentence.

Two shapes:
- **Broadcast** — `runners msg post "<text>"`. Goes to everyone's inbox. Use for status updates, open questions, mission-wide announcements.
- **Direct** — `runners msg post --to <runner> "<text>"`. Goes to that runner's inbox only. Use for targeted questions, replies, or private back-and-forth.

Examples:
- broadcast: `"Branch feat/x is ready. Touched auth.rs and session.rs."`
- direct: `runners msg post --to reviewer "Line 47 in auth.rs: null check missing when the token is expired."`
- direct reply: `runners msg post --to coder "Kept the 30s timeout — provider is slow on cold start."`

Messages are **flat in v0** — one stream per mission, no thread scoping. Each runner consumes messages through their **inbox** (§2.7.5): broadcasts plus directly-addressed messages.

Messages and signals are separate for good reasons:
- Signals are typed and small; orchestrator logic keys off them. Messages are prose; orchestrator doesn't parse them.
- A signal without prose works ("approved"). Prose without a signal works too ("I noticed X"). Conflating them forces every signal to carry prose and every note to carry a type.
- Runners (LLM agents) already know how to use both: signals are like exit codes, messages are like comments. The CLI keeps them linguistically separate.
- Direct messages enable real conversation between runners without forcing every interaction through the orchestrator policy.

#### 2.7.3 Inbox — *"what's in my mailbox"*

Every runner has an **inbox**: the subset of the mission's messages that are relevant to it. The inbox is a **projection** over the event log, not a separate data structure. For runner `X`:

```
inbox(X) = all message events in the mission where to = null OR to = X
```

`runners msg read` returns the calling runner's inbox, sorted by ULID (chronological).

This design keeps the storage model simple (one event log per mission, same as before) while giving each runner a clean "what's for me" view. Broadcasts end up in everyone's inbox; direct messages end up in exactly one.

**Why LLM agents won't see direct messages unless they're told:** agents act on their prompt. They don't spontaneously poll the filesystem. A message landing in the inbox doesn't itself make the agent aware of it. We solve this with an orchestrator action: when a directed message arrives, the orchestrator nudges the recipient's stdin with a one-line hint ("new message from `coder` — run `msg read`"). The agent then reads its inbox as a normal tool call. See §5.5 action `nudge_recipient`.

The inbox is not a queue in the delete-on-read sense — messages stay in the log forever (well, for the mission). The "read" in `msg read` is lookup, not consumption. `msg read --since <ts>` lets agents fetch just what's new.

#### 2.7.4 Thread *(v0.x)* — *scoped conversation*

When a mission has 3+ runners or runs for long enough to develop sub-topics, the flat message stream gets noisy. Threads add a scoping layer: messages can be posted to a named thread; runners can `msg read <thread>` to get just that conversation.

Cut from v0 because the v0 demo is two runners on one loop — the whole mission *is* the thread.

#### 2.7.5 Fact *(v0.x)* — *queryable state*

A KV whiteboard. Any runner can `ctx set key value` and `ctx get key`. Mission-scoped; each mission starts with an empty whiteboard. Backed by the event log as a `fact_recorded` event type, projected in-memory by the orchestrator for O(1) reads.

Facts differ from messages and signals: they're **current state**, not events. Reading a fact answers "what is true right now?" not "what happened?" Cut from v0 because the demo doesn't need a dashboard-style current-state view.

### 2.8 Events — *the unifying transport*

Every coordination primitive is persisted as an **event** — one line in the per-mission NDJSON file. Signals become `signal_emitted` events. Messages become `message_posted` events. (Later: `thread_opened`, `fact_recorded`, etc.) This is a transport detail, not a separate concept for users; runners interact through the CLI verbs, not the event schema.

An event has: `{id, ts, crew_id, mission_id, from, to, kind, payload, correlation_id, causation_id}`. `kind` distinguishes primitive types (`signal`, `message`, ...). The orchestrator and UI project events into the primitive-specific views.

## 3. Mission lifecycle

### 3.1 Start

```
user clicks Start Mission on a crew
  └─► MissionManager.start(crew_id):
        ├─ insert `missions` row (status=running, mission_id = ULID)
        ├─ mkdir $APPDATA/runners/crews/{crew_id}/missions/{mission_id}/
        ├─ touch events.ndjson
        ├─ for each runner in crew:
        │     composed_prompt = compose(runner.system_prompt,
        │                                mission.brief,
        │                                roster(crew),
        │                                coordination_notes(crew.signal_types))
        │     SessionManager.spawn(mission_id, runner, composed_prompt)
        ├─ Orchestrator.start(mission_id)  ← fresh in-memory state
        │     open events.ndjson, read history (empty), tail via notify
        └─ emit Tauri event: mission:{id}:started
```

### 3.2 End

```
user clicks End Mission  (or all sessions have exited)
  └─► MissionManager.end(mission_id, status):
        ├─ SessionManager.kill_all_in_mission(mission_id)
        ├─ Orchestrator.stop(mission_id)
        ├─ update `missions` row: status (completed/aborted), stopped_at
        └─ emit Tauri event: mission:{id}:ended
```

### 3.3 v0 constraint

One live mission per crew. Starting a new one while one is live is blocked in the UI. (v1: relax to concurrent missions.)

## 4. PTY runner sessions

### 4.1 Why PTY

Claude Code and Codex are TUIs. They check `isatty()`; if false, they degrade (no colors, no spinner, sometimes outright refuse). Their output is a stream of escape sequences (`\x1b[2K`, alt-screen toggles) that only a terminal emulator can render.

A pseudo-terminal gives the child a real terminal on stdin/stdout/stderr (full TUI mode) and hands us the master end as a byte stream that we forward to **xterm.js** in the webview.

Anything less (plain pipes, stdout-only capture) will look broken.

### 4.2 Spawn

```
portable_pty::openpty(rows, cols)
  ├─ master handle  → kept by SessionManager
  └─ slave handle   → given to child via spawn_command()

Child inherits:
  PATH                = $APPDATA/runners/bin:<original PATH>
  RUNNERS_CREW_ID     = <ulid>
  RUNNERS_MISSION_ID  = <ulid>
  RUNNERS_RUNNER_NAME = coder
  RUNNERS_EVENT_LOG   = $APPDATA/runners/crews/<crew>/missions/<mission>/events.ndjson

Reader thread (blocking):
  loop { read(master) → emit session:{id}:out event, push to scrollback ring }
  on EOF: wait(child) → emit session:{id}:exit { code } → update sessions row
```

System prompt is passed to the runtime via its native flag (`--append-system-prompt` for claude-code; equivalent for each runtime). The runtime enum in the `runners` table owns the flag mapping.

### 4.3 The composed system prompt

MissionManager builds each runner's prompt from four parts:

1. **The user-authored brief** (`runners.system_prompt`).
2. **The mission brief** (`missions.goal_override` or `crews.goal`).
3. **The roster** — crewmates' names, roles, one-line brief summaries.
4. **Coordination notes** — how to use `runners signal` and `runners msg`, and the crew's allowed signal types.

Example for a Reviewer:

```
You are Reviewer, a runner in crew "Feature Ship".
Your role: code review.

== Your brief ==
When the Coder requests review, read their messages and the diff,
then either approve or request changes with specific feedback.

== Mission ==
Goal: Implement feature X with tests and a clean PR.

== Your crewmates ==
- Coder (implementation): Writes code. Will signal review_requested and
  post messages explaining what changed.

== Coordination ==
- Signal milestones with `runners signal <type>`.
  Signal types: review_requested, changes_requested, approved, blocked.
- Post prose with `runners msg post "<text>"`.
- Read the mission's message stream with `runners msg read`.
```

### 4.4 Frontend wiring and human takeover

- On first view: fetch the session's scrollback ring; write to xterm.js to restore history.
- Subscribe to `session:{id}:out` for live output.
- xterm.js `onData` → `send_input(session_id, bytes)` → `master.writer.write_all(bytes)`.
- Frontend window resize → debounced (~100ms) `master.resize(rows, cols)` → SIGWINCH to child. Non-optional; without it, TUIs mis-render.

**Human takeover is a first-class capability.** At any moment, the human can type directly into any runner's stdin — the same writer the orchestrator uses for `inject_stdin`. This is deliberate: the human can step in to answer a prompt the agent is stuck on, correct a bad plan, kill a runaway tool call, or just chat with the agent mid-flight.

The UI surface for this is the xterm pane itself — it's a real terminal, not a log viewer. Typing sends keystrokes through untouched, including special keys (arrows, Enter, Ctrl-C). The agent on the other end can't tell whether the bytes came from the orchestrator, the human, or a replay — which is the whole point.

### 4.5 Sessions outlive the UI

Sessions live in the Rust backend and belong to the mission, not to any webview or tab. Closing the mission control window does *not* kill the sessions — the agents keep running, events keep flowing into the NDJSON file, the orchestrator keeps applying rules. Re-opening the window re-attaches: the frontend fetches each session's scrollback ring to rebuild xterm state, then subscribes to live output from wherever it was.

The only things that end a session in v0 are: user clicks End Mission, the child process exits, or the app itself quits. A closed webview window is none of those.

**Why this matters for human takeover:** if the only way to type into a runner required the UI to be visible, then minimizing or closing the mission view to focus on something else would silently cut the human out of the loop. That's wrong — the human should be able to close the monitor and still inject stdin (or let the orchestrator do it) without anything changing about how agents run.

### 4.6 Writer serialization

The PTY master writer is shared between the human (via `send_input` command) and the orchestrator (via `inject_stdin` action). Concurrent writers could interleave bytes mid-line, which would confuse the TUI on the other end.

Solution: wrap each session's writer in a `tokio::sync::Mutex`. Every write is one `write_all` call under the lock. Small writes (keystrokes, short prompts) are fast enough that contention is invisible.

### 4.7 Threads, not async

`portable-pty`'s reader is blocking. Spawn an OS thread per session. Writers stay on the Tauri async runtime (writes are short).

### 4.8 Scrollback in Rust

`VecDeque<String>` ring (~10k lines) per session in SessionManager, so scrollback survives tab-switches and app restarts. Overflow lines append to `missions/{mission_id}/sessions/{session_id}.log`. The ring sees raw bytes including alt-screen toggles — acceptable v0 scuff.

### 4.9 Death and kill

Reader thread owns the child handle. On EOF, it calls `wait()`, emits `session:{id}:exit`, updates the sessions row. No auto-restart in v0.

Kill: drop master → SIGHUP via `portable-pty`; escalate to SIGKILL if child lingers. v0 targets macOS; Linux best-effort; Windows deferred.

## 5. Coordination bus

### 5.1 Transport

```
$APPDATA/runners/crews/{crew_id}/missions/{mission_id}/events.ndjson
```

One line per event. Append-only. **Each mission has its own file** — scopes log rotation, crash-replay, and deletion.

Why a file instead of an in-memory bus:
- **Debuggable** — `tail -f events.ndjson | jq .`.
- **Crash-durable** — whatever's on disk survived the crash.
- **Atomic** — writes < `PIPE_BUF` (4KB on macOS) to `O_APPEND` are atomic; concurrent `runners` invocations interleave correctly.
- **Replayable for free** — restart the orchestrator, re-scan, resume.

### 5.2 Event schema

```jsonc
{
  "id": "01HG3K1YRG7RQ3N9...",     // ULID: time-sortable, monotonic within ms
  "ts": "2026-04-21T12:34:56.123Z",
  "crew_id": "01HG...",
  "mission_id": "01HG...",
  "kind": "signal",                 // signal | message  (v0.x adds: fact, thread_opened, ...)
  "from": "coder",                  // runner name | "human" | "orchestrator"
  "to": null,                       // null = broadcast; runner name = directed (messages);
                                    //   for signals, always null in v0 (policy decides routing)
  "type": "review_requested",       // for kind=signal; omitted for kind=message
  "payload": { "...": "..." },      // kind-specific
  "correlation_id": null,
  "causation_id": null
}
```

For a signal event: `kind=signal`, `type` is set, payload optional.
For a message event: `kind=message`, `payload.text` is the prose.

- **ULID `id`** — sortable, embeds a ms timestamp.
- **`correlation_id`** — groups events in one conversation (set by first emitter, propagated by orchestrator).
- **`causation_id`** — which event caused this one. Together with correlation, forms the event DAG.

### 5.3 How runners emit signals and messages

Three layers — answers "how does the agent know to append to the event log?"

#### Layer 1 — system prompt tells the convention

The composed prompt (§4.3) includes a Coordination section describing the `runners` CLI and listing allowed signal types. LLM agents already know how to read CLI docs and invoke tools — same capability they use for `git`, `gh`, `npm`.

#### Layer 2 — the CLI exists on PATH with context in env

The backend prepends `$APPDATA/runners/bin/` to PATH and drops the `runners` binary there at first run. At session spawn, env vars point at the mission's log and identify the runner.

On invocation, the CLI:
1. Reads env vars; errors if missing.
2. Builds an event (ULID, timestamps, `from` = `$RUNNERS_RUNNER_NAME`, `crew_id`, `mission_id`, `kind`).
3. For signals: validates `type` against the allowlist sidecar at `$APPDATA/runners/crews/{crew_id}/signal_types.json`.
4. Appends one JSON line to `$RUNNERS_EVENT_LOG` via `open(O_APPEND | O_WRONLY)` + `write_all` + close.
5. Exits 0.

Each invocation writes ≤ one 4KB line in one `write` syscall; concurrent emitters interleave safely.

#### Layer 3 — role briefs reinforce usage

User-authored briefs include examples at the moments where signaling or messaging matters. We ship sensible defaults per-runtime.

#### Why robust

- No in-band protocol in the PTY stream. Not parsing stdout for magic markers.
- Works for any CLI agent (MCP or not). Only requirement: can run shell commands.
- Fails visibly. If the agent forgets to signal, the orchestrator sees nothing and the user sees an idle runner.
- Fully observable. `$ runners signal review_requested` shows up literally in the terminal pane; the resulting event shows up in the timeline and messages panel.

#### Failure mode: hallucinated signal types

CLI validates against the allowlist; unknown types exit non-zero with a clear stderr message. Agent reads the error from shell history and self-corrects.

Messages have no allowlist — they're prose.

### 5.4 Consumers

Two subscribers to the NDJSON file, both via `notify`:

- **Orchestrator** — deserializes each new line. For signals, runs the policy and dispatches actions. For messages, no-op by default (v0.x: threads, routing-by-mention).
- **UI** — the backend re-emits each line as a `mission:{id}:event` Tauri event. Frontend splits by `kind` into the messages panel and the signal/timeline panel.

#### Startup replay

On orchestrator boot: open the mission's file, fold events into in-memory state (pending asks, correlation tracking), then switch to tailing. The file *is* the state.

### 5.5 Orchestrator actions

| Action | Effect | Emits event? |
|---|---|---|
| `inject_stdin` | write template + `\r` to target runner's PTY writer | `stdin_injected` (signal) |
| `nudge_recipient` | write a short "new message from X, run `msg read`" hint into the recipient's stdin | `recipient_nudged` (signal) |
| `ask_human` | add card to HITL panel; wait for click | `human_question` then `human_response` (signals) |
| `notify_human` | fire a toast | `human_notified` (signal) |
| `pause_runner` | SIGSTOP to target PTY | `runner_paused` (signal) |
| `resume_runner` | SIGCONT to target PTY | `runner_resumed` (signal) |

Emitted events have `causation_id` = the triggering event's `id`.

**Default policy for direct messages.** Every crew starts with a built-in rule `{ when: { kind: "message", to: "*" }, do: { action: "nudge_recipient" } }` that fires on any directed message. This ensures recipients know mail has arrived without the sender having to also emit a signal. Users can disable this per crew if they want silent inboxes.

**Crash correctness:** emit the event *before* performing the action. Worst case on crash+replay is a duplicate action, recoverable (stdin seen twice; HITL cards deduped by event id). Silent loss is not.

### 5.6 Who does delivery

Two different delivery models, by primitive kind:

- **Signals are orchestrator-routed.** Runners never address other runners with a signal. A signal is emitted into the bus; the orchestrator policy decides what happens (including whether to inject stdin into some specific runner). This keeps all control-flow routing in one place and lets you swap runners without rewriting emitters.
- **Messages support both broadcast and direct addressing.** A runner can `msg post` (everyone's inbox) or `msg post --to <runner>` (that runner's inbox only). No orchestrator in the delivery path; messages are data, not control. The orchestrator is involved only to nudge recipients so they know mail arrived (§5.5, `nudge_recipient`).

The split:

| | Sender addresses recipient? | Orchestrator involved? |
|---|:---:|:---:|
| Signal | No — policy decides | Always |
| Broadcast message | No | Only to nudge |
| Direct message | Yes (`--to`) | Only to nudge |

- **Decoupled control flow** — the Coder doesn't need to know the Reviewer's name to *signal* a review. Swap the Reviewer without rewriting the Coder's signal emissions.
- **Coupled content flow where it's natural** — if Coder wants to ask Reviewer a specific question, it can just `msg post --to reviewer ...`. The roster injection (§4.3) already tells each runner the current names of its crewmates, so direct addressing works without extra config.
- **Single policy location** for control — every "when signal X, do Y" lives on the crew row.
- **Orchestrator is the only side-effecting component** outside runner processes. Direct messaging doesn't violate this — a direct `msg post` writes an event; the actual stdin hint is still an orchestrator action.

### 5.7 Known failure modes

| Failure | Mitigation |
|---|---|
| Orchestrator crashes mid-action | Emit event before action; replay on boot; accept duplicates |
| Two runners ask human at once | HITL queues both; user answers in order |
| Event storm | Surface events/sec warning; no rate limit in v0 |
| Malformed NDJSON line | Skip and warn; file stays valid |
| NDJSON grows large | End the mission; new mission = new file |
| Hallucinated signal type | Allowlist validation + clear stderr |
| Runner posts messages nobody reads | Surface "unread by crewmate X" indicator in UI (v0.x) |

## 6. Shared context (mission-scoped)

Two layers in v0.

### 6.1 Mission brief (read-only, prompt-injected)

`missions.goal_override` or falls back to `crews.goal`. Injected into the composed prompt at spawn (§4.3). Never changes during a mission.

### 6.2 Roster (read-only, prompt-injected)

Rendered from `crew.runners` at mission start into each runner's prompt as `== Your crewmates ==` with name, role, and one-line brief summary. Never changes during a mission.

This is how the Reviewer knows there's a Coder.

### 6.3 The `runners` CLI surface in v0

```
runners signal <type> [--payload <json>] [--correlation-id <id>] [--causation-id <id>]
runners msg    post <text> [--to <runner>] [--correlation-id <id>] [--causation-id <id>]
runners msg    read [--since <ts>] [--from <runner>]
runners help
```

One binary. Two verbs. Context always from env.

- `msg post` with no `--to` → broadcast.
- `msg post --to <runner>` → directed; lands in that runner's inbox only.
- `msg read` → the calling runner's inbox (broadcasts + directs addressed to me), sorted by ULID.
- `msg read --from <runner>` → filter to messages authored by a specific sender.
- `msg read --since <ts>` → only messages newer than `ts` (for polling without re-reading history).

## 7. Data model

### 7.1 SQLite (config + session lifecycle)

```sql
crews (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  goal TEXT,
  orchestrator_policy TEXT,           -- JSON: [{ when, do }]
  signal_types TEXT,                  -- JSON array: allowlist
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

### 7.2 Filesystem

```
$APPDATA/runners/
├── bin/
│   └── runners                              # the CLI (signal + msg)
├── runners.db                               # SQLite
└── crews/
    └── {crew_id}/
        ├── signal_types.json                # CLI allowlist sidecar
        └── missions/
            └── {mission_id}/
                ├── events.ndjson            # per-mission event log
                └── sessions/
                    └── {session_id}.log     # scrollback overflow
```

macOS: `$APPDATA` = `~/Library/Application Support/com.wycstudios.runners`. Dev builds use `-dev` suffix.

## 8. Process and thread model

```
Tauri main thread
  ├── Tauri async runtime (tokio)
  │     ├── MissionManager (async)
  │     ├── Orchestrator task per live mission (notify + dispatch)
  │     └── Tauri command handlers (CRUD)
  ├── Thread per active session (blocking PTY reader)
  └── Webview process (React + xterm.js)
```

For v0 scale (one live mission, ≤ ~10 sessions): fine.

## 9. Out of scope for v0

- Threads (v0.x)
- Facts / shared whiteboard (v0.x)
- Mentions, reactions (v1)
- Concurrent live missions per crew
- Cross-mission memory
- Remote runners / SSH
- Sandboxing beyond the child's own permissions
- MCP-based signal emission
- Auto-restart on crash
- Event log rotation (solved implicitly by per-mission files)
- LLM-based orchestrator rules
- Multi-user / multi-machine coordination bus

## 10. Next milestones (vision)

### v0.x — Threads and Facts

**Threads** — when missions grow past 2 runners or 1 hour, messages get noisy. Add:
- `runners thread open <name>` → returns thread_id
- `runners msg post --thread <id> <text>`
- `runners msg read --thread <id>`
- Orchestrator rules gain "open thread on signal X" action
- UI splits message stream by thread

**Facts** — the shared whiteboard. Add:
- `runners ctx set/get/unset/list`
- `fact_recorded` event type; last-writer-wins projection in orchestrator
- UI gains a facts panel
- Solves "current state of the mission" at a glance

Both live on the same event log as new `kind` values. No transport changes.

### v1 — Mentions, reactions, richer routing

- `@runner` mentions inside messages → orchestrator can route on them
- Reactions (`👍`, `blocking`) on messages — lightweight signals
- Cross-mission memory / "crew memory"
- Concurrent missions per crew

## 11. Architectural bets

1. **Mission is the runtime unit.** Crew is config; mission is a run.
2. **PTY, not pipes.** TUI fidelity is non-negotiable.
3. **NDJSON file per mission, not broker.** Debuggable and crash-durable.
4. **CLI wrapper, not MCP.** Works with every agent today.
5. **Signals and messages as distinct primitives.** Keeps orchestrator simple and prose natural.
6. **Orchestrator is the only router.** Runners stay decoupled.
7. **Prompt composition at spawn time.** Replaces runtime handshakes.
8. **Incremental vocabulary.** v0 = signals + messages; v0.x adds threads + facts; v1 adds mentions + reactions.
9. **xterm.js for rendering.** Don't reinvent the terminal emulator.
10. **ULID for event IDs.** Sortable, monotonic within ms.

## 12. Open questions

1. CLI installation: bundled with `.app`, copied to `$APPDATA/runners/bin/` on first run — ok?
2. Resize debounce: 100ms is a guess; tune with a real TUI.
3. Signal type allowlist: per-crew only (current), or global defaults + per-crew overrides?
4. `from` field: locked to env (v0), or `--from` override?
5. Does `runners msg read` return everything or paginate? v0: everything, sorted by ULID.
6. Does the orchestrator ever inject messages (not signals) into runners' stdin? v0: yes — when it routes a signal to a runner, it can include a summary of recent messages as context.

## 13. What would break this architecture

- A runtime with no way to inject a system prompt at spawn (we'd type into stdin post-spawn — ugly but doable).
- An agent that won't learn to call CLI tools.
- NDJSON append atomicity breaking on an exotic filesystem (NFS, iCloud-synced). v0: document that app data must be on a local POSIX filesystem.
