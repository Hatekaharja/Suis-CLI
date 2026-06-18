//! Provider identity and transport classification.

use serde::{Deserialize, Serialize};

use suis_core::ProviderEntry;

/// How Suis talks to a provider over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    /// Ollama's native `/api/chat` protocol.
    Ollama,
    /// OpenAI-compatible `/v1/chat/completions` protocol (LM Studio, etc.).
    #[serde(rename = "openai")]
    OpenAiCompatible,
    /// Anthropic's native Messages protocol (`/v1/messages`).
    Anthropic,
}

/// A transport string in `providers.json` did not name a known language.
/// Carried as a load-time issue rather than silently coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTransport(pub String);

impl std::fmt::Display for UnknownTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown transport {:?}", self.0)
    }
}

impl TransportType {
    /// Every transport, in display order. The provider form's picker iterates
    /// this, so a new language (e.g. [`Anthropic`](Self::Anthropic)) appears as
    /// an option with no UI change — the transport layer is the single source.
    pub const ALL: &'static [TransportType] = &[
        TransportType::Ollama,
        TransportType::OpenAiCompatible,
        TransportType::Anthropic,
    ];

    /// The string used in `providers.json` (`"ollama"` / `"openai"` /
    /// `"anthropic"`).
    pub fn as_str(self) -> &'static str {
        match self {
            TransportType::Ollama => "ollama",
            TransportType::OpenAiCompatible => "openai",
            TransportType::Anthropic => "anthropic",
        }
    }

    /// Parse a `providers.json` transport string strictly. An unrecognized
    /// value is an error, never a silent fallback — so a typo speaks the wrong
    /// language to the endpoint rather than failing loudly ("Open And
    /// Inspectable").
    pub fn parse(s: &str) -> std::result::Result<Self, UnknownTransport> {
        match s {
            "ollama" => Ok(TransportType::Ollama),
            "openai" => Ok(TransportType::OpenAiCompatible),
            "anthropic" => Ok(TransportType::Anthropic),
            other => Err(UnknownTransport(other.to_string())),
        }
    }
}

/// A configuration problem found while loading `providers.json`: a single
/// entry that could not be turned into a probe target. Surfaced in `/providers`
/// rather than blocking the rest of the file ("Safe By Default").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIssue {
    /// The id of the offending entry (or its slot, for an unnamed one).
    pub id: String,
    /// Which field is at fault: `"transport"`, `"endpoint"`, or `"id"`.
    pub field: String,
    /// A human-readable explanation, e.g. `unknown transport "openai-compat"`.
    pub reason: String,
}

