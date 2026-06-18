//! Global configuration: `~/.config/suis/settings.json`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths;
use crate::errors::{ConfigError, Result};
use crate::util::write_atomic_private;

/// User-level settings. `#[serde(default)]` makes every field optional in the
/// on-disk JSON, so partial files merge cleanly with the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The provider id selected by default at startup, if any.
    pub default_provider: Option<String>,
    /// Whether edits are applied without an explicit diff approval.
    pub auto_apply: bool,
    /// Active UI theme name.
    pub theme: String,
    /// Token budget the agent prunes conversation history against, in estimated
    /// tokens. Used only as a fallback when a model's context window is unknown;
    /// `None` then uses the agent's built-in default
    /// (`suis_agent::DEFAULT_CONTEXT_BUDGET`).
    pub context_budget: Option<usize>,
    /// Per-model context-window overrides, keyed `"provider_id/model_id"`, in
    /// tokens. Takes precedence over the discovered/curated window — an escape
    /// hatch for models the curated table doesn't know or that you want to cap.
    pub model_context_windows: HashMap<String, usize>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_provider: None,
            auto_apply: false,
            theme: "default".to_string(),
            context_budget: None,
            model_context_windows: HashMap::new(),
        }
    }
}

/// The loaded global configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalConfig {
    pub settings: Settings,
}

impl GlobalConfig {
    /// Load the global config from the default config directory. If the file
    /// does not exist, defaults are written and returned.
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_dir())
    }

    /// Persist the global config to the default config directory.
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::config_dir())
    }

    /// Load from an explicit config directory (used by tests).
    pub fn load_from(dir: &Path) -> Result<Self> {
        let path = dir.join("settings.json");
        if !path.exists() {
            let config = Self::default();
            config.save_to(dir)?;
            return Ok(config);
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::ReadFailure {
            path: path.clone(),
            source,
        })?;
        let settings: Settings =
            serde_json::from_str(&raw).map_err(|source| ConfigError::ParseFailure {
                path: path.clone(),
                source,
            })?;
        Ok(Self { settings })
    }

    /// Persist to an explicit config directory (used by tests).
    pub fn save_to(&self, dir: &Path) -> Result<()> {
        let path = dir.join("settings.json");
        let json = serde_json::to_vec_pretty(&self.settings)
            .map_err(|source| ConfigError::SerializeFailure { source })?;
        // Owner-only, and keeps the shared config dir (0700) consistent with the
        // sibling credential/permission files.
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
    fn missing_file_creates_defaults() {
        let dir = TempDir::new();
        let config = GlobalConfig::load_from(dir.path()).unwrap();
        assert_eq!(config, GlobalConfig::default());
        // The defaults should now exist on disk.
        assert!(dir.child("settings.json").exists());
    }

    #[test]
    fn partial_fields_merge_with_defaults() {
        let dir = TempDir::new();
        std::fs::write(dir.child("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let config = GlobalConfig::load_from(dir.path()).unwrap();
        assert_eq!(config.settings.theme, "dark");
        // Unspecified fields fall back to defaults.
        assert!(!config.settings.auto_apply);
        assert_eq!(config.settings.default_provider, None);
        assert_eq!(config.settings.context_budget, None);
    }

    #[test]
    fn round_trip_serialization() {
        let dir = TempDir::new();
        let config = GlobalConfig {
            settings: Settings {
                default_provider: Some("ollama".into()),
                auto_apply: true,
                theme: "solarized".into(),
                context_budget: Some(8_000),
                model_context_windows: HashMap::from([("ollama/qwen3-coder".to_string(), 32_768)]),
            },
        };
        config.save_to(dir.path()).unwrap();
        let loaded = GlobalConfig::load_from(dir.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn invalid_json_returns_parse_failure() {
        let dir = TempDir::new();
        std::fs::write(dir.child("settings.json"), "{not valid json").unwrap();
        let err = GlobalConfig::load_from(dir.path()).unwrap_err();
        assert!(matches!(
            err,
            crate::errors::Error::Config(ConfigError::ParseFailure { .. })
        ));
    }
}
