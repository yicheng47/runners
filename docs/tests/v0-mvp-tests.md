# v0 MVP — Tests

Single source of truth for MVP test coverage. Pairs with `docs/impls/v0-mvp.md` (chunks) and `docs/arch/v0-arch.md` (system contracts). Each section is scoped to the chunk where that tier of test first becomes runnable — earlier chunks can't exercise it yet, later chunks should keep it passing.

## Test tiers

| Tier | Tool | Run by | Becomes runnable |
|---|---|---|---|
| **Unit** | `cargo test --lib` | CI + dev | C1 |
| **Integration** | `cargo test --test <name>` (headless, no Tauri) | CI + dev | C6 |
| **Smoke (UI)** | `pnpm tauri dev` + manual checklist | Human | C3 |
| **Demo path** | full Definition-of-Done run from `docs/impls/v0-mvp.md` | Human | C11 (gates the `main` merge) |

- **Unit** — pure Rust, SQL constraints, serde roundtrips. No PTY, no Tauri, no filesystem beyond tempdirs.
- **Integration** — seams: event log + CLI, orchestrator + bus, PTY + sessions. Headless, uses `tempfile` for `$APPDATA` isolation.
- **Smoke (UI)** — a human clicking through the Tauri app after each UI-bearing chunk. Each UI chunk PR must reproduce the matching checklist in its PR description (per `docs/impls/v0-mvp.md` chunking principles); this file is the authoritative version.
- **Demo path** — the single end-to-end run from `docs/impls/v0-mvp.md` §"Definition of done". Running it successfully gates the squash-merge of `feature/v0-mvp` into `main`.

### Chunk ↔ tier map

```
C1  unit
C2  + unit (invariant enforcement at the Rust layer)
C3  + smoke (first UI surface)
C4  + unit (event log primitives)
C5  + unit (mission bookkeeping)
C6  + integration (PTY)
C7  + unit (notify watcher + projections)
C8  + unit (orchestrator rules + replay)
C9  + integration (CLI ↔ event log)
C10 + smoke (workspace) + integration (headless E2E)
C11 + smoke (entrypoint) + demo path
```

### Out of scope for v0 testing

- **Windows.** macOS + Linux only for MVP. Skip Windows-specific assertions.
- **LLM-driven orchestrator.** Rules are deterministic; never assert on "the lead decides to…" beyond what the fixture runner's script does.
- **UI E2E frameworks** (Playwright / webdriver). Surface is too fluid pre-C11.
- **Network filesystems** (NFS/SMB/iCloud). Arch §5.1.1 documents this as a hard requirement on local POSIX.

### Running

```
# Rust — all units + integrations
cd src-tauri && cargo test

# TypeScript typecheck (no tests yet; v0 doesn't ship a JS test runner)
pnpm exec tsc --noEmit

# Dev app for smoke tests
pnpm tauri dev
```

### Cleanup between smokes

Most smoke scenarios accumulate state. Between runs, delete `$APPDATA/runners/runners.db` and the `$APPDATA/runners/crews/` directory. On macOS: `rm -rf "$HOME/Library/Application Support/com.wycstudios.runners"` (add `-dev` for the dev profile if applicable).

---

# Unit tests

Rust-level `#[cfg(test)] mod tests` co-located with the module under test. One-line descriptions below; the tests themselves are authoritative.

## C1 — `src-tauri/src/{db.rs, model.rs}`

Already landed (10 tests):

- `migrations_bootstrap_all_tables` — creates `crews`, `runners`, `missions`, `sessions`.
- `new_crew_is_seeded_with_default_signal_types` — the seven built-in types land via SQL `DEFAULT`.
- `one_lead_per_crew_index_rejects_second_lead` — partial unique index.
- `one_lead_per_crew_allows_leads_across_crews` — the index is crew-scoped.
- `unique_handle_within_crew` — `UNIQUE (crew_id, handle)`.
- `json_blob_columns_roundtrip` — `orchestrator_policy`, `signal_types`, `args_json`, `env_json`.
- `cascade_delete_removes_dependent_rows` — `ON DELETE CASCADE` on runners.
- `migrations_are_idempotent_on_reopen` — each migration applies exactly once across restarts.
- `signal_event_roundtrips_as_documented_envelope` — arch §5.2 shape preserved.
- `message_event_omits_type_when_serialized` — messages have no `type` field.

## C2 — `src-tauri/src/commands/{crew.rs, runner.rs}`

