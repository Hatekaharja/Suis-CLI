//! OpenAI-compatible transport (`POST {endpoint}/v1/chat/completions`).
//!
//! Reusable for LM Studio and any other OpenAI-compatible server. Streaming
//! uses Server-Sent Events: lines prefixed with `data: `, terminated by a
//! `data: [DONE]` sentinel.

use async_trait::async_trait;
use futures::stream::StreamExt;
use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::types::{ChatRequest, ChatResponse, Message, ToolCall, Usage};
use super::{
    lines_from, map_reqwest_error, parse_tool_arguments, tool_to_function_json, ChatStream,
    Transport,
};

/// Talks to a single OpenAI-compatible endpoint.
pub struct OpenAiTransport {
    client: reqwest::Client,
    endpoint: String,
    /// Resolved bearer token sent as `Authorization: Bearer <key>`, if any.
    bearer: Option<String>,
    /// The owning provider's id, used only to attribute remote errors. Never
    /// affects the request.
    provider_id: String,
    /// The configured key env-var name (not the key), so an auth failure can
    /// name what to check.
    api_key_env: Option<String>,
}

impl OpenAiTransport {
    /// Create a transport for `endpoint` (e.g. `http://localhost:1234`) with no
    /// authentication — requests are byte-identical to a local provider's.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_auth(endpoint, "provider", None, None)
    }

    /// Create a transport that sends `Authorization: Bearer <key>` when `key`
    /// is `Some`. A `None` key behaves exactly like [`new`](Self::new).
    pub fn with_key(endpoint: impl Into<String>, key: Option<String>) -> Self {
        Self::with_auth(endpoint, "provider", key, None)
    }

    /// Create a fully attributed transport: the bearer key (when `Some`), the
    /// owning `provider_id` for error attribution, and the key env-var name for
    /// the auth-failure hint. The id and env name never affect the wire request.
    pub fn with_auth(
        endpoint: impl Into<String>,
        provider_id: impl Into<String>,
        key: Option<String>,
        key_env: Option<String>,
    ) -> Self {
        OpenAiTransport {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
            bearer: key.filter(|k| !k.is_empty()),
            provider_id: provider_id.into(),
            api_key_env: key_env,
        }
    }

    /// Classify a non-success HTTP status into a typed, provider-attributed
    /// error (20.2). Classification is status-code based, not body-parsing, so
    /// it stays portable across OpenAI-compatible implementations. `model` is
    /// named in the not-found case. A 5xx (or any other) keeps the generic path.
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

    fn chat_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.endpoint.trim_end_matches('/')
        )
    }

    /// Start a POST to the chat endpoint, attaching the bearer token when one is
    /// configured. No key => no header.
    fn post_chat(&self) -> reqwest::RequestBuilder {
        let req = self.client.post(self.chat_url());
        match &self.bearer {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    fn body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request.messages.iter().map(out_message).collect();
        let tools: Vec<serde_json::Value> = request
            .tools
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(tool_to_function_json)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }
        // Streaming omits the usage block by default; opting in makes OpenAI
        // emit a final chunk whose `usage` carries the turn's token counts.
        if stream {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
        body
    }
}

/// Lower a [`Message`] to the OpenAI chat-message shape. Plain messages are
/// `{role, content}`; an assistant turn that called tools carries `tool_calls`,
/// and a tool result carries the `tool_call_id` it answers. Omitting either of
/// the latter makes OpenAI reject the follow-up turn with a 400, so a tool round
/// trip depends on both being sent.
fn out_message(m: &Message) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "role": m.role.as_str(),
        "content": m.content,
    });
    if !m.tool_calls.is_empty() {
        obj["tool_calls"] =
            serde_json::Value::Array(m.tool_calls.iter().map(out_tool_call).collect());
    }
    if let Some(id) = &m.tool_call_id {
        obj["tool_call_id"] = serde_json::Value::String(id.clone());
    }
    obj
}

/// Lower a [`ToolCall`] to OpenAI's request shape, where `arguments` is a
/// JSON-encoded *string* rather than an object.
fn out_tool_call(call: &ToolCall) -> serde_json::Value {
    serde_json::json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
        }
    })
}

#[derive(Deserialize)]
struct WireFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    function: WireFunction,
}

/// Parse OpenAI's stringified tool-call arguments into a JSON value. Delegates
/// to the shared tolerant parser so fenced/slightly-malformed argument strings
/// from weaker models still resolve to a usable object.
fn parse_arguments(raw: &str) -> serde_json::Value {
    parse_tool_arguments(raw)
}

