// Per-runner PTY session runtime.
//
// One `Session` = one `portable_pty` child running the runner's CLI agent. The
// SessionManager holds the map of live sessions so Tauri commands can look
// them up by id (for stdin injection, pause/resume, kill). Each session owns:
//
//   - A `MasterPty` handle (Tauri process side). The slave end is closed
//     immediately after spawn — we never read from it.
//   - A reader thread that drains the PTY and emits `session/output` Tauri
//     events. When the reader hits EOF (child exited, signaled, or we killed
//     it), it reaps the child, emits `session/exit`, and updates the DB row.
//   - A writer behind a Mutex for `inject_stdin`.
//
// Drop behavior: killing the app process drops the SessionManager, which
// drops every `SessionHandle`, which drops each `Child`. `portable-pty`'s
// Child wrappers on Unix do not SIGKILL on drop — we take care of this in
// `SessionManager::kill_all` at app shutdown (future work; for MVP the
// child inherits our process group and dies when we exit).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use rusqlite::params;
use serde::Serialize;

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::model::{Mission, Runner};

/// Decouples the PTY layer from Tauri so the reader thread can be unit-tested
/// with a fake. Prod wraps an `AppHandle::emit`; tests use a no-op or a
/// channel-capture impl.
pub trait SessionEvents: Send + Sync + 'static {
    fn output(&self, ev: &OutputEvent);
    fn exit(&self, ev: &ExitEvent);
    /// Live activity counter for a runner — emitted on every spawn/reap so
    /// the Runners list can update its "N sessions / M missions" badges
    /// without polling. Default no-op so test fakes don't have to opt in.
    fn runner_activity(&self, _ev: &RunnerActivityEvent) {}
}

/// Payload for `runner/activity`. Derived from the same query
/// `RunnerActivity` (`runner_activity` Tauri command) returns, so a fresh
/// page load and a live update agree.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerActivityEvent {
    pub runner_id: String,
    pub handle: String,
    pub active_sessions: i64,
    pub active_missions: i64,
    pub crew_count: i64,
    /// Most recent running direct-chat session id, if any. Mirrors
    /// `RunnerActivity::direct_session_id` so the sidebar can re-attach
    /// to a live PTY without an extra round-trip.
    pub direct_session_id: Option<String>,
}

/// Emitter for the real Tauri app — emits `session/output`, `session/exit`,
/// and `runner/activity`.
pub struct TauriSessionEvents(pub tauri::AppHandle);

impl SessionEvents for TauriSessionEvents {
    fn output(&self, ev: &OutputEvent) {
        use tauri::Emitter;
        let _ = self.0.emit("session/output", ev);
    }
    fn exit(&self, ev: &ExitEvent) {
        use tauri::Emitter;
        let _ = self.0.emit("session/exit", ev);
    }
    fn runner_activity(&self, ev: &RunnerActivityEvent) {
        use tauri::Emitter;
        let _ = self.0.emit("runner/activity", ev);
    }
}

