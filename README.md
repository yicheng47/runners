# Runner

Spawn a runner. Create your crew. Ship the feature.

Runner is a local desktop app for assembling a crew of CLI coding agents — Claude Code, Codex, and friends — giving each runner a role and a brief, and coordinating their work from one window.

> Status: pre-alpha. Crew + runner config, mission start/stop, and PTY-backed sessions all run end-to-end on macOS / Linux. The orchestrator, the `runner` CLI, and the mission workspace UI are still being built — see `docs/logs/` for the latest progress snapshot.

## What it does

- **Crews** — create a crew, pick which runners are on it.
- **Runners** — each runner is a local CLI runtime (claude, codex, ...) with its own role, system prompt, and working directory.
- **Event bus** — runners talk to each other through an append-only NDJSON log the orchestrator can read.
- **Orchestrator** — a rule-based policy that routes events between runners and decides when a human needs to be pulled in.

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
