// Domain types. Hand-synced with src-tauri/src/model.rs — change one, change the other.
//
// C5.5: runners are top-level; crews compose runners via `crew_runners`.
// Event envelopes (arch §5.2) unchanged.

export type Timestamp = string; // RFC3339
export type Ulid = string;
export type SignalType = string;

export interface Crew {
  id: string;
  name: string;
  purpose: string | null;
  goal: string | null;
  orchestrator_policy: unknown | null;
  signal_types: SignalType[];
  created_at: Timestamp;
  updated_at: Timestamp;
}

// Global runner definition. Lead / position are per-crew and live on
// CrewRunner rows, not here.
export interface Runner {
  id: string;
  handle: string;
  display_name: string;
  role: string;
  runtime: string;
  command: string;
  args: string[];
  working_dir: string | null;
  system_prompt: string | null;
  env: Record<string, string>;
  created_at: Timestamp;
  updated_at: Timestamp;
}

// A runner's membership in a specific crew. `crew_list_runners` returns
// these — the runner's fields are flattened alongside `position`, `lead`,
// `added_at` per `#[serde(flatten)]` on the Rust struct.
export interface CrewRunner extends Runner {
  position: number;
  lead: boolean;
  added_at: Timestamp;
}

export interface RunnerActivity {
  runner_id: string;
  active_sessions: number;
  active_missions: number;
  crew_count: number;
  last_started_at: Timestamp | null;
  /// Most recent running direct-chat session id for this runner, or null
  /// if no live direct PTY exists. Lets the sidebar re-attach to a session
  /// across page reloads without an extra command call.
  direct_session_id: string | null;
}

// Runner row plus its RunnerActivity, returned by runner_list_with_activity.
// Both halves are #[serde(flatten)]-merged on the Rust side.
export interface RunnerWithActivity extends Runner, RunnerActivity {}

// Crew membership row from runner_crews_list — used by Runner Detail
// "Crews using this runner" panel.
export interface CrewMembership {
  crew_id: string;
  crew_name: string;
  lead: boolean;
  position: number;
  added_at: Timestamp;
}

// Payload for the Tauri `runner/activity` event the SessionManager emits
// on every spawn / reap.
export interface RunnerActivityEvent {
  runner_id: string;
  handle: string;
  active_sessions: number;
  active_missions: number;
  crew_count: number;
  direct_session_id: string | null;
}

// Returned by session_start_direct (and by mission_start's session list).
// mission_id is null for the direct flavor.
export interface SpawnedSession {
  id: string;
  mission_id: string | null;
  runner_id: string;
  handle: string;
  pid: number | null;
}

export type MissionStatus = "running" | "completed" | "aborted";

export interface Mission {
  id: string;
  crew_id: string;
  title: string;
  status: MissionStatus;
  goal_override: string | null;
  cwd: string | null;
  started_at: Timestamp;
  stopped_at: Timestamp | null;
}

export type SessionStatus = "running" | "stopped" | "crashed";

// mission_id is null for direct-chat sessions that the user started
// against a runner without firing a mission.
export interface Session {
  id: string;
  mission_id: string | null;
  runner_id: string;
  cwd: string | null;
  status: SessionStatus;
  pid: number | null;
  started_at: Timestamp | null;
  stopped_at: Timestamp | null;
}

export type EventKind = "signal" | "message";

export interface Event {
  id: Ulid;
  ts: Timestamp;
  crew_id: string;
  mission_id: string;
  kind: EventKind;
  from: string;
  to: string | null;
  /** Present only when `kind === "signal"`. */
  type?: SignalType;
  payload: unknown;
}

// --- Command inputs ------------------------------------------------------
// Hand-synced with src-tauri/src/commands/{crew,runner,crew_runner,mission}.rs.
// Fields typed `X | null` on a declared-optional key mirror Rust's
// `Option<Option<T>>` pattern: omit to keep the existing value, pass null
// to clear it.

export interface CrewListItem extends Crew {
  runner_count: number;
}

export interface CreateCrewInput {
  name: string;
  purpose?: string | null;
  goal?: string | null;
}

export interface UpdateCrewInput {
  name?: string;
  purpose?: string | null;
  goal?: string | null;
  orchestrator_policy?: unknown | null;
  signal_types?: SignalType[];
}

export interface CreateRunnerInput {
  handle: string;
  display_name: string;
  role: string;
  runtime: string;
  command: string;
  args?: string[];
  working_dir?: string | null;
  system_prompt?: string | null;
  env?: Record<string, string>;
}

// `handle` is intentionally excluded: it's the runner's identity in events
// and CLI addressing and must not be renamed after creation.
export interface UpdateRunnerInput {
  display_name?: string;
  role?: string;
  runtime?: string;
  command?: string;
  args?: string[];
  working_dir?: string | null;
  system_prompt?: string | null;
  env?: Record<string, string>;
}

export interface StartMissionInput {
  crew_id: string;
  title: string;
  goal_override?: string | null;
  cwd?: string | null;
}

export interface StartMissionOutput {
  mission: Mission;
  goal: string;
}
