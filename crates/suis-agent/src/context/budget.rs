//! Token estimation and mechanical history pruning.
//!
//! Context is the scarce resource on a small local model, so before each
//! request the assembler prunes the conversation against a token budget. The
//! pruning here is *mechanical*: deterministic, free, and offline — no model
//! call, no network, never behind the user's back. (The information-preserving
//! alternative, `/compact`, is the one path that spends a model call, and only
//! when the user asks.)
//!
//! Two passes, oldest-first, least-valuable-first:
//! 1. Stale tool results (the bulk of a transcript, and worthless once acted
//!    on) older than the last two turns collapse to a one-line stub.
//! 2. If still over budget, whole oldest turns are dropped — never the system
//!    prompt, never a pinned work package, never the most recent four turns —
//!    leaving a single notice where the gap is.

use suis_providers::{output_reserve, Model};

use crate::conversation::{Message, Role};

/// Default token budget for a request, in estimated tokens. A conservative fit
/// for a 16k local context window once the model's own reply is accounted for.
/// Used when neither the model's context window nor `settings.json`'s
/// `context_budget` is known.
pub const DEFAULT_CONTEXT_BUDGET: usize = 12_000;

/// Resolve a session's fixed history-pruning budget (in estimated tokens).
///
/// This is [`budget_for`] over the model's context window — uniform across all
/// providers, Ollama included. Suis sizes only its own prompt pruning; how an
/// Ollama server allocates its context window and KV cache is governed entirely
/// by the user's Ollama settings, so we never reach in to size it.
///
/// Inputs come from the session's model and `settings.json`: a per-model
/// `window_override` (also a hard cap) and the `configured_budget` fallback.
pub fn resolve_context_budget(
    model: &Model,
    window_override: Option<usize>,
    configured_budget: Option<usize>,
) -> usize {
    let native = window_override.or(model.context_window);
    budget_for(native, configured_budget)
}

/// Resolve the history-pruning budget (in estimated tokens) from a context
/// window.
///
/// Prefers the model's real context `window` (minus its [`output_reserve`] for
/// the reply), so the budget — and the context gauge built from it — adapts to
/// each model. Falls back to an explicitly `configured` budget, then to
/// [`DEFAULT_CONTEXT_BUDGET`]. A window no larger than the reserve degrades to
/// the configured/default value rather than to zero.
pub fn budget_for(window: Option<usize>, configured: Option<usize>) -> usize {
    if let Some(window) = window {
        let usable = window.saturating_sub(output_reserve(window));
        if usable > 0 {
            return usable;
        }
    }
    configured.unwrap_or(DEFAULT_CONTEXT_BUDGET)
}

/// Turns always kept intact at the tail of the conversation, so recent context
/// is never pruned out from under an in-progress task.
const PROTECTED_TURNS: usize = 4;

/// One-line replacement for a stale tool result dropped from context.
const TOOL_STUB: &str = "[tool output pruned — re-run if needed]";

/// One-line replacement for an earlier read-only result that a later identical
/// call superseded. Read-only tools (read/search/tree) are pure for a given
/// argument set, so an older copy carries nothing the newest copy doesn't.
const DUP_STUB: &str =
    "[earlier identical read/search/tree result superseded — see the latest below]";

/// Synthetic marker inserted once where older turns were dropped.
const DROP_NOTICE: &str = "[older conversation pruned]";

/// Estimate the token count of `text` with the common chars/4 heuristic.
///
/// Deliberately approximate: it needs no tokenizer, no model call, and no
/// network. Every place this surfaces to the user is labelled as an estimate.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

/// Estimate one message's token cost: its content, any tool-call payload, and a
/// small fixed overhead for the role envelope the wire format adds.
pub fn estimate_message_tokens(message: &Message) -> usize {
    let mut tokens = estimate_tokens(&message.content);
    for call in &message.tool_calls {
        tokens += estimate_tokens(&call.name);
        tokens += estimate_tokens(&call.arguments.to_string());
    }
    tokens + 4
}

