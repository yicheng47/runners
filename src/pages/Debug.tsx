// Scratch page for C6 — lets you kick off a mission on any crew and watch
// the raw PTY output stream back from each session. Delete once C10 (mission
// workspace) lands. Intentionally minimal: no styling polish, no xterm.js,
// no scrollback management. Just prove the bytes flow end to end.

import { useEffect, useMemo, useRef, useState } from "react";

import { listen } from "@tauri-apps/api/event";

import type { Crew, Mission, SessionStatus } from "../lib/types";
import { api, SessionRow } from "../lib/api";
import { AppShell } from "../components/AppShell";

interface OutputEvent {
  session_id: string;
  mission_id: string;
  text: string;
}

interface ExitEvent {
  session_id: string;
  mission_id: string;
  exit_code: number | null;
  success: boolean;
}

interface SessionPane {
  row: SessionRow;
  output: string;
  status: SessionStatus;
  exitCode: number | null;
}

export default function Debug() {
  const [crews, setCrews] = useState<Crew[]>([]);
  const [crewId, setCrewId] = useState<string>("");
  const [title, setTitle] = useState("scratch mission");
  const [goal, setGoal] = useState("");
  const [cwd, setCwd] = useState("");
  const [mission, setMission] = useState<Mission | null>(null);
  const [sessions, setSessions] = useState<Record<string, SessionPane>>({});
  const [err, setErr] = useState<string | null>(null);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const missionIdRef = useRef<string | null>(null);

  useEffect(() => {
    void api.crew.list().then((items) => {
      setCrews(items.map((i) => i));
      if (items[0]) setCrewId(items[0].id);
    });
  }, []);

  // Subscribe once on mount; filter events by the current mission id so
  // multiple sequential starts don't bleed old output into a new run.
  useEffect(() => {
    let outputUnlisten: (() => void) | null = null;
    let exitUnlisten: (() => void) | null = null;

    void listen<OutputEvent>("session/output", (event) => {
      if (event.payload.mission_id !== missionIdRef.current) return;
      setSessions((prev) => {
        const pane = prev[event.payload.session_id];
        if (!pane) return prev;
        return {
          ...prev,
          [event.payload.session_id]: {
            ...pane,
            output: (pane.output + event.payload.text).slice(-16_000),
          },
        };
      });
    }).then((fn) => {
      outputUnlisten = fn;
    });

    void listen<ExitEvent>("session/exit", (event) => {
      if (event.payload.mission_id !== missionIdRef.current) return;
      setSessions((prev) => {
        const pane = prev[event.payload.session_id];
        if (!pane) return prev;
        return {
          ...prev,
          [event.payload.session_id]: {
            ...pane,
            status: event.payload.success ? "stopped" : "crashed",
            exitCode: event.payload.exit_code,
          },
        };
      });
    }).then((fn) => {
      exitUnlisten = fn;
    });

    return () => {
      outputUnlisten?.();
      exitUnlisten?.();
    };
  }, []);

  async function start() {
    setErr(null);
    setSessions({});
    try {
      const out = await api.mission.start({
        crew_id: crewId,
        title,
        goal_override: goal || null,
        cwd: cwd || null,
      });
      setMission(out.mission);
      missionIdRef.current = out.mission.id;

      // Give the backend a beat to insert rows + spawn.
      await new Promise((r) => setTimeout(r, 50));
      const rows = await api.session.list(out.mission.id);
      const seeded: Record<string, SessionPane> = {};
      for (const row of rows) {
        seeded[row.id] = {
          row,
          output: "",
          status: row.status,
          exitCode: null,
        };
      }
      setSessions(seeded);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function stopMission() {
    if (!mission) return;
    try {
      const m = await api.mission.stop(mission.id);
      setMission(m);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function inject(sessionId: string) {
    const text = inputs[sessionId];
    if (!text) return;
    try {
      await api.session.injectStdin(sessionId, text + "\n");
      setInputs({ ...inputs, [sessionId]: "" });
    } catch (e) {
      setErr(String(e));
    }
  }

  const paneList = useMemo(() => Object.values(sessions), [sessions]);

  return (
    <AppShell>
      <div className="flex-1 overflow-y-auto bg-neutral-50 p-6 font-mono text-sm text-neutral-900">
        <div className="mx-auto max-w-5xl space-y-4">
          <header>
            <h1 className="text-lg font-semibold">C6 Debug — PTY scratch</h1>
          </header>

        <section className="space-y-2 rounded border border-neutral-300 bg-white p-4">
          <div className="grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-1 text-xs text-neutral-600">
              Crew
              <select
                value={crewId}
                onChange={(e) => setCrewId(e.target.value)}
                className="rounded border border-neutral-300 bg-white p-1 text-sm"
              >
                {crews.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex flex-col gap-1 text-xs text-neutral-600">
              Title
              <input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                className="rounded border border-neutral-300 bg-white p-1 text-sm"
              />
            </label>
            <label className="col-span-2 flex flex-col gap-1 text-xs text-neutral-600">
              Goal (optional — falls back to crew default)
              <input
                value={goal}
                onChange={(e) => setGoal(e.target.value)}
                className="rounded border border-neutral-300 bg-white p-1 text-sm"
              />
            </label>
            <label className="col-span-2 flex flex-col gap-1 text-xs text-neutral-600">
              Working directory (optional — $MISSION_CWD)
              <input
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="/Users/you/projects/foo"
                className="rounded border border-neutral-300 bg-white p-1 text-sm"
              />
            </label>
          </div>
          <div className="flex gap-2">
            <button
              onClick={() => void start()}
              disabled={!crewId || !title}
              className="rounded bg-neutral-900 px-3 py-1.5 text-xs font-semibold text-white disabled:opacity-40"
            >
              Start mission
            </button>
            {mission && mission.status === "running" && (
              <button
                onClick={() => void stopMission()}
                className="rounded border border-neutral-300 bg-white px-3 py-1.5 text-xs font-semibold"
              >
                Stop
              </button>
            )}
            {mission && (
              <span className="ml-auto self-center text-xs text-neutral-500">
                mission {mission.id.slice(-6)} · {mission.status}
              </span>
            )}
          </div>
          {err && (
            <pre className="whitespace-pre-wrap rounded bg-red-50 p-2 text-xs text-red-700">
              {err}
            </pre>
          )}
        </section>

        <section className="space-y-3">
          {paneList.length === 0 && (
            <p className="text-xs text-neutral-500">
              Pick a crew and click <em>Start mission</em> — one pane per runner will appear.
            </p>
          )}
          {paneList.map((pane) => (
            <div key={pane.row.id} className="rounded border border-neutral-300 bg-white">
              <div className="flex items-center justify-between border-b border-neutral-200 bg-neutral-100 px-3 py-1.5 text-xs">
                <span className="font-semibold">@{pane.row.handle}</span>
                <span className="text-neutral-500">
                  pid {pane.row.pid ?? "?"} · {pane.status}
                  {pane.exitCode != null ? ` · exit ${pane.exitCode}` : ""}
                </span>
              </div>
              <pre className="max-h-80 overflow-auto whitespace-pre-wrap bg-neutral-900 p-3 text-xs leading-tight text-neutral-100">
                {pane.output || "(no output yet)"}
              </pre>
              <div className="flex gap-1 border-t border-neutral-200 p-2">
                <input
                  value={inputs[pane.row.id] ?? ""}
                  onChange={(e) =>
                    setInputs({ ...inputs, [pane.row.id]: e.target.value })
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void inject(pane.row.id);
                  }}
                  placeholder="stdin (hit ↵ to inject with newline)"
                  className="flex-1 rounded border border-neutral-300 bg-white p-1 text-xs"
                  disabled={pane.status !== "running"}
                />
                <button
                  onClick={() => void inject(pane.row.id)}
                  disabled={pane.status !== "running" || !inputs[pane.row.id]}
                  className="rounded bg-neutral-900 px-2 py-1 text-xs font-semibold text-white disabled:opacity-40"
                >
                  Send
                </button>
                <button
                  onClick={() => void api.session.kill(pane.row.id)}
                  disabled={pane.status !== "running"}
                  className="rounded border border-neutral-300 bg-white px-2 py-1 text-xs font-semibold disabled:opacity-40"
                >
                  Kill
                </button>
              </div>
            </div>
          ))}
        </section>
        </div>
      </div>
    </AppShell>
  );
}
