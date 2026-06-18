//! llama.cpp discovery via `GET {endpoint}/v1/models` (OpenAI-compatible).
//!
//! `llama-server` exposes the same OpenAI-compatible `/v1/models` endpoint as
//! LM Studio, so discovery is the shared [`probe_v1_models`]; this module
//! contributes only llama.cpp's identity and default port.
//!
//! [`probe_v1_models`]: super::openai_compat::probe_v1_models

/// Default llama.cpp (`llama-server`) endpoint.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:8080";

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
            r#"{"object":"list","data":[{"id":"qwen2.5-coder-7b-instruct","object":"model"}]}"#,
        );
        let client = reqwest::Client::new();
        let result = probe_v1_models(&client, &server.endpoint(), "llamacpp", "llama.cpp", None)
            .await
            .unwrap();
        assert_eq!(result.provider.id, "llamacpp");
        assert_eq!(result.provider.name, "llama.cpp");
        assert_eq!(result.provider.transport, TransportType::OpenAiCompatible);
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].model_id, "qwen2.5-coder-7b-instruct");
        assert_eq!(result.models[0].provider_id, "llamacpp");
    }

    #[tokio::test]
    async fn empty_model_list_is_ok() {
        let server = MockServer::json(r#"{"object":"list","data":[]}"#);
        let client = reqwest::Client::new();
        let result = probe_v1_models(&client, &server.endpoint(), "llamacpp", "llama.cpp", None)
            .await
            .unwrap();
        assert!(result.models.is_empty());
        assert_eq!(result.provider.id, "llamacpp");
    }

    #[tokio::test]
    async fn connection_refused_is_not_running() {
        let client = reqwest::Client::new();
        let err = probe_v1_models(&client, "http://127.0.0.1:1", "llamacpp", "llama.cpp", None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            suis_core::Error::Provider(ProviderError::NotRunning(_))
        ));
    }

    #[test]
    fn default_endpoint_is_llamacpp_port() {
        assert_eq!(DEFAULT_ENDPOINT, "http://localhost:8080");
    }
}