/// Estimate the total token cost of an assembled message list.
pub fn total_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Prune `messages` in place to fit `budget` estimated tokens.
///
/// The first `pinned_prefix` messages (the system prompt and any active-task
/// note) are always kept. When `pin_work_package` is set, the first history
/// turn is kept too — an implementation session never loses its objective. The
/// most recent [`PROTECTED_TURNS`] turns are never dropped, so even a tiny
/// budget leaves the live conversation intact (the request may then exceed the
/// budget — pruning protects recency over the number).
///
/// Returns whether anything was pruned, so the caller can mark it for the UI.
/// An under-budget history is returned untouched (`false`).
pub fn prune(
    messages: &mut Vec<Message>,
    pinned_prefix: usize,
    pin_work_package: bool,
    budget: usize,
) -> bool {
    if total_tokens(messages) <= budget {
        return false;
    }

    // The first prunable history index: after the pinned prefix and, when an
    // implementation session pins it, the work-package turn's opening message.
    let body_start = (pinned_prefix + usize::from(pin_work_package)).min(messages.len());
    let mut pruned = coalesce_read_only(messages, body_start);
    if total_tokens(messages) <= budget {
        return pruned;
    }

    // Pass 1 — stub tool results older than the last two turns.
    let turn_starts = user_turn_starts(messages, pinned_prefix);
    if turn_starts.len() > 2 {
        // Stub everything from the first prunable message up to (but not into)
        // the last two turns. `.max(body_start)` keeps the range non-empty even
        // if the pinned region reaches the boundary.
        let boundary = turn_starts[turn_starts.len() - 2].max(body_start);
        for msg in &mut messages[body_start..boundary] {
            if msg.role == Role::Tool && msg.content != TOOL_STUB {
                msg.content = TOOL_STUB.to_string();
                pruned = true;
            }
        }
    }
    if total_tokens(messages) <= budget {
        return pruned;
    }

    // Pass 2 — drop whole oldest turns until under budget or only the
    // protected tail (plus any pinned work package) remains.
    let mut dropped = false;
    while total_tokens(messages) > budget {
        let turn_starts = user_turn_starts(messages, pinned_prefix);
        if turn_starts.len() <= PROTECTED_TURNS {
            break;
        }
        // The oldest droppable turn: the first, unless that is the pinned
        // work-package turn.
        let oldest = usize::from(pin_work_package);
        if oldest >= turn_starts.len() - PROTECTED_TURNS {
            break;
        }
        let start = turn_starts[oldest];
        let end = turn_starts
            .get(oldest + 1)
            .copied()
            .unwrap_or(messages.len());
        messages.drain(start..end);
        pruned = true;
        dropped = true;
    }

    // A single notice marks the gap, right after the pinned prefix / package.
    if dropped {
        messages.insert(body_start, Message::text(Role::System, DROP_NOTICE));
    }

    pruned
}

/// Whether a tool is a pure read-only orientation tool, whose result is fully
/// determined by its arguments. Mirrors `runtime::agent::is_read_only`.
fn is_read_only_name(name: &str) -> bool {
    matches!(name, "read_lines" | "search" | "tree")
}

/// Collapse duplicate read-only results: keep only the most recent result of
/// each identical `(name, arguments)` call and stub the earlier copies.
///
/// A model that re-reads (or re-searches, re-`tree`s) the same thing — common
/// on weaker local models — otherwise stacks N identical payloads in context.
/// Because these tools are pure for a given argument set, every copy but the
/// newest is redundant. This runs before the turn-based passes so it shrinks a
/// runaway *single* turn too (where everything is within the protected tail and
/// the turn-based passes never fire). Returns whether anything was stubbed.
fn coalesce_read_only(messages: &mut [Message], body_start: usize) -> bool {
    use std::collections::{HashMap, HashSet};

    // Map each tool result's call id to the signature of the read-only call that
    // produced it, read off the assistant `tool_calls`. Owned, so the immutable
    // scan releases before the mutable pass below.
    let mut sig_of: HashMap<String, String> = HashMap::new();
    for msg in messages.iter() {
        for call in &msg.tool_calls {
            if is_read_only_name(&call.name) {
                sig_of.insert(
                    call.id.clone(),
                    format!("{}\u{1f}{}", call.name, call.arguments),
                );
            }
        }
    }
    if sig_of.is_empty() {
        return false;
    }

    // Walk newest-first: the first time a signature is seen is the latest result
    // (kept); every older result with that signature is stubbed.
    let mut seen: HashSet<String> = HashSet::new();
    let mut changed = false;
    for idx in (body_start..messages.len()).rev() {
        if messages[idx].role != Role::Tool || messages[idx].content == DUP_STUB {
            continue;
        }
        let sig = match messages[idx]
            .tool_call_id
            .as_deref()
            .and_then(|id| sig_of.get(id))
        {
            Some(sig) => sig.clone(),
            None => continue,
        };
        if !seen.insert(sig) {
            messages[idx].content = DUP_STUB.to_string();
            changed = true;
        }
    }
    changed
}