- `first_runner_added_to_crew_is_auto_lead` — invariant from plan.
- `cannot_set_a_second_lead_directly` — `runner_set_lead` on a non-lead with another lead already present fails cleanly (the DB catches it; Rust wraps the error).
- `runner_set_lead_reassigns_atomically` — one transaction: old lead unset + new lead set; partial state never observable from a concurrent reader.
- `deleting_lead_auto_promotes_lowest_position` — promoted runner is the one with the smallest `position`, not the oldest `created_at`.
- `deleting_last_runner_leaves_empty_crew` — crew row remains, runner table empty for that crew.
- `runner_reorder_rejects_missing_ids` — if `ordered_ids` drops or duplicates an id, reject without partial writes.
- `crew_delete_cascades_to_runners` — DB cascade verified via command, not raw SQL.
- `handle_must_be_lowercase_slug` — validation at the Rust layer before INSERT.

## C4 — `src-tauri/src/event_log/`

- `append_is_atomic_under_concurrent_writers` — N threads each append 1000 events; resulting NDJSON is exactly N × 1000 lines, none malformed.
- `append_ordering_is_stable` — ULIDs strictly monotonically increase within a single-process run.
- `ulid_is_collision_safe_within_the_same_millisecond` — generate 10k ULIDs at the same logical `ts`; all unique.
- `read_from_offset_resumes_exactly_where_append_left_off` — critical for C7's watcher.
- `parser_roundtrips_all_event_kinds` — `Signal`, `Message` variants both pass through `serde_json` losslessly.
- `path_helper_respects_appdata_layout` — arch §7.2 layout, including dev suffix.

## C5 — `src-tauri/src/commands/mission.rs`

- `mission_start_on_crewless_crew_errors` — "no runners" error variant, no DB rows created.
- `mission_start_on_leadless_crew_errors` — enforced in Rust even though C1 auto-leads — defense in depth.
- `mission_start_writes_two_opening_events` — exactly `mission_start` then `mission_goal`, with ULIDs in that order.
- `mission_start_exports_signal_types_sidecar` — arch §5.3 Layer 2 — verify the JSON at `$APPDATA/runners/crews/{crew_id}/signal_types.json` matches the crew's current `signal_types`.
- `mission_stop_appends_mission_stopped_and_marks_row` — row transitions to `completed`/`aborted`; terminal event is the last line.
- `mission_list_separates_active_and_past` — used by C11's tabs.

## C7 — `src-tauri/src/event_bus/`

- `watcher_detects_append_within_100ms` — `notify` modify event fires, bus parses new lines.
- `per_runner_inbox_projection_filters_correctly` — `events WHERE to IS NULL OR to = runner.handle`; directed messages to others are excluded.
- `watermark_advances_only_on_inbox_read` — arbitrary `--since` flags don't move the watermark.
- `watermark_rebuilds_on_boot_from_log_scan` — cold start with a pre-existing log; watermarks match what the live session would have produced.
- `malformed_line_is_skipped_with_warning` — the NDJSON file stays parseable; one bad line doesn't poison the bus.

## C8 — `src-tauri/src/orchestrator/`

- Rule-by-rule (each fires exactly once per triggering event):
  - `mission_goal → inject_stdin @lead`
  - `human_said with payload.target → inject_stdin @target`
  - `human_said without target → inject_stdin @lead` (default broadcast recipient)
  - `ask_lead → inject_stdin @lead`
  - `ask_human → emit human_question + open card`
  - `ask_human with on_behalf_of → carry it into human_question.payload`
  - `human_response → inject_stdin to the matching question's asker` (looked up by `question_id`)
- `dispatch_ledger_prevents_duplicate_actions_on_replay` — replay the same log twice, observe one action per triggering event.
- `human_response_without_matching_question_is_dropped_with_warning` — must not panic.
- `messages_do_not_trigger_any_orchestrator_action` — arch §5.5.0 invariant; only signals wake runners.
- `inject_stdin_enriches_with_unread_inbox_summary` — arch §5.5.1.
- `inject_stdin_advances_watermark_via_stdin_injected_signal` — so the next summary is scoped strictly newer.
- `replay_after_reopen_reconstructs_pending_ask_map` — cards re-appear on reboot if unresolved.

---

# Integration tests

Live in `src-tauri/tests/`. Headless: no Tauri runtime, no UI. Use `tempfile` for an isolated `$APPDATA`.

## I1 — C6: PTY session runtime

