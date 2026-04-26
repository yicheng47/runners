// Crews list — matches design/runners-design.pen frame `nqOot`.
//
// Vertical stack of dark crew cards. Empty state uses the shared
// EmptyStateCard so all three list pages stay visually consistent.

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { api } from "../lib/api";
import type { CrewListItem } from "../lib/types";
import { Button } from "../components/ui/Button";
import { Modal } from "../components/ui/Overlay";
import { Field, Input, Textarea } from "../components/ui/Field";
import { EmptyStateCard } from "../components/EmptyStateCard";

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
    if (!confirm(`Delete crew "${name}"? This removes all its slots.`)) return;
    try {
      await api.crew.delete(id);
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
                Crews
              </h1>
              <p className="text-sm text-fg-2">
                Named groups of runners with a shared goal.
              </p>
            </div>
            <Button variant="primary" onClick={() => setCreating(true)}>
              + New crew
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
              Failed to load crews.
            </div>
          ) : crews.length === 0 ? (
            <EmptyStateCard
              icon={<UsersIcon />}
              title="No crews yet"
              description="A crew is a named group of runners working a goal together. Spin up your first one to get started."
              action={
                <Button variant="primary" onClick={() => setCreating(true)}>
                  + New crew
                </Button>
              }
            />
          ) : (
            <div className="flex flex-col gap-3">
              {crews.map((c) => (
                <CrewCard
                  key={c.id}
                  item={c}
                  onOpen={() => navigate(`/crews/${c.id}`)}
                  onDelete={() => onDelete(c.id, c.name)}
                />
              ))}
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
    </>
  );
}

function UsersIcon() {
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
      <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
      <circle cx="9" cy="7" r="4" />
      <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
      <path d="M16 3.13a4 4 0 0 1 0 7.75" />
    </svg>
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
      className="group flex cursor-pointer flex-col gap-2 rounded-lg border border-line bg-panel p-4 transition-colors hover:border-line-strong focus:outline-none focus-visible:border-fg-3"
    >
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="truncate text-base font-semibold text-fg">
            {item.name}
          </div>
          {item.purpose ? (
            <div className="mt-0.5 line-clamp-2 text-xs text-fg-2">
              {item.purpose}
            </div>
          ) : (
            <div className="mt-0.5 text-xs italic text-fg-3">
              No purpose set
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-3 text-xs text-fg-2">
          <span>
            {item.runner_count === 1
              ? "1 runner"
              : `${item.runner_count} runners`}
          </span>
          <span className="text-accent transition-colors hover:underline">
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
            className="rounded px-1 py-0.5 text-fg-3 opacity-60 transition-colors hover:bg-danger/10 hover:text-danger focus-visible:opacity-100 group-hover:opacity-100"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
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
      title={
        <div className="flex flex-col gap-0.5">
          <span className="text-base font-semibold text-fg">New crew</span>
          <span className="text-xs font-normal text-fg-3">
            Group of runners that work missions together.
          </span>
        </div>
      }
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
        className="flex flex-col gap-4"
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
            placeholder="runners-feature"
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
        {error ? <p className="text-xs text-danger">{error}</p> : null}
      </form>
    </Modal>
  );
}
