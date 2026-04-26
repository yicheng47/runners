// Runtime catalog. Single source of truth for the runtimes the v0 UI
// exposes. Kept in a `.ts` file (not the component) so RuntimeSelect's
// React Fast-Refresh boundary stays clean.

export interface RuntimeOption {
  value: string;
  label: string;
  // The binary the runtime runs by default. Used by callers to pre-fill
  // the Command input on selection change.
  defaultCommand: string;
  description?: string;
}

// v0 narrows runtimes to just claude-code and codex. shell + aider were
// dropped to avoid encouraging untested paths; add them back here when
// they become first-class.
export const RUNTIME_OPTIONS: RuntimeOption[] = [
  {
    value: "claude-code",
    label: "claude-code",
    defaultCommand: "claude",
    description: "Anthropic Claude Code CLI",
  },
  {
    value: "codex",
    label: "codex",
    defaultCommand: "codex",
    description: "OpenAI Codex CLI",
  },
];