File: `src-tauri/tests/pty_runtime.rs`

### Scenarios

- **I1.1 — Spawn, inject, read.**
  Spawn a session running `sh`. Inject `echo hi\n`. Assert the output stream contains `hi` within 500ms.

- **I1.2 — Env wiring.**
  Spawn `sh -c 'env | grep RUNNERS_'`. Assert stdout contains the four env vars: `RUNNERS_CREW_ID`, `RUNNERS_MISSION_ID`, `RUNNERS_RUNNER_HANDLE`, `RUNNERS_EVENT_LOG`.

- **I1.3 — PATH preserves `runners` CLI first.**
  Spawn `sh -c 'which runners'`. Assert it resolves to `$APPDATA/runners/bin/runners`, not any system fallback.

- **I1.4 — Pause and resume (Unix).**
  Spawn `sh -c 'while true; do echo tick; sleep 0.1; done'`. After 3 ticks, `SIGSTOP`. Wait 500ms — no new ticks. `SIGCONT`. Ticks resume. `kill`. Process exits.

- **I1.5 — Ring-buffer scrollback bound.**
  Produce > N KB of output. Buffer size stays ≤ N KB. The overflow is accessible via `sessions/{session_id}.log` (arch §7.2).

- **I1.6 — Clean shutdown on `mission_stop`.**
  C5's `mission_stop` kills every session for the mission. Assert each session row transitions to `stopped` and no child processes remain (check via `ps` snapshot).

### Fixture sketch

```rust
let tmp = tempfile::tempdir()?;
let pool = db::open_pool(&tmp.path().join("runners.db"))?;
let mut mgr = SessionManager::new(pool.clone(), tmp.path().to_path_buf());
let mission = fixtures::mission_with_one_shell_runner(&pool, tmp.path())?;
mgr.spawn(&mission, &mission.runners[0])?;
mgr.inject_stdin(&sid, "echo hi\n")?;
// assert on mgr.output_stream(sid) with a timeout
```

## I2 — C9: `runners` CLI ↔ event log roundtrip

File: `cli/tests/roundtrip.rs` (in the `cli/` crate).

### Scenarios

- **I2.1 — `runners signal` appends one line.**
  Spawn `runners signal mission_goal --payload '{"text":"go"}'` with the env a real session has. Assert the NDJSON file grew by exactly one line, parsable as a v0.2 envelope (arch §5.2), with `from` = `$RUNNERS_RUNNER_HANDLE`.

- **I2.2 — `runners signal` rejects unknown types.**
  With the sidecar at `$APPDATA/runners/crews/{id}/signal_types.json` containing the default seven, run `runners signal not_a_real_type`. Exit code non-zero, stderr mentions the allowlist, no line appended.

- **I2.3 — `runners msg post --to impl` routes.**
  Assert the envelope has `kind: "message"`, `to: "impl"`, `payload.text` set.

- **I2.4 — `runners msg post --to ghost` rejects unknown handles.**
  Exit non-zero, stderr mentions the crew roster; no line appended.

- **I2.5 — `runners msg read` emits `inbox_read`.**
  Pre-populate the log with two directed messages to `impl`. Run `runners msg read`. Assert: stdout contains both messages in ULID order, and a final `signal inbox_read` line was appended with `payload.up_to` = max ULID of the two.

- **I2.6 — Concurrent writers interleave atomically.**
  10 shells × 100 invocations each write signals to the same log. Resulting NDJSON: exactly 1000 lines, no partial lines, no interleaved bytes. Every line parses.

- **I2.7 — Missing env vars fail fast.**
  Unset `RUNNERS_EVENT_LOG`; CLI exits non-zero with a pointer at which env var is missing.

### Fixture sketch

```rust
let tmp = tempfile::tempdir()?;
let mission_dir = prepare_mission_dir(tmp.path(), "c1", "m1");
let env = &[
    ("RUNNERS_CREW_ID", "c1"),
    ("RUNNERS_MISSION_ID", "m1"),
    ("RUNNERS_RUNNER_HANDLE", "impl"),
    ("RUNNERS_EVENT_LOG", mission_dir.join("events.ndjson").to_str().unwrap()),
    ("PATH", &format!("{}:{}", cli_bin_dir.display(), std::env::var("PATH")?)),
];
let out = Command::new("sh").args(["-c", "runners signal mission_goal"]).envs(env).output()?;
assert!(out.status.success());
// parse the last line of events.ndjson and assert the envelope shape
```

