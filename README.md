# Runners

An editor for teams of local coding agents. Build your crew of Claude Code, Codex, and friends, give each one a role and a brief, and coordinate their work from one window.

> Status: scaffolding. Nothing works yet.

## What it does

- **Teams** — create a team, pick which agents are on it.
- **Agents** — each agent is a local CLI runtime (claude, codex, ...) with its own role, system prompt, and working directory.
- **Event bus** — agents talk to each other through an append-only NDJSON log the orchestrator can read.
- **Orchestrator** — a rule-based policy that routes events between agents and decides when a human needs to be pulled in.

## Stack

- Tauri 2 + Rust backend
- React 19 + TypeScript + Tailwind 4 frontend
- SQLite for persistence
- PTY-based subprocess control (portable-pty) for running real CLI agents

## Develop

```sh
npm install
npm run tauri dev
```

## License

MIT
