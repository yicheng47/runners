// Thin invoke wrappers for Tauri commands.
//
// Tauri auto-converts top-level arg names between camelCase (JS) and
// snake_case (Rust), so `crewId` here maps to `crew_id` in the Rust
// handlers. Nested struct fields pass through unchanged — `input` objects
// match the Rust struct shapes in src-tauri/src/commands/{crew,runner,crew_runner,mission,session}.rs,
// mirrored by src/lib/types.ts.

import { invoke } from "@tauri-apps/api/core";

import type {
  CreateCrewInput,
  CreateRunnerInput,
  Crew,
  CrewListItem,
  CrewRunner,
  Mission,
  Runner,
  RunnerActivity,
  Session,
  StartMissionInput,
  StartMissionOutput,
  UpdateCrewInput,
  UpdateRunnerInput,
} from "./types";

/** Session row joined with the runner's handle for UI labels. */
export interface SessionRow extends Session {
  handle: string;
}

export const api = {
  crew: {
    list: () => invoke<CrewListItem[]>("crew_list"),
    get: (id: string) => invoke<Crew>("crew_get", { id }),
    create: (input: CreateCrewInput) => invoke<Crew>("crew_create", { input }),
    update: (id: string, input: UpdateCrewInput) =>
      invoke<Crew>("crew_update", { id, input }),
    delete: (id: string) => invoke<void>("crew_delete", { id }),

    // Crew membership (the slot operations).
    listRunners: (crewId: string) =>
      invoke<CrewRunner[]>("crew_list_runners", { crewId }),
    addRunner: (crewId: string, runnerId: string) =>
      invoke<CrewRunner>("crew_add_runner", { crewId, runnerId }),
    removeRunner: (crewId: string, runnerId: string) =>
      invoke<void>("crew_remove_runner", { crewId, runnerId }),
    setLead: (crewId: string, runnerId: string) =>
      invoke<CrewRunner>("crew_set_lead", { crewId, runnerId }),
    reorder: (crewId: string, orderedIds: string[]) =>
      invoke<CrewRunner[]>("crew_reorder", { crewId, orderedIds }),
  },
  runner: {
    list: () => invoke<Runner[]>("runner_list"),
    get: (id: string) => invoke<Runner>("runner_get", { id }),
    create: (input: CreateRunnerInput) =>
      invoke<Runner>("runner_create", { input }),
    update: (id: string, input: UpdateRunnerInput) =>
      invoke<Runner>("runner_update", { id, input }),
    delete: (id: string) => invoke<void>("runner_delete", { id }),
    activity: (id: string) => invoke<RunnerActivity>("runner_activity", { id }),
  },
  mission: {
    list: (crewId?: string) =>
      invoke<Mission[]>("mission_list", crewId ? { crewId } : {}),
    get: (id: string) => invoke<Mission>("mission_get", { id }),
    start: (input: StartMissionInput) =>
      invoke<StartMissionOutput>("mission_start", { input }),
    stop: (id: string) => invoke<Mission>("mission_stop", { id }),
  },
  session: {
    list: (missionId: string) =>
      invoke<SessionRow[]>("session_list", { missionId }),
    injectStdin: (sessionId: string, text: string) =>
      invoke<void>("session_inject_stdin", { sessionId, text }),
    kill: (sessionId: string) => invoke<void>("session_kill", { sessionId }),
  },
};
