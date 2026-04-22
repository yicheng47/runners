// Event log primitives — append-only NDJSON, monotonic ULIDs, path helpers.
// Consumed by the Tauri binary (C5 mission lifecycle, C7 watcher) and by the
// standalone `runners` CLI (C9).

pub mod log;
pub mod path;
pub mod ulid;

pub use log::{EventLog, LogEntry};
pub use path::{crew_dir, events_path, mission_dir, signal_types_path, EVENTS_FILENAME};
pub use ulid::UlidGen;
