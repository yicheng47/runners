// `runner help` — long-form help, in the same shape as docs/arch
// §6.3. clap's `--help` covers the short auto-generated form; this is
// the verbose one with examples.

pub fn print() {
    println!(
        r#"runner — coordinate with the rest of the crew via the mission event log.

USAGE:
  runner signal <type> [--payload <json>]
  runner msg post <text> [--to <handle>]
  runner msg read [--since <ulid>] [--from <handle>]
  runner status busy|idle [--note <text>]
  runner help

ENVIRONMENT:
  RUNNER_CREW_ID, RUNNER_MISSION_ID, RUNNER_HANDLE, RUNNER_EVENT_LOG
  Set automatically by the parent app when this binary is spawned inside
  a mission session. Direct-chat sessions intentionally don't set them;
  every verb except `help` is a no-op in that context.

EXAMPLES:
  runner signal mission_goal --payload '{{"text":"ship v0"}}'
      Emit a typed signal that the parent-process router handles.

  runner msg post --to reviewer "ready for review on PR #42"
      Direct message; lands in @reviewer's inbox only.

  runner msg post "starting work on feature X"
      Broadcast; lands in every crewmate's inbox.

  runner msg read --since 01HG... --from coder
      Print messages addressed to you (broadcasts + directs) since the
      given ULID, optionally filtered by sender. Emits inbox_read on
      success so the parent's watermark advances.

  runner status idle --note "ready for next task"
      Sugar for `runner signal runner_status --payload {{state, note}}`.
      The router uses this to wake the lead when a worker becomes idle.

DOCS:
  Architecture: docs/arch/v0-arch.md (§5 coordination bus, §6.3 CLI)
  Implementation: docs/impls/v0-mvp.md (C9 — runner CLI binary)
"#
    );
}
