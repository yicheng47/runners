// Runners list — top-level surface for the runner config (C8.5).
//
// Mirrors the design's `2Oecf` frame. Layout vocabulary matches Crews.tsx:
// vertical stack of cards, dashed empty-state at the bottom, primary
// "+ New runner" CTA in the page header.
//
// Activity counters ("3 sessions · 1 mission") are seeded from
// `runner_list_with_activity` on mount and patched live by subscribing to
// the `runner/activity` Tauri event the SessionManager emits on every
// spawn / reap. No polling; the page reconciles on focus by refetching.

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { listen } from "@tauri-apps/api/event";

import { api } from "../lib/api";
import type { RunnerActivityEvent, RunnerWithActivity } from "../lib/types";
import { AppShell } from "../components/AppShell";
import { Button } from "../components/ui/Button";
import { CreateRunnerModal } from "../components/CreateRunnerModal";

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

  // Live activity. We patch the matching row in place rather than
  // refetching the whole list so the UI doesn't flicker on every spawn.
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

  const onDelete = async (id: string, handle: string) => {
    if (
      !confirm(
        `Delete runner @${handle}? This removes it from every crew it's in. Sessions stay in history.`,
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
    <AppShell>
      <div className="flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-4xl flex-col gap-6 px-8 py-8">
          <header className="flex items-center justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h1 className="text-2xl font-bold tracking-tight text-neutral-900">
                Runners
              </h1>
              <p className="text-sm text-neutral-500">
                Reusable CLI agents — pick one for a crew slot or chat
                directly.
              </p>
            </div>
            <Button variant="primary" onClick={() => setCreating(true)}>
              + New runner
            </Button>
          </header>

          {error ? (
            <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
              {error}
            </div>
          ) : null}

          {loading ? (
            <div className="text-sm text-neutral-500">Loading…</div>
          ) : !loaded ? (
            <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
              Failed to load runners.
            </div>
          ) : runners.length === 0 ? (
            <div className="rounded-md border border-[#E5E5E5] bg-white px-4 py-6 text-center text-sm text-neutral-500">
              No runners yet. Use{" "}
              <span className="font-medium text-neutral-700">+ New runner</span>{" "}
              above to create one.
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              {runners.map((r) => (
                <RunnerCard
                  key={r.id}
                  item={r}
                  onOpen={() => navigate(`/runners/${r.handle}`)}
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
    </AppShell>
  );
}

function RunnerCard({
  item,
  onOpen,
  onDelete,
}: {
  item: RunnerWithActivity;
  onOpen: () => void;
  onDelete: () => void;
}) {
  const sessionsLabel =
    item.active_sessions === 1 ? "1 session" : `${item.active_sessions} sessions`;
  const missionsLabel =
    item.active_missions === 1 ? "1 mission" : `${item.active_missions} missions`;
  const crewsLabel =
    item.crew_count === 1 ? "in 1 crew" : `in ${item.crew_count} crews`;

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
      className="group flex cursor-pointer flex-col gap-2 rounded-lg border border-[#E5E5E5] bg-white p-5 transition-colors hover:border-neutral-300 focus:outline-none focus-visible:border-neutral-400 focus-visible:ring-2 focus-visible:ring-neutral-300"
    >
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="font-mono text-base font-semibold text-neutral-900">
              @{item.handle}
            </span>
            <span className="rounded bg-neutral-100 px-1.5 py-0.5 text-[11px] font-medium text-neutral-500">
              {item.runtime}
            </span>
          </div>
          <div className="mt-0.5 truncate text-xs text-neutral-500">
            {item.display_name}
            <span className="text-neutral-300"> · </span>
            <span className="text-neutral-500">{item.role}</span>
            <span className="text-neutral-300"> · </span>
            <span className="text-neutral-500">{crewsLabel}</span>
          </div>
          <div className="mt-1 truncate font-mono text-[11px] text-neutral-400">
            $ {item.command}
            {item.args.length > 0 ? " " + item.args.join(" ") : ""}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-3 text-xs text-neutral-500">
          <span className={item.active_sessions > 0 ? "text-emerald-600" : ""}>
            {sessionsLabel}
          </span>
          <span className="text-neutral-300">·</span>
          <span className={item.active_missions > 0 ? "text-emerald-600" : ""}>
            {missionsLabel}
          </span>
          <span className="text-neutral-300">·</span>
          <span className="text-[#0066CC] transition-colors hover:underline">
            Open
          </span>
          <button
            type="button"
            aria-label={`Delete runner @${item.handle}`}
            title="Delete runner"
            onClick={(e) => {
              e.stopPropagation();
              onDelete();
            }}
            className="rounded px-1 py-0.5 text-xs text-neutral-400 opacity-60 transition-colors hover:bg-red-50 hover:text-red-600 focus-visible:opacity-100 group-hover:opacity-100"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  );
}