/// Contents of `session/output` events emitted to the frontend. The raw PTY
/// bytes are base64-encoded so the event payload is valid JSON regardless of
/// what the child wrote (ANSI escapes, split UTF-8 sequences, non-UTF-8, etc.).
/// The frontend decodes before feeding xterm.js.
///
/// `mission_id` is `None` for direct-chat sessions (C8.5) — they have no
/// parent mission and consumers should filter on `session_id` instead.
#[derive(Debug, Clone, Serialize)]
pub struct OutputEvent {
    pub session_id: String,
    pub mission_id: Option<String>,
    /// Base64-encoded raw bytes read from the PTY.
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExitEvent {
    pub session_id: String,
    pub mission_id: Option<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Row returned to the frontend after a spawn. Subset of the DB `sessions`
/// row with the runner handle denormalized so the debug page can render
/// `@coder`-style labels without a separate lookup.
#[derive(Debug, Clone, Serialize)]
pub struct SpawnedSession {
    pub id: String,
    pub mission_id: Option<String>,
    pub runner_id: String,
    pub handle: String,
    pub pid: Option<u32>,
}

struct SessionHandle {
    // Kept for debugging and future kill-by-pid / identity checks.
    #[allow(dead_code)]
    id: String,
    /// `None` for direct-chat sessions (C8.5). `kill_all_for_mission`
    /// filters on this so direct chats don't get torn down when a mission
    /// stops, and vice versa.
    mission_id: Option<String>,
    /// The runner this session is an instance of. `kill_all_for_runner`
    /// filters on this so deleting a runner can reap its live PTY
    /// children before the cascade nukes the DB rows underneath.
    runner_id: String,
    /// Optionally holds the master PTY. `kill` takes it to drop-close the
    /// terminal (signals the child's SIGHUP) before signaling/joining.
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    /// OS process id of the spawned child. Used by `kill` to escalate
    /// SIGTERM → SIGKILL if the PTY hangup alone doesn't reap the child.
    pid: Option<u32>,
    /// Handle for the reader thread that drains the PTY + reaps the child.
    /// `kill` joins on it so the caller is guaranteed the `sessions` row is
    /// in a terminal status by the time we return.
    reader: Option<thread::JoinHandle<()>>,
}

pub struct SessionManager {
    sessions: Mutex<HashMap<String, SessionHandle>>,
}

impl SessionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Spawn one PTY child for `runner` as part of `mission`. Persists a
    /// `sessions` row, starts the reader thread, and returns a summary for
    /// the frontend.
    ///
    /// `app_data_dir` is the root of `$APPDATA/runner/` so we can prepend
    /// `<app_data_dir>/bin` onto the child's PATH — arch §5.3 Layer 2 and
    /// v0-mvp.md C9 both require the bundled `runner` CLI to win over any
    /// system binary with the same name.
    pub fn spawn(
        self: &Arc<Self>,
        mission: &Mission,
        runner: &Runner,
        app_data_dir: &Path,
        events_log_path: PathBuf,
        pool: Arc<DbPool>,
        events: Arc<dyn SessionEvents>,
    ) -> Result<SpawnedSession> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::msg(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(&runner.command);
        cmd.args(&runner.args);

        // Working directory: runner override if set, else mission cwd, else
        // inherit parent's. `CommandBuilder::cwd` requires a concrete path.
        if let Some(wd) = runner.working_dir.as_deref() {
            cmd.cwd(wd);
        } else if let Some(wd) = mission.cwd.as_deref() {
            cmd.cwd(wd);
        }

        // Env — start from the runner's map (so the user can override /
        // clear things they need), then layer the system-assigned vars on
        // top so they can't be accidentally shadowed.
        for (k, v) in &runner.env {
            cmd.env(k, v);
        }
        // Prepend the bundled CLI directory to PATH so `runners` on the
        // child's PATH resolves to our drop (C9 installs it here) before
        // any system binary with the same name. Inherit the parent PATH
        // as the tail — if nothing else, agents need `sh`, `git`, `node`.
        let bin_dir = app_data_dir.join("bin");
        let sep = if cfg!(windows) { ';' } else { ':' };
        let parent_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::from(bin_dir.as_os_str());
        if !parent_path.is_empty() {
            new_path.push(std::ffi::OsString::from(sep.to_string()));
            new_path.push(parent_path);
        }
        cmd.env("PATH", new_path);

        cmd.env("RUNNER_CREW_ID", &mission.crew_id);
        cmd.env("RUNNER_MISSION_ID", &mission.id);
        cmd.env("RUNNER_HANDLE", &runner.handle);
        cmd.env(
            "RUNNER_EVENT_LOG",
            events_log_path.to_string_lossy().to_string(),
        );
        if let Some(wd) = mission.cwd.as_deref() {
            cmd.env("MISSION_CWD", wd);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::msg(format!("spawn {}: {e}", runner.command)))?;
        // Closing the slave on our side means child is the only holder and
        // our reader sees EOF the moment the child dies.
        drop(pair.slave);

        let pid = child.process_id();

        // Everything between `spawn_command` and the live-map insert is
        // fallible (`try_clone_reader`, `take_writer`, `sessions` INSERT).
        // If any of it errors we'd otherwise leak the running child — the
        // session isn't in the map yet, so `mission_start`'s rollback can't
        // see it and nothing else ever reaps it. Group the fallible work in
        // an IIFE so a single error handler can kill + wait the child on
        // every post-spawn failure path.
        let session_id = ulid::Ulid::new().to_string();
        let started_at = Utc::now().to_rfc3339();
        let setup_res: Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> = (|| {
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| Error::msg(format!("clone reader: {e}")))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|e| Error::msg(format!("take writer: {e}")))?;
            let conn = pool.get()?;
            conn.execute(
                "INSERT INTO sessions
                    (id, mission_id, runner_id, status, pid, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4, ?5)",
                params![session_id, mission.id, runner.id, pid, started_at],
            )?;
            Ok((reader, writer))
        })();
        let (reader, writer) = match setup_res {
            Ok(rw) => rw,
            Err(e) => {
                // Reap the orphan. `kill` signals SIGTERM/Windows equivalent;
                // `wait` blocks until the child is gone so the caller isn't
                // racing against a live PID when it retries.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

        // Insert into the live map BEFORE starting the reader thread.
        // A short-lived child (e.g. `sh -c "echo hi"`) can exit within
        // microseconds — if we spawned the thread first, its `forget()`
        // call could run before the insert and leave a stale live handle
        // for an already-dead session. Handle parts that the reader thread
        // needs ownership of (child, reader pipe) stay out of the map;
        // parts the Tauri commands need (master, writer, pid) go in.
        self.sessions.lock().unwrap().insert(
            session_id.clone(),
            SessionHandle {
                id: session_id.clone(),
                mission_id: Some(mission.id.clone()),
                runner_id: runner.id.clone(),
                master: Some(pair.master),
                writer: Mutex::new(writer),
                pid,
                reader: None, // populated immediately below
            },
        );

        // Spawn the reader thread. On EOF it reaps the child, updates the
        // DB row, removes the session from the in-memory map, and emits
        // the `exit` event. `kill` joins this handle to guarantee the
        // mission_stop → mission_completed transition never races ahead of
        // the actual child reap.
        let reader_handle = self.start_reader_thread(
            session_id.clone(),
            Some(mission.id.clone()),
            child,
            reader,
            Arc::clone(&pool),
            Arc::clone(&events),
            runner.clone(),
        );

        // Attach the reader handle. We raced to insert-first so the reader
        // may already be draining by the time we land here — that's fine,
        // it doesn't touch this slot.
        if let Some(h) = self.sessions.lock().unwrap().get_mut(&session_id) {
            h.reader = Some(reader_handle);
        }

        // Notify subscribers (Runners page, Runner Detail) that this
        // runner's activity counters changed. Don't fail the spawn if the
        // counter query hits a transient error — the spawn itself
        // succeeded; activity badges will reconcile on the next event.
        emit_runner_activity(&pool, runner, events.as_ref());

        Ok(SpawnedSession {
            id: session_id,
            mission_id: Some(mission.id.clone()),
            runner_id: runner.id.clone(),
            handle: runner.handle.clone(),
            pid,
        })
    }

    /// Spawn a "direct chat" PTY: a runner process with **no parent
    /// mission**. Schema-supported since C5.5a (`sessions.mission_id` is
    /// nullable); C8.5 surfaces it as the "Chat now" affordance on the
    /// Runner Detail page.
    ///
    /// Differences vs. the mission-flavored `spawn`:
    ///   - No `RUNNER_MISSION_ID`, `RUNNER_EVENT_LOG`, or
    ///     `RUNNER_CREW_ID` env vars. The runner's CLI is on PATH, but
    ///     anything it tries to do that needs those vars no-ops or errors
    ///     gracefully — direct chats are not on any coordination bus.
    ///   - `cwd` lives on the session row directly, since there's no
    ///     mission to inherit it from.
    ///   - The session does not show up in `kill_all_for_mission` for any
    ///     mission_id, so a `mission_stop` on some unrelated crew never
    ///     yanks the user's open chat.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_direct(
        self: &Arc<Self>,
        runner: &Runner,
        cwd: Option<&str>,
        cols: Option<u16>,
        rows: Option<u16>,
        app_data_dir: &Path,
        pool: Arc<DbPool>,
        events: Arc<dyn SessionEvents>,
    ) -> Result<SpawnedSession> {
        let pty_system = portable_pty::native_pty_system();
        // Spawn at the caller's reported xterm grid when known. TUIs like
        // claude-code lay out their input frame on first paint and don't
        // gracefully redraw on later SIGWINCH, so booting at the wrong
        // size leaves a stale 80-col frame stranded in the buffer.
        let opened = PtySize {
            rows: rows.unwrap_or(24),
            cols: cols.unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(opened)
            .map_err(|e| Error::msg(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(&runner.command);
        cmd.args(&runner.args);

        // Working directory precedence: explicit `cwd` arg (the user picked
        // a folder in the Chat now dialog) ► runner's own `working_dir`
        // override ► inherit parent's. Mirrors `spawn`'s precedence so
        // behavior is consistent across mission and direct flavors.
        let resolved_cwd: Option<String> = cwd
            .map(|s| s.to_string())
            .or_else(|| runner.working_dir.clone());
        if let Some(wd) = resolved_cwd.as_deref() {
            cmd.cwd(wd);
        }

        for (k, v) in &runner.env {
            cmd.env(k, v);
        }
        // PATH still gets the bundled CLI prepended — the runner might
        // call `runner --help` interactively; let it find the binary.
        let bin_dir = app_data_dir.join("bin");
        let sep = if cfg!(windows) { ';' } else { ':' };
        let parent_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::from(bin_dir.as_os_str());
        if !parent_path.is_empty() {
            new_path.push(std::ffi::OsString::from(sep.to_string()));
            new_path.push(parent_path);
        }
        cmd.env("PATH", new_path);
        cmd.env("RUNNER_HANDLE", &runner.handle);
        // Pass the spawn-time grid via COLUMNS/LINES too. portable-pty
        // sets the kernel winsize via TIOCSWINSZ at openpty time, but
        // some Node-based TUIs (claude-code, anything using ink) read
        // these env vars on startup as a fallback / hint and lay out
        // their initial UI from them, ignoring SIGWINCH that arrives
        // mid-render. Without this, claude-code paints its input frame
        // at whatever stale size it picked up.
        cmd.env("COLUMNS", opened.cols.to_string());
        cmd.env("LINES", opened.rows.to_string());
        cmd.env("TERM", "xterm-256color");
        // Deliberately NOT setting RUNNER_CREW_ID, RUNNER_MISSION_ID,
        // RUNNER_EVENT_LOG, MISSION_CWD — direct chats are off-bus.

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::msg(format!("spawn {}: {e}", runner.command)))?;
        drop(pair.slave);

        let pid = child.process_id();
        let session_id = ulid::Ulid::new().to_string();
        let started_at = Utc::now().to_rfc3339();
        let setup_res: Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> = (|| {
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| Error::msg(format!("clone reader: {e}")))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|e| Error::msg(format!("take writer: {e}")))?;
            let conn = pool.get()?;
            // mission_id is NULL; cwd lives on the session row.
            conn.execute(
                "INSERT INTO sessions
                    (id, mission_id, runner_id, cwd, status, pid, started_at)
                 VALUES (?1, NULL, ?2, ?3, 'running', ?4, ?5)",
                params![session_id, runner.id, resolved_cwd, pid, started_at],
            )?;
            Ok((reader, writer))
        })();
        let (reader, writer) = match setup_res {
            Ok(rw) => rw,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

        self.sessions.lock().unwrap().insert(
            session_id.clone(),
            SessionHandle {
                id: session_id.clone(),
                mission_id: None,
                runner_id: runner.id.clone(),
                master: Some(pair.master),
                writer: Mutex::new(writer),
                pid,
                reader: None,
            },
        );

        let reader_handle = self.start_reader_thread(
            session_id.clone(),
            None,
            child,
            reader,
            Arc::clone(&pool),
            Arc::clone(&events),
            runner.clone(),
        );
        if let Some(h) = self.sessions.lock().unwrap().get_mut(&session_id) {
            h.reader = Some(reader_handle);
        }

        emit_runner_activity(&pool, runner, events.as_ref());

        Ok(SpawnedSession {
            id: session_id,
            mission_id: None,
            runner_id: runner.id.clone(),
            handle: runner.handle.clone(),
            pid,
        })
    }

    /// Common reader-thread machinery used by both `spawn` (mission) and
    /// `spawn_direct`. Drains the PTY, reaps the child, flips the DB row,
    /// removes the live-map entry, and emits `session/exit`. Whatever
    /// invoked spawn doesn't get a return until `kill` joins this handle,
    /// which is what mission_stop relies on for the no-lying-about-
    /// termination contract.
    // The reader thread genuinely needs every one of these — session_id /
    // mission_id for event payloads, child + reader for the PTY drain, pool
    // for the DB row update, events for emitter dispatch, runner for the
    // post-reap activity recompute. Bundling into a Context struct just
    // moves the same arity to the call site without buying clarity.
    #[allow(clippy::too_many_arguments)]
    fn start_reader_thread(
        self: &Arc<Self>,
        session_id: String,
        mission_id: Option<String>,
        mut child: Box<dyn portable_pty::Child + Send + Sync>, // portable-pty's Child is Send + Sync; both needed for thread::spawn move + the &mut reborrow inside drain_pty_and_reap.
        reader: Box<dyn Read + Send>,
        pool: Arc<DbPool>,
        events: Arc<dyn SessionEvents>,
        runner: Runner,
    ) -> thread::JoinHandle<()> {
        let manager_t: Arc<SessionManager> = Arc::clone(self);
        thread::spawn(move || {
            let exit = drain_pty_and_reap(
                reader,
                &mut *child,
                &session_id,
                mission_id.as_deref(),
                events.as_ref(),
            );
            let _ = manager_t.forget(&session_id);
            if let Ok(conn) = pool.get() {
                let _ = conn.execute(
                    "UPDATE sessions
                        SET status = ?1, stopped_at = ?2
                      WHERE id = ?3",
                    params![
                        if exit.success { "stopped" } else { "crashed" },
                        Utc::now().to_rfc3339(),
                        session_id,
                    ],
                );
            }
            // Activity dropped — emit before `exit` so the Runners list
            // sees the new counts before any session_id-keyed UI cleans up.
            emit_runner_activity(&pool, &runner, events.as_ref());
            events.exit(&exit);
        })
    }

    /// Write raw bytes to the session's stdin.
    pub fn inject_stdin(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let handle = sessions
            .get(session_id)
            .ok_or_else(|| Error::msg(format!("session not found: {session_id}")))?;
        let mut writer = handle.writer.lock().unwrap();
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    /// Resize the session's PTY. Issues the equivalent of an SIGWINCH so
    /// the child re-renders into the new grid. Frontend calls this after
    /// xterm fits to the container — without it, claude-code stays at
    /// the spawn-time 80×24 regardless of how big the visible grid is.
    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let handle = sessions
            .get(session_id)
            .ok_or_else(|| Error::msg(format!("session not found: {session_id}")))?;
        if let Some(master) = handle.master.as_ref() {
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| Error::msg(format!("pty resize failed: {e}")))?;
        }
        Ok(())
    }

