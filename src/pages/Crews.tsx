// Crews list — matches design/runners-design.pen frame `nqOot`.
//
// Vertical stack of crew cards (not a grid) plus a trailing dashed
// "+ Add another crew" card that doubles as an empty state. Each crew
// card opens its CrewEditor on click; hovering surfaces an `Edit` affordance
// on the right alongside the runner count.

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { api } from "../lib/api";
import type { CrewListItem } from "../lib/types";
import { AppShell } from "../components/AppShell";
import { Button } from "../components/ui/Button";
import { Modal } from "../components/ui/Overlay";
import { Field, Input, Textarea } from "../components/ui/Field";

export default function Crews() {
  const [crews, setCrews] = useState<CrewListItem[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const navigate = useNavigate();

  const refresh = useCallback(async () => {
    try {
      setError(null);
      const list = await api.crew.list();
      setCrews(list);
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

  const onDelete = async (id: string, name: string) => {
    if (!confirm(`Delete crew "${name}"? This removes all its runners.`)) return;
    try {
      await api.crew.delete(id);
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
                Crews
              </h1>
              <p className="text-sm text-neutral-500">
                Named groups of runners with a shared goal.
              </p>
            </div>
            <Button variant="primary" onClick={() => setCreating(true)}>
              + New crew
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
              Failed to load crews.
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              {crews.map((c) => (
                <CrewCard
                  key={c.id}
                  item={c}
                  onOpen={() => navigate(`/crews/${c.id}`)}
                  onDelete={() => onDelete(c.id, c.name)}
                />
              ))}
              <AddAnotherCard
                isOnlyCard={crews.length === 0}
                onClick={() => setCreating(true)}
              />
            </div>
          )}
        </div>
      </div>

      <CreateCrewModal
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={async (created) => {
          setCreating(false);
          await refresh();
          navigate(`/crews/${created.id}`);
        }}
      />
    </AppShell>
  );
}

function CrewCard({
  item,
  onOpen,
  onDelete,
}: {
  item: CrewListItem;
  onOpen: () => void;
  onDelete: () => void;
}) {
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
      className="group flex cursor-pointer flex-col gap-3 rounded-lg border border-[#E5E5E5] bg-white p-5 transition-colors hover:border-neutral-300 focus:outline-none focus-visible:border-neutral-400 focus-visible:ring-2 focus-visible:ring-neutral-300"
    >
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="truncate text-base font-semibold text-neutral-900">
            {item.name}
          </div>
          {item.purpose ? (
            <div className="mt-0.5 line-clamp-2 text-xs text-neutral-500">
              {item.purpose}
            </div>
          ) : (
            <div className="mt-0.5 text-xs italic text-neutral-400">
              No purpose set
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-3 text-xs text-neutral-500">
          <span>
            {item.runner_count === 1
              ? "1 runner"
              : `${item.runner_count} runners`}
          </span>
          <span className="text-neutral-300">·</span>
          <span className="text-[#0066CC] transition-colors hover:underline">
            Edit
          </span>
          <button
            type="button"
            aria-label={`Delete crew ${item.name}`}
            title="Delete crew"
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

function AddAnotherCard({
  isOnlyCard,
  onClick,
}: {
  isOnlyCard: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex flex-col items-center gap-1.5 rounded-lg border border-dashed border-[#D0D0D0] bg-[#FAFAFA] p-5 text-center transition-colors hover:border-neutral-400 hover:bg-white"
    >
      <span className="text-sm font-medium text-neutral-600">
        + {isOnlyCard ? "Create your first crew" : "Add another crew"}
      </span>
      <span className="text-xs text-neutral-400">
        Groups of runners you use together often.
      </span>
    </button>
  );
}

function CreateCrewModal({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (crew: { id: string }) => void | Promise<void>;
}) {
  const [name, setName] = useState("");
  const [purpose, setPurpose] = useState("");
  const [goal, setGoal] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setName("");
      setPurpose("");
      setGoal("");
      setError(null);
    }
  }, [open]);

  const submit = async () => {
    if (!name.trim()) {
      setError("Name is required");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const created = await api.crew.create({
        name: name.trim(),
        purpose: purpose.trim() || null,
        goal: goal.trim() || null,
      });
      await onCreated({ id: created.id });
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={submitting ? () => {} : onClose}
      title="New crew"
      footer={
        <>
          <Button onClick={onClose} disabled={submitting}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={submitting}>
            {submitting ? "Creating…" : "Create crew"}
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field id="crew-name" label="Name">
          <Input
            id="crew-name"
            value={name}
            autoFocus
            placeholder="e.g. runners-feature"
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <Field id="crew-purpose" label="Purpose" hint="optional">
          <Textarea
            id="crew-purpose"
            rows={2}
            placeholder="What does this crew exist to do?"
            value={purpose}
            onChange={(e) => setPurpose(e.target.value)}
          />
        </Field>
        <Field id="crew-goal" label="Default goal" hint="optional">
          <Textarea
            id="crew-goal"
            rows={3}
            placeholder="Pre-fills the Start Mission goal."
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
          />
        </Field>
        {error ? <p className="text-xs text-red-600">{error}</p> : null}
      </form>
    </Modal>
  );
}