## I3 — C10: full mission lifecycle without UI

File: `src-tauri/tests/mission_e2e.rs`

This test is the MVP's automated stand-in for the demo path. The UI covers the same ground manually in smoke tests; this one runs in CI.

### Scenario

Start a mission on a two-runner crew (`lead` + `impl`, both `shell` with scripted stdin-reply behavior), drive it through one full lead-mediated HITL round, and assert the exact sequence of events.

Driver pseudocode:

```
1.  bootstrap pool + create crew {lead, impl}
2.  mission_start(crew, "E2E", goal = "solve it", cwd = tmp)
    → events.ndjson now has: mission_start, mission_goal
3.  orchestrator starts; observes mission_goal → inject_stdin @lead
    → appends: stdin_injected(target=lead, triggered_by=mission_goal.id)
4.  driver feeds the `lead` PTY a scripted response: `runners msg post --to impl "go"`
    → appends: message(from=lead, to=impl, text="go")
5.  driver feeds the `impl` PTY: `runners msg read` then `runners signal ask_lead …`
    → appends: inbox_read(up_to=<msg ulid>), ask_lead(from=impl)
6.  orchestrator observes ask_lead → inject_stdin @lead (with inbox summary)
    → appends: stdin_injected(target=lead, watermark=<inbox max>)
7.  driver feeds `lead`: `runners signal ask_human --payload '{"prompt":"…","choices":["yes","no"],"on_behalf_of":"impl"}'`
    → appends: ask_human(from=lead, payload.on_behalf_of=impl), human_question(from=orchestrator, payload.triggered_by=<ask_human.id>)
8.  driver simulates human click: orchestrator.handle_human_click(question_id, "yes")
    → appends: human_response(from=human, payload.question_id=<q.id>, choice=yes), stdin_injected(target=lead, triggered_by=<human_response.id>)
9.  driver feeds `lead`: `runners msg post --to impl "Human approved."`
    → appends: message(from=lead, to=impl)
10. driver feeds `impl`: `runners msg read`
    → appends: inbox_read(up_to=<approved msg ulid>)
11. mission_stop
    → appends: mission_stopped
```

