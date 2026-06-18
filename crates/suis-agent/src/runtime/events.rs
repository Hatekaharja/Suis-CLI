//! Events the agent loop emits to the UI, and the decision the UI sends back
//! for a permission prompt.

use serde_json::Value;
use tokio::sync::oneshot;

use suis_core::PermissionScope;

use crate::tasks::Task;
use crate::tools::plan::PlanDraft;
use crate::tools::ToolResult;

/// A decision returned by the UI in response to an
/// [`AgentEvent::PermissionRequest`].
///
/// Denials carry a duration just like grants do: once (this invocation only),
/// for the rest of the session, or persistently for the project. Recorded
/// denials apply to shell commands; for other actions (paths, ungated tools)
/// every denial simply blocks that invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Allow, with a grant duration and (for commands) an optional wildcard
    /// pattern instead of the exact command.
    Grant {
        /// The scope the user chose (`Once`/`Session`/`Project`/`Always`).
        scope: PermissionScope,
        /// Whether to store the grant as a wildcard (e.g. `cargo *`).
        wildcard: bool,
    },
    /// Block this invocation only. Also the fail-safe when no UI answers.
    DenyOnce,
    /// Block this exact command for the rest of the session (not persisted).
    DenySession,
    /// Block this exact command persistently for the project.
    DenyProject,
}

impl PermissionDecision {
    /// Allow for this invocation only.
    pub fn once() -> Self {
        PermissionDecision::Grant {
            scope: PermissionScope::Once,
            wildcard: false,
        }
    }

    /// Deny this invocation only.
    pub fn deny() -> Self {
        PermissionDecision::DenyOnce
    }

    /// A grant at the given scope.
    pub fn grant(scope: PermissionScope, wildcard: bool) -> Self {
        PermissionDecision::Grant { scope, wildcard }
    }

    /// Whether this decision blocks execution.
    pub fn is_denied(self) -> bool {
        matches!(
            self,
            PermissionDecision::DenyOnce
                | PermissionDecision::DenySession
                | PermissionDecision::DenyProject
        )
    }
}

/// A verdict returned by the UI in response to an [`AgentEvent::PlanProposal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecision {
    /// Persist the draft to `.suis/plans.json`.
    Approve,
    /// Discard the draft; nothing touches disk. Also the fail-safe when no UI
    /// answers.
    Reject,
}