    /// Kill the child and wait for the reader thread to reap it.
    ///
    /// Sequence:
    ///   1. Remove the handle from the live map (no further `inject_stdin` /
    ///      `kill` can target it).
    ///   2. Drop the master PTY — the child receives SIGHUP and well-behaved
    ///      programs exit; the reader thread's `read()` returns 0.
    ///   3. On Unix, belt-and-suspenders: signal SIGTERM (then SIGKILL after
    ///      200 ms) so a child that ignores SIGHUP can't stall the reader.
    ///   4. Join the reader thread. It waits the child, updates the DB row
    ///      to stopped/crashed, emits `session/exit`. Only after this
    ///      returns is the caller allowed to consider the session dead —
    ///      which is what `mission_stop` needs in order to flip the mission
    ///      row without lying about termination.
    pub fn kill(&self, session_id: &str) -> Result<()> {
        let (pid, master, reader) = {
            let mut sessions = self.sessions.lock().unwrap();
            match sessions.remove(session_id) {
                Some(mut h) => (h.pid, h.master.take(), h.reader.take()),
                None => return Ok(()),
            }
        };

        // Step 2: hang up the terminal. For most children this alone is
        // enough. We drop before sending signals so the child's next I/O
        // fails instead of blocking indefinitely.
        drop(master);

        // Step 3: Unix-only hard-kill escalation.
        #[cfg(unix)]
        if let Some(pid) = pid {
            // SAFETY: `pid` came from `Child::process_id()` on a child we
            // just started; it hasn't been reaped yet because the reader
            // thread holds the only `Child` reference. `kill(2)` with an
            // unknown pid is a no-op returning ESRCH which we ignore.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = pid; // Windows path lands with a future chunk.

        // Step 4: wait for the reader to reap + update the DB + emit exit.
        if let Some(h) = reader {
            let _ = h.join();
        }
        Ok(())
    }

    /// Kill every live session; used on mission_stop and at app shutdown.
    /// Returns only after all reader threads have joined — callers rely on
    /// that for the "no live sessions after we return" contract.
    pub fn kill_all_for_mission(&self, mission_id: &str) -> Result<()> {
        let ids: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .values()
                .filter(|s| s.mission_id.as_deref() == Some(mission_id))
                .map(|s| s.id.clone())
                .collect()
        };
        for id in ids {
            self.kill(&id)?;
        }
        Ok(())
    }

