// Direct-chat pane (C8.5) — `/runners/:handle/chat`.
//
// One-on-one PTY between the human and the runner's CLI. No mission, no
// orchestrator, no event bus. The chat page spawns the session itself
// (rather than the Runner Detail page doing it before navigating) so the
// event listener attaches BEFORE the PTY's reader thread starts emitting
// — without that ordering, fast-exit runners or startup failures can
// finish before the listener exists, leaving the pane stuck at
// "running" with no output.
//
// Uses xterm.js so real TUIs (claude-code, codex) render correctly with
// ANSI colors, cursor movement, and mouse tracking. A plain `<pre>`
// can't interpret the control sequences these agents emit.

import { useEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router-dom";

import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

import { api } from "../lib/api";
import {
  clearActiveSession,
  setActiveSession,
} from "../lib/activeSessions";
import type { SessionStatus } from "../lib/types";

interface OutputEvent {
  session_id: string;
  mission_id: string | null;
  data: string;
}

interface ExitEvent {
  session_id: string;
  mission_id: string | null;
  exit_code: number | null;
  success: boolean;
}

// Two ways to land on the chat pane:
//   - "spawn" mode: come from the runner detail's `Chat now` button.
//     Carry `runnerId` (+ optional cwd) and let RunnerChat call
//     session_start_direct on mount.
//   - "attach" mode: come from the sidebar's SESSION list, which
//     already knows about a live session for this runner. Carry
//     `sessionId` and skip the spawn — re-subscribe to the existing
//     session's output stream instead.
interface RunnerChatLocationState {
  runnerId?: string;
  cwd?: string | null;
  sessionId?: string;
}

const TERMINAL_THEME = {
  // Carbon & Plasma palette. Background matches the page bg so the
  // xterm canvas blends with the chrome instead of looking like a
  // pasted-in box.
  background: "#0E0E10",
  foreground: "#EDEDF0",
  cursor: "#00FF9C",
  cursorAccent: "#0E0E10",
  selectionBackground: "#1F2127",
  black: "#0E0E10",
  red: "#FF4D6D",
  green: "#00FF9C",
  yellow: "#FFB020",
  blue: "#39E5FF",
  magenta: "#C792EA",
  cyan: "#39E5FF",
  white: "#EDEDF0",
  brightBlack: "#5A5C66",
  brightRed: "#FF7B8E",
  brightGreen: "#5FFFB8",
  brightYellow: "#FFCB6B",
  brightBlue: "#82AAFF",
  brightMagenta: "#C792EA",
  brightCyan: "#89DDFF",
  brightWhite: "#FFFFFF",
};

function decodeBase64Chunk(data: string): Uint8Array {
  const raw = atob(data);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes;
}

export default function RunnerChat() {
  const { handle } = useParams<{ handle: string }>();
  const location = useLocation();
  const navigate = useNavigate();
  const state = location.state as RunnerChatLocationState | null;

  const [sessionId, setSessionId] = useState<string | null>(null);
  const [status, setStatus] = useState<SessionStatus>("running");
  const [exitCode, setExitCode] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  // Set by `End chat` so the exit handler can distinguish a user-
  // initiated kill (we want it to read as "stopped") from an actual
  // crash. Without this, every End chat lands on status="crashed"
  // because SIGKILL bubbles up as a non-zero exit.
  const userEndedRef = useRef(false);
  // Pre-spawn buffer: the listener attaches before we have a session
  // id, but the PTY's reader thread can emit between `spawn_direct`
  // returning and our promise resolving. Anything that arrives in that
  // window goes here and is replayed once we know our id.
  const preSpawnBuffer = useRef<{
    outputs: OutputEvent[];
    exits: ExitEvent[];
  }>({ outputs: [], exits: [] });
  // Guard against React strict-mode double-mount re-spawning the PTY.
  const startedRef = useRef(false);

  // Mount xterm once.
  useEffect(() => {
    if (!containerRef.current) return;
    const term = new Terminal({
      cols: 80,
      rows: 24,
      theme: TERMINAL_THEME,
      // System monospace stack. Menlo ships with macOS and carries full
      // Unicode box-drawing + braille ranges, so claude-code's dividers
      // and spinner glyphs render without fallback to a proportional
      // font (which would blow out cell metrics and misalign rows).
      fontFamily:
        'Menlo, "SF Mono", Monaco, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerRef.current);
    // WebGL renderer mounts after `open` — it needs the DOM context.
    // The default canvas renderer in xterm v6 has a known cursor-row
    // misalignment when the host font's reported metrics disagree with
    // its measured cell box; WebGL bypasses that path entirely.
    try {
      const webgl = new WebglAddon();
      term.loadAddon(webgl);
    } catch {
      // No WebGL context (rare in Tauri's webview, but fall through to
      // canvas if so).
    }
    fit.fit();
    term.focus();

    // Forward keystrokes to the PTY. xterm converts arrow keys, ctrl
    // chords, etc. into the right escape sequences before this fires,
    // so we just pipe the resulting string straight through.
    const onDataDisposable = term.onData((data) => {
      const sid = sessionIdRef.current;
      if (!sid) return;
      void api.session.injectStdin(sid, data).catch((e) => {
        setErr(String(e));
      });
    });

    // Refit on window resize. We push the new grid down to the PTY so
    // claude-code (and friends) re-render at full width instead of
    // staying boxed at the spawn-time 80×24.
    const pushSize = () => {
      const t = termRef.current;
      const sid = sessionIdRef.current;
      if (!t || !sid) return;
      void api.session
        .resize(sid, t.cols, t.rows)
        .catch(() => {
          // ignore — session may have already exited
        });
    };
    const onResize = () => {
      try {
        fit.fit();
        pushSize();
      } catch {
        // ignore — happens during teardown
      }
    };
    window.addEventListener("resize", onResize);

    // Repaint when the window comes back to the foreground. macOS
    // discards the canvas layer's contents while the Tauri window is
    // backgrounded, so on return the xterm grid would otherwise show
    // blank until the next byte of PTY output arrives. `refresh` walks
    // xterm's in-memory buffer and re-renders every visible row.
    const refreshTerm = () => {
      const t = termRef.current;
      if (!t) return;
      try {
        t.refresh(0, t.rows - 1);
      } catch {
        // ignore — happens during teardown
      }
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") refreshTerm();
    };
    window.addEventListener("focus", refreshTerm);
    document.addEventListener("visibilitychange", onVisibility);

    termRef.current = term;
    fitRef.current = fit;

    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("focus", refreshTerm);
      document.removeEventListener("visibilitychange", onVisibility);
      onDataDisposable.dispose();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, []);

  // Subscribe + spawn.
  useEffect(() => {
    let unlistenOutput: (() => void) | null = null;
    let unlistenExit: (() => void) | null = null;
    let cancelled = false;

    const consumeOutput = (ev: OutputEvent) => {
      termRef.current?.write(decodeBase64Chunk(ev.data));
    };
    const consumeExit = (ev: ExitEvent) => {
      setStatus(ev.success || userEndedRef.current ? "stopped" : "crashed");
      setExitCode(ev.exit_code);
      if (handle) clearActiveSession(handle);
    };

    const attach = (id: string) => {
      sessionIdRef.current = id;
      setSessionId(id);
      void api.session.injectStdin(id, "").catch((e: unknown) => {
        const msg = String(e);
        if (msg.toLowerCase().includes("session not found")) {
          setStatus("stopped");
          if (handle) clearActiveSession(handle);
        } else {
          setErr(msg);
        }
      });
      // Re-attach lands on a fresh xterm with no scrollback. SIGWINCH
      // dance (one col narrower, then back) makes claude-code redraw
      // its live state onto the blank grid. Without this, the pane
      // stays empty until the user types and forces an emit.
      const t = termRef.current;
      if (t) {
        const cols = t.cols;
        const rows = t.rows;
        void api.session
          .resize(id, Math.max(2, cols - 1), rows)
          .then(() => api.session.resize(id, cols, rows))
          .catch(() => {});
      }
      for (const ev of preSpawnBuffer.current.outputs) {
        if (ev.session_id === id) consumeOutput(ev);
      }
      for (const ev of preSpawnBuffer.current.exits) {
        if (ev.session_id === id) consumeExit(ev);
      }
      preSpawnBuffer.current = { outputs: [], exits: [] };
    };

    // Critical ordering: register both event listeners BEFORE any
    // spawn/attach call. Tauri's `listen()` is async — if we kick off
    // `session_start_direct` first, the child can emit its first output
    // (or exit, for fast-fail runners) before the listener exists, and
    // the pane stays stuck on "starting…" with the bytes on the floor.
    void (async () => {
      const [fnOut, fnExit] = await Promise.all([
        listen<OutputEvent>("session/output", (event) => {
          const sid = sessionIdRef.current;
          if (sid === null) {
            preSpawnBuffer.current.outputs.push(event.payload);
            return;
          }
          if (event.payload.session_id !== sid) return;
          consumeOutput(event.payload);
        }),
        listen<ExitEvent>("session/exit", (event) => {
          const sid = sessionIdRef.current;
          if (sid === null) {
            preSpawnBuffer.current.exits.push(event.payload);
            return;
          }
          if (event.payload.session_id !== sid) return;
          consumeExit(event.payload);
        }),
      ]);
      if (cancelled) {
        fnOut();
        fnExit();
        return;
      }
      unlistenOutput = fnOut;
      unlistenExit = fnExit;

      if (startedRef.current) return;
      startedRef.current = true;

      // Attach mode — caller already knows the session id (sidebar
      // re-entry).
      if (state?.sessionId) {
        attach(state.sessionId);
        return;
      }

      // Spawn mode — first entry from the runner detail page.
      if (state?.runnerId) {
        const runnerId = state.runnerId;
        const initTerm = termRef.current;
        try {
          const spawned = await api.session.startDirect(
            runnerId,
            state.cwd ?? null,
            initTerm?.cols ?? null,
            initTerm?.rows ?? null,
          );
          if (cancelled) return;
          sessionIdRef.current = spawned.id;
          setSessionId(spawned.id);
          if (handle) setActiveSession(handle, spawned.id);
          const t = termRef.current;
          if (t) {
            void api.session
              .resize(spawned.id, t.cols, t.rows)
              .catch(() => {});
          }
          for (const ev of preSpawnBuffer.current.outputs) {
            if (ev.session_id === spawned.id) consumeOutput(ev);
          }
          for (const ev of preSpawnBuffer.current.exits) {
            if (ev.session_id === spawned.id) consumeExit(ev);
          }
          preSpawnBuffer.current = { outputs: [], exits: [] };
        } catch (e) {
          setErr(String(e));
        }
        return;
      }

      // No location.state — typical after a window reload while on the
      // chat route. Look up the runner's live direct-chat session id
      // from the backend (the same field the sidebar consumes from
      // `runner/activity`) and re-attach.
      if (!handle) {
        setErr(
          "Direct chat must be opened from the runner detail page or the sidebar.",
        );
        return;
      }
      try {
        const runner = await api.runner.getByHandle(handle);
        if (cancelled) return;
        const activity = await api.runner.activity(runner.id);
        if (cancelled) return;
        if (activity.direct_session_id) {
          attach(activity.direct_session_id);
        } else {
          setErr(
            "No live direct-chat session for this runner. Start one from the runner detail page.",
          );
        }
      } catch (e) {
        setErr(String(e));
      }
    })();

    return () => {
      cancelled = true;
      unlistenOutput?.();
      unlistenExit?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state?.runnerId, state?.cwd, state?.sessionId]);

  async function endChat() {
    if (!sessionId) return;
    userEndedRef.current = true;
    try {
      await api.session.kill(sessionId);
    } catch (e) {
      setErr(String(e));
    }
  }

  const statusColor =
    status === "running"
      ? "text-accent"
      : status === "crashed"
        ? "text-danger"
        : "text-fg-2";

  return (
    <div className="flex h-full flex-1 flex-col bg-bg">
        <header className="flex items-center justify-between gap-4 border-b border-line bg-panel px-8 pb-4 pt-9">
          <div className="flex items-baseline gap-2 text-sm text-fg-2">
            <Link to="/runners" className="hover:text-fg">
              Runners
            </Link>
            <span className="text-line-strong">›</span>
            <Link to={`/runners/${handle}`} className="hover:text-fg">
              @{handle}
            </Link>
            <span className="text-line-strong">›</span>
            <span className="text-fg">direct chat</span>
            <span className="ml-2 text-[11px]">
              {sessionId ? (
                <>
                  <span className="text-fg-3">session {sessionId.slice(-6)} · </span>
                  <span className={statusColor}>{status}</span>
                </>
              ) : (
                <span className="text-fg-3">starting…</span>
              )}
              {exitCode != null ? (
                <span className="text-fg-3"> · exit {exitCode}</span>
              ) : null}
            </span>
          </div>
          <div className="flex gap-2">
            {status === "running" && sessionId ? (
              <button
                onClick={() => void endChat()}
                className="cursor-pointer rounded border border-line-strong bg-raised px-3 py-1.5 text-xs font-semibold text-fg hover:border-fg-3"
              >
                End chat
              </button>
            ) : (
              <button
                onClick={() => navigate(`/runners/${handle}`)}
                className="cursor-pointer rounded border border-line-strong bg-raised px-3 py-1.5 text-xs font-semibold text-fg hover:border-fg-3"
              >
                Back to runner
              </button>
            )}
          </div>
        </header>

        {err ? (
          <div className="mx-8 mt-4 rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
            {err}
          </div>
        ) : null}

        {/* Terminal pane fills the remaining height. xterm renders into
            this div; we don't put any other content inside. */}
        <div className="flex-1 overflow-hidden p-4">
          <div ref={containerRef} className="h-full w-full" />
        </div>
    </div>
  );
}
