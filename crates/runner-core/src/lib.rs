// Runner shared core — types and event-log primitives used by both the Tauri
// app binary and the `runner` CLI.

pub mod error;
pub mod event_log;
pub mod model;

pub use error::{Error, Result};
pub use event_log::{EventLog, EVENTS_FILENAME};
pub use model::{Event, EventDraft, EventKind, SignalType, Timestamp, Ulid};
