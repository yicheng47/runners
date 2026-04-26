# Runner — Claude Code Instructions

## What this is

A desktop editor for crews of local CLI coding agents (Claude Code, Codex, aider, ...). Users assemble a "crew" of runners, each with a role and a system prompt, and coordinate their work from one UI.

Vocabulary:
- **Crew** — a named group of runners working together on a goal.
- **Runner** — an individual CLI agent process (one PTY, one system prompt, one role).
- **Session** — a single run of a runner's process.
- **Event** — an NDJSON line runners emit to coordinate; routed by the orchestrator.

## Stack

- **Backend:** Rust, Tauri 2, SQLite (rusqlite), WAL mode
- **Frontend:** React 19, TypeScript, Tailwind CSS 4, Vite, React Router
- **Session runtime:** `portable-pty` for spawning each runner's CLI
- **Event bus:** `notify` crate watching an append-only NDJSON log

## Project layout

```
src/                    # React frontend
  pages/                # Home, Crews, CrewEditor
  components/ui/        # Primitives
src-tauri/              # Rust backend
  src/commands/         # Tauri command modules
  src/session/          # PTY session runtime
  src/event_bus/        # Inter-runner NDJSON event bus
  src/orchestrator/     # Rule-based human-in-the-loop router
design/                 # Pencil .pen source files
docs/
  arch/                 # Architecture docs and PRDs (v0-prd.md)
  features/             # Product-level feature specs
  impls/                # Implementation plans (with Figma prompts)
```

## Design principles

1. **Run the real CLI, don't reimplement it.** Runners are PTY subprocesses of the actual agent binaries.
2. **Event bus = append-only NDJSON.** One line per event, structured, watched via `notify`. Still tailable with `tail -f`.
3. **Orchestrator is deterministic first.** Rule-based policy. Don't add LLM-in-the-loop until the event vocabulary is stable.

## Conventions

- Commits: one commit per feature branch (amend), unless told otherwise.
- Backend tests: unit-test new commands before wiring frontend.
- Cargo.lock: run `cargo check` after version bumps before committing.
