//! Anthropic Messages transport (`POST {endpoint}/v1/messages`).
//!
//! The second language (Project 21): proof that a new transport is a
//! transport-layer change. It maps the shared [`ChatRequest`]/[`ChatResponse`]
//! types onto Anthropic's Messages API and back — the system prompt becomes the
//! top-level `system` field, tool definitions become `input_schema` tools, and
//! `tool_use`/`tool_result` content blocks map to the trait's [`ToolCall`] and
//! tool-role messages. Streaming consumes Anthropic's SSE event flow and
//! reassembles it into the same incremental [`ChatStream`] every other
//! transport produces.
//!
//! Everything from Projects 18–20 (consent-gated caps, auth/rate-limit errors,
//! the form's transport picker) applies automatically because it keys off
//! *having a key* and *the [`Transport`] trait*, not transport identity.

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::stream::StreamExt;
use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::types::{ChatRequest, ChatResponse, Message, Role, ToolCall, Usage};
use super::{lines_from, map_reqwest_error, ChatStream, Transport};

/// The Messages API requires `max_tokens`; this ceiling is generous for an
/// interactive coding turn while staying within every current model's limit.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// The Messages API version header value. Pinned, not derived, so a server
/// upgrade never silently changes the wire contract.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Talks to a single Anthropic Messages endpoint.
pub struct AnthropicTransport {
    client: reqwest::Client,
    endpoint: String,
    /// The `x-api-key` value, if any. A `None` key sends no key header (so the
    /// no-auth constructor exists for parity with the other transports, even
    /// though Anthropic itself requires a key).
    api_key: Option<String>,
    /// The owning provider's id, used only to attribute errors. Never affects
    /// the request.
    provider_id: String,
    /// The configured key env-var name (not the key), so an auth failure can
    /// name what to check.
    api_key_env: Option<String>,
}

impl AnthropicTransport {
    /// Create a transport for `endpoint` (e.g. `https://api.anthropic.com`) with
    /// no key.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_auth(endpoint, "anthropic", None, None)
    }

    /// Create a fully attributed transport: the `x-api-key` (when `Some`), the
    /// owning `provider_id` for error attribution, and the key env-var name for
    /// the auth-failure hint. The id and env name never affect the wire request.
    pub fn with_auth(
        endpoint: impl Into<String>,
        provider_id: impl Into<String>,
        key: Option<String>,
        key_env: Option<String>,
    ) -> Self {
        AnthropicTransport {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
            api_key: key.filter(|k| !k.is_empty()),
            provider_id: provider_id.into(),
            api_key_env: key_env,
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.endpoint.trim_end_matches('/'))
    }

    /// Start a POST to the messages endpoint, attaching the key and version
    /// headers. No key => no `x-api-key` header.
    fn post_messages(&self) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(self.messages_url())
            .header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(key) = &self.api_key {
            req = req.header("x-api-key", key);
        }
        req
    }

    /// Classify a non-success HTTP status into a typed, provider-attributed
    /// error, identical in spirit to the OpenAI transport's (20.2): the same
    /// status codes carry the same meaning across both languages, so the UI
    /// renders one set of actionable hints.
    fn status_error(&self, status: reqwest::StatusCode, model: &str) -> ProviderError {
        match status.as_u16() {
            401 | 403 => ProviderError::AuthFailed {
                provider: self.provider_id.clone(),
                key_env: self.api_key_env.clone(),
            },
            404 => ProviderError::ModelNotFound {
                provider: self.provider_id.clone(),
                model: model.to_string(),
            },
            429 => ProviderError::RateLimited(self.provider_id.clone()),
            other => ProviderError::RequestError(format!(
                "{} endpoint returned status {other}",
                self.provider_id
            )),
        }
    }

    fn body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        let (system, messages) = lower_messages(&request.messages);
        let mut body = serde_json::json!({
            "model": request.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "messages": messages,
            "stream": stream,
        });
        if !system.is_empty() {
            body["system"] = serde_json::Value::String(system);
        }
        if let Some(tools) = request.tools.as_deref() {
            let tools: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(tools);
            }
        }
        body
    }
}

