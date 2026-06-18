//! Transport abstraction: how Suis sends a [`ChatRequest`] to a provider and
//! receives a [`ChatResponse`], streaming or not.
//!
//! Three concrete transports implement [`Transport`]:
//! [`ollama::OllamaTransport`], [`openai::OpenAiTransport`], and
//! [`anthropic::AnthropicTransport`]. Each reduces its wire protocol to the
//! shared types in [`types`].

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod tool_text;
pub mod types;

use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};

use suis_core::{ProviderError, Result};

pub use types::{ChatRequest, ChatResponse, Message, Role, ToolCall, ToolDefinition};

/// A boxed stream of response chunks. Boxing keeps [`Transport`] object-safe so
/// the agent layer can hold a `Box<dyn Transport>` over either backend.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatResponse>> + Send>>;

/// A bidirectional channel to a model.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a request and await the full response.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Send a request and receive response chunks as they arrive.
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream>;
}

/// Floor for [`output_reserve`]: the per-reply output budget never drops below
/// this, so even a tiny context window leaves room for a usable reply. Matches
/// the previous fixed reserve, so small/local boxes behave exactly as before.
pub const MIN_OUTPUT_RESERVE: usize = 4_096;

/// Ceiling for [`output_reserve`]. Mirrors opencode's flat 32k output
/// reservation: past this, more generation room buys little and only eats into
/// the prompt budget.
pub const MAX_OUTPUT_RESERVE: usize = 32_768;

/// Tokens reserved from a context `window` for the model's own reply — both the
/// `num_predict` cap sent to Ollama and the slice the agent's prompt budget
/// holds back from the window.
///
/// Scales with the window (a quarter of it), clamped to
/// [`MIN_OUTPUT_RESERVE`, `MAX_OUTPUT_RESERVE`]: a roomy box gets a generous
/// generation budget so long reasoning no longer truncates mid-thought, while a
/// small box keeps a tight reserve so the prompt still fits. This is the single
/// source of truth the transport and the agent's budgeting both derive from, so
/// the reply cap and the held-back room always agree.
pub fn output_reserve(window: usize) -> usize {
    (window / 4).clamp(MIN_OUTPUT_RESERVE, MAX_OUTPUT_RESERVE)
}

/// Whether `endpoint` points at the local machine (loopback).
///
/// Used to decide when sending an API key over plaintext `http://` is
/// acceptable: a key sent to a *remote* `http://` endpoint travels in clear
/// text and can be sniffed, but a loopback request never leaves the host. Hosts
/// recognised as local: `localhost` (and `*.localhost`), `127.0.0.0/8`, and
/// IPv6 `::1`.
pub fn is_local_endpoint(endpoint: &str) -> bool {
    let after_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Separate host from port, handling bracketed IPv6 literals like `[::1]:80`.
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
        || host.starts_with("127.")
        || host.ends_with(".localhost")
}

/// Whether attaching an API key to `endpoint` would transmit it in clear text:
/// a key is present, the scheme is not `https`, and the host is not local.
pub fn key_sent_in_plaintext(endpoint: &str, has_key: bool) -> bool {
    has_key
        && !endpoint.trim().to_ascii_lowercase().starts_with("https://")
        && !is_local_endpoint(endpoint)
}

/// Lower a [`ToolDefinition`] to the `{"type":"function","function":{..}}`
/// shape used by both the Ollama and OpenAI APIs.
pub(crate) fn tool_to_function_json(tool: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }
    })
}

/// Parse a model-supplied tool-argument string into a JSON value, tolerating the
/// mistakes weaker local models make: markdown code fences (```json … ```),
/// leading prose before the object, and trailing commas. Conservative on quote
/// repair (only when no double quotes are present at all). Empty input yields an
/// empty object; the raw string is kept only as a last resort, so a tool sees
/// *something* rather than a bare type error.
pub(crate) fn parse_tool_arguments(raw: &str) -> serde_json::Value {
    use serde_json::Value;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Object(Default::default());
    }
    // 1. Straight parse.
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return normalize_arguments(v);
    }
    // 2. Strip a ```json / ``` fence and retry.
    let unfenced = strip_code_fence(trimmed);
    if unfenced != trimmed {
        if let Ok(v) = serde_json::from_str::<Value>(unfenced) {
            return normalize_arguments(v);
        }
    }
    // 3. Extract the first balanced {…} span, then try light repairs on it.
    if let Some(span) = first_object_span(unfenced) {
        for candidate in [span.to_string(), strip_trailing_commas(span)] {
            if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
                return normalize_arguments(v);
            }
        }
        if let Some(sq) = repair_single_quotes(span) {
            let sq = strip_trailing_commas(&sq);
            if let Ok(v) = serde_json::from_str::<Value>(&sq) {
                return normalize_arguments(v);
            }
        }
    }
    // 4. Last resort: keep the raw string so the failure stays visible.
    Value::String(raw.to_string())
}

/// If `v` is a JSON string that itself encodes the arguments object (some
/// providers/templates double-encode it), parse it; otherwise return it as-is.
pub(crate) fn normalize_arguments(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.starts_with('{') || t.starts_with('[') || t.starts_with("```") {
                parse_tool_arguments(&s)
            } else {
                serde_json::Value::String(s)
            }
        }
        other => other,
    }
}

/// Strip a leading ```/```json fence (and its closing ```), returning the inner
/// text trimmed. A no-op when `s` isn't fenced.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // Drop an optional language tag on the fence's first line.
        let rest = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        return rest.trim().strip_suffix("```").unwrap_or(rest).trim();
    }
    s
}

