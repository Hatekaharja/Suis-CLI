//! Provider discovery: probing well-known local endpoints to find running
//! providers and the models they expose.

pub mod anthropic;
pub mod llamacpp;
pub mod lmstudio;
pub mod ollama;
pub mod openai_compat;

use suis_core::ProviderError;

use crate::model::Model;
use crate::provider::Provider;

pub use ollama::OllamaDiscovery;
pub use openai_compat::probe_v1_models;

/// Classify a failed `reqwest` send into the provider error that best matches,
/// so every transport's discovery agrees on the distinction. A *timeout* (a
/// connect- or request-timeout — `is_timeout` is checked first so a connect
/// that times out counts as a timeout, not a refusal) maps to
/// [`ProviderError::Timeout`]; a refused/unresolved connection maps to
/// [`ProviderError::NotRunning`]; anything else is a generic
/// [`ProviderError::RequestError`]. This lets the UI tell a silently-unreachable
/// host (timeout) from a closed port (refused).
pub(crate) fn classify_send_error(e: &reqwest::Error, endpoint: &str) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout(endpoint.to_string())
    } else if e.is_connect() {
        ProviderError::NotRunning(endpoint.to_string())
    } else {
        ProviderError::RequestError(e.to_string())
    }
}

/// The outcome of successfully probing a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryResult {
    /// The provider that answered.
    pub provider: Provider,
    /// The models it exposes (capabilities are discovery defaults until
    /// [`crate::detection`] refines them).
    pub models: Vec<Model>,
}
