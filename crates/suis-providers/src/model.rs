//! A single model exposed by a provider.

use serde::{Deserialize, Serialize};

use crate::capability::Capabilities;

/// A model offered by a provider, together with its detected capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// Owning provider's id (e.g. `"ollama"`).
    pub provider_id: String,
    /// Provider-native model identifier (e.g. `"qwen3-coder:latest"`).
    pub model_id: String,
    /// Name shown in the UI; defaults to `model_id`.
    pub display_name: String,
    /// What this model can do.
    pub capabilities: Capabilities,
    /// Whether [`capabilities`](Self::capabilities) are trusted as-is — either
    /// advertised by the provider at discovery time or resolved by detection.
    /// When false, the capabilities are a discovery default awaiting a probe.
    #[serde(default)]
    pub verified: bool,
    /// The model's maximum context window in tokens, when known. Sourced from a
    /// curated table (and, for Ollama, the model's own `/api/show` metadata).
    /// `None` means unknown — the agent falls back to its default budget.
    #[serde(default)]
    pub context_window: Option<usize>,
}

impl Model {
    /// Construct a model with the given capabilities, using `model_id` as the
    /// display name. The capabilities are treated as unverified (a probe may
    /// refine them); use [`verified`](Self::verified_caps) for trusted ones.
    pub fn new(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        capabilities: Capabilities,
    ) -> Self {
        let model_id = model_id.into();
        Model {
            provider_id: provider_id.into(),
            display_name: model_id.clone(),
            model_id,
            capabilities,
            verified: false,
            context_window: None,
        }
    }

    /// Set the maximum context window (in tokens), consuming and returning the
    /// model so discovery can attach it fluently.
    pub fn with_context_window(mut self, window: Option<usize>) -> Self {
        self.context_window = window;
        self
    }

    /// Construct a model whose capabilities are trusted as-is (e.g. advertised
    /// by the provider), so capability resolution won't probe it.
    pub fn verified_caps(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        capabilities: Capabilities,
    ) -> Self {
        Model {
            verified: true,
            ..Model::new(provider_id, model_id, capabilities)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_model_id_as_display_name() {
        let model = Model::new("ollama", "llama3:8b", Capabilities::default());
        assert_eq!(model.display_name, "llama3:8b");
        assert_eq!(model.provider_id, "ollama");
    }

    #[test]
    fn round_trips_through_json() {
        let model = Model::new(
            "ollama",
            "qwen3-coder:latest",
            Capabilities::discovery_default(),
        );
        let json = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(model, back);
    }
}
