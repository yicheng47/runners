// Tauri command handlers exposed to the frontend.
//
// Each submodule splits into pure-SQL functions (unit-testable against an
// in-memory pool) plus thin `#[tauri::command]` wrappers that pull a
// connection from the r2d2 pool and delegate. See docs/impls/v0-mvp.md §C2.

pub mod crew;
pub mod crew_runner;
pub mod mission;
pub mod runner;
pub mod session;