/// Lower the shared messages onto Anthropic's `(system, messages)` shape:
/// system-role text is hoisted into the top-level `system` string; the rest
/// become `user`/`assistant` messages whose content is a block array. Tool-role
/// messages become `tool_result` blocks in a `user` message; assistant
/// `tool_calls` become `tool_use` blocks. Consecutive messages mapping to the
/// same role are merged, since the Messages API requires alternating roles.
fn lower_messages(messages: &[Message]) -> (String, Vec<serde_json::Value>) {
    let mut system = String::new();
    let mut out: Vec<(&'static str, Vec<serde_json::Value>)> = Vec::new();

    for msg in messages {
        let (role, blocks) = match msg.role {
            Role::System => {
                if !msg.content.is_empty() {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&msg.content);
                }
                continue;
            }
            Role::User => (
                "user",
                vec![text_block(&msg.content)]
                    .into_iter()
                    .flatten()
                    .collect(),
            ),
            Role::Assistant => {
                let mut blocks: Vec<serde_json::Value> =
                    text_block(&msg.content).into_iter().collect();
                for call in &msg.tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                ("assistant", blocks)
            }
            Role::Tool => {
                let block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content,
                });
                ("user", vec![block])
            }
        };

        if blocks.is_empty() {
            // An empty content array is rejected by the API; skip it.
            continue;
        }
        match out.last_mut() {
            Some((last_role, last_blocks)) if *last_role == role => last_blocks.extend(blocks),
            _ => out.push((role, blocks)),
        }
    }

    let messages = out
        .into_iter()
        .map(|(role, blocks)| serde_json::json!({ "role": role, "content": blocks }))
        .collect();
    (system, messages)
}

/// A single text content block, or `None` when the text is empty (so an empty
/// string never produces an empty block).
fn text_block(text: &str) -> Option<serde_json::Value> {
    (!text.is_empty()).then(|| serde_json::json!({ "type": "text", "text": text }))
}

// --- Non-streaming response ---

#[derive(Deserialize)]
struct WireContentBlock {
    #[serde(rename = "type")]
    kind: String,
    // text block
    #[serde(default)]
    text: String,
    // tool_use block
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

/// Anthropic's `usage` block. `input_tokens` is the prompt cost,
/// `output_tokens` what the model generated. Present on the non-streaming
/// response, in `message_start` (input), and in `message_delta` (final output).
#[derive(Deserialize, Clone, Copy, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: usize,
    #[serde(default)]
    output_tokens: usize,
}

