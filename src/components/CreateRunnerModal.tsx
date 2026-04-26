// Create a runner from the top-level Runners page (C8.5).
//
// Distinct from `AddSlotModal` — that one creates a runner *and* adds it
// to a specific crew in one shot. This surface only owns the runner row;
// crew membership lives on Crew Detail.

import { useEffect, useState } from "react";

import { api } from "../lib/api";
import type { CreateRunnerInput, Runner } from "../lib/types";
import { Button } from "./ui/Button";
import { Modal } from "./ui/Overlay";
import { Field, Input, Textarea } from "./ui/Field";
import { RuntimeSelect } from "./ui/RuntimeSelect";
import { RUNTIME_OPTIONS } from "./ui/runtimes";

// Mirrors src-tauri/src/commands/runner.rs::validate_handle.
const HANDLE_RE = /^[a-z0-9][a-z0-9_-]{0,31}$/;

export function CreateRunnerModal({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (runner: Runner) => void | Promise<void>;
}) {
  const [handle, setHandle] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [role, setRole] = useState("");
  const [runtime, setRuntime] = useState<string>(RUNTIME_OPTIONS[0].value);
  const [command, setCommand] = useState(RUNTIME_OPTIONS[0].defaultCommand);
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
      setRuntime(RUNTIME_OPTIONS[0].value);
      setCommand(RUNTIME_OPTIONS[0].defaultCommand);
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
    try {
      const runner = await api.runner.create(input);
      await onCreated(runner);
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
          <span className="text-base font-semibold text-fg">New runner</span>
          <span className="text-xs font-normal text-fg-3">
            Reusable across crews and direct chat sessions.
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
            {submitting ? "Creating…" : "Create runner"}
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
        <Field
          id="new-runner-handle"
          label="Handle"
          hint="lowercase slug, globally unique, immutable after creation"
          error={handleError}
        >
          <div className="flex items-center rounded border border-line-strong bg-bg px-2.5 py-1.5 text-sm focus-within:border-fg-3">
            <span className="select-none pr-1 font-mono font-semibold text-fg-3">
              @
            </span>
            <input
              id="new-runner-handle"
              autoFocus
              value={handle}
              placeholder="architect"
              onChange={(e) => setHandle(e.target.value.toLowerCase())}
              className="flex-1 bg-transparent font-mono text-fg outline-none placeholder:text-fg-3"
            />
          </div>
        </Field>

        <div className="grid grid-cols-2 gap-3">
          <Field
            id="new-runner-display-name"
            label="Display name"
            hint="optional, shown in cards alongside the handle"
          >
            <Input
              id="new-runner-display-name"
              value={displayName}
              placeholder="Architect"
              onChange={(e) => setDisplayName(e.target.value)}
            />
          </Field>
          <Field id="new-runner-role" label="Role">
            <Input
              id="new-runner-role"
              value={role}
              placeholder="impl, reviewer, architect"
              onChange={(e) => setRole(e.target.value)}
            />
          </Field>
        </div>

        <Field
          id="new-runner-runtime"
          label="Runtime"
          hint="picks the default command — override below if needed"
        >
          <RuntimeSelect
            id="new-runner-runtime"
            value={runtime}
            onChange={(opt) => {
              setRuntime(opt.value);
              setCommand(opt.defaultCommand);
            }}
          />
        </Field>

        <Field
          id="new-runner-command"
          label="Command"
          hint="the binary to spawn; ↵ to add flags via Args"
        >
          <Input
            id="new-runner-command"
            value={command}
            placeholder="claude, codex, sh"
            onChange={(e) => setCommand(e.target.value)}
          />
        </Field>

        <Field id="new-runner-args" label="Args" hint="whitespace-separated">
          <Input
            id="new-runner-args"
            value={argsText}
            placeholder="--dangerously-skip-permissions"
            onChange={(e) => setArgsText(e.target.value)}
          />
        </Field>

        <Field
          id="new-runner-working-dir"
          label="Working directory"
          hint="optional fallback when no mission/session specifies one"
        >
          <Input
            id="new-runner-working-dir"
            value={workingDir}
            placeholder="/absolute/path"
            onChange={(e) => setWorkingDir(e.target.value)}
          />
        </Field>

        <Field
          id="new-runner-system-prompt"
          label="Default system prompt"
          hint="used whenever this runner spawns. Per-slot overrides land in v0.x"
        >
          <Textarea
            id="new-runner-system-prompt"
            rows={5}
            value={systemPrompt}
            placeholder="You are the architect for this crew. When a mission starts, decompose the goal into 2–4 tasks and assign each to a @handle in the crew."
            onChange={(e) => setSystemPrompt(e.target.value)}
          />
        </Field>

        {error ? <p className="text-xs text-danger">{error}</p> : null}
      </form>
    </Modal>
  );
}
