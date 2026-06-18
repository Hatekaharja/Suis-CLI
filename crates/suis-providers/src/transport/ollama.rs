//! Ollama-native transport (`POST {endpoint}/api/chat`).
//!
//! Streaming responses are newline-delimited JSON (NDJSON): one JSON object
//! per line, the last carrying `"done": true`.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::stream::StreamExt;
use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::types::{ChatRequest, ChatResponse, Message, ToolCall, Usage};
use super::{
    lines_from, map_reqwest_error, normalize_arguments, tool_to_function_json, ChatStream,
    Transport,
};

/// Talks to a single Ollama endpoint.
pub struct OllamaTransport {
    client: reqwest::Client,
    endpoint: String,
}

impl OllamaTransport {
    /// Create a transport for `endpoint` (e.g. `http://localhost:11434`).
    pub fn new(endpoint: impl Into<String>) -> Self {
        OllamaTransport {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.endpoint.trim_end_matches('/'))
    }

    fn body(&self, request: &ChatRequest, stream: bool) -> serde_json::Value {
        // Map each tool-call id to its tool name, so a tool-result message can
        // carry `tool_name` (Ollama matches a result to its call by name).
        let call_names: HashMap<&str, &str> = request
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .map(|c| (c.id.as_str(), c.name.as_str()))
            .collect();
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| out_message(m, &call_names))
            .collect();
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
        // We deliberately send no `options` (num_ctx, num_predict) or
        // `keep_alive`: how Ollama sizes its context window, caps generation, and
        // manages model residency is governed by the user's own Ollama settings.
        // Suis only issues the chat request.
        body
    }
}

/// Lower a [`Message`] to Ollama's chat-message shape. Plain messages are
/// `{role, content}`; an assistant turn that called tools carries `tool_calls`,
/// and a tool result carries `tool_name` (Ollama matches a result to its call by
/// name, looked up in `call_names`). Without the tool fields, a replayed
/// conversation loses every prior tool call — the assistant turns arrive empty —
/// and the model loses the thread.
fn out_message(m: &Message, call_names: &HashMap<&str, &str>) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "role": m.role.as_str(),
        "content": m.content,
    });
    if !m.tool_calls.is_empty() {
        obj["tool_calls"] =
            serde_json::Value::Array(m.tool_calls.iter().map(out_tool_call).collect());
    }
    if let Some(id) = &m.tool_call_id {
        if let Some(name) = call_names.get(id.as_str()) {
            obj["tool_name"] = serde_json::Value::String((*name).to_string());
        }
    }
    obj
}

/// Lower a [`ToolCall`] to Ollama's request shape. Unlike OpenAI, Ollama takes
/// `arguments` as a JSON *object* (not a stringified one) and needs no call id.
fn out_tool_call(call: &ToolCall) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.arguments,
        }
    })
}

#[derive(Deserialize, Default)]
struct WireMessage {
    #[serde(default)]
    content: String,
    /// Reasoning text from a thinking model run with `think: true`. Empty
    /// otherwise.
    #[serde(default)]
    thinking: String,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Deserialize, Default)]
struct WireResponse {
    #[serde(default)]
    message: WireMessage,
    #[serde(default)]
    done: bool,
    /// Tokens Ollama evaluated for the prompt; present on the final `done`
    /// object.
    #[serde(default)]
    prompt_eval_count: usize,
    /// Tokens Ollama generated for the reply; present on the final `done`
    /// object.
    #[serde(default)]
    eval_count: usize,
}

impl WireResponse {
    fn into_chat_response(self) -> ChatResponse {
        let tool_calls = self
            .message
            .tool_calls
            .into_iter()
            .enumerate()
            .map(|(i, tc)| ToolCall {
                id: format!("call_{i}"),
                name: tc.function.name,
                // Some templates double-encode arguments as a JSON string; the
                // shared normalizer parses those back into an object.
                arguments: normalize_arguments(tc.function.arguments),
            })
            .collect();
        // Token counts ride the terminal `done` object only.
        let usage =
            (self.done && (self.prompt_eval_count > 0 || self.eval_count > 0)).then_some(Usage {
                prompt_tokens: self.prompt_eval_count,
                completion_tokens: self.eval_count,
            });
        ChatResponse {
            content: self.message.content,
            reasoning: self.message.thinking,
            tool_calls,
            done: self.done,
            usage,
        }
    }
}

fn parse_line(line: &str) -> Result<ChatResponse> {
    let wire: WireResponse = serde_json::from_str(line)
        .map_err(|e| ProviderError::ParseError(format!("ollama chunk: {e}")))?;
    Ok(wire.into_chat_response())
}

