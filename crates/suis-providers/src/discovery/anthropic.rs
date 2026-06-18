//! Anthropic model discovery via `GET {endpoint}/v1/models`.
//!
//! Mirrors [`probe_v1_models`](super::openai_compat::probe_v1_models) in shape
//! and error attribution, differing only in the headers Anthropic expects
//! (`x-api-key` + `anthropic-version`) and the capability shortcut: every
//! Anthropic model supports tools and streaming, so the discovered models are
//! advertised as verified ([`Model::verified_caps`]) and never probed.

use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::{classify_send_error, DiscoveryResult};
use crate::capability::Capabilities;
use crate::model::Model;
use crate::provider::{Provider, TransportType};

/// The default Anthropic API endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";

/// The Messages API version header, pinned to match the transport.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Capabilities advertised for every Anthropic model: chat, streaming, and tool
/// use are all supported, so no per-model probe is ever needed.
fn anthropic_caps() -> Capabilities {
    Capabilities {
        chat: true,
        streaming: true,
        tool_use: true,
        structured_output: false,
    }
}

/// Probe Anthropic's `/v1/models` endpoint, attributing the result to the given
/// provider id/name. When `api_key` is `Some`, the `x-api-key` and
/// `anthropic-version` headers are sent; a `None` key sends only the version
/// header (the endpoint will then 401, surfaced as [`ProviderError::AuthFailed`]).
///
/// Connection refused maps to [`ProviderError::NotRunning`] and a timeout to
/// [`ProviderError::Timeout`]; a 401/403 maps to [`ProviderError::AuthFailed`]
/// attributed to `provider_id`; any other non-success is a
/// [`ProviderError::RequestError`].
pub async fn probe_models(
    client: &reqwest::Client,
    endpoint: &str,
    provider_id: &str,
    provider_name: &str,
    api_key: Option<&str>,
) -> Result<DiscoveryResult> {
    let url = format!("{}/v1/models", endpoint.trim_end_matches('/'));
    let mut req = client
        .get(&url)
        .header("anthropic-version", ANTHROPIC_VERSION);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| classify_send_error(&e, endpoint))?;

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(ProviderError::AuthFailed {
            provider: provider_id.to_string(),
            key_env: None,
        }
        .into());
    }
    if !status.is_success() {
        return Err(
            ProviderError::RequestError(format!("{provider_id} returned status {status}")).into(),
        );
    }

    let text = resp
        .text()
        .await
        .map_err(|e| ProviderError::RequestError(e.to_string()))?;
    let parsed: ModelsResponse = serde_json::from_str(&text)
        .map_err(|e| ProviderError::ParseError(format!("{provider_id} /v1/models: {e}")))?;

    let provider = Provider {
        id: provider_id.to_string(),
        name: provider_name.to_string(),
        endpoint: endpoint.to_string(),
        transport: TransportType::Anthropic,
        enabled: true,
        api_key: None,
        api_key_env: None,
    };
    let models = parsed
        .data
        .into_iter()
        .map(|m| {
            let window = crate::model_meta::lookup_context_window(provider_id, &m.id);
            Model::verified_caps(provider_id, m.id, anthropic_caps()).with_context_window(window)
        })
        .collect();

    Ok(DiscoveryResult { provider, models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockServer;

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn keyed_probe_sends_key_and_version_and_advertises_caps() {
        let server = MockServer::json(
            r#"{"data":[{"id":"claude-opus-4","type":"model"},{"id":"claude-haiku-4","type":"model"}]}"#,
        );
        let result = probe_models(
            &client(),
            &server.endpoint(),
            "anthropic",
            "Anthropic",
            Some("sk-ant"),
        )
        .await
        .unwrap();
        assert_eq!(result.models.len(), 2);
        assert_eq!(result.provider.transport, TransportType::Anthropic);
        // Every model advertises tool use, verified (no probe needed).
        assert!(result.models[0].capabilities.tool_use);
        assert!(result.models[0].verified);
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
    async fn unauthorized_maps_to_auth_failed_attributed_to_provider() {
        let server = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let err = probe_models(
            &client(),
            &server.endpoint(),
            "anthropic",
            "Anthropic",
            None,
        )
        .await
        .unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::AuthFailed { provider, .. }) => {
                assert_eq!(provider, "anthropic");
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let err = probe_models(
            &client(),
            "http://127.0.0.1:1",
            "anthropic",
            "Anthropic",
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }
}
