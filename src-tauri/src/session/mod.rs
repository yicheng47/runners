// Session runtime — spawns and controls each runner's local CLI process
// via a pseudo-terminal (portable-pty). See docs/impls/v0-mvp.md §C6.
//
// The `manager` submodule owns the per-process PTY machinery. The app wires
// it into AppState and calls into it from mission/session Tauri commands.

pub mod manager;

pub use manager::SessionManager;
