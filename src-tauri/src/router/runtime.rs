// Runtime adapter: maps `runner.runtime` + `runner.system_prompt` to the
// extra CLI args the child process needs to receive the prompt.
//
// arch §4.2 / §4.3: the system prompt is passed through the runtime's native
// flag — `--append-system-prompt` for `claude-code`, the equivalent for each
// other runtime. The `runtime` enum in the `runners` table owns that mapping.
//
// Keeping this in one place means both `SessionManager::spawn` (mission) and
// `spawn_direct` (chat) get the same behavior with no chance of drift.
//
// Lives under router/ because the router's `mission_goal` handler already
// owns prompt composition; the runtime adapter is the related "how do we
// hand a prompt to a real CLI" piece.

/// Compute the extra args (in declaration order) to append after the
/// runner's configured `args` so the child receives `system_prompt` via the
/// runtime's native flag. Returns an empty Vec when no prompt is set or the
/// runtime is unrecognized — unrecognized runtimes degrade silently rather
/// than failing the spawn (the user might be prototyping a custom CLI).
pub fn system_prompt_args(runtime: &str, system_prompt: Option<&str>) -> Vec<String> {
    let prompt = match system_prompt {
        Some(p) if !p.trim().is_empty() => p,
        _ => return Vec::new(),
    };
    match runtime {
        // claude-code accepts --append-system-prompt <text>; the flag layers
        // onto its built-in default rather than replacing it, which is what
        // we want — we're appending the runner's brief, not overwriting.
        "claude-code" => vec!["--append-system-prompt".into(), prompt.to_string()],
        // codex / shell / unknown — no flag. We tried `codex --instructions`
        // first but the installed Codex CLI rejects it ("unexpected argument
        // --instructions found"). Until a verified prompt mechanism lands
        // (e.g. a documented flag on a pinned Codex version, or a wrapper
        // script convention), Codex runners spawn without the brief; the
        // prompt is still on the runner row and the user can configure
        // their own wrapper. Tracked for follow-up.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_appends_system_prompt() {
        let args = system_prompt_args("claude-code", Some("be helpful"));
        assert_eq!(args, vec!["--append-system-prompt", "be helpful"]);
    }

    #[test]
    fn codex_runtime_omits_flag_until_verified_mechanism() {
        // The installed `codex` CLI rejects `--instructions`. Until a
        // documented prompt flag is verified, the codex runtime degrades
        // to no-flag rather than crashing the spawn.
        assert!(system_prompt_args("codex", Some("be helpful")).is_empty());
    }

    #[test]
    fn shell_runtime_omits_flag() {
        assert!(system_prompt_args("shell", Some("ignored")).is_empty());
    }

    #[test]
    fn missing_or_blank_prompt_omits_flag() {
        assert!(system_prompt_args("claude-code", None).is_empty());
        assert!(system_prompt_args("claude-code", Some("")).is_empty());
        assert!(system_prompt_args("claude-code", Some("   ")).is_empty());
    }

    #[test]
    fn unknown_runtime_degrades_to_no_flag() {
        assert!(system_prompt_args("aider-future", Some("hi")).is_empty());
    }
}