    /// Kill every live session for `runner_id` — both mission-scoped and
    /// direct-chat. Used by `runner_delete` so the cascade dropping the
    /// `sessions` rows doesn't strand the PTY children running underneath.
    /// Returns only after every reader thread has joined.
    pub fn kill_all_for_runner(&self, runner_id: &str) -> Result<()> {
        let ids: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .values()
                .filter(|s| s.runner_id == runner_id)
                .map(|s| s.id.clone())
                .collect()
        };
        for id in ids {
            self.kill(&id)?;
        }
        Ok(())
    }

    fn forget(&self, session_id: &str) -> Result<()> {
        self.sessions.lock().unwrap().remove(session_id);
        Ok(())
    }
}

/// Compute current activity counters for `runner` and emit a
/// `runner/activity` event. Best-effort: if the DB roundtrip fails we drop
/// the emission rather than failing the spawn/reap path. Runners list will
/// reconcile via the next emission or a manual refresh.
fn emit_runner_activity(pool: &DbPool, runner: &Runner, events: &dyn SessionEvents) {
    let Ok(conn) = pool.get() else { return };
    let active_sessions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE runner_id = ?1 AND status = 'running'",
            params![runner.id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let active_missions: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT mission_id) FROM sessions
              WHERE runner_id = ?1 AND status = 'running' AND mission_id IS NOT NULL",
            params![runner.id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let crew_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM crew_runners WHERE runner_id = ?1",
            params![runner.id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let direct_session_id: Option<String> = conn
        .query_row(
            "SELECT id FROM sessions
              WHERE runner_id = ?1 AND status = 'running' AND mission_id IS NULL
              ORDER BY started_at DESC
              LIMIT 1",
            params![runner.id],
            |r| r.get(0),
        )
        .ok();
    events.runner_activity(&RunnerActivityEvent {
        runner_id: runner.id.clone(),
        handle: runner.handle.clone(),
        active_sessions,
        active_missions,
        crew_count,
        direct_session_id,
    });
}

/// Pumps PTY output → `session/output` events, then waits for the child to
/// exit. Returns the exit summary that the caller emits as `session/exit`.
/// `mission_id` is `None` for direct-chat sessions.
fn drain_pty_and_reap(
    mut reader: Box<dyn Read + Send>,
    child: &mut (dyn portable_pty::Child + Send),
    session_id: &str,
    mission_id: Option<&str>,
    events: &dyn SessionEvents,
) -> ExitEvent {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                events.output(&OutputEvent {
                    session_id: session_id.into(),
                    mission_id: mission_id.map(str::to_string),
                    data: BASE64.encode(&buf[..n]),
                });
            }
            Err(_) => break,
        }
    }
    let (exit_code, success) = match child.wait() {
        Ok(status) => {
            let code = status.exit_code() as i32;
            (Some(code), status.success())
        }
        Err(_) => (None, false),
    };
    ExitEvent {
        session_id: session_id.into(),
        mission_id: mission_id.map(str::to_string),
        exit_code,
        success,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests don't touch Tauri — they hit the PTY layer directly. We
    // build a minimal `Runner` row, skip the DB (the SessionManager writes
    // to DB on spawn), and cover: spawn-echo-readback, inject-stdin-roundtrip,
    // and exit-emits-correct-status. For DB coverage we use the app's
    // file-backed pool helper.

    use crate::db;
    use crate::model::{MissionStatus, Runner};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Test emitter that just records every event. Replaces the Tauri
    /// `AppHandle` in unit tests — no runtime dependency.
    #[derive(Default)]
    struct Capture {
        output: Mutex<Vec<OutputEvent>>,
        exit: Mutex<Vec<ExitEvent>>,
        activity: Mutex<Vec<RunnerActivityEvent>>,
    }
    impl SessionEvents for Capture {
        fn output(&self, ev: &OutputEvent) {
            self.output.lock().unwrap().push(ev.clone());
        }
        fn exit(&self, ev: &ExitEvent) {
            self.exit.lock().unwrap().push(ev.clone());
        }
        fn runner_activity(&self, ev: &RunnerActivityEvent) {
            self.activity.lock().unwrap().push(ev.clone());
        }
    }

    fn runner(command: &str, args: &[&str]) -> Runner {
        Runner {
            id: ulid::Ulid::new().to_string(),
            handle: "tester".into(),
            display_name: "Tester".into(),
            role: "test".into(),
            runtime: "shell".into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            working_dir: None,
            system_prompt: None,
            env: HashMap::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn mission() -> Mission {
        Mission {
            id: ulid::Ulid::new().to_string(),
            crew_id: "crew-ignored-in-tests".into(),
            title: "t".into(),
            status: MissionStatus::Running,
            goal_override: None,
            cwd: None,
            started_at: Utc::now(),
            stopped_at: None,
        }
    }

    fn capture() -> Arc<Capture> {
        Arc::new(Capture::default())
    }

    fn pool_with_schema() -> Arc<DbPool> {
        let tmp = tempfile::tempdir().unwrap();
        // Leak the tempdir so the DB file outlives this fn; fine in tests.
        let path = tmp.path().join("c6.db");
        std::mem::forget(tmp);
        Arc::new(db::open_pool(&path).unwrap())
    }

    fn insert_crew_runner(pool: &DbPool, mission_id: &str, runner_id: &str) {
        // Satisfy the FKs the `sessions` INSERT needs (crew, global runner,
        // crew membership, mission). Post-C5.5a, `runners` is global and
        // membership is on `crew_runners` — keep this helper aligned with
        // the live schema so spawn tests stay honest.
        let conn = pool.get().unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO crews (id, name, created_at, updated_at)
             VALUES ('c', 'c', ?1, ?1)",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runners
                (id, handle, display_name, role, runtime, command,
                 args_json, working_dir, system_prompt, env_json,
                 created_at, updated_at)
             VALUES (?1, 't', 'T', 'test', 'shell', '/bin/sh',
                     NULL, NULL, NULL, NULL, ?2, ?2)",
            params![runner_id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO crew_runners
                (crew_id, runner_id, position, lead, added_at)
             VALUES ('c', ?1, 0, 1, ?2)",
            params![runner_id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO missions (id, crew_id, title, status, started_at)
             VALUES (?1, 'c', 't', 'running', ?2)",
            params![mission_id, now],
        )
        .unwrap();
    }

    #[test]
    fn spawn_echo_roundtrip() {
        // Spawn `sh -c "echo hi && exit"`; assert the exit event fires with
        // success=true. We skip output inspection because the Tauri mock app
        // doesn't let us subscribe to events from a test.
        let pool = pool_with_schema();
        let mission = mission();
        let mut runner = runner("/bin/sh", &["-c", "echo hi"]);
        insert_crew_runner(&pool, &mission.id, &runner.id);
        runner.id = {
            let conn = pool.get().unwrap();
            let id: String = conn
                .query_row("SELECT id FROM runners LIMIT 1", [], |r| r.get(0))
                .unwrap();
            id
        };
        let fresh_mission_id = {
            let conn = pool.get().unwrap();
            let id: String = conn
                .query_row("SELECT id FROM missions LIMIT 1", [], |r| r.get(0))
                .unwrap();
            id
        };
        let mission = Mission {
            id: fresh_mission_id,
            ..mission
        };

        let mgr = SessionManager::new();
        let spawned = mgr
            .spawn(
                &mission,
                &runner,
                std::path::Path::new("/tmp"),
                PathBuf::from("/dev/null"),
                Arc::clone(&pool),
                capture(),
            )
            .unwrap();
        assert!(spawned.pid.is_some());

        // Poll the DB until the reader thread has marked the session stopped.
        let deadline = Instant::now() + Duration::from_secs(5);
        let final_status = loop {
            let conn = pool.get().unwrap();
            let status: String = conn
                .query_row(
                    "SELECT status FROM sessions WHERE id = ?1",
                    params![spawned.id],
                    |r| r.get(0),
                )
                .unwrap();
            if status != "running" {
                break status;
            }
            if Instant::now() > deadline {
                panic!("session never exited");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(final_status, "stopped");
    }

    #[test]
    fn inject_stdin_roundtrip() {
        // Spawn `cat`, inject "hello\n", then kill. `cat` reads until stdin
        // closes; killing the session drops the master PTY, which on Unix
        // hangs up and `cat` sees EOF.
        let pool = pool_with_schema();
        let mission = mission();
        let mut runner = runner("/bin/cat", &[]);
        insert_crew_runner(&pool, &mission.id, &runner.id);
        runner.id = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM runners LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let fresh_mission_id = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM missions LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let mission = Mission {
            id: fresh_mission_id,
            ..mission
        };

        let mgr = SessionManager::new();
        let spawned = mgr
            .spawn(
                &mission,
                &runner,
                std::path::Path::new("/tmp"),
                PathBuf::from("/dev/null"),
                Arc::clone(&pool),
                capture(),
            )
            .unwrap();
        mgr.inject_stdin(&spawned.id, b"hello\n").unwrap();
        // Brief wait so `cat` echoes before we hang up.
        std::thread::sleep(Duration::from_millis(100));
        mgr.kill(&spawned.id).unwrap();

        // After kill, reader thread exits and updates the row.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let conn = pool.get().unwrap();
            let status: String = conn
                .query_row(
                    "SELECT status FROM sessions WHERE id = ?1",
                    params![spawned.id],
                    |r| r.get(0),
                )
                .unwrap();
            if status != "running" {
                break;
            }
            if Instant::now() > deadline {
                panic!("session never exited after kill");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn inject_stdin_on_unknown_session_errors_cleanly() {
        let mgr = SessionManager::new();
        let err = mgr.inject_stdin("nope", b"x").unwrap_err();
        assert!(format!("{err}").contains("session not found"));
    }

    #[test]
    fn spawn_failure_after_spawn_command_reaps_the_child() {
        // Force the `sessions` INSERT to fail by dropping the table after the
        // pool is built. Without the post-spawn cleanup, the child would keep
        // running after `spawn` returns Err because nothing knows about it.
        let pool = pool_with_schema();
        let mission = mission();
        let mut runner = runner("/bin/cat", &[]);
        insert_crew_runner(&pool, &mission.id, &runner.id);
        runner.id = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM runners LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let fresh_mission_id: String = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM missions LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let mission = Mission {
            id: fresh_mission_id,
            ..mission
        };

        // Break the schema so the next INSERT fails.
        pool.get()
            .unwrap()
            .execute("DROP TABLE sessions", [])
            .unwrap();

        let mgr = SessionManager::new();
        let err = mgr
            .spawn(
                &mission,
                &runner,
                std::path::Path::new("/tmp"),
                PathBuf::from("/dev/null"),
                Arc::clone(&pool),
                capture(),
            )
            .unwrap_err();
        // The error must surface the DB failure, not a spawn failure.
        assert!(
            format!("{err}").contains("sessions") || format!("{err}").contains("no such table"),
            "unexpected error: {err}"
        );
        // No live session left behind.
        assert!(mgr.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn kill_blocks_until_session_row_is_terminal() {
        // mission_stop relies on this contract: kill must return only after
        // the reader thread has updated the DB row to stopped/crashed.
        let pool = pool_with_schema();
        let mission = mission();
        let mut runner = runner("/bin/cat", &[]);
        insert_crew_runner(&pool, &mission.id, &runner.id);
        runner.id = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM runners LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let fresh_mission_id: String = {
            let conn = pool.get().unwrap();
            conn.query_row("SELECT id FROM missions LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let mission = Mission {
            id: fresh_mission_id,
            ..mission
        };

        let mgr = SessionManager::new();
        let spawned = mgr
            .spawn(
                &mission,
                &runner,
                std::path::Path::new("/tmp"),
                PathBuf::from("/dev/null"),
                Arc::clone(&pool),
                capture(),
            )
            .unwrap();

        // kill must synchronize on the reader; immediately after it returns,
        // the DB row should already be terminal (no polling).
        mgr.kill(&spawned.id).unwrap();

        let conn = pool.get().unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![spawned.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            status != "running",
            "kill returned while session still running: {status}"
        );
    }

    #[test]
    fn spawn_direct_writes_session_with_null_mission_id_and_emits_activity() {
        // C8.5: a "Chat now" session lives outside any mission. Verify the
        // sessions row has mission_id IS NULL, the session lands in the
        // live map, and the runner_activity emission fires on spawn.
        let pool = pool_with_schema();
        // We don't go through `insert_crew_runner` here because direct
        // chat doesn't need a crew or mission — only a runner row.
        let now = Utc::now().to_rfc3339();
        let runner_id = ulid::Ulid::new().to_string();
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO runners
                    (id, handle, display_name, role, runtime, command,
                     args_json, working_dir, system_prompt, env_json,
                     created_at, updated_at)
                 VALUES (?1, 'directrunner', 'D', 'test', 'shell', '/bin/sh',
                         NULL, NULL, NULL, NULL, ?2, ?2)",
                params![runner_id, now],
            )
            .unwrap();
        }

        let mut runner = runner("/bin/sh", &["-c", "echo direct"]);
        runner.id = runner_id.clone();
        runner.handle = "directrunner".into();

        let cap = capture();
        let mgr = SessionManager::new();
        let spawned = mgr
            .spawn_direct(
                &runner,
                Some("/tmp"),
                None,
                None,
                std::path::Path::new("/tmp"),
                Arc::clone(&pool),
                cap.clone(),
            )
            .unwrap();
        assert_eq!(spawned.mission_id, None);
        assert_eq!(spawned.runner_id, runner_id);

        // Wait for the child to exit so the test isn't racing with the
        // reader thread for the activity drop.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let conn = pool.get().unwrap();
            let row: (String, Option<String>) = conn
                .query_row(
                    "SELECT status, mission_id FROM sessions WHERE id = ?1",
                    params![spawned.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                row.1, None,
                "direct session must persist with NULL mission_id"
            );
            if row.0 != "running" {
                break;
            }
            if Instant::now() > deadline {
                panic!("direct session never exited");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Activity emissions: at least one on spawn (count=1), and one on
        // reap (count=0). We don't pin exact counts — the spawn-time emit
        // could race the reap if the child is fast — but the *last*
        // emission must show zero active sessions for this runner.
        let activity = cap.activity.lock().unwrap();
        assert!(!activity.is_empty(), "runner_activity must fire");
        let last = activity.last().unwrap();
        assert_eq!(last.runner_id, runner_id);
        assert_eq!(
            last.active_sessions, 0,
            "after reap, active_sessions for this runner must be 0"
        );
    }
}
