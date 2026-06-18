//! LM Studio discovery via `GET {endpoint}/v1/models` (OpenAI-compatible).
//!
//! LM Studio and `llama-server` expose the identical OpenAI-compatible
//! `/v1/models` endpoint, so discovery is the shared [`probe_v1_models`]; this
//! module contributes only LM Studio's identity and default port.
//!
//! [`probe_v1_models`]: super::openai_compat::probe_v1_models

/// Default LM Studio endpoint.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:1234";

#[cfg(test)]
mod tests {
    use super::DEFAULT_ENDPOINT;
    use crate::discovery::openai_compat::probe_v1_models;
    use crate::provider::TransportType;
    use crate::test_util::MockServer;
    use suis_core::ProviderError;

    #[tokio::test]
    async fn valid_response_populates_models() {
        let server = MockServer::json(
            r#"{"object":"list","data":[{"id":"llama-3-8b-instruct","object":"model"}]}"#,
        );
        let client = reqwest::Client::new();
        let result = probe_v1_models(&client, &server.endpoint(), "lmstudio", "LM Studio", None)
            .await
            .unwrap();
        assert_eq!(result.provider.id, "lmstudio");
        assert_eq!(result.provider.transport, TransportType::OpenAiCompatible);
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].model_id, "llama-3-8b-instruct");
    }

    #[tokio::test]
    async fn empty_model_list_is_ok() {
        let server = MockServer::json(r#"{"object":"list","data":[]}"#);
        let client = reqwest::Client::new();
        let result = probe_v1_models(&client, &server.endpoint(), "lmstudio", "LM Studio", None)
            .await
            .unwrap();
        assert!(result.models.is_empty());
        assert_eq!(result.provider.id, "lmstudio");
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let client = reqwest::Client::new();
        let err = probe_v1_models(&client, "http://127.0.0.1:1", "lmstudio", "LM Studio", None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }

    #[test]
    fn default_endpoint_is_lmstudio_port() {
        assert_eq!(DEFAULT_ENDPOINT, "http://localhost:1234");
    }
}
