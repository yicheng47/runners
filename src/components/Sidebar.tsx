// App sidebar — Carbon & Plasma dark theme. RUNNER nav, search box,
// ACTIVE section listing currently-running runners. The active list is
// fed by the same `runner/activity` Tauri events the Runners list uses,
// projected to "any runner whose active_sessions > 0".

import { useCallback, useEffect, useRef, useState } from "react";
import { NavLink, useLocation, useNavigate } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";

import { api } from "../lib/api";
import {
  clearActiveSession,
  getActiveSession,
  setActiveSession,
} from "../lib/activeSessions";
import type { RunnerActivityEvent, RunnerWithActivity } from "../lib/types";

type NavItem = {
  to: string;
  label: string;
  enabled: boolean;
  hint?: string;
};

const NAV: NavItem[] = [
  { to: "/runners", label: "Runner", enabled: true },
  { to: "/crews", label: "Crew", enabled: true },
  { to: "/missions", label: "Mission", enabled: false, hint: "Coming with C11" },
];

interface ActiveRunner {
  id: string;
  handle: string;
  active_sessions: number;
  active_missions: number;
}

// Resize bounds + persistence, mirroring quill's Sidebar.tsx pattern.
const SIDEBAR_MIN = 200;
const SIDEBAR_MAX = 480;
const SIDEBAR_DEFAULT = 240;
const STORAGE_KEY = "runner.sidebar.width";

function getStoredWidth(): number {
  if (typeof localStorage === "undefined") return SIDEBAR_DEFAULT;
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored) {
    const n = parseInt(stored, 10);
    if (!Number.isNaN(n) && n >= SIDEBAR_MIN && n <= SIDEBAR_MAX) return n;
  }
  return SIDEBAR_DEFAULT;
}

