//! Wire-agnostic request/response types shared by all transports.
//!
//! Each transport (Ollama, OpenAI-compatible) maps these to and from its own
//! provider-specific JSON. The rest of Suis only ever sees these types.

use serde::{Deserialize, Serialize};

/// The author of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    /// The lowercase wire string (`"system"`, `"user"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A single conversation message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls requested by the model (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The id of the tool call this message answers (tool messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// A plain text message with no tool metadata.
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Message {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A tool the model is allowed to call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-supplied id, or one Suis synthesizes when absent.
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A chat request, before being lowered onto a specific transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    pub stream: bool,
}

impl ChatRequest {
    /// A non-streaming request with no tools.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        ChatRequest {
            model: model.into(),
            messages,
            tools: None,
            stream: false,
        }
    }
}

/// Token counts a provider reports for a turn. `prompt_tokens` covers
/// everything sent (system prompt, history, tools); `completion_tokens` is what
/// the model generated. Together they let the UI show real context occupancy
/// and a running session total, rather than a chars/4 estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
}

/// A chat response (one chunk when streaming, the whole thing otherwise).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Text content for this chunk/response (may be empty).
    pub content: String,
    /// Reasoning/thinking text for this chunk, when the provider streams it on a
    /// channel separate from `content` (OpenAI-compatible `reasoning_content` /
    /// `reasoning`, Anthropic `thinking` blocks, Ollama `thinking`). Empty for
    /// providers — and models — that don't emit reasoning. Inline `<think>` tags
    /// some models fold into `content` are split out later, in the agent loop.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// Any tool calls present in this chunk/response.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Whether this is the terminal chunk.
    pub done: bool,
    /// Token usage reported by the provider, when available. Carried on the
    /// terminal chunk for streams that report it (and on the whole response for
    /// non-streaming calls); `None` when the server omits usage.
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(Role::Tool.as_str(), "tool");
    }

    #[test]
    fn message_omits_empty_tool_fields() {
        let msg = Message::text(Role::User, "hi");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn chat_request_round_trips() {
        let req = ChatRequest {
            model: "llama3".into(),
            messages: vec![Message::text(Role::User, "hello")],
            tools: Some(vec![ToolDefinition {
                name: "read".into(),
                description: "read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            stream: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