/// A discovered or configured AI provider.
///
/// `api_key` is the *resolved* credential (never an env-var name) and is held
/// only in memory; `Debug` is implemented by hand to redact it. `api_key_env`
/// is preserved so [`to_entry`](Self::to_entry) can round-trip the source
/// without persisting the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Stable identifier, e.g. `"ollama"`.
    pub id: String,
    /// Human-readable name, e.g. `"Ollama"`.
    pub name: String,
    /// Base endpoint URL.
    pub endpoint: String,
    /// Which transport to use.
    pub transport: TransportType,
    /// Whether the provider is enabled for use.
    pub enabled: bool,
    /// The resolved API key (env-first), if any. Redacted in `Debug`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// The configured key env-var name, preserved for round-trip and for the
    /// "key env not set" flag (env named but unresolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl Provider {
    /// Build a [`Provider`] from a persisted [`ProviderEntry`], resolving its
    /// API key (env-first) and using its display name when present (falling
    /// back to the id). An unknown transport is coerced to OpenAI-compatible
    /// here as a last resort; the configuration boundary validates transports
    /// strictly before an entry reaches this path.
    pub fn from_entry(entry: &ProviderEntry) -> Self {
        let name = entry
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| entry.id.clone());
        Provider {
            id: entry.id.clone(),
            name,
            endpoint: entry.endpoint.clone(),
            transport: TransportType::parse(&entry.transport)
                .unwrap_or(TransportType::OpenAiCompatible),
            enabled: entry.enabled,
            api_key: entry.resolve_api_key(),
            api_key_env: entry.api_key_env.clone(),
        }
    }

    /// Whether a key env-var was configured but resolved to nothing (unset or
    /// empty, with no literal fallback) — surfaced as a flag in `/providers`.
    pub fn key_env_unresolved(&self) -> bool {
        self.api_key_env.is_some() && self.api_key.is_none()
    }

    /// Convert back into a persistable [`ProviderEntry`]. The display name is
    /// only emitted when it differs from the id; auth fields round-trip without
    /// resolving — an env-backed provider re-emits `api_key_env` and never the
    /// resolved secret.
    pub fn to_entry(&self) -> ProviderEntry {
        let (api_key_env, api_key) = match &self.api_key_env {
            Some(env) => (Some(env.clone()), None),
            None => (None, self.api_key.clone()),
        };
        ProviderEntry {
            id: self.id.clone(),
            endpoint: self.endpoint.clone(),
            transport: self.transport.as_str().to_string(),
            enabled: self.enabled,
            name: (self.name != self.id).then(|| self.name.clone()),
            api_key_env,
            api_key,
        }
    }
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("transport", &self.transport)
            .field("enabled", &self.enabled)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_key_env", &self.api_key_env)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_string_round_trip() {
        assert_eq!(TransportType::Ollama.as_str(), "ollama");
        assert_eq!(TransportType::OpenAiCompatible.as_str(), "openai");
        assert_eq!(TransportType::parse("ollama"), Ok(TransportType::Ollama));
        assert_eq!(
            TransportType::parse("openai"),
            Ok(TransportType::OpenAiCompatible)
        );
        assert_eq!(TransportType::Anthropic.as_str(), "anthropic");
        assert_eq!(
            TransportType::parse("anthropic"),
            Ok(TransportType::Anthropic)
        );
        // Serialized form matches the parse string (snake_case = "anthropic").
        assert_eq!(
            serde_json::to_string(&TransportType::Anthropic).unwrap(),
            "\"anthropic\""
        );
    }

    #[test]
    fn unknown_transport_is_an_error_not_a_fallback() {
        let err = TransportType::parse("openai-compat").unwrap_err();
        assert_eq!(err, UnknownTransport("openai-compat".into()));
        assert!(err.to_string().contains("openai-compat"));
    }

    #[test]
    fn transport_serializes_as_lowercase() {
        let json = serde_json::to_string(&TransportType::Ollama).unwrap();
        assert_eq!(json, "\"ollama\"");
        let json = serde_json::to_string(&TransportType::OpenAiCompatible).unwrap();
        assert_eq!(json, "\"openai\"");
    }

    #[test]
    fn provider_round_trips_through_json() {
        let provider = Provider {
            id: "ollama".into(),
            name: "Ollama".into(),
            endpoint: "http://localhost:11434".into(),
            transport: TransportType::Ollama,
            enabled: true,
            api_key: None,
            api_key_env: None,
        };
        let json = serde_json::to_string(&provider).unwrap();
        let back: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(provider, back);
    }

    #[test]
    fn provider_round_trips_through_entry() {
        let entry = ProviderEntry {
            id: "lmstudio".into(),
            endpoint: "http://localhost:1234".into(),
            transport: "openai".into(),
            enabled: false,
            name: None,
            api_key_env: None,
            api_key: None,
        };
        let provider = Provider::from_entry(&entry);
        assert_eq!(provider.transport, TransportType::OpenAiCompatible);
        assert!(!provider.enabled);
        // No stored name => display name falls back to the id.
        assert_eq!(provider.name, "lmstudio");
        assert_eq!(provider.to_entry(), entry);
    }

    #[test]
    fn from_entry_uses_display_name_and_resolves_key() {
        let var = "SUIS_TEST_PROVIDER_KEY";
        std::env::set_var(var, "k-123");
        let entry = ProviderEntry {
            id: "work".into(),
            endpoint: "https://proxy.example/v1".into(),
            transport: "openai".into(),
            enabled: true,
            name: Some("Work Proxy".into()),
            api_key_env: Some(var.into()),
            api_key: None,
        };
        let provider = Provider::from_entry(&entry);
        assert_eq!(provider.name, "Work Proxy");
        assert_eq!(provider.api_key.as_deref(), Some("k-123"));
        assert_eq!(provider.api_key_env.as_deref(), Some(var));
        assert!(!provider.key_env_unresolved());
        std::env::remove_var(var);

        // to_entry re-emits the env name, never the resolved secret.
        let back = provider.to_entry();
        assert_eq!(back.name.as_deref(), Some("Work Proxy"));
        assert_eq!(back.api_key_env.as_deref(), Some(var));
        assert_eq!(back.api_key, None);
    }

    #[test]
    fn key_env_unresolved_when_var_missing() {
        let var = "SUIS_TEST_PROVIDER_MISSING";
        std::env::remove_var(var);
        let entry = ProviderEntry {
            id: "work".into(),
            endpoint: "https://proxy.example/v1".into(),
            transport: "openai".into(),
            enabled: true,
            name: None,
            api_key_env: Some(var.into()),
            api_key: None,
        };
        let provider = Provider::from_entry(&entry);
        assert!(provider.key_env_unresolved());
    }

    #[test]
    fn debug_redacts_resolved_key() {
        let provider = Provider {
            id: "work".into(),
            name: "Work".into(),
            endpoint: "https://proxy.example/v1".into(),
            transport: TransportType::OpenAiCompatible,
            enabled: true,
            api_key: Some("top-secret-value".into()),
            api_key_env: Some("WORK_KEY".into()),
        };
        let debug = format!("{provider:?}");
        assert!(!debug.contains("top-secret-value"));
        assert!(debug.contains("<redacted>"));
        // The env-var name is not secret and may appear.
        assert!(debug.contains("WORK_KEY"));
    }
}
