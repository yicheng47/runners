// Domain types. Hand-synced with src-tauri/src/model.rs — change one, change the other.
//
// Mirrors the four SQLite row shapes (arch §7.1) plus the event envelope (arch §5.2).

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

export interface Runner {
  id: string;
  crew_id: string;
  handle: string;
  display_name: string;
  role: string;
  runtime: string;
  command: string;
  args: string[];
  working_dir: string | null;
  system_prompt: string | null;
  env: Record<string, string>;
  lead: boolean;
  position: number;
  created_at: Timestamp;
  updated_at: Timestamp;
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

export interface Session {
  id: string;
  mission_id: string;
  runner_id: string;
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

// --- C2 command inputs ---------------------------------------------------
// Hand-synced with src-tauri/src/commands/{crew,runner}.rs input structs.
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
  crew_id: string;
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