#[async_trait]
impl Transport for OllamaTransport {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let resp = self
            .client
            .post(self.chat_url())
            .json(&self.body(&request, false))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Err(ProviderError::RequestError(format!(
                "ollama returned status {}",
                resp.status()
            ))
            .into());
        }
        let text = resp.text().await.map_err(map_reqwest_error)?;
        parse_line(&text)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let resp = self
            .client
            .post(self.chat_url())
            .json(&self.body(&request, true))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Err(ProviderError::RequestError(format!(
                "ollama returned status {}",
                resp.status()
            ))
            .into());
        }
        let lines = lines_from(resp.bytes_stream());
        let stream = lines.map(|line| line.and_then(|l| parse_line(&l)));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockServer;
    use crate::transport::types::{Message, Role};

    fn request() -> ChatRequest {
        ChatRequest::new("llama3", vec![Message::text(Role::User, "hi")])
    }

    #[tokio::test]
    async fn non_streaming_chat_returns_content() {
        let server = MockServer::json(
            r#"{"message":{"role":"assistant","content":"hello there"},"done":true}"#,
        );
        let transport = OllamaTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.content, "hello there");
        assert!(resp.done);
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn done_object_carries_eval_counts_as_usage() {
        let server = MockServer::json(
            r#"{"message":{"content":"ok"},"done":true,"prompt_eval_count":200,"eval_count":12}"#,
        );
        let transport = OllamaTransport::new(server.endpoint());
        let usage = transport.chat(request()).await.unwrap().usage.unwrap();
        assert_eq!(usage.prompt_tokens, 200);
        assert_eq!(usage.completion_tokens, 12);
    }

    #[tokio::test]
    async fn non_done_chunk_reports_no_usage() {
        let server = MockServer::json(r#"{"message":{"content":"he"},"done":false}"#);
        let transport = OllamaTransport::new(server.endpoint());
        assert!(transport.chat(request()).await.unwrap().usage.is_none());
    }

    #[tokio::test]
    async fn streaming_chat_yields_multiple_chunks() {
        let body = concat!(
            "{\"message\":{\"content\":\"he\"},\"done\":false}\n",
            "{\"message\":{\"content\":\"llo\"},\"done\":false}\n",
            "{\"message\":{\"content\":\"\"},\"done\":true}\n"
        );
        let server = MockServer::json(body);
        let transport = OllamaTransport::new(server.endpoint());
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
    async fn tool_calls_are_parsed() {
        let server = MockServer::json(
            r#"{"message":{"content":"","tool_calls":[{"function":{"name":"read","arguments":{"path":"a.rs"}}}]},"done":true}"#,
        );
        let transport = OllamaTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "read");
        assert_eq!(resp.tool_calls[0].arguments["path"], "a.rs");
    }

    #[tokio::test]
    async fn stringified_tool_arguments_normalize_to_an_object() {
        // Some Ollama templates emit `arguments` as a JSON-encoded string rather
        // than an object; it must still resolve to fields the tools can read.
        let server = MockServer::json(
            r#"{"message":{"content":"","tool_calls":[{"function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]},"done":true}"#,
        );
        let transport = OllamaTransport::new(server.endpoint());
        let resp = transport.chat(request()).await.unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].arguments["path"], "a.rs");
    }

    #[tokio::test]
    async fn http_error_status_is_wrapped() {
        let server = MockServer::status(500, "boom");
        let transport = OllamaTransport::new(server.endpoint());
        let err = transport.chat(request()).await.unwrap_err();
        assert!(err.to_string().contains("500"));
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        // Nothing listening on this port.
        let transport = OllamaTransport::new("http://127.0.0.1:1");
        let err = transport.chat(request()).await.unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }

    #[test]
    fn assistant_tool_calls_and_tool_results_serialize_for_ollama() {
        // The follow-up turn must carry the assistant's tool_calls and tag each
        // tool result with its tool name, or the replayed conversation loses
        // every prior call (the assistant turns arrive empty) and the model
        // loses the thread.
        let request = ChatRequest {
            model: "llama3".into(),
            messages: vec![
                Message::text(Role::User, "read it"),
                Message {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_0".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({ "path": "README.md" }),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "file contents".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call_0".into()),
                },
            ],
            tools: None,
            stream: false,
        };
        let transport = OllamaTransport::new("http://x");
        let body = transport.body(&request, false);
        let msgs = body["messages"].as_array().unwrap();

        // A plain user message stays {role, content}.
        assert!(msgs[0].get("tool_calls").is_none());

        // The assistant turn carries the call in Ollama's shape, with arguments
        // as a JSON *object* (not a stringified one).
        let tc = &msgs[1]["tool_calls"][0];
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "read");
        assert_eq!(tc["function"]["arguments"]["path"], "README.md");

        // The tool result is tagged with the tool name Ollama matches it to.
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_name"], "read");
        assert_eq!(msgs[2]["content"], "file contents");
    }

    #[test]
    fn body_sends_no_options_or_keep_alive() {
        // Context window, generation cap, and model residency are the user's
        // Ollama settings to manage — Suis sends none of them.
        let transport = OllamaTransport::new("http://x");
        let body = transport.body(&request(), false);
        assert!(body.get("options").is_none());
        assert!(body.get("keep_alive").is_none());
    }
}
