// Crew detail — matches design/runners-design.pen frame `CUKjM`.
//
// Layout: top toolbar (back to Crews + inline name field + Save + Start
// mission) above a two-section body (Purpose, Slots). Start mission is
// disabled in C3 — it belongs to C11.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
} from "react";
import { Link, useParams } from "react-router-dom";

import { api } from "../lib/api";
import type { Crew, CrewRunner } from "../lib/types";
import { AddSlotModal } from "../components/AddSlotModal";
import { RunnerEditDrawer } from "../components/RunnerEditDrawer";
import { Button } from "../components/ui/Button";

export default function CrewEditor() {
  const { crewId } = useParams<{ crewId: string }>();
  const [crew, setCrew] = useState<Crew | null>(null);
  const [runners, setRunners] = useState<CrewRunner[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<CrewRunner | null>(null);
  const [nameDraft, setNameDraft] = useState("");
  const [savingName, setSavingName] = useState(false);
  const [reordering, setReordering] = useState(false);
  const reorderInFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (!crewId) return;
    try {
      setError(null);
      const [c, rs] = await Promise.all([
        api.crew.get(crewId),
        api.crew.listRunners(crewId),
      ]);
      setCrew(c);
      setRunners(rs);
      setNameDraft(c.name);
      setLoaded(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [crewId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onSaveName = async () => {
    if (!crew || !crewId) return;
    const next = nameDraft.trim();
    if (!next || next === crew.name) return;
    setSavingName(true);
    try {
      await api.crew.update(crewId, { name: next });
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingName(false);
    }
  };

  const onSetLead = async (runnerId: string) => {
    if (!crewId) return;
    try {
      await api.crew.setLead(crewId, runnerId);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const onRemoveFromCrew = async (r: CrewRunner) => {
    if (!crewId) return;
    const tail = r.lead
      ? "\nAs the LEAD, leadership will pass to the next runner by position."
      : "";
    if (!confirm(`Remove @${r.handle} from this crew?${tail}`)) return;
    try {
      await api.crew.removeRunner(crewId, r.id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const onCommitReorder = async (newOrder: CrewRunner[]) => {
    if (!crewId) return;
    if (reorderInFlight.current) return;
    reorderInFlight.current = true;
    setReordering(true);
    setRunners(newOrder);
    try {
      const updated = await api.crew.reorder(
        crewId,
        newOrder.map((r) => r.id),
      );
      setRunners(updated);
    } catch (e) {
      setError(String(e));
      await refresh();
    } finally {
      reorderInFlight.current = false;
      setReordering(false);
    }
  };

  if (!crewId) {
    return <div className="p-8 text-sm text-danger">Missing crew id.</div>;
  }

  const nameDirty =
    crew !== null && nameDraft.trim() !== crew.name && nameDraft.trim().length > 0;

  return (
    <>
      <div className="flex items-center justify-between gap-4 border-b border-line bg-panel px-8 pb-4 pt-9">
        <div className="flex min-w-0 flex-1 items-center gap-3">
          <Link
            to="/crews"
            className="shrink-0 text-sm text-fg-2 transition-colors hover:text-fg"
          >
            ‹ Crews
          </Link>
          <span className="text-line-strong">›</span>
          {crew ? (
            <input
              value={nameDraft}
              onChange={(e) => setNameDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void onSaveName();
                }
                if (e.key === "Escape") {
                  setNameDraft(crew.name);
                  (e.target as HTMLInputElement).blur();
                }
              }}
              className="min-w-0 max-w-sm rounded border border-line bg-bg px-2.5 py-1.5 text-sm font-semibold text-fg focus:border-fg-3 focus:outline-none"
            />
          ) : (
            <span className="text-sm text-fg-3">…</span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Button
            onClick={onSaveName}
            disabled={!nameDirty || savingName}
            title={nameDirty ? "Save crew name" : "No changes"}
          >
            {savingName ? "Saving…" : "Save"}
          </Button>
          <Button variant="primary" disabled title="Start Mission arrives in C11">
            Start mission
          </Button>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="p-8 text-sm text-fg-2">Loading…</div>
        ) : !loaded ? (
          <div className="m-8 rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
            {error ?? "Failed to load crew."}
          </div>
        ) : crew === null ? (
          <div className="p-8 text-sm text-danger">Crew not found.</div>
        ) : (
          <div className="mx-auto flex max-w-4xl flex-col gap-8 px-8 py-8">
            {error ? (
              <div className="rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
                {error}
              </div>
            ) : null}

            <section className="flex flex-col gap-1.5">
              <div className="text-[10px] font-semibold uppercase tracking-[0.15em] text-fg-3">
                Purpose
              </div>
              {crew.purpose ? (
                <p className="text-sm text-fg">{crew.purpose}</p>
              ) : (
                <p className="text-sm italic text-fg-3">No purpose set.</p>
              )}
            </section>

            <section className="flex flex-col gap-4">
              <div className="flex items-end justify-between gap-4">
                <div className="flex flex-col gap-0.5">
                  <h2 className="text-xl font-bold text-fg">Slots</h2>
                  <p className="text-xs text-fg-2">
                    Positions in the crew. Each slot binds a handle to a runner.
                    The{" "}
                    <span className="font-semibold text-accent">LEAD</span> is the
                    crew's face — receives human messages by default and dispatches
                    back to other slots.
                  </p>
                </div>
                <Button variant="primary" onClick={() => setAdding(true)}>
                  + Add slot
                </Button>
              </div>

              <RunnerList
                runners={runners}
                reordering={reordering}
                onSetLead={onSetLead}
                onEdit={(r) => setEditing(r)}
                onRemove={onRemoveFromCrew}
                onReorder={onCommitReorder}
              />
            </section>
          </div>
        )}
      </div>

      <AddSlotModal
        open={adding}
        crewId={crewId}
        isFirstRunner={runners.length === 0}
        onClose={() => setAdding(false)}
        onCreated={async () => {
          setAdding(false);
          await refresh();
        }}
      />

      <RunnerEditDrawer
        open={editing !== null}
        runner={editing}
        onClose={() => setEditing(null)}
        onSaved={async () => {
          setEditing(null);
          await refresh();
        }}
      />
    </>
  );
}

function RunnerList({
  runners,
  reordering,
  onSetLead,
  onEdit,
  onRemove,
  onReorder,
}: {
  runners: CrewRunner[];
  reordering: boolean;
  onSetLead: (id: string) => void;
  onEdit: (r: CrewRunner) => void;
  onRemove: (r: CrewRunner) => void;
  onReorder: (newOrder: CrewRunner[]) => void;
}) {
  if (runners.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-line-strong bg-panel/40 px-5 py-8 text-center">
        <p className="text-sm text-fg">No slots yet.</p>
        <p className="mt-1 text-xs text-fg-3">
          Use <span className="font-medium text-fg">+ Add slot</span> above —
          the first runner auto-assigns as LEAD.
        </p>
      </div>
    );
  }
  return (
    <ol className="flex flex-col gap-2">
      {runners.map((r, i) => (
        <RunnerRow
          key={r.id}
          runner={r}
          index={i}
          total={runners.length}
          dragDisabled={reordering}
          onSetLead={() => onSetLead(r.id)}
          onEdit={() => onEdit(r)}
          onRemove={() => onRemove(r)}
          onReorderDrop={(fromIndex) => {
            if (fromIndex === i) return;
            const next = moveItem(runners, fromIndex, i);
            onReorder(next);
          }}
        />
      ))}
    </ol>
  );
}

function moveItem<T>(arr: T[], from: number, to: number): T[] {
  const copy = arr.slice();
  const [item] = copy.splice(from, 1);
  copy.splice(to, 0, item);
  return copy;
}

function RunnerRow({
  runner,
  index,
  total,
  dragDisabled,
  onSetLead,
  onEdit,
  onRemove,
  onReorderDrop,
}: {
  runner: CrewRunner;
  index: number;
  total: number;
  dragDisabled: boolean;
  onSetLead: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onReorderDrop: (fromIndex: number) => void;
}) {
  const [dragOver, setDragOver] = useState(false);
  const draggable = total > 1 && !dragDisabled;

  const onDragStart = (e: DragEvent<HTMLLIElement>) => {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", String(index));
  };
  const onDragOver = (e: DragEvent<HTMLLIElement>) => {
    if (dragDisabled) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    setDragOver(true);
  };
  const onDragLeave = () => setDragOver(false);
  const onDrop = (e: DragEvent<HTMLLIElement>) => {
    if (dragDisabled) return;
    e.preventDefault();
    setDragOver(false);
    const from = Number(e.dataTransfer.getData("text/plain"));
    if (!Number.isNaN(from)) onReorderDrop(from);
  };

  const summary = useMemo(() => {
    const parts = [runner.command, ...runner.args];
    return parts.filter(Boolean).join(" ");
  }, [runner.command, runner.args]);

  return (
    <li
      draggable={draggable}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
      className={`group flex items-center gap-4 rounded-lg border bg-panel p-4 transition-colors ${
        dragOver
          ? "border-accent/50 bg-accent/5"
          : "border-line hover:border-line-strong"
      }`}
    >
      <div
        className={`flex shrink-0 select-none items-center text-[14px] leading-none text-fg-3 ${
          draggable ? "cursor-grab" : "opacity-40"
        }`}
        title={draggable ? "Drag to reorder" : undefined}
      >
        ⋮⋮
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-[13px] font-medium text-fg">
            @{runner.handle}
          </span>
          {runner.lead ? (
            <span className="rounded bg-accent/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-accent">
              Lead
            </span>
          ) : null}
          <span className="rounded bg-raised px-1.5 py-0.5 text-[10px] font-medium text-fg-2">
            {runner.runtime}
          </span>
          {runner.role ? (
            <span className="text-xs text-fg-2">{runner.role}</span>
          ) : null}
        </div>
        {runner.system_prompt ? (
          <div className="mt-1 line-clamp-1 text-xs text-fg-2">
            {runner.system_prompt}
          </div>
        ) : null}
        {summary ? (
          <div className="mt-1 truncate font-mono text-[11px] text-fg-3">
            $ {summary}
          </div>
        ) : null}
      </div>

      <div className="flex shrink-0 items-center gap-3 text-xs opacity-60 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
        {runner.lead ? (
          <span title="Already the crew's lead" className="text-fg-3">
            Lead
          </span>
        ) : (
          <button
            type="button"
            onClick={onSetLead}
            className="text-fg-2 transition-colors hover:text-fg"
          >
            Set as lead
          </button>
        )}
        <button
          type="button"
          onClick={onEdit}
          className="text-accent transition-colors hover:underline"
        >
          Prompt
        </button>
        <button
          type="button"
          onClick={onRemove}
          className="text-danger transition-colors hover:underline"
        >
          Remove
        </button>
      </div>
    </li>
  );
}
