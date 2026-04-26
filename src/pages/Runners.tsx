// Runners list — top-level surface for the runner config (C8.5).
//
// Mirrors design frame `2Oecf`: dark cards with @handle + runtime badge,
// a green "Chat" pill action, the runner's brief, the command preview,
// and live activity counters. The 3-dot menu offers Edit details /
// Duplicate / Delete runner (Duplicate is a v0.x stub for now — kept off
// the menu until the backend supports it).

import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { listen } from "@tauri-apps/api/event";

import { api } from "../lib/api";
import type { RunnerActivityEvent, RunnerWithActivity } from "../lib/types";
// AppShell is supplied by the layout route in App.tsx; pages render
// their content as-is and the shell wraps them automatically.
import { Button } from "../components/ui/Button";
import { CreateRunnerModal } from "../components/CreateRunnerModal";
import { EmptyStateCard } from "../components/EmptyStateCard";

export default function Runners() {
  const [runners, setRunners] = useState<RunnerWithActivity[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const navigate = useNavigate();

  const refresh = useCallback(async () => {
    try {
      setError(null);
      const list = await api.runner.listWithActivity();
      setRunners(list);
      setLoaded(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<RunnerActivityEvent>("runner/activity", (event) => {
      const ev = event.payload;
      setRunners((prev) =>
        prev.map((r) =>
          r.id === ev.runner_id
            ? {
                ...r,
                active_sessions: ev.active_sessions,
                active_missions: ev.active_missions,
                crew_count: ev.crew_count,
              }
            : r,
        ),
      );
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  const onChat = (handle: string) => navigate(`/runners/${handle}`);

  const onDelete = async (id: string, handle: string) => {
    if (
      !confirm(
        `Delete runner @${handle}? This removes it from every crew it's in, kills any live chats, and erases its session history. Crews and missions are kept.`,
      )
    )
      return;
    try {
      await api.runner.delete(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <>
      <div className="flex flex-1 flex-col overflow-y-auto">
        <div className="flex w-full flex-1 flex-col gap-6 px-8 py-8">
          <header className="flex items-center justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h1 className="text-2xl font-bold tracking-tight text-fg">
                Runners
              </h1>
              <p className="text-sm text-fg-2">
                Reusable CLI agents — pick one for a crew slot or chat
                directly.
              </p>
            </div>
            <Button variant="primary" onClick={() => setCreating(true)}>
              + New runner
            </Button>
          </header>

          {error ? (
            <div className="rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              {error}
            </div>
          ) : null}

          {loading ? (
            <div className="text-sm text-fg-2">Loading…</div>
          ) : !loaded ? (
            <div className="rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              Failed to load runners.
            </div>
          ) : runners.length === 0 ? (
            <EmptyStateCard
              icon={<TerminalIcon />}
              title="No runners yet"
              description="A runner is a reusable CLI agent — claude-code, codex, a custom shell — that crews pull in. Add one to start composing crews."
              action={
                <Button variant="primary" onClick={() => setCreating(true)}>
                  + New runner
                </Button>
              }
            />
          ) : (
            <div className="flex flex-col gap-3">
              {runners.map((r) => (
                <RunnerCard
                  key={r.id}
                  item={r}
                  onOpen={() => navigate(`/runners/${r.handle}`)}
                  onChat={() => onChat(r.handle)}
                  onDelete={() => onDelete(r.id, r.handle)}
                />
              ))}
            </div>
          )}
        </div>
      </div>

      <CreateRunnerModal
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={async (created) => {
          setCreating(false);
          await refresh();
          navigate(`/runners/${created.handle}`);
        }}
      />
    </>
  );
}

function TerminalIcon() {
  // 22x22 terminal-prompt glyph, mirrors the empty-state badge in
  // design/runners-design.pen (frame `GmFmi`).
  return (
    <svg
      width="22"
      height="22"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

function RunnerCard({
  item,
  onOpen,
  onChat,
  onDelete,
}: {
  item: RunnerWithActivity;
  onOpen: () => void;
  onChat: () => void;
  onDelete: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenuOpen(false);
    };
    window.addEventListener("mousedown", onDoc);
    return () => window.removeEventListener("mousedown", onDoc);
  }, [menuOpen]);

  const sessionsLabel =
    item.active_sessions === 1 ? "1 session" : `${item.active_sessions} sessions`;
  const missionsLabel =
    item.active_missions === 1 ? "1 mission" : `${item.active_missions} missions`;
  const crewsLabel =
    item.crew_count === 1 ? "in 1 crew" : `in ${item.crew_count} crews`;
  const live = item.active_sessions > 0 || item.active_missions > 0;

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="group relative flex cursor-pointer flex-col gap-2 rounded-lg border border-line bg-panel p-4 transition-colors hover:border-line-strong focus:outline-none focus-visible:border-fg-3"
    >
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="font-mono text-base font-semibold text-fg">
              @{item.handle}
            </span>
            <span className="rounded bg-raised px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-fg-2">
              {item.runtime}
            </span>
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onChat();
              }}
              className="ml-1 inline-flex items-center gap-1 rounded bg-accent/10 px-2 py-0.5 text-[11px] font-semibold text-accent hover:bg-accent/20"
              title="Open runner detail · Chat now"
            >
              <span aria-hidden>💬</span>
              <span>Chat</span>
            </button>
          </div>
          <p className="mt-1 line-clamp-2 text-xs text-fg-2">
            {item.display_name}
            {item.role ? (
              <>
                <span className="text-line-strong"> · </span>
                {item.role}
              </>
            ) : null}
          </p>
          <div className="mt-1.5 truncate font-mono text-[11px] text-fg-3">
            $ {item.command}
            {item.args.length > 0 ? " " + item.args.join(" ") : ""}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-xs text-fg-2">
          {live ? (
            <span className="text-accent">
              {sessionsLabel}
              {item.active_missions > 0 ? ` · ${missionsLabel}` : ""}
            </span>
          ) : (
            <span className="text-fg-3">{crewsLabel}</span>
          )}
          <div className="relative" ref={menuRef}>
            <button
              type="button"
              aria-label={`More actions for @${item.handle}`}
              title="More actions"
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen((v) => !v);
              }}
              className="rounded px-1.5 py-0.5 text-fg-2 hover:bg-raised hover:text-fg"
            >
              ⋯
            </button>
            {menuOpen ? (
              <div
                className="absolute right-0 top-full z-20 mt-1 flex w-44 flex-col overflow-hidden rounded border border-line-strong bg-panel py-1 text-xs shadow-xl"
                onClick={(e) => e.stopPropagation()}
              >
                <button
                  type="button"
                  onClick={() => {
                    setMenuOpen(false);
                    onOpen();
                  }}
                  className="flex w-full items-center px-3 py-1.5 text-left text-fg hover:bg-raised"
                >
                  Edit details
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setMenuOpen(false);
                    onDelete();
                  }}
                  className="flex w-full items-center px-3 py-1.5 text-left text-danger hover:bg-raised"
                >
                  Delete runner
                </button>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}
