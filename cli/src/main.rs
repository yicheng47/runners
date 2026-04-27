// `runner` — the bundled CLI that agents inside spawned PTYs invoke to
// append to the mission's NDJSON event log. See `docs/arch/v0-arch.md`
// §6.3 for the user-facing surface and `docs/impls/v0-mvp.md#C9` for the
// chunk's scope.
//
// Design notes:
// - Thin shell over `runner_core::event_log`. The CLI never reimplements
//   log semantics (flock, monotonic ULIDs, lossy reads); it owns only env
//   resolution, sidecar reads, and verb dispatch.
// - Direct-chat sessions deliberately set no `RUNNER_*` env vars
//   (`SessionManager::spawn_direct`). Every verb except `help` checks the
//   env up front: all-set → run; none-set → exit 0 with a stderr nudge
//   (so a curious agent calling `runner status idle` in a chat doesn't
//   crash); partially set → exit 2 with a precise pointer at which var
//   is missing.

mod allowlist;
mod env;
mod help;
mod msg;
mod roster;
mod signal;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "runner",
    about = "Coordinate with the rest of the crew via the mission event log.",
    version,
    // Our `Help` subcommand prints the long-form usage from arch §6.3.
    // clap also auto-generates a `help` subcommand by default; disable
    // it so the two don't collide.
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Emit a typed signal that the parent-process router handles.
    Signal {
        /// Signal type (must be in the crew's signal_types allowlist).
        r#type: String,
        /// Optional JSON payload object. Defaults to `{}`.
        #[arg(long)]
        payload: Option<String>,
    },
    /// Post or read prose messages.
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },
    /// Sugar over `signal runner_status` — report `busy` or `idle`.
    Status {
        state: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Print usage help.
    Help,
}

#[derive(Subcommand, Debug)]
enum MsgCmd {
    /// Post a message — broadcast unless `--to <handle>` is given.
    Post {
        text: String,
        #[arg(long)]
        to: Option<String>,
    },
    /// Print the caller's inbox; emits `signal inbox_read` on success.
    Read {
        /// Lower-bound by ULID; messages with id ≤ this are skipped.
        #[arg(long)]
        since: Option<String>,
        /// Filter by sender handle.
        #[arg(long)]
        from: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Help => {
            help::print();
            0
        }
        Cmd::Signal { r#type, payload } => signal::run(&r#type, payload.as_deref()),
        Cmd::Status { state, note } => signal::run_status(&state, note.as_deref()),
        Cmd::Msg {
            cmd: MsgCmd::Post { text, to },
        } => msg::post(&text, to.as_deref()),
        Cmd::Msg {
            cmd: MsgCmd::Read { since, from },
        } => msg::read(since.as_deref(), from.as_deref()),
    };
    std::process::exit(code);
}