fn convert_tool_calls(calls: Vec<WireToolCall>) -> Vec<ToolCall> {
    calls
        .into_iter()
        .enumerate()
        .map(|(i, tc)| ToolCall {
            id: tc.id.unwrap_or_else(|| format!("call_{i}")),
            name: tc.function.name,
            arguments: parse_arguments(&tc.function.arguments),
        })
        .collect()
}

// --- Non-streaming response ---

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
}

/// OpenAI's `usage` block (`{prompt_tokens, completion_tokens, total_tokens}`),
/// present on non-streaming responses and the final streamed chunk.
#[derive(Deserialize, Clone, Copy)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: usize,
    #[serde(default)]
    completion_tokens: usize,
}

impl From<WireUsage> for Usage {
    fn from(w: WireUsage) -> Self {
        Usage {
            prompt_tokens: w.prompt_tokens,
            completion_tokens: w.completion_tokens,
        }
    }
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

fn parse_full(text: &str) -> Result<ChatResponse> {
    let wire: WireResponse = serde_json::from_str(text)
        .map_err(|e| ProviderError::ParseError(format!("openai response: {e}")))?;
    let usage = wire.usage.map(Into::into);
    let choice = wire
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::ParseError("openai response had no choices".into()))?;
    Ok(ChatResponse {
        content: choice.message.content.unwrap_or_default(),
        reasoning: String::new(),
        tool_calls: convert_tool_calls(choice.message.tool_calls),
        done: true,
        usage,
    })
}

// --- Streaming (SSE) response ---
//
// OpenAI streams a single tool call across many deltas: the first carries the
// `index`, `id`, and `function.name` with empty `arguments`; later deltas carry
// only that `index` and the next slice of the (stringified) `arguments`. They
// must be reassembled by index before the call is usable — emitting per-delta
// would yield a named call with no arguments followed by a flock of nameless
// ones. Content deltas, by contrast, are streamed straight through.

/// One streaming tool-call fragment, addressed by its `index` within the turn.
#[derive(Deserialize)]
struct WireStreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: WireStreamFunction,
}

#[derive(Deserialize, Default)]
struct WireStreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning text on a side channel. DeepSeek and Ollama's OpenAI-compatible
    /// endpoint use `reasoning_content`; OpenRouter and others use `reasoning`.
    /// Accept either.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireStreamToolCall>,
}

#[derive(Deserialize)]
struct WireStreamChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
    /// The final streamed chunk (with `stream_options.include_usage`) carries
    /// `usage` and an empty `choices` array.
    #[serde(default)]
    usage: Option<WireUsage>,
}