export function Sidebar() {
  const navigate = useNavigate();
  const location = useLocation();
  const [search, setSearch] = useState("");
  const [active, setActive] = useState<ActiveRunner[]>([]);
  const [width, setWidth] = useState<number>(getStoredWidth);
  const resizingRef = useRef(false);

  // Click handler for the SESSION list — re-attach to the live PTY if
  // we know its id (frontend tracks this in lib/activeSessions because
  // the backend doesn't expose a "list running sessions" query yet);
  // otherwise fall back to the runner detail page.
  //
  // If we're already on this chat route, no-op: re-navigating with new
  // location.state would tear down the chat page's output/exit
  // listeners (effect deps include state.sessionId) without re-spawning
  // anything, leaving the pane blank.
  const openSession = useCallback(
    (handle: string) => {
      const target = `/runners/${handle}/chat`;
      if (location.pathname === target) return;
      const sessionId = getActiveSession(handle);
      if (sessionId) {
        navigate(target, { state: { sessionId } });
      } else {
        navigate(`/runners/${handle}`);
      }
    },
    [navigate, location.pathname],
  );

  const handleResizeStart = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      resizingRef.current = true;
      const startX = e.clientX;
      const startWidth = width;
      const onMouseMove = (ev: MouseEvent) => {
        const next = Math.min(
          SIDEBAR_MAX,
          Math.max(SIDEBAR_MIN, startWidth + ev.clientX - startX),
        );
        setWidth(next);
      };
      const onMouseUp = () => {
        resizingRef.current = false;
        document.removeEventListener("mousemove", onMouseMove);
        document.removeEventListener("mouseup", onMouseUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        // Persist the final value. Read from React state via setter to
        // get the latest committed width without retriggering a render.
        setWidth((w) => {
          try {
            localStorage.setItem(STORAGE_KEY, String(w));
          } catch {
            // ignore quota / disabled-storage errors
          }
          return w;
        });
      };
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      document.addEventListener("mousemove", onMouseMove);
      document.addEventListener("mouseup", onMouseUp);
    },
    [width],
  );

  // Seed with the current snapshot, then patch from runner/activity events.
  // Also rehydrate the activeSessions handle→sessionId map so post-reload
  // clicks can re-attach to the live PTY instead of falling back to the
  // runner detail page.
  useEffect(() => {
    void api.runner.listWithActivity().then((rows: RunnerWithActivity[]) => {
      setActive(
        rows
          .filter((r) => r.active_sessions > 0)
          .map((r) => ({
            id: r.id,
            handle: r.handle,
            active_sessions: r.active_sessions,
            active_missions: r.active_missions,
          })),
      );
      for (const r of rows) {
        if (r.direct_session_id) {
          setActiveSession(r.handle, r.direct_session_id);
        } else {
          clearActiveSession(r.handle);
        }
      }
    });
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<RunnerActivityEvent>("runner/activity", (event) => {
      const ev = event.payload;
      if (ev.direct_session_id) {
        setActiveSession(ev.handle, ev.direct_session_id);
      } else {
        clearActiveSession(ev.handle);
      }
      setActive((prev) => {
        const without = prev.filter((r) => r.id !== ev.runner_id);
        if (ev.active_sessions === 0) return without;
        return [
          ...without,
          {
            id: ev.runner_id,
            handle: ev.handle,
            active_sessions: ev.active_sessions,
            active_missions: ev.active_missions,
          },
        ].sort((a, b) => a.handle.localeCompare(b.handle));
      });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  const filtered = search.trim()
    ? active.filter((r) =>
        r.handle.toLowerCase().includes(search.trim().toLowerCase()),
      )
    : active;

  return (
    <aside
      style={{ width }}
      className="relative flex h-full shrink-0 select-none flex-col overflow-hidden border-r border-line bg-raised"
    >
      <div data-tauri-drag-region className="h-7" />

      <div className="flex items-center gap-2 px-5 pb-5 pt-1">
        <BrandMark />
        <span className="text-base font-semibold tracking-tight text-fg">
          Runner
        </span>
      </div>

      <SectionHeader>WORKSPACE</SectionHeader>
      <nav className="flex flex-col gap-0.5 px-3 pb-4">
        {NAV.map((item) =>
          item.enabled ? (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) =>
                `rounded px-2.5 py-1.5 text-sm transition-colors ${
                  isActive
                    ? "font-semibold text-fg"
                    : "text-fg-2 hover:text-fg"
                }`
              }
            >
              {item.label.toLowerCase()}
            </NavLink>
          ) : (
            <span
              key={item.to}
              title={item.hint}
              aria-disabled="true"
              className="cursor-not-allowed rounded px-2.5 py-1.5 text-sm text-fg-3"
            >
              {item.label.toLowerCase()}
            </span>
          ),
        )}
      </nav>

      <SectionHeader>SESSION</SectionHeader>
      <div className="px-3 pb-2">
        <div className="flex items-center gap-2 rounded border border-line bg-bg px-2.5 py-1.5 text-xs">
          <span className="text-fg-3" aria-hidden>
            ⌕
          </span>
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search…"
            className="flex-1 bg-transparent text-fg placeholder:text-fg-3 focus:outline-none"
          />
          <span className="font-mono text-[10px] text-fg-3">⌘K</span>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto px-3 pb-4">
        {filtered.length === 0 ? (
          <p className="px-2.5 py-1 text-xs text-fg-3">
            {search.trim() ? "No matches." : "No live sessions."}
          </p>
        ) : (
          filtered.map((r) => (
            <button
              key={r.id}
              type="button"
              onClick={() => openSession(r.handle)}
              className="flex w-full cursor-pointer items-center gap-2 rounded border border-line bg-bg px-2.5 py-1.5 text-left text-xs text-fg-2 hover:border-line-strong hover:text-fg"
              title={`${r.active_sessions} session${r.active_sessions === 1 ? "" : "s"}${
                r.active_missions > 0
                  ? ` · ${r.active_missions} mission${r.active_missions === 1 ? "" : "s"}`
                  : ""
              }`}
            >
              <span className="inline-flex h-1.5 w-1.5 shrink-0 rounded-full bg-accent" />
              <span className="truncate font-mono">@{r.handle} direct</span>
            </button>
          ))
        )}
      </div>

      {/* Resize handle — 4px hit area on the right edge, 1px visible
          accent bar on hover/drag. Mirrors quill's Sidebar pattern. */}
      <div
        onMouseDown={handleResizeStart}
        title="Drag to resize"
        className="absolute right-0 top-0 z-20 h-full w-1 cursor-col-resize bg-transparent transition-colors hover:bg-accent/40"
      />
    </aside>
  );
}

function BrandMark() {
  // Three stacked lucide `chevron-right` glyphs — main 14×14 centered,
  // two 9×9 ghosts at 40% opacity in the upper-left and lower-left.
  // Mirrors the brand mark in design/runners-design.pen (frame `88D24`).
  return (
    <svg
      width="32"
      height="32"
      viewBox="0 0 32 32"
      aria-hidden
      className="shrink-0"
    >
      <Chevron x={3} y={3} size={9} opacity={0.4} />
      <Chevron x={9} y={9} size={14} opacity={1} />
      <Chevron x={3} y={20} size={9} opacity={0.4} />
    </svg>
  );
}

function Chevron({
  x,
  y,
  size,
  opacity,
}: {
  x: number;
  y: number;
  size: number;
  opacity: number;
}) {
  // Lucide chevron-right path inside a 24×24 viewBox, scaled to `size`
  // via the inner svg.
  return (
    <svg x={x} y={y} width={size} height={size} viewBox="0 0 24 24">
      <polyline
        points="9 18 15 12 9 6"
        fill="none"
        stroke="#00FF9C"
        strokeWidth={2}
        strokeLinecap="round"
        strokeLinejoin="round"
        opacity={opacity}
      />
    </svg>
  );
}

function SectionHeader({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-5 pb-1.5 text-[10px] font-semibold uppercase tracking-[0.15em] text-fg-3">
      {children}
    </div>
  );
}