/// Indices of the turn-start messages (the user messages) within the history
/// portion of `messages` (everything from `pinned_prefix` onward). A turn is a
/// user message and the assistant/tool messages that answer it.
fn user_turn_starts(messages: &[Message], pinned_prefix: usize) -> Vec<usize> {
    (pinned_prefix..messages.len())
        .filter(|&i| messages[i].role == Role::User)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_providers::{MAX_OUTPUT_RESERVE, MIN_OUTPUT_RESERVE};

    fn user(text: &str) -> Message {
        Message::text(Role::User, text)
    }
    fn assistant(text: &str) -> Message {
        Message::text(Role::Assistant, text)
    }
    fn tool(text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: text.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: Some("tc".into()),
        }
    }

    /// An assistant message that issues a single `read_lines` call with id `id`.
    fn read_assistant(id: &str, path: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![suis_providers::ToolCall {
                id: id.to_string(),
                name: "read_lines".to_string(),
                arguments: serde_json::json!({ "path": path }),
            }],
            tool_call_id: None,
        }
    }

    /// The tool result answering the call with id `id`.
    fn read_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: Some(id.to_string()),
        }
    }

    /// A long string whose estimated token cost is `tokens`.
    fn filler(tokens: usize) -> String {
        "x".repeat(tokens * 4)
    }

    /// `prefix` system messages followed by `turns` turns, each a user message,
    /// an assistant reply, and one tool result of `tool_tokens` tokens.
    fn transcript(prefix: usize, turns: usize, tool_tokens: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        for i in 0..prefix {
            messages.push(Message::text(Role::System, format!("system {i}")));
        }
        for i in 0..turns {
            messages.push(user(&format!("user {i}")));
            messages.push(assistant(&format!("assistant {i}")));
            messages.push(tool(&filler(tool_tokens)));
        }
        messages
    }

    #[test]
    fn budget_prefers_window_minus_reserve() {
        // A known window reserves room for the reply — the reserve scales with
        // the window (a quarter, clamped), so a large window holds back more.
        assert_eq!(
            budget_for(Some(128_000), None),
            128_000 - output_reserve(128_000)
        );
        // The window wins even when a budget is also configured.
        assert_eq!(
            budget_for(Some(32_768), Some(8_000)),
            32_768 - output_reserve(32_768)
        );
    }

    #[test]
    fn budget_falls_back_when_window_unknown_or_tiny() {
        // Unknown window: the configured value, then the default.
        assert_eq!(budget_for(None, Some(8_000)), 8_000);
        assert_eq!(budget_for(None, None), DEFAULT_CONTEXT_BUDGET);
        // A window no larger than the reserve degrades to the fallback, not 0.
        assert_eq!(budget_for(Some(MIN_OUTPUT_RESERVE), Some(5_000)), 5_000);
        assert_eq!(budget_for(Some(100), None), DEFAULT_CONTEXT_BUDGET);
    }

    #[test]
    fn output_reserve_scales_with_the_window_and_clamps() {
        // Small window: the floor (identical to the old flat reserve, so small
        // local boxes are unchanged).
        assert_eq!(output_reserve(8_192), MIN_OUTPUT_RESERVE);
        // Mid window: a quarter of it, so a roomy box gets a generous reply
        // budget — long reasoning no longer truncates at a flat 4k.
        assert_eq!(output_reserve(64_000), 16_000);
        // Huge window: clamped at the ceiling.
        assert_eq!(output_reserve(1_000_000), MAX_OUTPUT_RESERVE);
    }

    #[test]
    fn resolve_budget_uses_the_window_for_every_provider() {
        // Ollama is treated no differently from any other provider: the budget is
        // the model's window minus the reply reserve. Suis never sizes Ollama's
        // own context window or KV cache.
        for provider in ["openai", "ollama"] {
            let model = Model::new(provider, "m", suis_providers::Capabilities::default())
                .with_context_window(Some(128_000));
            assert_eq!(
                resolve_context_budget(&model, None, None),
                128_000 - output_reserve(128_000)
            );
            // A per-model window override wins (and caps).
            assert_eq!(
                resolve_context_budget(&model, Some(16_384), None),
                16_384 - output_reserve(16_384)
            );
        }
    }

    #[test]
    fn resolve_budget_falls_back_when_window_unknown() {
        // No window and no configured budget: the built-in default.
        let model = Model::new("ollama", "m", suis_providers::Capabilities::default());
        assert_eq!(
            resolve_context_budget(&model, None, None),
            DEFAULT_CONTEXT_BUDGET
        );
        // A configured budget is used when the window is unknown.
        assert_eq!(resolve_context_budget(&model, None, Some(8_000)), 8_000);
    }

    #[test]
    fn estimate_is_chars_over_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(400)), 100);
    }

    #[test]
    fn coalesce_keeps_latest_duplicate_read_and_stubs_earlier() {
        // The same file read three times: only the newest copy's content is kept.
        let big = filler(200);
        let mut messages = vec![Message::text(Role::System, "sys")];
        for i in 0..3 {
            messages.push(user(&format!("u{i}")));
            messages.push(read_assistant(&format!("c{i}"), "src/main.rs"));
            messages.push(read_result(&format!("c{i}"), &big));
        }
        // Over budget for three copies, but one copy fits — so coalescing alone
        // (no turn-based pruning) brings it under.
        assert!(prune(&mut messages, 1, false, 400));
        let stubs = messages.iter().filter(|m| m.content == DUP_STUB).count();
        assert_eq!(stubs, 2, "the two older identical reads are stubbed");
        assert!(
            messages
                .iter()
                .any(|m| m.role == Role::Tool && m.content == big),
            "the newest read keeps its real content"
        );
    }

    #[test]
    fn coalesce_leaves_distinct_reads_untouched() {
        // Four reads of *different* paths: nothing is a duplicate, so the
        // coalescing pass never stubs (turn-based passes may, but never as DUP).
        let big = filler(200);
        let mut messages = vec![Message::text(Role::System, "sys")];
        for i in 0..4 {
            messages.push(user(&format!("u{i}")));
            messages.push(read_assistant(&format!("c{i}"), &format!("src/f{i}.rs")));
            messages.push(read_result(&format!("c{i}"), &big));
        }
        assert!(prune(&mut messages, 1, false, 300));
        assert!(
            messages.iter().all(|m| m.content != DUP_STUB),
            "distinct reads must not be coalesced"
        );
    }

    #[test]
    fn under_budget_history_is_untouched() {
        let mut messages = transcript(1, 3, 5);
        let before = messages.clone();
        assert!(!prune(&mut messages, 1, false, 100_000));
        assert_eq!(messages, before);
    }

    #[test]
    fn tool_results_pruned_before_whole_turns() {
        // Six turns with fat tool results. The full transcript (~396 tokens)
        // is over budget, but stubbing the four stale tool results (~164 tokens
        // saved) brings it under — so pass 1 alone suffices and no turn drops.
        let mut messages = transcript(1, 6, 50);
        let budget = 300;
        assert!(prune(&mut messages, 1, false, budget));

        let stub_count = messages
            .iter()
            .filter(|m| m.role == Role::Tool && m.content == TOOL_STUB)
            .count();
        assert!(stub_count > 0, "tool results should be stubbed");
        assert!(
            !messages.iter().any(|m| m.content == DROP_NOTICE),
            "stubbing alone should avoid dropping turns"
        );
        // The two most recent turns keep their real tool output.
        assert!(messages
            .iter()
            .any(|m| m.role == Role::Tool && m.content != TOOL_STUB));
    }

    #[test]
    fn tiny_budget_keeps_system_prompt_and_last_four_turns() {
        let mut messages = transcript(1, 8, 5);
        assert!(prune(&mut messages, 1, false, 1));

        // The system prompt (pinned prefix) survives.
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "system 0");
        // Exactly the last four turns' user messages remain.
        let users: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(users, vec!["user 4", "user 5", "user 6", "user 7"]);
    }

    #[test]
    fn drop_notice_appears_exactly_once() {
        let mut messages = transcript(1, 10, 5);
        assert!(prune(&mut messages, 1, false, 1));
        let notices = messages.iter().filter(|m| m.content == DROP_NOTICE).count();
        assert_eq!(notices, 1);
        // It sits at the gap: right after the pinned system prefix.
        assert_eq!(messages[1].content, DROP_NOTICE);
    }

    #[test]
    fn pinned_work_package_survives_a_tiny_budget() {
        // The first history turn is the work package. Even at budget 1 it must
        // survive, alongside the system prompt and the last four turns.
        let mut messages = transcript(1, 8, 5);
        // Mark the first user message as the recognizable work package.
        messages[1] = user("WORK PACKAGE");
        assert!(prune(&mut messages, 1, true, 1));

        assert!(messages.iter().any(|m| m.content == "WORK PACKAGE"));
        // The pinned package stays first in the history region (before the gap
        // notice), and recent turns are still present.
        let pkg = messages
            .iter()
            .position(|m| m.content == "WORK PACKAGE")
            .unwrap();
        let notice = messages
            .iter()
            .position(|m| m.content == DROP_NOTICE)
            .unwrap();
        assert!(pkg < notice, "package precedes the pruned gap");
        let users: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(users.first(), Some(&"WORK PACKAGE"));
        assert!(users.contains(&"user 7"));
    }
}
