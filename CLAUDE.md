# Runners — Claude Code Instructions

## What this is

An editor for teams of local coding agents (Claude Code, Codex, aider, ...). Users assemble a "crew" of agents, each with a role and a system prompt, and coordinate their work from one UI.

## Stack

- **Backend:** Rust, Tauri 2, SQLite (rusqlite), WAL mode
- **Frontend:** React 19, TypeScript, Tailwind CSS 4, Vite, React Router
- **Session runtime:** `portable-pty` for spawning local CLI agents
- **Event bus:** `notify` crate watching an append-only NDJSON log

## Project layout

```
src/                    # React frontend
  pages/                # Home, Teams, TeamEditor
  components/ui/        # Primitives
src-tauri/              # Rust backend
  src/commands/         # Tauri command modules
  src/session/          # PTY session runtime
  src/event_bus/        # Inter-agent NDJSON event bus
  src/orchestrator/     # Rule-based human-in-the-loop router
docs/
  features/             # Product-level feature specs
  impls/                # Implementation plans (with Figma prompts)
```

## Design principles

1. **Run the real CLI, don't reimplement it.** Sessions are PTY subprocesses of the actual agent binaries.
2. **Event bus = append-only NDJSON.** One line per event, structured, watched via `notify`. Still tailable with `tail -f`.
3. **Orchestrator is deterministic first.** Rule-based policy. Don't add LLM-in-the-loop until the event vocabulary is stable.

## Conventions

- Commits: one commit per feature branch (amend), unless told otherwise.
- Backend tests: unit-test new commands before wiring frontend.
- Cargo.lock: run `cargo check` after version bumps before committing.