/// One in-progress tool call being reassembled across streaming deltas.
#[derive(Default)]
struct ToolCallBuilder {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// The relevant parts of one parsed SSE chunk.
#[derive(Default)]
struct ParsedChunk {
    /// Text delta for this chunk (may be empty).
    content: String,
    /// Reasoning delta for this chunk (may be empty).
    reasoning: String,
    /// Tool-call fragments to fold into the builders.
    fragments: Vec<WireStreamToolCall>,
    /// Whether the model signalled the turn is finished.
    finished: bool,
    /// Token usage, present only on the final usage-only chunk.
    usage: Option<Usage>,
}

/// Parse one SSE `data:` payload into its content/tool/finish/usage parts.
/// `None` means the line carried nothing actionable (e.g. a keep-alive) and
/// should be skipped. The terminal usage-only chunk (empty `choices`, a `usage`
/// block) is surfaced so its counts reach the final response.
fn parse_stream_chunk(payload: &str) -> Result<Option<ParsedChunk>> {
    let chunk: WireStreamChunk = serde_json::from_str(payload)
        .map_err(|e| ProviderError::ParseError(format!("openai chunk: {e}")))?;
    let usage = chunk.usage.map(Into::into);
    match chunk.choices.into_iter().next() {
        Some(choice) => Ok(Some(ParsedChunk {
            content: choice.delta.content.unwrap_or_default(),
            reasoning: choice
                .delta
                .reasoning_content
                .or(choice.delta.reasoning)
                .unwrap_or_default(),
            fragments: choice.delta.tool_calls,
            finished: choice.finish_reason.is_some(),
            usage,
        })),
        // No choice: only meaningful when it carried usage; otherwise skip.
        None if usage.is_some() => Ok(Some(ParsedChunk {
            usage,
            ..ParsedChunk::default()
        })),
        None => Ok(None),
    }
}

/// Fold one tool-call fragment into `builders`, keyed by its `index`: a present
/// `id`/`name` sets the slot, and `arguments` slices are appended in order.
fn accumulate(builders: &mut Vec<ToolCallBuilder>, frag: WireStreamToolCall) {
    if builders.len() <= frag.index {
        builders.resize_with(frag.index + 1, ToolCallBuilder::default);
    }
    let slot = &mut builders[frag.index];
    if let Some(id) = frag.id.filter(|s| !s.is_empty()) {
        slot.id = Some(id);
    }
    if let Some(name) = frag.function.name.filter(|s| !s.is_empty()) {
        slot.name = name;
    }
    if let Some(args) = frag.function.arguments {
        slot.arguments.push_str(&args);
    }
}

/// Finalize the accumulated builders into tool calls, dropping any slot that
/// never received a name (a stray fragment) and parsing each call's joined
/// argument string.
fn assemble(builders: Vec<ToolCallBuilder>) -> Vec<ToolCall> {
    builders
        .into_iter()
        .enumerate()
        .filter(|(_, b)| !b.name.is_empty())
        .map(|(i, b)| ToolCall {
            id: b.id.unwrap_or_else(|| format!("call_{i}")),
            name: b.name,
            arguments: parse_arguments(&b.arguments),
        })
        .collect()
}

/// A boxed stream of decoded SSE text lines.
type LineStream = std::pin::Pin<Box<dyn futures::stream::Stream<Item = Result<String>> + Send>>;

/// The reassembly state threaded through the streaming `unfold`.
struct StreamState {
    lines: LineStream,
    builders: Vec<ToolCallBuilder>,
    /// Set once a `finish_reason` has been seen. The terminal chunk is held back
    /// until `[DONE]` or the stream end, so the trailing usage-only chunk (which
    /// OpenAI sends *after* the finish reason) is captured first.
    saw_finish: bool,
    /// Token usage from the final usage chunk, attached to the terminal chunk.
    usage: Option<Usage>,
    /// Set once the terminal chunk has been emitted, ending the stream.
    flushed: bool,
}

#[async_trait]
impl Transport for OpenAiTransport {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let resp = self
            .post_chat()
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
            .post_chat()
            .json(&self.body(&request, true))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Err(self.status_error(resp.status(), &request.model).into());
        }
        let state = StreamState {
            lines: Box::pin(lines_from(resp.bytes_stream())),
            builders: Vec::new(),
            saw_finish: false,
            usage: None,
            flushed: false,
        };
        // SSE with tool-call reassembly: content deltas pass straight through,
        // tool-call fragments accumulate by index, and the assembled calls plus
        // any reported usage are emitted as one terminal chunk at `[DONE]` (or
        // the stream end). Empty-choice keep-alives are skipped; the usage-only
        // chunk is captured rather than skipped.
        let stream = futures::stream::unfold(state, |mut st| async move {
            loop {
                if st.flushed {
                    return None;
                }
                match st.lines.next().await {
                    None => {
                        // The body ended without a `[DONE]`; flush a terminal
                        // chunk if the turn produced anything, else just stop.
                        st.flushed = true;
                        let calls = assemble(std::mem::take(&mut st.builders));
                        if calls.is_empty() && st.usage.is_none() && !st.saw_finish {
                            return None;
                        }
                        return Some((
                            Ok(ChatResponse {
                                content: String::new(),
                                reasoning: String::new(),
                                tool_calls: calls,
                                done: true,
                                usage: st.usage,
                            }),
                            st,
                        ));
                    }
                    Some(Err(e)) => {
                        st.flushed = true;
                        return Some((Err(e), st));
                    }
                    Some(Ok(line)) => {
                        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                            continue;
                        };
                        if payload == "[DONE]" {
                            // Now that the trailing usage chunk has been seen,
                            // emit the terminal chunk carrying calls and usage.
                            st.flushed = true;
                            let calls = assemble(std::mem::take(&mut st.builders));
                            return Some((
                                Ok(ChatResponse {
                                    content: String::new(),
                                    reasoning: String::new(),
                                    tool_calls: calls,
                                    done: true,
                                    usage: st.usage,
                                }),
                                st,
                            ));
                        }
                        match parse_stream_chunk(payload) {
                            Err(e) => {
                                st.flushed = true;
                                return Some((Err(e), st));
                            }
                            Ok(None) => continue,
                            Ok(Some(parsed)) => {
                                for frag in parsed.fragments {
                                    accumulate(&mut st.builders, frag);
                                }
                                if parsed.finished {
                                    st.saw_finish = true;
                                }
                                if let Some(usage) = parsed.usage {
                                    st.usage = Some(usage);
                                }
                                if !parsed.content.is_empty() || !parsed.reasoning.is_empty() {
                                    return Some((
                                        Ok(ChatResponse {
                                            content: parsed.content,
                                            reasoning: parsed.reasoning,
                                            tool_calls: Vec::new(),
                                            done: false,
                                            usage: None,
                                        }),
                                        st,
                                    ));
                                }
                                // Nothing to surface this line (a tool fragment,
                                // finish marker, or captured usage); pull next.
                                continue;
                            }
                        }
                    }
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockServer;
    use crate::transport::types::{Message, Role};

    fn request() -> ChatRequest {
        ChatRequest::new("gpt", vec![Message::text(Role::User, "hi")])
    }

    #[tokio::test]
    async fn standard_completion_returns_content() {
        let server = MockServer::json(
            r#"{"choices":[{"message":{"role":"assistant","content":"hi back"},"finish_reason":"stop"}]}"#,
        );
        let transport = OpenAiTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.content, "hi back");
        assert!(resp.done);
    }

    #[tokio::test]
    async fn non_streaming_response_carries_usage() {
        let server = MockServer::json(
            r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":8,"total_tokens":128}}"#,
        );
        let transport = OpenAiTransport::new(server.endpoint());
        let usage = transport.chat(request()).await.unwrap().usage.unwrap();
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 8);
    }

    #[tokio::test]
    async fn streaming_captures_trailing_usage_chunk() {
        // OpenAI sends the usage block in a final, choice-less chunk *after* the
        // finish_reason chunk; it must land on the terminal response.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        );
        let server = MockServer::sse(body);
        let transport = OpenAiTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut content = String::new();
        let mut usage = None;
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            content.push_str(&chunk.content);
            if let Some(u) = chunk.usage {
                usage = Some(u);
            }
        }
        assert_eq!(content, "hi");
        let usage = usage.expect("terminal chunk should carry usage");
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.completion_tokens, 4);
    }

    #[tokio::test]
    async fn sse_stream_handles_done_marker() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"he\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let server = MockServer::sse(body);
        let transport = OpenAiTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut chunks = Vec::new();
        while let Some(item) = stream.next().await {
            chunks.push(item.unwrap());
        }
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].content, "he");
        assert_eq!(chunks[1].content, "llo");
        assert!(chunks[2].done);
    }

    #[tokio::test]
    async fn tool_call_in_response_is_parsed() {
        let server = MockServer::json(
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call_x","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let transport = OpenAiTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        // Empty content + tool call must not panic.
        assert_eq!(resp.content, "");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "call_x");
        assert_eq!(resp.tool_calls[0].name, "read");
        assert_eq!(resp.tool_calls[0].arguments["path"], "a.rs");
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let transport = OpenAiTransport::new("http://127.0.0.1:1");
        let err = transport.chat(request()).await.unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn keyed_chat_sends_authorization_header() {
        let server = MockServer::json(
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        );
        let transport = OpenAiTransport::with_key(server.endpoint(), Some("sk-test-123".into()));
        transport.chat(request()).await.unwrap();
        assert_eq!(
            server.received_header("authorization").as_deref(),
            Some("Bearer sk-test-123")
        );
    }

    #[tokio::test]
    async fn status_401_classifies_as_auth_failed_with_provider_and_env() {
        let server = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let transport = OpenAiTransport::with_auth(
            server.endpoint(),
            "openrouter",
            Some("sk-bad".into()),
            Some("OPENROUTER_API_KEY".into()),
        );
        let err = transport.chat(request()).await.unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::AuthFailed { provider, key_env }) => {
                assert_eq!(provider, "openrouter");
                assert_eq!(key_env.as_deref(), Some("OPENROUTER_API_KEY"));
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_429_classifies_as_rate_limited() {
        let server = MockServer::json_status(429, r#"{"error":"slow down"}"#);
        let transport = OpenAiTransport::with_auth(server.endpoint(), "openrouter", None, None);
        let err = transport.chat(request()).await.unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::RateLimited(id)) => {
                assert_eq!(id, "openrouter");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_404_classifies_as_model_not_found_naming_the_model() {
        let server = MockServer::json_status(404, r#"{"error":"no such model"}"#);
        let transport = OpenAiTransport::with_auth(server.endpoint(), "openrouter", None, None);
        let err = transport.chat(request()).await.unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::ModelNotFound { provider, model }) => {
                assert_eq!(provider, "openrouter");
                // `request()` asks for model "gpt".
                assert_eq!(model, "gpt");
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_500_keeps_the_generic_path() {
        let server = MockServer::json_status(500, r#"{"error":"boom"}"#);
        let transport = OpenAiTransport::with_auth(server.endpoint(), "openrouter", None, None);
        let err = transport.chat(request()).await.unwrap_err();
        assert!(
            matches!(
                err,
                suis_core::Error::Provider(ProviderError::RequestError(_))
            ),
            "5xx stays generic, got {err:?}"
        );
    }

    #[tokio::test]
    async fn streaming_classifies_status_too() {
        let server = MockServer::json_status(429, r#"{"error":"slow down"}"#);
        let transport = OpenAiTransport::with_auth(server.endpoint(), "openrouter", None, None);
        match transport.chat_stream(request()).await {
            Err(suis_core::Error::Provider(ProviderError::RateLimited(id))) => {
                assert_eq!(id, "openrouter");
            }
            Err(other) => panic!("expected RateLimited, got {other:?}"),
            Ok(_) => panic!("expected an error from a 429 response"),
        }
    }

    #[tokio::test]
    async fn unkeyed_chat_sends_no_authorization_header() {
        let server = MockServer::json(
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        );
        let transport = OpenAiTransport::new(server.endpoint());
        transport.chat(request()).await.unwrap();
        assert_eq!(server.received_header("authorization"), None);
    }

    #[tokio::test]
    async fn streaming_tool_call_is_reassembled_from_fragments() {
        // OpenAI splits one tool call over several deltas: name+id first, then
        // the argument string in slices. They must collapse into one call.
        let body = [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ]
        .join("\n\n")
            + "\n\n";
        let server = MockServer::sse(&body);
        let transport = OpenAiTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut calls = Vec::new();
        while let Some(item) = stream.next().await {
            calls.extend(item.unwrap().tool_calls);
        }
        assert_eq!(calls.len(), 1, "the fragments collapse into one call");
        assert_eq!(calls[0].id, "call_x");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "README.md");
    }

    #[tokio::test]
    async fn streaming_interleaves_content_then_a_tool_call() {
        // Text streams through verbatim; the trailing tool call is reassembled.
        let body = [
            r#"data: {"choices":[{"delta":{"content":"reading"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ]
        .join("\n\n")
            + "\n\n";
        let server = MockServer::sse(&body);
        let transport = OpenAiTransport::new(server.endpoint());
        let mut stream = transport.chat_stream(request()).await.unwrap();

        let mut content = String::new();
        let mut calls = Vec::new();
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            content.push_str(&chunk.content);
            calls.extend(chunk.tool_calls);
        }
        assert_eq!(content, "reading");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
    }

    #[test]
    fn assistant_tool_calls_and_tool_results_serialize_for_openai() {
        // The follow-up turn must carry the assistant's `tool_calls` and link
        // each tool result by `tool_call_id`, or OpenAI rejects it with a 400.
        let request = ChatRequest {
            model: "gpt".into(),
            messages: vec![
                Message::text(Role::User, "read it"),
                Message {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_x".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({ "path": "README.md" }),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "file contents".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call_x".into()),
                },
            ],
            tools: None,
            stream: false,
        };
        let transport = OpenAiTransport::new("http://x");
        let body = transport.body(&request, false);
        let msgs = body["messages"].as_array().unwrap();

        // A plain user message stays {role, content}.
        assert!(msgs[0].get("tool_calls").is_none());

        // The assistant turn carries the call in OpenAI's function shape, with
        // arguments as a JSON-encoded *string*.
        let tc = &msgs[1]["tool_calls"][0];
        assert_eq!(tc["id"], "call_x");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "read");
        assert_eq!(tc["function"]["arguments"], r#"{"path":"README.md"}"#);

        // The tool result is linked back by id.
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_x");
        assert_eq!(msgs[2]["content"], "file contents");
    }
}
