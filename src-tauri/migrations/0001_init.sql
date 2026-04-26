-- Migration 0001: initial schema (shared-runner edition).
--
-- Superseded the per-crew runner model from the original v0 draft. See
-- docs/impls/v0-mvp-c5-5-shared-runners.md. In MVP (no prod data yet) we
-- rewrite DDL in place rather than layering a 0002 migration. Dev users
-- delete their local DB file ($APPDATA/runner/runner.db) once to pick
-- up the new shape.
--
-- Overview:
--   - crews        — named groups. Own the orchestrator_policy + signal
--                    allowlist. No direct runner column: composition lives
--                    in crew_runners.
--   - runners      — global, shareable. One handle = one runner everywhere
--                    it appears in the event log.
--   - crew_runners — join: crew <-> runner, with per-crew position + lead
--                    invariant enforced by a partial unique index.
--   - missions     — scoped to a crew. Spawns one session per crew_runner.
--   - sessions     — a PTY run of a runner. mission_id is nullable:
--                    "direct chat" sessions exist without a mission
--                    (cwd lives on the session row in that case).

CREATE TABLE crews (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    purpose TEXT,
    goal TEXT,
    orchestrator_policy TEXT,
    signal_types TEXT NOT NULL DEFAULT '["mission_goal","human_said","ask_lead","ask_human","human_question","human_response","inbox_read"]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE runners (
    id TEXT PRIMARY KEY,
    handle TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL,
    runtime TEXT NOT NULL,
    command TEXT NOT NULL,
    args_json TEXT,
    working_dir TEXT,
    system_prompt TEXT,
    env_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE crew_runners (
    crew_id TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
    runner_id TEXT NOT NULL REFERENCES runners(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    lead INTEGER NOT NULL DEFAULT 0,
    added_at TEXT NOT NULL,
    PRIMARY KEY (crew_id, runner_id),
    UNIQUE (crew_id, position)
);

CREATE UNIQUE INDEX one_lead_per_crew ON crew_runners(crew_id) WHERE lead = 1;

CREATE TABLE missions (
    id TEXT PRIMARY KEY,
    crew_id TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    goal_override TEXT,
    cwd TEXT,
    started_at TEXT NOT NULL,
    stopped_at TEXT
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    mission_id TEXT REFERENCES missions(id) ON DELETE SET NULL,
    runner_id TEXT NOT NULL REFERENCES runners(id) ON DELETE CASCADE,
    cwd TEXT,
    status TEXT NOT NULL,
    pid INTEGER,
    started_at TEXT,
    stopped_at TEXT
);