/// The first balanced `{…}` span in `s` (brace-counting, string-aware), or
/// `None` if there isn't a complete one. Braces/quotes are ASCII, so the byte
/// indices used to slice always fall on char boundaries.
fn first_object_span(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Drop commas that sit (past whitespace) immediately before a `}` or `]`,
/// leaving commas inside string literals untouched.
fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                continue; // a trailing comma: drop it
            }
        }
        out.push(c);
    }
    out
}

/// Convert single-quoted JSON to double-quoted, but only when it is unambiguous
/// — no double quotes anywhere (so we can't corrupt a `'` that lives inside a
/// double-quoted value). `None` when the heuristic doesn't apply.
fn repair_single_quotes(s: &str) -> Option<String> {
    if s.contains('"') || !s.contains('\'') {
        return None;
    }
    Some(s.replace('\'', "\""))
}

/// Map a `reqwest` error to a [`ProviderError`], distinguishing a refused/
/// unreachable endpoint (the provider isn't running) from other failures.
pub(crate) fn map_reqwest_error(err: reqwest::Error) -> ProviderError {
    if err.is_connect() || err.is_timeout() {
        ProviderError::NotRunning(err.to_string())
    } else {
        ProviderError::RequestError(err.to_string())
    }
}

/// Turn a byte stream into a stream of non-empty text lines, splitting on
/// `\n`. Used to parse both NDJSON (Ollama) and SSE (OpenAI) bodies.
pub(crate) fn lines_from<S, B>(inner: S) -> impl Stream<Item = Result<String>> + Send
where
    S: Stream<Item = reqwest::Result<B>> + Send + Unpin + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    struct State<S> {
        inner: S,
        buf: String,
        done: bool,
    }

    futures::stream::unfold(
        State {
            inner,
            buf: String::new(),
            done: false,
        },
        |mut st| async move {
            loop {
                if let Some(pos) = st.buf.find('\n') {
                    let line: String = st.buf.drain(..=pos).collect();
                    let trimmed = line.trim_matches(|c| c == '\r' || c == '\n').to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    return Some((Ok(trimmed), st));
                }
                if st.done {
                    let rem = st.buf.trim().to_string();
                    st.buf.clear();
                    if rem.is_empty() {
                        return None;
                    }
                    return Some((Ok(rem), st));
                }
                match st.inner.next().await {
                    Some(Ok(chunk)) => match std::str::from_utf8(chunk.as_ref()) {
                        Ok(s) => st.buf.push_str(s),
                        Err(_) => {
                            let err = ProviderError::ParseError("invalid utf-8 in stream".into());
                            return Some((Err(err.into()), st));
                        }
                    },
                    Some(Err(e)) => {
                        return Some((Err(map_reqwest_error(e).into()), st));
                    }
                    None => st.done = true,
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_object() {
        let v = parse_tool_arguments(r#"{"path":"a.rs"}"#);
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn empty_input_is_empty_object() {
        assert_eq!(parse_tool_arguments("   "), serde_json::json!({}));
    }

    #[test]
    fn strips_markdown_fence() {
        let v = parse_tool_arguments("```json\n{\"path\": \"a.rs\"}\n```");
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn extracts_object_after_leading_prose() {
        let v = parse_tool_arguments("Sure! Here you go: {\"command\": \"ls -la\"} thanks");
        assert_eq!(v["command"], "ls -la");
    }

    #[test]
    fn repairs_trailing_comma() {
        let v = parse_tool_arguments(r#"{"path":"a.rs",}"#);
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn repairs_single_quotes_when_unambiguous() {
        let v = parse_tool_arguments("{'path': 'a.rs'}");
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn keeps_commas_inside_strings() {
        let v = parse_tool_arguments(r#"{"command":"echo a,b,c"}"#);
        assert_eq!(v["command"], "echo a,b,c");
    }

    #[test]
    fn normalizes_double_encoded_arguments() {
        // A JSON string that itself encodes the object.
        let v = normalize_arguments(serde_json::json!("{\"path\":\"a.rs\"}"));
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn leaves_plain_string_alone() {
        let v = normalize_arguments(serde_json::json!("not json"));
        assert_eq!(v, serde_json::json!("not json"));
    }

    #[test]
    fn unrecoverable_input_falls_back_to_raw_string() {
        let v = parse_tool_arguments("definitely not json");
        assert_eq!(v, serde_json::json!("definitely not json"));
    }

    #[test]
    fn local_endpoints_are_recognised() {
        assert!(is_local_endpoint("http://localhost:11434"));
        assert!(is_local_endpoint("http://127.0.0.1:1234"));
        assert!(is_local_endpoint("http://127.0.0.5"));
        assert!(is_local_endpoint("http://[::1]:8080"));
        assert!(is_local_endpoint("http://app.localhost"));
        assert!(!is_local_endpoint("http://api.example.com"));
        assert!(!is_local_endpoint("https://api.anthropic.com"));
        assert!(!is_local_endpoint("http://10.0.0.5:1234"));
    }

    #[test]
    fn plaintext_key_detection() {
        // Remote http with a key: leaks.
        assert!(key_sent_in_plaintext("http://api.example.com", true));
        // https never leaks; localhost never leaves the host; no key, nothing to leak.
        assert!(!key_sent_in_plaintext("https://api.example.com", true));
        assert!(!key_sent_in_plaintext("http://localhost:1234", true));
        assert!(!key_sent_in_plaintext("http://api.example.com", false));
        // Scheme check is case-insensitive.
        assert!(!key_sent_in_plaintext("HTTPS://api.example.com", true));
    }
}