/// Something the agent loop produced for the UI to render or act on.
#[derive(Debug)]
pub enum AgentEvent {
    /// A chunk of streamed assistant text.
    StreamChunk(String),
    /// A chunk of streamed model reasoning ("thinking"), surfaced separately from
    /// the answer so the UI can show it in a collapsible block. Sourced from a
    /// provider's dedicated reasoning channel (OpenAI-compatible
    /// `reasoning_content`/`reasoning`, Anthropic `thinking`, Ollama `thinking`)
    /// or split out of inline `<think>…</think>` tags in the text stream.
    /// Reasoning is display-only — it is never recorded into the conversation
    /// history fed back to the model.
    ReasoningChunk(String),
    /// A tool call is about to be executed.
    ToolCallStarted {
        /// The tool's name.
        name: String,
        /// The model-supplied arguments.
        args: Value,
    },
    /// A tool call finished (successfully or with an error result).
    ToolCallCompleted {
        /// The result fed back to the model.
        result: ToolResult,
    },
    /// The agent needs a permission decision before proceeding. The UI replies
    /// by sending a [`PermissionDecision`] on `sender`.
    PermissionRequest {
        /// Human-readable description of the action awaiting approval.
        action: String,
        /// The channel the UI answers on.
        sender: oneshot::Sender<PermissionDecision>,
    },
    /// The agent drafted a plan and needs the user's verdict before anything
    /// is written. The UI replies by sending a [`PlanDecision`] on `sender`.
    PlanProposal {
        /// The validated draft, for the review pane.
        draft: PlanDraft,
        /// The channel the UI answers on.
        sender: oneshot::Sender<PlanDecision>,
    },
    /// The task list changed; carries the full current list.
    TaskUpdated(Vec<Task>),
    /// Context accounting for the request about to run: estimated tokens used
    /// against the budget, and whether mechanical pruning acted this turn. The
    /// UI renders the pressure gauge and a `pruned` marker from this.
    ContextUsage {
        /// Estimated tokens the assembled request occupies (chars/4).
        used_tokens: usize,
        /// The token budget history is pruned against.
        budget: usize,
        /// Whether pruning altered the history for this request.
        pruned: bool,
    },
    /// The exact prompt token count, surfaced mid-stream as soon as the provider
    /// reports it — Anthropic sends it at `message_start`, before the body
    /// streams. Lets the UI replace its chars/4 prompt estimate with the real
    /// input immediately (live chat total and context gauge), instead of waiting
    /// for [`Self::TokenUsage`] at turn end. Providers that report only at the
    /// end emit no `PromptTokens`; their numbers settle with `TokenUsage`.
    PromptTokens {
        /// Exact tokens the prompt occupied (system + history + tools).
        prompt_tokens: usize,
    },
    /// Real token counts the provider reported for the request that just
    /// finished. `prompt_tokens` is the live context occupancy (replacing the
    /// estimate in the gauge); both counts feed the running session total. Only
    /// emitted when the provider reports usage.
    TokenUsage {
        /// Tokens the prompt occupied (system + history + tools).
        prompt_tokens: usize,
        /// Tokens the model generated this turn.
        completion_tokens: usize,
    },
    /// The conversation was compacted (`/compact`): history was replaced by a
    /// model-written summary. Carries the summary text for the UI to render in
    /// place of the prior transcript.
    Compacted {
        /// The summary that now stands in for the prior conversation.
        summary: String,
    },
    /// A task finished during an implementation session and its working context
    /// was silently compacted before the next task began. The UI shows a thin
    /// marker (e.g. `✓ w1 done — context compacted`); the handoff text itself is
    /// never sent here.
    TaskCompacted {
        /// The derived task id (`w1`, `v2`, …).
        id: String,
        /// The task's title.
        title: String,
    },
    /// Automatic verification (Phase 2) is about to run the project's
    /// `verify_command` after a turn's edits, in Agent mode. The UI shows a thin
    /// status line; the command runs through the normal permission path.
    VerifyStarted {
        /// The project's configured check command being run.
        command: String,
    },
    /// Automatic verification finished. On failure the agent loops to let the
    /// model self-correct (bounded by a round cap); on success the turn settles.
    /// `summary` is a short, one-line gist of the outcome for the status line —
    /// the full command output goes to the model, not here.
    VerifyResult {
        /// Whether the verify command exited successfully.
        passed: bool,
        /// A short, human-readable gist (first meaningful output line).
        summary: String,
    },
    /// A sub-agent was started for a delegated subtask (Phase 4). Its work runs
    /// in a fresh, lean context; the UI shows a thin marker and only the summary
    /// crosses back into the parent's context.
    SubAgentStarted {
        /// The sub-agent type that was spawned (`explore`/`find`/`delegate`).
        kind: String,
        /// The delegated objective.
        objective: String,
    },
    /// A sub-agent finished; `summary` is the dense handoff note folded back to
    /// the parent as the sub-agent tool's result.
    SubAgentFinished {
        /// The sub-agent type that finished (`explore`/`find`/`delegate`).
        kind: String,
        /// The handoff summary returned to the parent agent.
        summary: String,
    },
    /// A transient model error is being retried before the turn is given up.
    /// Emitted just before a backoff sleep so the UI can show progress instead
    /// of appearing hung. `attempt` is 1-based; `max` is the retry ceiling.
    Retrying {
        /// 1-based index of the retry about to happen.
        attempt: usize,
        /// Maximum number of retries that will be made.
        max: usize,
        /// Human-readable reason (the transient error's message).
        reason: String,
        /// How long the agent will wait before the retry, in milliseconds.
        delay_ms: u64,
    },
    /// A fatal error ended the turn.
    Error(String),
    /// The user interrupted the turn (Esc): the stream was abandoned and any
    /// not-yet-started tool calls were skipped. History stays coherent — text
    /// streamed so far is kept, and skipped calls carry synthetic results.
    Interrupted,
    /// The turn finished normally.
    Done,
}
