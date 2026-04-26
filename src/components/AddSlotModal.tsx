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
import { RuntimeSelect } from "./ui/RuntimeSelect";
import { RUNTIME_OPTIONS } from "./ui/runtimes";

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
    let createdRunnerId: string | null = null;
    try {
      const runner = await api.runner.create(input);
      createdRunnerId = runner.id;
      await api.crew.addRunner(crewId, runner.id);
      await onCreated();
    } catch (e) {
      setError(String(e));
      if (createdRunnerId) {
        try {
          await api.runner.delete(createdRunnerId);
        } catch {
          // best-effort cleanup
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
          <span className="text-base font-semibold text-fg">Add slot</span>
          <span className="text-xs font-normal text-fg-3">
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
        <Field
          id="runner-handle"
          label="Handle"
          hint="lowercase, immutable"
          error={handleError}
        >
          <div className="flex items-center rounded border border-line-strong bg-bg px-2.5 py-1.5 text-sm focus-within:border-fg-3">
            <span className="select-none pr-1 font-mono font-semibold text-fg-3">
              @
            </span>
            <input
              id="runner-handle"
              autoFocus
              value={handle}
              placeholder="reviewer"
              onChange={(e) => setHandle(e.target.value.toLowerCase())}
              className="flex-1 bg-transparent font-mono text-fg outline-none placeholder:text-fg-3"
            />
          </div>
        </Field>

        <div className="grid grid-cols-2 gap-3">
          <Field id="runner-display-name" label="Display name">
            <Input
              id="runner-display-name"
              value={displayName}
              placeholder="Implementer"
              onChange={(e) => setDisplayName(e.target.value)}
            />
          </Field>
          <Field id="runner-role" label="Role">
            <Input
              id="runner-role"
              value={role}
              placeholder="impl, reviewer, architect"
              onChange={(e) => setRole(e.target.value)}
            />
          </Field>
        </div>

        <Field id="runner-runtime" label="Runtime">
          <RuntimeSelect
            id="runner-runtime"
            value={runtime}
            onChange={(opt) => {
              setRuntime(opt.value);
              setCommand(opt.defaultCommand);
            }}
          />
        </Field>

        <Field id="runner-command" label="Command" hint="the binary to spawn">
          <Input
            id="runner-command"
            value={command}
            placeholder="claude, codex, sh"
            onChange={(e) => setCommand(e.target.value)}
          />
        </Field>

        <Field id="runner-args" label="Args" hint="whitespace-separated">
          <Input
            id="runner-args"
            value={argsText}
            placeholder="--dangerously-skip-permissions"
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

        {error ? <p className="text-xs text-danger">{error}</p> : null}
      </form>
    </Modal>
  );
}
