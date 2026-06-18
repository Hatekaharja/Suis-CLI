//! Shared `/v1/models` discovery for OpenAI-compatible providers.
//!
//! LM Studio and `llama-server` both expose the same
//! `GET {endpoint}/v1/models` endpoint and differ only in identity and default
//! port, so they share [`probe_v1_models`].

use serde::Deserialize;

use suis_core::{ProviderError, Result};

use super::{classify_send_error, DiscoveryResult};
use crate::capability::Capabilities;
use crate::model::Model;
use crate::provider::{Provider, TransportType};

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Probe an OpenAI-compatible `/v1/models` endpoint, attributing the result to
/// the given provider id/name. When `api_key` is `Some`, an
/// `Authorization: Bearer <key>` header is sent; a `None` key sends none, so a
/// local provider's probe is byte-identical to before.
///
/// An empty model list is a valid result, not an error; connection refused or
/// timeout maps to [`ProviderError::NotRunning`] so the caller can treat the
/// provider as simply absent. A 401/403 maps to [`ProviderError::AuthFailed`]
/// (attributed to `provider_id`), distinct from "offline".
pub async fn probe_v1_models(
    client: &reqwest::Client,
    endpoint: &str,
    provider_id: &str,
    provider_name: &str,
    api_key: Option<&str>,
) -> Result<DiscoveryResult> {
    let url = format!("{}/v1/models", endpoint.trim_end_matches('/'));
    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| classify_send_error(&e, endpoint))?;

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // Discovery does not know the configured env-var name; the registry
        // attributes the failure to the target id, and the provider form names
        // the env var from the draft. The key itself is never carried.
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
        transport: TransportType::OpenAiCompatible,
        enabled: true,
        api_key: None,
        api_key_env: None,
    };
    let models = parsed
        .data
        .into_iter()
        .map(|m| {
            let window = crate::model_meta::lookup_context_window(provider_id, &m.id);
            Model::new(provider_id, m.id, Capabilities::discovery_default())
                .with_context_window(window)
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
    async fn keyed_probe_sends_one_authorization_header() {
        let server = MockServer::json(r#"{"object":"list","data":[{"id":"gpt-4o-mini"}]}"#);
        let result = probe_v1_models(
            &client(),
            &server.endpoint(),
            "openai",
            "OpenAI",
            Some("sk-9"),
        )
        .await
        .unwrap();
        assert_eq!(result.models.len(), 1);
        assert_eq!(server.request_count(), 1);
        assert_eq!(
            server.received_header("authorization").as_deref(),
            Some("Bearer sk-9")
        );
    }

    #[tokio::test]
    async fn unkeyed_probe_sends_no_authorization_header() {
        let server = MockServer::json(r#"{"object":"list","data":[]}"#);
        probe_v1_models(&client(), &server.endpoint(), "lmstudio", "LM Studio", None)
            .await
            .unwrap();
        assert_eq!(server.received_header("authorization"), None);
    }

    #[tokio::test]
    async fn unauthorized_maps_to_auth_failed_attributed_to_provider() {
        let server = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let err = probe_v1_models(
            &client(),
            &server.endpoint(),
            "openrouter",
            "OpenRouter",
            Some("sk-bad"),
        )
        .await
        .unwrap_err();
        match err {
            suis_core::Error::Provider(ProviderError::AuthFailed { provider, .. }) => {
                assert_eq!(provider, "openrouter");
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }
}
