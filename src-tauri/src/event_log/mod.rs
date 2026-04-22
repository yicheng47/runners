// C4 — Event log primitives.
//
// Append-only NDJSON log, ULID generator, and filesystem-layout helpers for the
// per-mission event bus. No watcher here — that's C7 (see `crate::event_bus`).

#![allow(dead_code, unused_imports)] // Exports land in C4 for consumption across C5+.

pub mod log;
pub mod path;
pub mod ulid;

pub use log::{EventLog, LogEntry, EVENTS_FILENAME};
pub use path::{crew_dir, events_path, mission_dir, signal_types_path};
pub use ulid::UlidGen;