impl From<WireUsage> for Usage {
    fn from(w: WireUsage) -> Self {
        Usage {
            prompt_tokens: w.input_tokens,
            completion_tokens: w.output_tokens,
        }
    }
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    content: Vec<WireContentBlock>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

fn parse_full(text: &str) -> Result<ChatResponse> {
    let wire: WireResponse = serde_json::from_str(text)
        .map_err(|e| ProviderError::ParseError(format!("anthropic response: {e}")))?;
    let usage = wire.usage.map(Into::into);
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in wire.content {
        match block.kind.as_str() {
            "text" => content.push_str(&block.text),
            "tool_use" => tool_calls.push(ToolCall {
                id: block
                    .id
                    .unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                name: block.name.unwrap_or_default(),
                arguments: block.input.unwrap_or_else(|| serde_json::json!({})),
            }),
            _ => {}
        }
    }
    Ok(ChatResponse {
        content,
        reasoning: String::new(),
        tool_calls,
        done: true,
        usage,
    })
}

// --- Streaming (SSE) response ---

#[derive(Deserialize)]
struct WireStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    content_block: Option<WireBlockStart>,
    #[serde(default)]
    delta: Option<WireStreamDelta>,
    /// `message_start` nests the opening message (whose `usage` carries the
    /// prompt token count).
    #[serde(default)]
    message: Option<WireMessageStart>,
    /// `message_delta` carries the running `usage` (final output token count).
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireMessageStart {
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireBlockStart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct WireStreamDelta {
    #[serde(rename = "type")]
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    /// Extended-thinking text, carried by a `thinking_delta`. Present only when
    /// the request enabled thinking; harmless to accept otherwise.
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

/// A tool-use block being assembled across stream events: its id/name arrive in
/// `content_block_start`, its JSON arguments arrive as `input_json_delta`
/// fragments to be concatenated and parsed once.
struct ToolAccum {
    id: String,
    name: String,
    json: String,
}

/// Parse an assembled `input_json_delta` string into a JSON value, falling back
/// to an empty object for a tool call that streamed no arguments.
fn parse_tool_input(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

#[async_trait]
impl Transport for AnthropicTransport {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let resp = self
            .post_messages()
            .json(&self.body(&request, false))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Err(self.status_error(resp.status(), &request.model).into());
        }
        let text = resp.text().await.map_err(map_reqwest_error)?;
        parse_full(&text)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let resp = self
            .post_messages()
            .json(&self.body(&request, true))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Err(self.status_error(resp.status(), &request.model).into());
        }

        // Stream state: in-progress tool blocks (by content index), accumulated
        // token usage, and whether the terminal done-chunk has been emitted.
        struct State<S> {
            lines: S,
            tools: BTreeMap<usize, ToolAccum>,
            input_tokens: usize,
            output_tokens: usize,
            prompt_reported: bool,
            done: bool,
        }

        let stream = futures::stream::unfold(
            State {
                // Box-pin the line stream so it is `Unpin` and can be polled with
                // `.next()` from inside the unfold state.
                lines: Box::pin(lines_from(resp.bytes_stream())),
                tools: BTreeMap::new(),
                input_tokens: 0,
                output_tokens: 0,
                prompt_reported: false,
                done: false,
            },
            |mut st| async move {
                loop {
                    let Some(line) = st.lines.next().await else {
                        // Stream ended; emit a final done chunk once, carrying
                        // any assembled tool calls, in case `message_stop` was
                        // never seen.
                        if st.done {
                            return None;
                        }
                        st.done = true;
                        let usage = accumulated_usage(st.input_tokens, st.output_tokens);
                        return Some((Ok(final_chunk(&mut st.tools, usage)), st));
                    };
                    let line = match line {
                        Ok(l) => l,
                        Err(e) => return Some((Err(e), st)),
                    };
                    // SSE carries `event:` and `data:` lines; only `data:` holds
                    // the JSON payload (which itself names its `type`).
                    let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                        continue;
                    };
                    let event: WireStreamEvent = match serde_json::from_str(payload) {
                        Ok(ev) => ev,
                        Err(e) => {
                            let err = ProviderError::ParseError(format!("anthropic event: {e}"));
                            return Some((Err(err.into()), st));
                        }
                    };
                    match event.kind.as_str() {
                        "message_start" => {
                            if let Some(usage) = event.message.and_then(|m| m.usage) {
                                st.input_tokens = usage.input_tokens;
                                st.output_tokens = usage.output_tokens;
                                // Anthropic reports the real prompt size here,
                                // before the body streams. Surface it as a
                                // mid-stream preview (`done: false`) so the UI
                                // shows exact input from the first chunk instead
                                // of riding its estimate; the terminal chunk still
                                // carries the final input+output counts.
                                if st.input_tokens > 0 && !st.prompt_reported {
                                    st.prompt_reported = true;
                                    let preview = ChatResponse {
                                        content: String::new(),
                                        reasoning: String::new(),
                                        tool_calls: Vec::new(),
                                        done: false,
                                        usage: Some(Usage {
                                            prompt_tokens: st.input_tokens,
                                            completion_tokens: 0,
                                        }),
                                    };
                                    return Some((Ok(preview), st));
                                }
                            }
                        }
                        "message_delta" => {
                            // Carries the cumulative final output count (and
                            // occasionally a refined input count).
                            if let Some(usage) = event.usage {
                                st.output_tokens = usage.output_tokens;
                                if usage.input_tokens > 0 {
                                    st.input_tokens = usage.input_tokens;
                                }
                            }
                        }
                        "content_block_start" => {
                            if let Some(block) = event.content_block {
                                if block.kind == "tool_use" {
                                    st.tools.insert(
                                        event.index,
                                        ToolAccum {
                                            id: block
                                                .id
                                                .unwrap_or_else(|| format!("call_{}", event.index)),
                                            name: block.name.unwrap_or_default(),
                                            json: String::new(),
                                        },
                                    );
                                }
                            }
                        }
                        "content_block_delta" => {
                            if let Some(delta) = event.delta {
                                match delta.kind.as_deref() {
                                    Some("text_delta") => {
                                        let text = delta.text.unwrap_or_default();
                                        if !text.is_empty() {
                                            let chunk = ChatResponse {
                                                content: text,
                                                reasoning: String::new(),
                                                tool_calls: Vec::new(),
                                                done: false,
                                                usage: None,
                                            };
                                            return Some((Ok(chunk), st));
                                        }
                                    }
                                    Some("thinking_delta") => {
                                        let thinking = delta.thinking.unwrap_or_default();
                                        if !thinking.is_empty() {
                                            let chunk = ChatResponse {
                                                content: String::new(),
                                                reasoning: thinking,
                                                tool_calls: Vec::new(),
                                                done: false,
                                                usage: None,
                                            };
                                            return Some((Ok(chunk), st));
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let (Some(acc), Some(frag)) =
                                            (st.tools.get_mut(&event.index), delta.partial_json)
                                        {
                                            acc.json.push_str(&frag);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "message_stop" => {
                            if st.done {
                                return None;
                            }
                            st.done = true;
                            let usage = accumulated_usage(st.input_tokens, st.output_tokens);
                            return Some((Ok(final_chunk(&mut st.tools, usage)), st));
                        }
                        _ => {}
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }
}

/// Build a [`Usage`] from accumulated stream counts, or `None` when nothing was
/// reported (so the agent falls back to its estimate).
fn accumulated_usage(input_tokens: usize, output_tokens: usize) -> Option<Usage> {
    (input_tokens > 0 || output_tokens > 0).then_some(Usage {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
    })
}

/// Drain the assembled tool blocks into the terminal `done` chunk, carrying the
/// turn's token `usage`. Blocks are emitted in content-index order so a
/// multi-tool turn keeps the model's order.
fn final_chunk(tools: &mut BTreeMap<usize, ToolAccum>, usage: Option<Usage>) -> ChatResponse {
    let tool_calls = std::mem::take(tools)
        .into_values()
        .map(|acc| ToolCall {
            id: acc.id,
            name: acc.name,
            arguments: parse_tool_input(&acc.json),
        })
        .collect();
    ChatResponse {
        content: String::new(),
        reasoning: String::new(),
        tool_calls,
        done: true,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockServer;
    use crate::transport::types::{Message, Role, ToolDefinition};

    fn request() -> ChatRequest {
        ChatRequest::new("claude-x", vec![Message::text(Role::User, "hi")])
    }

    #[tokio::test]
    async fn standard_completion_returns_content() {
        let server = MockServer::json(
            r#"{"content":[{"type":"text","text":"hi back"}],"stop_reason":"end_turn"}"#,
        );
        let transport = AnthropicTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.content, "hi back");
        assert!(resp.done);
    }

    #[tokio::test]
    async fn non_streaming_response_carries_usage() {
        let server = MockServer::json(
            r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":300,"output_tokens":15}}"#,
        );
        let transport = AnthropicTransport::new(server.endpoint());
        let usage = transport.chat(request()).await.unwrap().usage.unwrap();
        assert_eq!(usage.prompt_tokens, 300);
        assert_eq!(usage.completion_tokens, 15);
    }

    #[tokio::test]
    async fn streaming_combines_input_and_output_usage() {
        // input_tokens arrive in message_start; the final output count in
        // message_delta. They combine onto the terminal chunk.
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":300,\"output_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let server = MockServer::sse(body);
        let transport = AnthropicTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut usage = None;
        while let Some(item) = stream.next().await {
            if let Some(u) = item.unwrap().usage {
                usage = Some(u);
            }
        }
        let usage = usage.expect("terminal chunk should carry usage");
        assert_eq!(usage.prompt_tokens, 300);
        assert_eq!(usage.completion_tokens, 15);
    }

    #[tokio::test]
    async fn streaming_previews_the_prompt_size_before_the_body() {
        // message_start carries the real input count; the transport surfaces it
        // immediately as a mid-stream (`done: false`) chunk so the UI can show
        // exact input before any text streams.
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":300,\"output_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let server = MockServer::sse(body);
        let transport = AnthropicTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        // The first chunk previews the prompt: input only, no body, not done.
        let first = stream.next().await.unwrap().unwrap();
        assert!(first.content.is_empty());
        assert!(!first.done);
        let preview = first.usage.expect("preview carries the prompt size");
        assert_eq!(preview.prompt_tokens, 300);
        assert_eq!(preview.completion_tokens, 0);

        // The prompt is previewed exactly once; no further input-only previews
        // follow, and the terminal chunk still carries the full input+output.
        let mut previews = 0;
        let mut terminal = None;
        while let Some(item) = stream.next().await {
            if let Some(u) = item.unwrap().usage {
                if u.completion_tokens == 0 {
                    previews += 1;
                } else {
                    terminal = Some(u);
                }
            }
        }
        assert_eq!(
            previews, 0,
            "no further input-only previews after the first"
        );
        let terminal = terminal.expect("terminal chunk carries final usage");
        assert_eq!(terminal.prompt_tokens, 300);
        assert_eq!(terminal.completion_tokens, 15);
    }

    #[tokio::test]
    async fn tool_use_in_response_is_parsed() {
        let server = MockServer::json(
            r#"{"content":[{"type":"text","text":"let me read"},{"type":"tool_use","id":"toolu_1","name":"read","input":{"path":"a.rs"}}],"stop_reason":"tool_use"}"#,
        );
        let transport = AnthropicTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.content, "let me read");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_1");
        assert_eq!(resp.tool_calls[0].name, "read");
        assert_eq!(resp.tool_calls[0].arguments["path"], "a.rs");
    }

    #[tokio::test]
    async fn keyed_chat_sends_key_and_version_headers() {
        let server = MockServer::json(r#"{"content":[{"type":"text","text":"ok"}]}"#);
        let transport = AnthropicTransport::with_auth(
            server.endpoint(),
            "anthropic",
            Some("sk-ant".into()),
            None,
        );
        transport.chat(request()).await.unwrap();
        assert_eq!(
            server.received_header("x-api-key").as_deref(),
            Some("sk-ant")
        );
        assert_eq!(
            server.received_header("anthropic-version").as_deref(),
            Some("2023-06-01")
        );
    }

    #[tokio::test]
    async fn unkeyed_chat_sends_no_key_header() {
        let server = MockServer::json(r#"{"content":[{"type":"text","text":"ok"}]}"#);
        let transport = AnthropicTransport::new(server.endpoint());
        transport.chat(request()).await.unwrap();
        assert_eq!(server.received_header("x-api-key"), None);
    }

    #[tokio::test]
    async fn system_prompt_is_hoisted_and_tools_become_input_schema() {
        let server = MockServer::json(r#"{"content":[{"type":"text","text":"ok"}]}"#);
        let transport = AnthropicTransport::new(server.endpoint());
        let mut req = ChatRequest::new(
            "claude-x",
            vec![
                Message::text(Role::System, "be terse"),
                Message::text(Role::User, "hi"),
            ],
        );
        req.tools = Some(vec![ToolDefinition {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type":"object"}),
        }]);
        // The body builder runs inside chat(); assert on its shape directly.
        let body = transport.body(&req, false);
        assert_eq!(body["system"], "be terse");
        // The system message is not in the messages array (only the user turn).
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        transport.chat(req).await.unwrap();
    }

    #[test]
    fn tool_result_and_tool_use_round_trip_into_blocks() {
        // assistant tool_use → tool_use block; tool role → tool_result block in
        // a user message; consecutive tool results merge into one user message.
        let messages = vec![
            Message::text(Role::User, "do it"),
            Message {
                role: Role::Assistant,
                content: "calling".into(),
                tool_calls: vec![ToolCall {
                    id: "toolu_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.rs"}),
                }],
                tool_call_id: None,
            },
            Message {
                role: Role::Tool,
                content: "file contents".into(),
                tool_calls: Vec::new(),
                tool_call_id: Some("toolu_1".into()),
            },
        ];
        let (system, msgs) = lower_messages(&messages);
        assert!(system.is_empty());
        assert_eq!(msgs.len(), 3);
        // assistant message carries text + tool_use.
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "text");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["id"], "toolu_1");
        // tool result becomes a tool_result block in a user message.
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[tokio::test]
    async fn sse_stream_assembles_text_then_tool_call() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"He\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let server = MockServer::sse(body);
        let transport = AnthropicTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut chunks = Vec::new();
        while let Some(item) = stream.next().await {
            chunks.push(item.unwrap());
        }
        // Two text deltas, then a terminal done chunk carrying the tool call.
        assert_eq!(chunks[0].content, "He");
        assert_eq!(chunks[1].content, "llo");
        let last = chunks.last().unwrap();
        assert!(last.done);
        assert_eq!(last.tool_calls.len(), 1);
        assert_eq!(last.tool_calls[0].name, "read");
        assert_eq!(last.tool_calls[0].arguments["path"], "a.rs");
    }

    #[tokio::test]
    async fn status_401_classifies_as_auth_failed() {
        let server = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let transport = AnthropicTransport::with_auth(
            server.endpoint(),
            "anthropic",
            Some("sk-bad".into()),
            Some("ANTHROPIC_API_KEY".into()),
        );
        let err = transport.chat(request()).await.unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::AuthFailed { provider, key_env }) => {
                assert_eq!(provider, "anthropic");
                assert_eq!(key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_429_classifies_as_rate_limited() {
        let server = MockServer::json_status(429, r#"{"error":"slow down"}"#);
        let transport = AnthropicTransport::with_auth(server.endpoint(), "anthropic", None, None);
        let err = transport.chat(request()).await.unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::RateLimited(id)) if id == "anthropic"
        ));
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let transport = AnthropicTransport::new("http://127.0.0.1:1");
        let err = transport.chat(request()).await.unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }
}
