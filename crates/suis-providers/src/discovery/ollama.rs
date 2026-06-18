//! Ollama discovery via `GET {endpoint}/api/tags`.

use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::{classify_send_error, DiscoveryResult};
use crate::capability::Capabilities;
use crate::model::Model;
use crate::provider::{Provider, TransportType};

/// Default Ollama endpoint.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:11434";

/// Probes for a running Ollama instance over its native `/api/tags` protocol.
///
/// Stateless: the HTTP client is supplied per [`probe`](OllamaDiscovery::probe)
/// so a single timeout-configured client can be shared across a discovery run.
pub struct OllamaDiscovery;

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
    /// Per-model capability tags (e.g. `["completion","tools"]`). Newer Ollama
    /// builds include this; older ones omit it, leaving the model unverified.
    #[serde(default)]
    capabilities: Vec<String>,
}

/// The slice of `/api/show` we read: `model_info` carries GGUF metadata whose
/// `<arch>.*` keys hold the model's trained context window.
#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    model_info: serde_json::Map<String, serde_json::Value>,
}

/// Find a `model_info` value by architecture-agnostic key suffix (the keys are
/// prefixed with the architecture, e.g. `qwen3.context_length`).
fn meta_u64(info: &serde_json::Map<String, serde_json::Value>, suffix: &str) -> Option<usize> {
    info.iter()
        .find(|(key, _)| key.ends_with(suffix) || key.as_str() == suffix.trim_start_matches('.'))
        .and_then(|(_, value)| value.as_u64())
        .map(|n| n as usize)
}

/// Read a single model's trained context window from Ollama's `/api/show`, the
/// most authoritative source for a locally-served model (its window is set by
/// the Modelfile, which the curated table can't know). Best-effort: any network
/// or parse failure yields `None` so the caller falls back to the curated table.
async fn fetch_context_window(
    client: &reqwest::Client,
    endpoint: &str,
    name: &str,
) -> Option<usize> {
    let url = format!("{}/api/show", endpoint.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success())?;
    let show = resp.json::<ShowResponse>().await.ok()?;
    meta_u64(&show.model_info, ".context_length")
}

impl OllamaDiscovery {
    /// Create a discoverer.
    pub fn new() -> Self {
        OllamaDiscovery
    }

    /// Probe `endpoint` for a running Ollama instance and its models, using the
    /// supplied (shared, timeout-configured) HTTP client.
    ///
    /// Returns [`ProviderError::NotRunning`] if the endpoint refuses the
    /// connection, [`ProviderError::Timeout`] if it never answers in time, and
    /// [`ProviderError::ParseError`] on an unexpected payload.
    pub async fn probe(&self, client: &reqwest::Client, endpoint: &str) -> Result<DiscoveryResult> {
        let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| classify_send_error(&e, endpoint))?;

        if !resp.status().is_success() {
            return Err(ProviderError::RequestError(format!(
                "ollama returned status {}",
                resp.status()
            ))
            .into());
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::RequestError(e.to_string()))?;
        let tags: TagsResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::ParseError(format!("ollama /api/tags: {e}")))?;

        let provider = Provider {
            id: "ollama".into(),
            name: "Ollama".into(),
            endpoint: endpoint.to_string(),
            transport: TransportType::Ollama,
            enabled: true,
            api_key: None,
            api_key_env: None,
        };
        // When a model advertises capabilities, trust them (no probe needed);
        // otherwise fall back to the discovery default for later detection. The
        // context window comes from the model's own `/api/show` metadata, with
        // the curated table as a fallback when the probe doesn't surface one.
        let mut models = Vec::with_capacity(tags.models.len());
        for t in tags.models {
            let window = fetch_context_window(client, endpoint, &t.name)
                .await
                .or_else(|| crate::model_meta::lookup_context_window("ollama", &t.name));
            let model = if t.capabilities.is_empty() {
                Model::new("ollama", t.name.as_str(), Capabilities::discovery_default())
            } else {
                Model::verified_caps(
                    "ollama",
                    t.name.as_str(),
                    Capabilities::from_ollama_tags(&t.capabilities),
                )
            };
            models.push(model.with_context_window(window));
        }

        Ok(DiscoveryResult { provider, models })
    }
}

impl Default for OllamaDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockServer;

    #[tokio::test]
    async fn valid_response_yields_provider_and_models() {
        let server =
            MockServer::json(r#"{"models":[{"name":"qwen3-coder:latest"},{"name":"llama3:8b"}]}"#);
        let client = reqwest::Client::new();
        let result = OllamaDiscovery::new()
            .probe(&client, &server.endpoint())
            .await
            .unwrap();
        assert_eq!(result.provider.id, "ollama");
        assert_eq!(result.provider.transport, TransportType::Ollama);
        assert_eq!(result.models.len(), 2);
        assert_eq!(result.models[0].model_id, "qwen3-coder:latest");
        assert!(result.models[0].capabilities.streaming);
        assert!(!result.models[0].capabilities.tool_use);
    }

    #[tokio::test]
    async fn advertised_tools_capability_is_trusted() {
        let server = MockServer::json(
            r#"{"models":[
                {"name":"qwen3-coder:latest","capabilities":["completion","tools"]},
                {"name":"llama3:8b","capabilities":["completion"]}
            ]}"#,
        );
        let client = reqwest::Client::new();
        let result = OllamaDiscovery::new()
            .probe(&client, &server.endpoint())
            .await
            .unwrap();
        let coder = &result.models[0];
        assert!(coder.capabilities.tool_use);
        assert!(coder.verified, "advertised model should not need a probe");

        let llama = &result.models[1];
        assert!(!llama.capabilities.tool_use);
        assert!(llama.verified);
    }

    #[tokio::test]
    async fn model_without_capabilities_stays_unverified() {
        let server = MockServer::json(r#"{"models":[{"name":"llama3:8b"}]}"#);
        let client = reqwest::Client::new();
        let result = OllamaDiscovery::new()
            .probe(&client, &server.endpoint())
            .await
            .unwrap();
        assert!(!result.models[0].verified, "missing caps → needs probe");
        assert!(!result.models[0].capabilities.tool_use);
    }

    #[tokio::test]
    async fn show_parses_context_window() {
        // A representative `/api/show` payload: architecture-prefixed GGUF keys.
        let server = MockServer::json(r#"{"model_info":{"qwen3.context_length":40960}}"#);
        let client = reqwest::Client::new();
        let window = fetch_context_window(&client, &server.endpoint(), "qwen3:32b").await;
        assert_eq!(window, Some(40_960));
    }

    #[tokio::test]
    async fn show_window_is_none_when_metadata_missing() {
        let server = MockServer::json(r#"{"model_info":{}}"#);
        let client = reqwest::Client::new();
        let window = fetch_context_window(&client, &server.endpoint(), "x").await;
        assert!(window.is_none());
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let client = reqwest::Client::new();
        let err = OllamaDiscovery::new()
            .probe(&client, "http://127.0.0.1:1")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn unexpected_json_shape_is_parse_error() {
        let server = MockServer::json(r#"{"unexpected":true,"models":"not-an-array"}"#);
        let client = reqwest::Client::new();
        let err = OllamaDiscovery::new()
            .probe(&client, &server.endpoint())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::ParseError(_))
        ));
    }
}
