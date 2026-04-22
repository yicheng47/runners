// Thin invoke wrappers for the C2 Tauri commands.
//
// Tauri auto-converts top-level arg names between camelCase (JS) and
// snake_case (Rust), so `crewId` here maps to `crew_id` in the Rust
// handlers. Nested struct fields pass through unchanged — `input` objects
// match the Rust struct shapes in src-tauri/src/commands/{crew,runner}.rs,
// mirrored by src/lib/types.ts.

import { invoke } from "@tauri-apps/api/core";

import type {
  CreateCrewInput,
  CreateRunnerInput,
  Crew,
  CrewListItem,
  Runner,
  UpdateCrewInput,
  UpdateRunnerInput,
} from "./types";

export const api = {
  crew: {
    list: () => invoke<CrewListItem[]>("crew_list"),
    get: (id: string) => invoke<Crew>("crew_get", { id }),
    create: (input: CreateCrewInput) =>
      invoke<Crew>("crew_create", { input }),
    update: (id: string, input: UpdateCrewInput) =>
      invoke<Crew>("crew_update", { id, input }),
    delete: (id: string) => invoke<void>("crew_delete", { id }),
  },
  runner: {
    list: (crewId: string) =>
      invoke<Runner[]>("runner_list", { crewId }),
    get: (id: string) => invoke<Runner>("runner_get", { id }),
    create: (input: CreateRunnerInput) =>
      invoke<Runner>("runner_create", { input }),
    update: (id: string, input: UpdateRunnerInput) =>
      invoke<Runner>("runner_update", { id, input }),
    delete: (id: string) => invoke<void>("runner_delete", { id }),
    setLead: (id: string) => invoke<Runner>("runner_set_lead", { id }),
    reorder: (crewId: string, orderedIds: string[]) =>
      invoke<Runner[]>("runner_reorder", { crewId, orderedIds }),
  },
};
