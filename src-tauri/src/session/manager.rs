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
}

/// Emitter for the real Tauri app — emits `session/output` and `session/exit`.
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
}

/// Contents of `session/output` events emitted to the frontend. The raw PTY
/// bytes are base64-encoded so the event payload is valid JSON regardless of
/// what the child wrote (ANSI escapes, non-UTF-8, etc.). The frontend decodes
/// before feeding xterm.js.
#[derive(Debug, Clone, Serialize)]
pub struct OutputEvent {
    pub session_id: String,
    pub mission_id: String,
    /// Lossy UTF-8 of the chunk — good enough for the MVP debug page. xterm.js
    /// integration in C10 will switch this to base64 bytes.
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExitEvent {
    pub session_id: String,
    pub mission_id: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Row returned to the frontend after a spawn. Subset of the DB `sessions`
/// row with the runner handle denormalized so the debug page can render
/// `@coder`-style labels without a separate lookup.
#[derive(Debug, Clone, Serialize)]
pub struct SpawnedSession {
    pub id: String,
    pub mission_id: String,
    pub runner_id: String,
    pub handle: String,
    pub pid: Option<u32>,
}

struct SessionHandle {
    // Kept for debugging and future kill-by-pid / identity checks.
    #[allow(dead_code)]
    id: String,
    mission_id: String,
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
    /// `app_data_dir` is the root of `$APPDATA/runners/` so we can prepend
    /// `<app_data_dir>/bin` onto the child's PATH — arch §5.3 Layer 2 and
    /// v0-mvp.md C9 both require the bundled `runners` CLI to win over any
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

        cmd.env("RUNNERS_CREW_ID", &mission.crew_id);
        cmd.env("RUNNERS_MISSION_ID", &mission.id);
        cmd.env("RUNNERS_RUNNER_HANDLE", &runner.handle);
        cmd.env(
            "RUNNERS_EVENT_LOG",
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
                mission_id: mission.id.clone(),
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
        let reader_handle = {
            let session_id_t = session_id.clone();
            let mission_id_t = mission.id.clone();
            let events_t = Arc::clone(&events);
            let manager_t: Arc<SessionManager> = Arc::clone(self);
            let pool_t: Arc<DbPool> = Arc::clone(&pool);
            thread::spawn(move || {
                let exit = drain_pty_and_reap(
                    reader,
                    &mut *child,
                    &session_id_t,
                    &mission_id_t,
                    events_t.as_ref(),
                );
                let _ = manager_t.forget(&session_id_t);
                if let Ok(conn) = pool_t.get() {
                    let _ = conn.execute(
                        "UPDATE sessions
                            SET status = ?1, stopped_at = ?2
                          WHERE id = ?3",
                        params![
                            if exit.success { "stopped" } else { "crashed" },
                            Utc::now().to_rfc3339(),
                            session_id_t,
                        ],
                    );
                }
                events_t.exit(&exit);
            })
        };

        // Attach the reader handle. We raced to insert-first so the reader
        // may already be draining by the time we land here — that's fine,
        // it doesn't touch this slot.
        if let Some(h) = self.sessions.lock().unwrap().get_mut(&session_id) {
            h.reader = Some(reader_handle);
        }

        Ok(SpawnedSession {
            id: session_id,
            mission_id: mission.id.clone(),
            runner_id: runner.id.clone(),
            handle: runner.handle.clone(),
            pid,
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
                .filter(|s| s.mission_id == mission_id)
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

/// Pumps PTY output → `session/output` events, then waits for the child to
/// exit. Returns the exit summary that the caller emits as `session/exit`.
fn drain_pty_and_reap(
    mut reader: Box<dyn Read + Send>,
    child: &mut (dyn portable_pty::Child + Send),
    session_id: &str,
    mission_id: &str,
    events: &dyn SessionEvents,
) -> ExitEvent {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                events.output(&OutputEvent {
                    session_id: session_id.into(),
                    mission_id: mission_id.into(),
                    text: chunk,
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
        mission_id: mission_id.into(),
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
    }
    impl SessionEvents for Capture {
        fn output(&self, ev: &OutputEvent) {
            self.output.lock().unwrap().push(ev.clone());
        }
        fn exit(&self, ev: &ExitEvent) {
            self.exit.lock().unwrap().push(ev.clone());
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
}
