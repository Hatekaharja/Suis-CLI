//! Well-known provider presets: prefilled form fields, not code.
//!
//! A preset is the "provider = endpoint" vision made visible — OpenAI and
//! OpenRouter enter the codebase as four-field data rows speaking the existing
//! `openai` language, reproducible by hand through the form's Custom path.
//! There is **zero** provider-specific runtime behavior here: presets exist only
//! to prefill the add-provider form.

use crate::provider::TransportType;

/// A prefilled set of form fields for a well-known endpoint. Choosing one drops
/// the user into the ordinary provider form with these values; nothing here runs
/// at request time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Display name (also the basis for the derived id).
    pub name: &'static str,
    /// Base endpoint URL. Chosen so the transport's fixed `{endpoint}/v1/…`
    /// paths are correct against the service (OpenRouter's base is `/api`).
    pub endpoint: &'static str,
    /// The language Suis speaks to the endpoint.
    pub transport: TransportType,
    /// The conventional environment variable holding the API key, if the service
    /// needs one. Local presets need no key.
    pub key_env: Option<&'static str>,
}

/// The static catalog of well-known endpoints offered in the Add flow. Custom
/// (a blank form) is offered alongside these by the form, not listed here.
pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "OpenAI",
        endpoint: "https://api.openai.com",
        transport: TransportType::OpenAiCompatible,
        key_env: Some("OPENAI_API_KEY"),
    },
    ProviderPreset {
        name: "OpenRouter",
        endpoint: "https://openrouter.ai/api",
        transport: TransportType::OpenAiCompatible,
        key_env: Some("OPENROUTER_API_KEY"),
    },
    ProviderPreset {
        name: "Anthropic",
        endpoint: "https://api.anthropic.com",
        transport: TransportType::Anthropic,
        key_env: Some("ANTHROPIC_API_KEY"),
    },
    ProviderPreset {
        name: "Mistral",
        endpoint: "https://api.mistral.ai",
        transport: TransportType::OpenAiCompatible,
        key_env: Some("MISTRAL_API_KEY"),
    },
    ProviderPreset {
        name: "Ollama",
        endpoint: "http://localhost:11434",
        transport: TransportType::Ollama,
        key_env: None,
    },
    ProviderPreset {
        name: "LM Studio",
        endpoint: "http://localhost:1234",
        transport: TransportType::OpenAiCompatible,
        key_env: None,
    },
    ProviderPreset {
        name: "llama.cpp",
        endpoint: "http://localhost:8080",
        transport: TransportType::OpenAiCompatible,
        key_env: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_endpoint_is_a_valid_url_with_host() {
        // Each preset must pass the loader's endpoint validation as-is (18.3),
        // so a chosen preset can be saved without edits.
        for preset in PRESETS {
            let url = reqwest::Url::parse(preset.endpoint)
                .unwrap_or_else(|e| panic!("{} endpoint does not parse: {e}", preset.name));
            assert!(
                url.has_host(),
                "{} endpoint has no host: {}",
                preset.name,
                preset.endpoint
            );
        }
    }

    #[test]
    fn remote_presets_name_a_key_env_locals_do_not() {
        let openai = PRESETS.iter().find(|p| p.name == "OpenAI").unwrap();
        assert_eq!(openai.key_env, Some("OPENAI_API_KEY"));
        let openrouter = PRESETS.iter().find(|p| p.name == "OpenRouter").unwrap();
        assert_eq!(openrouter.key_env, Some("OPENROUTER_API_KEY"));

        // The Anthropic preset speaks the second language and names its key env.
        let anthropic = PRESETS.iter().find(|p| p.name == "Anthropic").unwrap();
        assert_eq!(anthropic.key_env, Some("ANTHROPIC_API_KEY"));
        assert_eq!(anthropic.transport, TransportType::Anthropic);

        // Mistral is remote and uses the OpenAI-compatible transport.
        let mistral = PRESETS.iter().find(|p| p.name == "Mistral").unwrap();
        assert_eq!(mistral.key_env, Some("MISTRAL_API_KEY"));
        assert_eq!(mistral.transport, TransportType::OpenAiCompatible);

        for local in ["Ollama", "LM Studio", "llama.cpp"] {
            let preset = PRESETS.iter().find(|p| p.name == local).unwrap();
            assert_eq!(preset.key_env, None, "{local} should need no key");
        }
    }
}