Assertions:
- The ordered sequence of `(kind, type or None, from, to)` tuples matches exactly.
- No duplicate orchestrator actions even if the test replays the log through a second orchestrator instance (C8's dispatch-ledger idempotence).
- Every audit signal carries `payload.triggered_by` pointing at a real event id.
- After `mission_stop`, every session row is `stopped` and no child processes remain.

### Crash-replay assertion (same file)

After step 8, **hard-kill** the driver's orchestrator and restart it from scratch against the same log:
- The rebuilt in-memory `pending_ask` map is empty (the question was resolved before the crash).
- No rule re-fires: steps 3, 6, 8's audit signals exist exactly once each.
- Watermarks match the live run.

---

# Smoke (UI, manual)

Each scenario: **Steps** then **Expected**. Before each smoke, delete `$APPDATA/runners/` to start clean.

## C3 — Config UI (Crews, Crew Detail, Add Slot)

### Prereqs & setup

- C1, C2, C3 merged. `pnpm install` run. Clean `$APPDATA/runners/`.
- `pnpm tauri dev`.

### Scenarios

**S3.1 — Create a crew**

1. On Crews, click **+ New Crew**, name `Demo Crew`, save.

Expected: card appears; clicking it routes to empty Crew Detail with an **Add Slot** button; no LEAD badge (empty crews are valid).

**S3.2 — First runner auto-leads**

1. Add Slot → handle `lead`, runtime `claude-code`, command `claude`. Save.

Expected: runner row at position 0 with `LEAD` badge; no **Set as lead** action on this row.

**S3.3 — Second runner is not lead**

1. Add Slot → `impl` / `shell` / `sh`. Save.

Expected: position 1, no badge, **Set as lead** action visible. Original lead still badged.

**S3.4 — Reassign lead (transactional)**

1. On `impl`, click **Set as lead**.

Expected: only `impl` shows `LEAD` now. Refresh: persists. At no point do two rows show the badge simultaneously (verifies C2's transaction + C1's partial unique index).

**S3.5 — Delete lead auto-promotes**

1. Add a third runner `worker` at position 2.
2. With `impl` as lead, delete `impl`.

Expected: `lead` (lowest remaining `position` = 0) gains the `LEAD` badge automatically. Crew never has zero leads while runners exist.

**S3.6 — Empty crew is allowed**

1. Delete every runner one by one.

Expected: after each delete, at least one runner holds `LEAD` until the last. After the last: empty crew, no errors, crew row not auto-deleted.

**S3.7 — Handle uniqueness**

1. Try Add Slot twice with the same handle.

Expected: second save errors with a clear message referencing uniqueness; modal stays open with input preserved.

**S3.8 — Drag-reorder preserves lead**

1. Three runners, lead at position 0. Drag lead to position 2.

Expected: positions persist (refresh to confirm); `LEAD` badge still attached to the same runner. `runner_reorder` is called once per drop, not per hover frame.

### Known gaps — do NOT verify in C3

- Per-slot system-prompt override: UI field exists but is a stub until v0.x — typing stores the value but it has no effect at runtime.
- Standalone Runners list / Runner Detail pages: built in **C8.5**, not C3. Verify under that chunk's manual-test plan.
- Mission workspace behavior: C10.
- Start Mission: C11.

## C10 — Mission Workspace

### Prereqs & setup

- C1–C10 merged. `runners` CLI on PATH inside PTYs (C6's env setup).
- Fixture crew **Smoke Crew** with exactly two runners, both runtime `shell`, command `sh`: `lead` (carries `LEAD`) and `impl`. Using `shell` runners makes behavior deterministic — the smoker types `runners signal …` and `runners msg post …` by hand.
- `pnpm tauri dev`. Pre-C11, start the mission via DevTools:

```js
await window.__TAURI__.core.invoke('mission_start', {
  crewId: '<Smoke Crew id>',
  title: 'Smoke C10',
  goalOverride: 'Verify workspace',
  cwd: null,
});
```

### Scenarios

**S10.1 — Workspace renders with both PTYs live**

Expected on first mount:
- Title shows `Smoke C10`.
- Event feed shows `mission_start` then `mission_goal` (both from C5).
- Runners rail shows `lead` (badged) and `impl`, each with a green status dot.
- Each terminal streams real shell output (`echo hi` only echoes in that pane).

**S10.2 — Lead receives the goal via stdin**

Expected within ~1s of `mission_start`:
- `lead` terminal shows the composed prompt (template from arch §4: goal + roster + coordination).
- Feed contains `stdin_injected` audit with `payload.target = "lead"`, `payload.triggered_by = <mission_goal.id>`.

**S10.3 — Directed message is pull-based**

1. In `lead` pane: `runners msg post --to impl "Start reading the spec."`
2. Wait 2s.

Expected:
- A `message` event appears in the feed (`from: "lead"`, `to: "impl"`).
- `impl` PTY shows nothing yet — messages don't wake recipients (arch §2.7.3, §5.6).

3. In `impl` pane: `runners msg read`.

Expected: the message is returned in stdout; an `inbox_read` signal event appends with `payload.up_to = <that message's ULID>`.

**S10.4 — Lead-mediated HITL**

1. `impl`: `runners signal ask_lead --payload '{"question":"A or B?","context":"A fast, B small."}'`

Expected: `ask_lead` event; `lead` PTY receives rendered injection containing the question; injection includes the unread-inbox summary per arch §5.5.1 (possibly empty).

2. `lead`: `runners signal ask_human --payload '{"prompt":"Use A?","choices":["yes","no"],"on_behalf_of":"impl"}'`

Expected: card appears in the side panel with attribution chain `*@impl → @lead → you*` (because `on_behalf_of` is set); **yes** / **no** buttons visible.

3. Click **yes**.

Expected: `human_response` signal appended with `payload: { question_id, choice: "yes" }`; `lead` PTY receives the response (lead is the asker of record); card resolves/disappears.

4. `lead`: `runners msg post --to impl "Human approved: use A."`
5. `impl`: `runners msg read`.

Expected: `impl` sees the forwarded message; a second `inbox_read` appends.

**S10.5 — Broadcast human input lands on lead**

1. Type `Kick off the review.` in MissionInput with default recipient `@lead`. Post.

Expected:
- `signal human_said` appended with `payload.text` set and `payload.target` either unset or `"lead"` (per built-in rule).
- Envelope `to` is **null** (signals carry `to: null` in v0 per arch §5.2; target lives in payload).
- `lead` PTY receives the text via injection. `impl` does not.

**S10.6 — Directed human input**

1. Switch recipient to `@impl`, post `Skip the first step.`.

Expected: `payload.target: "impl"`; `impl` PTY injected; `lead` PTY silent.

**S10.7 — Close and reopen**

1. Route back to Missions. Reopen the mission.

Expected:
- Feed replays every event in order.
- Sessions reconnect or show a clear "stopped" state consistent with C6's close behavior.
- Read-watermarks rebuilt from the log; no action double-fires.
- Any pending `ask_human` cards re-render (none in this scenario).

**S10.8 — Messages/signals split**

Expected: feed visibly segregates `kind: message` rows from `kind: signal`. Orchestrator-emitted audit signals (`inject_stdin`, `human_question`, `human_response`, `inbox_read`) all go to the signal panel.

### Known gaps — do NOT verify in C10

- Start Mission button (C11).
- Concurrent `ask_human` cards — arch §5.5.0 declares concurrent prompts out of scope. Do not open a second card while one is pending.
- Messages triggering wake-ups. They don't, by design.

### Cleanup

```js
await window.__TAURI__.core.invoke('mission_stop', { missionId: '<id>' });
```

Optionally delete `$APPDATA/runners/crews/<crew_id>/missions/<mission_id>/` for the next run.

## C11 — Missions list + Start Mission modal

### Prereqs & setup

- C1–C11 merged. Clean `$APPDATA/runners/`.
- `pnpm tauri dev`.

### Scenarios

**S11.1 — Missions list with Active / Past tabs**

Expected on first open: both tabs render; Active empty, Past empty, no errors.

**S11.2 — Start Mission modal happy path**

1. From Missions (or Home), click **Start Mission**.
2. Pick Crew `Demo Crew` (from C3 smokes).
3. Title `S11 first mission`. Goal textarea: `Do the thing.`.
4. Cwd: leave blank or Browse… to pick a project dir.
5. Start.

Expected: modal closes; route becomes `/missions/<id>`; workspace opens (all C10 behaviors apply).

**S11.3 — Start on empty/leadless crew is blocked**

1. In Crews, create `Empty Crew` with zero runners.
2. Start Mission → pick `Empty Crew` → Start.

Expected: modal surfaces a clean error ("crew has no runners" or "no lead"); mission row is NOT created; no log directory appears under `$APPDATA/runners/crews/<empty crew id>/missions/`.

**S11.4 — Active tab highlights pending asks**

1. Start a mission from `Demo Crew`. From the `lead` PTY, open an `ask_human` card (as in S10.4). Close the tab without clicking the card.
2. Return to Missions list.

Expected: the mission row in Active shows a "pending ask" flag (derived from orchestrator state). Clicking it reopens the workspace with the card still pending.

**S11.5 — Past tab shows stopped missions**

1. Stop a mission (via workspace controls or DevTools `mission_stop`).

Expected: row moves from Active to Past; status reflects terminal state (`completed` / `aborted`); clicking it opens the workspace read-only (or at least with the feed replayed and no live PTYs).

**S11.6 — Advanced collapse is stubbed**

Expected: clicking **Advanced** expands but any inner controls are inert — the plan calls this out as stubbed. Don't assert on their effect.

### Known gaps — do NOT verify in C11

- Mission archive / search / filter — deferred beyond MVP.
- Any change to C3 (config) or C10 (workspace) surfaces.

---

# Demo path (the Definition of Done)

Verbatim from `docs/impls/v0-mvp.md` §"Definition of done". Run this once `feature/v0-mvp` is ready to squash-merge into `main`.

From a clean launch of the app:

1. On Crews, create **Demo Crew**. Add two runners: one `claude-code` `lead` (real LLM agent), one `shell` worker (e.g. `sh`). The lead invariant holds at every step.
2. Click **Start Mission**, fill goal `Write a README stub for this repo.`, cwd = a scratch dir. Workspace opens with two live PTYs.
3. Lead receives the goal via stdin, drafts a plan, and posts a directed message to the worker. Worker picks it up on its next `runners msg read`.
4. Worker emits an `ask_lead` signal; lead decides to escalate via `ask_human`; click **Approve** on the resulting card; lead receives the response and forwards it to the worker via a directed message.
5. Post a broadcast human signal from the workspace input; it lands on the lead by default (payload omits `target`).
6. Close the mission tab and reopen from the Missions list; the feed replays and the orchestrator's in-memory state reconstructs (pending asks, watermarks, dispatch ledger).

All six steps must succeed in one session without restarting the app. Capture a screen recording and attach it to the squash-merge PR.

Anything beyond this run is explicitly v0.x or later.
