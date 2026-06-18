//! Fallback parsing of tool calls a model emitted as *text* rather than through
//! the provider's structured tool-calling API.
//!
//! Weaker / locally-served models (Qwen3 and other Hermes-template models) often
//! print a tool call into the message content — e.g.
//! `<tool_call>{"name":"read","arguments":{"path":"a.rs"}}</tool_call>` or a
//! fenced ```json block — instead of returning it on the `tool_calls` channel.
//! Without this, such a turn looks like a plain final answer and the agent stops.
//!
//! [`parse_text_tool_calls`] recovers those calls and returns the content with
//! the consumed spans removed, so the recorded message keeps only the prose.

use serde_json::Value;

use super::types::ToolCall;
use super::{normalize_arguments, parse_tool_arguments};

/// The outcome of scanning message content for text-emitted tool calls.
pub struct TextToolCalls {
    /// The content with the matched tool-call spans removed and trimmed.
    pub cleaned: String,
    /// The recovered tool calls, in order, with synthesized ids.
    pub calls: Vec<ToolCall>,
}

/// Scan `content` for tool calls written as text and return them plus the
/// content with those spans stripped. Empty `calls` means nothing was found and
/// `cleaned` equals the trimmed input.
pub fn parse_text_tool_calls(content: &str) -> TextToolCalls {
    let mut spans: Vec<(usize, usize, ToolCall)> = Vec::new();

    // 1. Hermes-style <tool_call>…</tool_call> blocks. The explicit tag is a
    //    strong signal, so a missing `arguments` defaults to an empty object.
    collect_tag_blocks(content, &mut spans);
    // 2. Fenced ```json blocks — only when the object actually looks like a call
    //    (has `name` and `arguments`/`parameters`), so example JSON isn't
    //    mistaken for one.
    collect_fenced_blocks(content, &mut spans);
    // 3. As a last resort, a bare top-level JSON object that looks like a call.
    if spans.is_empty() {
        if let Some(call) = call_from_json_str(content.trim(), true) {
            spans.push((0, content.len(), call));
        }
    }

    spans.sort_by_key(|(start, _, _)| *start);

    let mut calls = Vec::new();
    let mut cleaned = String::new();
    let mut cursor = 0;
    for (start, end, mut call) in spans {
        if start < cursor {
            continue; // overlapping span: keep the earlier one
        }
        cleaned.push_str(&content[cursor..start]);
        cursor = end.min(content.len());
        call.id = format!("call_{}", calls.len());
        calls.push(call);
    }
    cleaned.push_str(&content[cursor.min(content.len())..]);

    TextToolCalls {
        cleaned: cleaned.trim().to_string(),
        calls,
    }
}

/// Append every `<tool_call>…</tool_call>` block's parsed call.
fn collect_tag_blocks(content: &str, out: &mut Vec<(usize, usize, ToolCall)>) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut search = 0;
    while let Some(rel) = content[search..].find(OPEN) {
        let start = search + rel;
        let inner_start = start + OPEN.len();
        let Some(crel) = content[inner_start..].find(CLOSE) else {
            break;
        };
        let inner_end = inner_start + crel;
        let end = inner_end + CLOSE.len();
        if let Some(call) = call_from_json_str(content[inner_start..inner_end].trim(), false) {
            out.push((start, end, call));
        }
        search = end;
    }
}

/// Append every fenced code block that parses as a tool call.
fn collect_fenced_blocks(content: &str, out: &mut Vec<(usize, usize, ToolCall)>) {
    const FENCE: &str = "```";
    let mut search = 0;
    while let Some(rel) = content[search..].find(FENCE) {
        let start = search + rel;
        let after_open = start + FENCE.len();
        let Some(crel) = content[after_open..].find(FENCE) else {
            break;
        };
        let inner = &content[after_open..after_open + crel];
        let end = after_open + crel + FENCE.len();
        // Drop an optional language tag on the fence's first line.
        let body = match inner.find('\n') {
            Some(i) => &inner[i + 1..],
            None => inner,
        };
        if let Some(call) = call_from_json_str(body.trim(), true) {
            out.push((start, end, call));
        }
        search = end;
    }
}

/// Parse a `{ "name": …, "arguments": … }` object into a [`ToolCall`] (id left
/// blank for the caller to assign). When `require_args` is set, a call without an
/// `arguments`/`parameters` field is rejected — used for fenced/bare JSON where
/// the structure alone is the signal; tag blocks pass `false` and default the
/// arguments to an empty object.
fn call_from_json_str(s: &str, require_args: bool) -> Option<ToolCall> {
    if !s.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(s).ok()?;
    let obj = value.as_object()?;
    let name = obj.get("name").and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    let raw_args = obj.get("arguments").or_else(|| obj.get("parameters"));
    let arguments = match raw_args {
        Some(Value::String(encoded)) => parse_tool_arguments(encoded),
        Some(other) => normalize_arguments(other.clone()),
        None if require_args => return None,
        None => Value::Object(Default::default()),
    };
    Some(ToolCall {
        id: String::new(),
        name: name.to_string(),
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hermes_tag_block() {
        let content = "Let me read it.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}</tool_call>";
        let parsed = parse_text_tool_calls(content);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "read");
        assert_eq!(parsed.calls[0].arguments["path"], "a.rs");
        assert_eq!(parsed.calls[0].id, "call_0");
        assert_eq!(parsed.cleaned, "Let me read it.");
    }

    #[test]
    fn parses_fenced_json_call() {
        let content =
            "Sure:\n```json\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}\n```";
        let parsed = parse_text_tool_calls(content);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "bash");
        assert_eq!(parsed.calls[0].arguments["command"], "ls");
    }

    #[test]
    fn tag_block_without_arguments_defaults_to_empty_object() {
        let content = "<tool_call>{\"name\": \"task\"}</tool_call>";
        let parsed = parse_text_tool_calls(content);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "task");
        assert!(parsed.calls[0].arguments.is_object());
    }

    #[test]
    fn fenced_non_call_json_is_ignored() {
        // Example JSON without name/arguments must not be treated as a call.
        let content = "Here is a config:\n```json\n{\"path\": \"a.rs\"}\n```";
        let parsed = parse_text_tool_calls(content);
        assert!(parsed.calls.is_empty());
    }

    #[test]
    fn plain_prose_yields_no_calls() {
        let parsed = parse_text_tool_calls("I will read the file and report back.");
        assert!(parsed.calls.is_empty());
        assert_eq!(parsed.cleaned, "I will read the file and report back.");
    }

    #[test]
    fn parses_multiple_tag_blocks_with_distinct_ids() {
        let content = "<tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"a\"}}</tool_call>\
            <tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"b\"}}</tool_call>";
        let parsed = parse_text_tool_calls(content);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(parsed.calls[0].id, "call_0");
        assert_eq!(parsed.calls[1].id, "call_1");
        assert_eq!(parsed.calls[1].arguments["path"], "b");
    }
}
