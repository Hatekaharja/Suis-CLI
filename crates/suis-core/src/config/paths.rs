//! Resolution of the global configuration directory and the files within it.
//!
//! By default the config dir is `~/.config/suis` (via the `dirs` crate). It can
//! be overridden with the `SUIS_CONFIG_DIR` environment variable, which keeps
//! tests and sandboxed runs off the real user config.

use std::path::PathBuf;

const ENV_CONFIG_DIR: &str = "SUIS_CONFIG_DIR";

/// The Suis global configuration directory (`~/.config/suis` by default).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_CONFIG_DIR) {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .map(|d| d.join("suis"))
        .unwrap_or_else(|| PathBuf::from(".suis-config"))
}

/// Path to the global `settings.json`.
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Path to the global `providers.json`.
pub fn providers_path() -> PathBuf {
    config_dir().join("providers.json")
}

/// Path to the global `permissions.json` (user-wide `Always` grants).
pub fn permissions_path() -> PathBuf {
    config_dir().join("permissions.json")
}

/// Directory holding cached model capability files.
pub fn models_dir() -> PathBuf {
    config_dir().join("models")
}

/// Path to a specific provider's cached capability file.
pub fn model_cache_path(provider_id: &str) -> PathBuf {
    models_dir().join(format!("{provider_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_hang_off_config_dir() {
        let base = config_dir();
        assert_eq!(settings_path(), base.join("settings.json"));
        assert_eq!(providers_path(), base.join("providers.json"));
        assert_eq!(permissions_path(), base.join("permissions.json"));
        assert_eq!(models_dir(), base.join("models"));
        assert_eq!(
            model_cache_path("ollama"),
            base.join("models").join("ollama.json")
        );
    }
}
