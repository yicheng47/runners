// Add a runner slot to a crew (C5.5).
//
// Two invokes per submit: create the global runner, then join it to the
// crew. Errors surface a best-effort cleanup — if the runner was created
// but the add-to-crew step fails, we delete the orphan so the user isn't
// left with a ghost runner in their global list.
//
// `args` is a single shell-style text field split on whitespace
// client-side; keep the split rule dumb and obvious (no shell quoting)
// until a user actually needs spaces in an arg. Backend re-validates the
// handle.

import { useEffect, useState } from "react";

import { api } from "../lib/api";
import type { CreateRunnerInput } from "../lib/types";
import { Button } from "./ui/Button";
import { Modal } from "./ui/Overlay";
import { Field, Input, Textarea } from "./ui/Field";

const RUNTIMES = ["shell", "claude-code", "codex", "aider"] as const;

// Mirrors src-tauri/src/commands/runner.rs::validate_handle. Kept in sync
// for instant UX feedback; the backend is the source of truth.
const HANDLE_RE = /^[a-z0-9][a-z0-9_-]{0,31}$/;

export function AddSlotModal({
  open,
  crewId,
  isFirstRunner,
  onClose,
  onCreated,
}: {
  open: boolean;
  crewId: string;
  isFirstRunner: boolean;
  onClose: () => void;
  onCreated: () => void | Promise<void>;
}) {
  const [handle, setHandle] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [role, setRole] = useState("");
  const [runtime, setRuntime] = useState<string>("shell");
  const [command, setCommand] = useState("");
  const [argsText, setArgsText] = useState("");
  const [workingDir, setWorkingDir] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setHandle("");
      setDisplayName("");
      setRole("");
      setRuntime("shell");
      setCommand("");
      setArgsText("");
      setWorkingDir("");
      setSystemPrompt("");
      setError(null);
    }
  }, [open]);

  const handleError = (() => {
    if (!handle) return null;
    if (!HANDLE_RE.test(handle))
      return "Lowercase letters, digits, '-' or '_'; must start with a letter or digit; up to 32 chars.";
    return null;
  })();

  const canSubmit =
    handle.length > 0 &&
    handleError === null &&
    displayName.trim().length > 0 &&
    role.trim().length > 0 &&
    command.trim().length > 0 &&
    !submitting;

  const submit = async () => {
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const input: CreateRunnerInput = {
      handle,
      display_name: displayName.trim(),
      role: role.trim(),
      runtime,
      command: command.trim(),
      args: argsText.trim() ? argsText.trim().split(/\s+/) : [],
      working_dir: workingDir.trim() || null,
      system_prompt: systemPrompt.trim() || null,
    };
    let createdRunnerId: string | null = null;
    try {
      // Step 1: create the global runner.
      const runner = await api.runner.create(input);
      createdRunnerId = runner.id;
      // Step 2: attach it to this crew as a slot. If this fails (e.g. the
      // crew was deleted mid-flight), roll back the runner so we don't
      // leave orphans in the global list.
      await api.crew.addRunner(crewId, runner.id);
      await onCreated();
    } catch (e) {
      setError(String(e));
      if (createdRunnerId) {
        try {
          await api.runner.delete(createdRunnerId);
        } catch {
          // Best-effort cleanup — surface the original error to the user.
        }
      }
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
          <span className="text-base font-semibold text-neutral-900">
            Add slot
          </span>
          <span className="text-xs font-normal text-neutral-500">
            {isFirstRunner
              ? "First slot in the crew — it becomes the LEAD automatically."
              : "Adds a new runner to this crew."}
          </span>
        </div>
      }
      widthClass="w-full max-w-xl"
      footer={
        <>
          <Button onClick={onClose} disabled={submitting}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canSubmit}>
            {submitting ? "Adding…" : "Add slot"}
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="grid grid-cols-2 gap-3">
          <Field
            id="runner-handle"
            label="Handle"
            hint="immutable"
            error={handleError}
          >
            <div className="flex items-center rounded-md border border-neutral-300 bg-neutral-50 px-2.5 py-1.5 text-sm focus-within:border-neutral-400 focus-within:bg-white focus-within:ring-2 focus-within:ring-neutral-300">
              <span className="select-none pr-1 font-mono font-semibold text-neutral-400">
                @
              </span>
              <input
                id="runner-handle"
                autoFocus
                value={handle}
                placeholder="reviewer"
                onChange={(e) => setHandle(e.target.value.toLowerCase())}
                className="flex-1 bg-transparent font-mono text-neutral-900 outline-none placeholder:text-neutral-400"
              />
            </div>
            <p className="text-[11px] text-neutral-500">
              Lowercase slug, unique in this crew. Immutable once used in a
              mission.
            </p>
          </Field>
          <Field id="runner-display-name" label="Display name">
            <Input
              id="runner-display-name"
              value={displayName}
              placeholder="e.g. Implementer"
              onChange={(e) => setDisplayName(e.target.value)}
            />
          </Field>
        </div>

        <div className="grid grid-cols-2 gap-3">
          <Field id="runner-role" label="Role">
            <Input
              id="runner-role"
              value={role}
              placeholder="e.g. impl, reviewer, architect"
              onChange={(e) => setRole(e.target.value)}
            />
          </Field>
          <Field id="runner-runtime" label="Runtime">
            <select
              id="runner-runtime"
              value={runtime}
              onChange={(e) => setRuntime(e.target.value)}
              className="w-full rounded-md border border-neutral-300 bg-white px-2.5 py-1.5 text-sm text-neutral-900 focus:outline-none focus:ring-2 focus:ring-neutral-400"
            >
              {RUNTIMES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          </Field>
        </div>

        <Field
          id="runner-command"
          label="Command"
          hint="the binary to spawn"
        >
          <Input
            id="runner-command"
            value={command}
            placeholder="e.g. claude, codex, sh"
            onChange={(e) => setCommand(e.target.value)}
          />
        </Field>

        <Field id="runner-args" label="Args" hint="whitespace-separated">
          <Input
            id="runner-args"
            value={argsText}
            placeholder="e.g. --dangerously-skip-permissions"
            onChange={(e) => setArgsText(e.target.value)}
          />
        </Field>

        <Field
          id="runner-working-dir"
          label="Working directory"
          hint="optional — defaults to mission cwd"
        >
          <Input
            id="runner-working-dir"
            value={workingDir}
            placeholder="/absolute/path"
            onChange={(e) => setWorkingDir(e.target.value)}
          />
        </Field>

        <Field
          id="runner-system-prompt"
          label="System prompt"
          hint="optional"
        >
          <Textarea
            id="runner-system-prompt"
            rows={4}
            value={systemPrompt}
            placeholder="Behavioral instructions for this runner."
            onChange={(e) => setSystemPrompt(e.target.value)}
          />
        </Field>

        {error ? <p className="text-xs text-red-600">{error}</p> : null}
      </form>
    </Modal>
  );
}
