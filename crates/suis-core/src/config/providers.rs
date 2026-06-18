//! Provider configuration storage: `~/.config/suis/providers.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths;
use crate::errors::{ConfigError, Result};
use crate::util::write_atomic_private;

fn default_true() -> bool {
    true
}

/// A single stored provider entry.
///
/// The first four fields (`id`, `endpoint`, `transport`, `enabled`) are the
/// original schema; every later field is `#[serde(default)]` /
/// `skip_serializing_if` so a legacy `providers.json` deserializes unchanged
/// and an entry without the new fields re-serializes byte-identically.
///
/// `Debug` is implemented by hand to redact `api_key`: a stray log line can
/// never leak a literal credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Stable identifier, e.g. `"ollama"`.
    pub id: String,
    /// Base endpoint URL, e.g. `"http://localhost:11434"`.
    pub endpoint: String,
    /// Transport kind: `"ollama"` or `"openai"`.
    pub transport: String,
    /// Whether the provider is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional human-readable display name. Falls back to `id` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Name of the environment variable holding the API key. Preferred over a
    /// literal `api_key`; the key itself is never stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Literal API key fallback. Discouraged (visible in the file); prefer
    /// `api_key_env`. Redacted in `Debug` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl ProviderEntry {
    /// Resolve the API key for this entry: read `api_key_env` from the
    /// environment first (a set, non-empty value wins), otherwise fall back to
    /// the literal `api_key`. Returns `None` when neither yields a key — the
    /// provider is then probed without auth, never with a guessed key.
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(var) = &self.api_key_env {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
        self.api_key.clone().filter(|k| !k.is_empty())
    }
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("id", &self.id)
            .field("endpoint", &self.endpoint)
            .field("transport", &self.transport)
            .field("enabled", &self.enabled)
            .field("name", &self.name)
            .field("api_key_env", &self.api_key_env)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The persisted set of configured providers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
}

impl ProviderConfig {
    /// Load from the default config directory. Missing file => empty list.
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_dir())
    }

    /// Persist to the default config directory.
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::config_dir())
    }

    /// Load from an explicit config directory (used by tests).
    pub fn load_from(dir: &Path) -> Result<Self> {
        let path = dir.join("providers.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::ReadFailure {
            path: path.clone(),
            source,
        })?;
        let config = serde_json::from_str(&raw).map_err(|source| ConfigError::ParseFailure {
            path: path.clone(),
            source,
        })?;
        Ok(config)
    }

    /// Persist to an explicit config directory (used by tests).
    pub fn save_to(&self, dir: &Path) -> Result<()> {
        let path = dir.join("providers.json");
        let json = serde_json::to_vec_pretty(self)
            .map_err(|source| ConfigError::SerializeFailure { source })?;
        // Owner-only: this file may hold a literal `api_key` fallback.
        write_atomic_private(&path, &json)
            .map_err(|source| ConfigError::WriteFailure { path, source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = TempDir::new();
        let config = ProviderConfig::load_from(dir.path()).unwrap();
        assert!(config.providers.is_empty());
    }

    #[test]
    fn deserializes_valid_file() {
        let dir = TempDir::new();
        let json = r#"{"providers":[
            {"id":"ollama","endpoint":"http://localhost:11434","transport":"ollama","enabled":true}
        ]}"#;
        std::fs::write(dir.child("providers.json"), json).unwrap();
        let config = ProviderConfig::load_from(dir.path()).unwrap();
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].id, "ollama");
        assert_eq!(config.providers[0].transport, "ollama");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new();
        let config = ProviderConfig {
            providers: vec![
                ProviderEntry {
                    id: "ollama".into(),
                    endpoint: "http://localhost:11434".into(),
                    transport: "ollama".into(),
                    enabled: true,
                    name: None,
                    api_key_env: None,
                    api_key: None,
                },
                ProviderEntry {
                    id: "lmstudio".into(),
                    endpoint: "http://localhost:1234".into(),
                    transport: "openai".into(),
                    enabled: false,
                    name: None,
                    api_key_env: None,
                    api_key: None,
                },
            ],
        };
        config.save_to(dir.path()).unwrap();
        let loaded = ProviderConfig::load_from(dir.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn enabled_defaults_to_true_when_absent() {
        let dir = TempDir::new();
        let json = r#"{"providers":[
            {"id":"ollama","endpoint":"http://localhost:11434","transport":"ollama"}
        ]}"#;
        std::fs::write(dir.child("providers.json"), json).unwrap();
        let config = ProviderConfig::load_from(dir.path()).unwrap();
        assert!(config.providers[0].enabled);
    }

    #[test]
    fn legacy_four_field_entry_round_trips_byte_identically() {
        // A legacy entry with none of the new fields must deserialize with them
        // `None` and re-serialize without emitting them.
        let dir = TempDir::new();
        let json = "{\n  \"providers\": [\n    {\n      \"id\": \"ollama\",\n      \"endpoint\": \"http://localhost:11434\",\n      \"transport\": \"ollama\",\n      \"enabled\": true\n    }\n  ]\n}";
        std::fs::write(dir.child("providers.json"), json).unwrap();
        let config = ProviderConfig::load_from(dir.path()).unwrap();
        let entry = &config.providers[0];
        assert_eq!(entry.name, None);
        assert_eq!(entry.api_key_env, None);
        assert_eq!(entry.api_key, None);

        let reserialized = serde_json::to_string_pretty(&config).unwrap();
        assert_eq!(reserialized, json, "new fields must not appear on disk");
    }

    #[test]
    fn resolve_api_key_prefers_env_over_literal() {
        let var = "SUIS_TEST_KEY_PREFER";
        std::env::set_var(var, "from-env");
        let entry = ProviderEntry {
            id: "x".into(),
            endpoint: "http://localhost:1".into(),
            transport: "openai".into(),
            enabled: true,
            name: None,
            api_key_env: Some(var.into()),
            api_key: Some("from-literal".into()),
        };
        assert_eq!(entry.resolve_api_key().as_deref(), Some("from-env"));
        std::env::remove_var(var);
    }

    #[test]
    fn resolve_api_key_falls_back_to_literal_when_env_unset() {
        let var = "SUIS_TEST_KEY_UNSET";
        std::env::remove_var(var);
        let entry = ProviderEntry {
            id: "x".into(),
            endpoint: "http://localhost:1".into(),
            transport: "openai".into(),
            enabled: true,
            name: None,
            api_key_env: Some(var.into()),
            api_key: Some("from-literal".into()),
        };
        assert_eq!(entry.resolve_api_key().as_deref(), Some("from-literal"));
    }

    #[test]
    fn resolve_api_key_none_when_env_set_and_no_literal() {
        let var = "SUIS_TEST_KEY_NO_LITERAL";
        std::env::remove_var(var);
        let entry = ProviderEntry {
            id: "x".into(),
            endpoint: "http://localhost:1".into(),
            transport: "openai".into(),
            enabled: true,
            name: None,
            api_key_env: Some(var.into()),
            api_key: None,
        };
        assert_eq!(entry.resolve_api_key(), None);
    }

    #[test]
    fn debug_redacts_api_key() {
        let entry = ProviderEntry {
            id: "x".into(),
            endpoint: "http://localhost:1".into(),
            transport: "openai".into(),
            enabled: true,
            name: None,
            api_key_env: None,
            api_key: Some("super-secret-token".into()),
        };
        let debug = format!("{entry:?}");
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("<redacted>"));
    }
}
