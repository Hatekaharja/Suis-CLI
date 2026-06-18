//! Tool result type.
//!
//! [`ToolDefinition`] and [`ToolCall`] are reused from `suis-providers` (they
//! are the same shapes sent over the wire). [`ToolResult`] is new: it carries a
//! tool's output back to the model, tagged with the call it answers.

pub use suis_providers::{ToolCall, ToolDefinition};

/// The outcome of executing a [`ToolCall`], fed back to the model as a `tool`
/// message and surfaced to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The id of the [`ToolCall`] this result answers.
    pub tool_call_id: String,
    /// Human/model-readable output (file contents, command output, an error
    /// message, ...).
    pub content: String,
    /// Whether `content` describes a failure.
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result.
    pub fn ok(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ToolResult {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// An error result.
    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ToolResult {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}
