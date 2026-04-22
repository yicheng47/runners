// SQLite row types for the app binary.
//
// The on-the-wire event envelope (Event, EventKind, SignalType, EventDraft) lives
// in `runners-core` so the standalone CLI can reuse it without pulling in
// rusqlite. Those are re-exported at the bottom of this file for backward-
// compatible imports across the app code.

#![allow(dead_code, unused_imports)] // Types land in C1 but get consumed by C2+.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Re-exports from the shared core so `crate::model::Event` keeps working.
pub use runners_core::model::{Event, EventDraft, EventKind, SignalType};
pub type Timestamp = DateTime<Utc>;
pub type Ulid = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Crew {
    pub id: String,
    pub name: String,
    pub purpose: Option<String>,
    pub goal: Option<String>,
    pub orchestrator_policy: Option<serde_json::Value>,
    pub signal_types: Vec<SignalType>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    pub id: String,
    pub crew_id: String,
    pub handle: String,
    pub display_name: String,
    pub role: String,
    pub runtime: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub system_prompt: Option<String>,
    pub env: HashMap<String, String>,
    pub lead: bool,
    pub position: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissionStatus {
    Running,
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: String,
    pub crew_id: String,
    pub title: String,
    pub status: MissionStatus,
    pub goal_override: Option<String>,
    pub cwd: Option<String>,
    pub started_at: Timestamp,
    pub stopped_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Running,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub mission_id: String,
    pub runner_id: String,
    pub status: SessionStatus,
    pub pid: Option<i64>,
    pub started_at: Option<Timestamp>,
    pub stopped_at: Option<Timestamp>,
}
