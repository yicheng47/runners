// Hardcoded signal handlers. One function per built-in signal type.
// Per arch §5.2, signals always carry `to: null`; per-target routing lives
// in `payload.target` (only `human_said` uses this in v0). Per arch §5.5.0
// invariant, messages never reach this module — `Router::handle_event`
// short-circuits non-signal events.
//
// Stdin pushes are silent: handlers do NOT write `inject_stdin_*` audit
// events. The originating signal already records the cause in the log.
// Only `ask_human` results in a derived event (`human_question`), because
// that one is consumed by the workspace UI as a card.

use runner_core::model::Event;

use super::prompt::{compose_launch_prompt, LaunchPromptInput, RosterEntry};
use super::{Router, RunnerStatus};

pub(super) fn mission_goal(router: &Router, event: &Event) {
    let goal = event
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let launch = router.launch();
    let roster_entries: Vec<RosterEntry> = launch
        .roster()
        .iter()
        .map(|r| RosterEntry {
            handle: r.handle(),
            display_name: r.display_name(),
            role: r.role(),
            lead: r.is_lead(),
        })
        .collect();
    let prompt = compose_launch_prompt(&LaunchPromptInput {
        lead: launch.lead(),
        crew_name: launch.crew_name(),
        mission_goal: goal,
        roster: &roster_entries,
        allowed_signals: launch.allowed_signals(),
    });
    if let Err(e) = router.inject_to_handle(launch.lead().handle.as_str(), prompt.as_bytes()) {
        router.warn(format!("mission_goal injection to lead failed: {e}"));
    }
}

pub(super) fn human_said(router: &Router, event: &Event) {
    let text = event
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let target = event
        .payload
        .get("target")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| router.lead_handle().to_string());

    // Always end with a newline so the TUI submits the line.
    let mut bytes = text.to_string();
    if !bytes.ends_with('\n') {
        bytes.push('\n');
    }
    if let Err(e) = router.inject_to_handle(&target, bytes.as_bytes()) {
        router.warn(format!(
            "human_said injection to @{target} failed: {e} (text: {text:?})"
        ));
    }
}

pub(super) fn ask_lead(router: &Router, event: &Event) {
    let question = event
        .payload
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let context = event.payload.get("context").and_then(|v| v.as_str());

    // Render a stdin template the lead can read in-stream. The asker handle
    // (`event.from`) is preserved in the prefix so the lead knows whom to
    // reply to with `runner msg post --to <asker>`.
    let mut text = format!(
        "[ask_lead from @{asker}] {question}\n",
        asker = event.from,
        question = question,
    );
    if let Some(ctx) = context {
        let ctx = ctx.trim();
        if !ctx.is_empty() {
            text.push_str("Context:\n");
            text.push_str(ctx);
            text.push('\n');
        }
    }

    let lead_handle = router.lead_handle().to_string();
    if let Err(e) = router.inject_to_handle(&lead_handle, text.as_bytes()) {
        router.warn(format!("ask_lead injection to lead failed: {e}"));
    }
}

pub(super) fn ask_human(router: &Router, event: &Event) {
    let prompt = event
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let choices = event
        .payload
        .get("choices")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let on_behalf_of = event
        .payload
        .get("on_behalf_of")
        .and_then(|v| v.as_str());

    // Append the `human_question` card first; its id is the canonical
    // `question_id` per arch §5.5.0. The asker is the runner that emitted
    // the `ask_human` signal (typically the lead, or a worker in the
    // direct-fallback flow). Pending-ask map is keyed on the *card* id so
    // a matching `human_response` (which carries
    // `payload.question_id = human_question.id`) routes back to the
    // original asker. If the append fails, no mapping is recorded — the
    // human_response would have nothing to reference anyway, and the
    // failure is already logged inside `append_human_question`.
    if let Some(card_id) = router.append_human_question(&event.id, prompt, &choices, on_behalf_of) {
        router.record_pending_ask(card_id, event.from.clone());
    }
}

pub(super) fn human_response(router: &Router, event: &Event) {
    let Some(question_id) = event
        .payload
        .get("question_id")
        .and_then(|v| v.as_str())
    else {
        router.warn("human_response missing payload.question_id");
        return;
    };
    let Some(asker) = router.take_pending_ask(question_id) else {
        router.warn(format!(
            "human_response references unknown question_id {question_id}"
        ));
        return;
    };

    // Render the human's choice as a single line. Free-text answers (a
    // future v0.x extension) would land in `payload.text`; for now we
    // expect `choice` only.
    let choice = event
        .payload
        .get("choice")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = format!("[human_response] {choice}\n");
    if let Err(e) = router.inject_to_handle(&asker, text.as_bytes()) {
        router.warn(format!(
            "human_response injection to @{asker} failed: {e}"
        ));
    }
}

pub(super) fn runner_status(router: &Router, event: &Event) {
    let state = match event
        .payload
        .get("state")
        .and_then(|v| v.as_str())
    {
        Some("busy") => RunnerStatus::Busy,
        Some("idle") => RunnerStatus::Idle,
        other => {
            router.warn(format!(
                "runner_status from @{} has unknown state {:?}",
                event.from, other
            ));
            return;
        }
    };
    router.set_status(event.from.clone(), state);

    // Wake the lead only when a non-lead reports idle. A worker reporting
    // busy is already implicit in the fact that they're working; spamming
    // the lead on every busy→still-busy transition would be noise. arch
    // §5.5.1.
    let lead_handle = router.lead_handle().to_string();
    if state == RunnerStatus::Idle && event.from != lead_handle {
        let note = event
            .payload
            .get("note")
            .and_then(|v| v.as_str())
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        let text = format!(
            "[runner_status] @{worker} is idle{note}\n",
            worker = event.from
        );
        if let Err(e) = router.inject_to_handle(&lead_handle, text.as_bytes()) {
            router.warn(format!(
                "runner_status idle notice to lead failed: {e}"
            ));
        }
    }
}
