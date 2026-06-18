//! suis-agent — agent orchestration layer for Suis.
//!
//! Owns the conversation loop, tool lifecycle, task tracking, and context
//! assembly. Depends on suis-core and suis-providers.
//!
//! Layout:
//! - [`tools`] — the six MVP tools, the [`Tool`](tools::Tool) trait, and the
//!   permission-gated [`ToolExecutor`](tools::ToolExecutor).
//! - [`conversation`] — message history for a session.
//! - [`tasks`] — session-scoped task tracking.
//! - [`context`] — assembling a [`ChatRequest`] from session state.
//! - [`runtime`] — the agentic loop and the [`AgentEvent`](runtime::AgentEvent)
//!   stream the UI consumes.
//!
//! The agent reuses the wire types from `suis-providers`
//! ([`Message`](conversation::Message), [`ToolCall`], [`ToolDefinition`])
//! rather than redefining them, so a request can be built and sent without any
//! intermediate conversion.

pub mod context;
pub mod conversation;
pub mod diff;
pub mod runtime;
pub mod tasks;
pub mod tools;

#[cfg(test)]
mod test_util;

pub use context::{
    budget_for, detect_profile, resolve_context_budget, Assembled, ContextAssembler,
    DEFAULT_CONTEXT_BUDGET,
};
pub use conversation::{ConversationHistory, Message, Role};
pub use runtime::{
    Agent, AgentEvent, ImplementTarget, LedgerEntry, Mode, PermissionDecision, Phase, PlanDecision,
    Session, TurnOutcome,
};
pub use tasks::{Task, TaskStatus, TaskStore};
pub use tools::{AccessLog, PlanDraft, Tool, ToolContext, ToolExecutor, ToolResult};

// Re-export the provider wire types the agent surface speaks in.
pub use suis_providers::{ChatRequest, ChatResponse, ToolCall, ToolDefinition, Transport};
