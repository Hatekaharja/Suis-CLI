//! Project configuration stored at `.suis/project.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::errors::{ConfigError, Result};
use crate::util::write_atomic;
use crate::workspace::Workspace;

/// Which providers a project may use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderScope {
    /// All discovered providers are allowed.
    #[default]
    All,
    /// Only the listed provider ids are allowed.
    List(Vec<String>),
}

/// Which models a project may use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelScope {
    /// All models of allowed providers.
    #[default]
    All,
    /// Only the listed model ids.
    List(Vec<String>),
}

/// Level of git access granted to the agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitAccess {
    /// No git operations.
    Disabled,
    /// Read-only git inspection (status, log, diff).
    #[default]
    ReadOnly,
    /// Full git access including commits.
    ReadWrite,
}

/// A compact, cached brief of what a project is and how it is built and tested.
///
/// Inferred once (deterministically, offline) from the project's manifest files
/// and cached in [`ProjectConfig`] so every session opens already knowing the
/// project's shape instead of rediscovering it with `tree`/`search`. Every field
/// is plain data; rendering it for the prompt lives in `suis-agent`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectProfile {
    /// One-line description of the project's shape (e.g. "Rust workspace").
    pub summary: String,
    /// The detected toolchain (e.g. "Rust (cargo)", "Node.js (pnpm)").
    pub toolchain: String,
    /// The likely build command, if one could be inferred.
    pub build_cmd: Option<String>,
    /// The likely test command, if one could be inferred.
    pub test_cmd: Option<String>,
    /// Short, confident conventions worth stating up front.
    pub conventions: Vec<String>,
    /// UTC date (`YYYY-MM-DD`) the profile was generated, for `/profile` display.
    pub generated_at: String,
}

/// Per-project configuration. `#[serde(default)]` lets partial files merge with
/// defaults so older/handwritten configs keep working.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// Optional human-friendly project name.
    pub name: Option<String>,
    /// Which providers may be used.
    pub provider_scope: ProviderScope,
    /// Which models may be used.
    pub model_scope: ModelScope,
    /// Apply edits without per-diff approval.
    pub auto_apply: bool,
    /// Git access level.
    pub git_access: GitAccess,
    /// Tools exposed to the agent. Empty means no tools are exposed.
    pub allowed_tools: Vec<String>,
    /// Glob patterns for files hidden from reads/listings.
    pub hidden: Vec<String>,
    /// Glob patterns for files that require approval before writes.
    pub hardened: Vec<String>,
    /// The project's check command (e.g. `cargo test`, `npm test`), run to
    /// self-verify edits. `None` ⇒ no automatic verification.
    pub verify_command: Option<String>,
    /// Cached project brief injected into the system prompt. `None` ⇒ the prompt
    /// stays project-blind (byte-identical to before profiles existed).
    pub profile: Option<ProjectProfile>,
}

impl ProjectConfig {
    fn path(workspace: &Workspace) -> PathBuf {
        workspace.suis_dir.join("project.json")
    }

    /// Load `.suis/project.json`, returning defaults if it does not exist.
    pub fn load(workspace: &Workspace) -> Result<Self> {
        let path = Self::path(workspace);
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

    /// Write the config to `.suis/project.json` atomically.
    pub fn save(&self, workspace: &Workspace) -> Result<()> {
        let path = Self::path(workspace);
        let json = serde_json::to_vec_pretty(self)
            .map_err(|source| ConfigError::SerializeFailure { source })?;
        write_atomic(&path, &json).map_err(|source| ConfigError::WriteFailure { path, source })?;
        Ok(())
    }

    /// Create `.suis/` and write default config, returning it.
    pub fn init(workspace: &Workspace) -> Result<Self> {
        std::fs::create_dir_all(&workspace.suis_dir).map_err(|source| {
            ConfigError::WriteFailure {
                path: workspace.suis_dir.clone(),
                source,
            }
        })?;
        let config = Self::default();
        config.save(workspace)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn ws(dir: &TempDir) -> Workspace {
        Workspace::detect(dir.path()).unwrap()
    }

    #[test]
    fn missing_returns_defaults() {
        let dir = TempDir::new();
        let config = ProjectConfig::load(&ws(&dir)).unwrap();
        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn init_creates_suis_dir_with_defaults() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        assert!(!workspace.suis_dir.exists());
        let config = ProjectConfig::init(&workspace).unwrap();
        assert!(workspace.suis_dir.join("project.json").exists());
        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn partial_config_fills_missing_with_defaults() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        std::fs::create_dir_all(&workspace.suis_dir).unwrap();
        std::fs::write(
            workspace.suis_dir.join("project.json"),
            r#"{"name":"demo","auto_apply":true}"#,
        )
        .unwrap();
        let config = ProjectConfig::load(&workspace).unwrap();
        assert_eq!(config.name.as_deref(), Some("demo"));
        assert!(config.auto_apply);
        // Defaults for the rest.
        assert_eq!(config.provider_scope, ProviderScope::All);
        assert_eq!(config.git_access, GitAccess::ReadOnly);
        assert!(config.allowed_tools.is_empty());
    }

    #[test]
    fn empty_allowed_tools_means_no_tools() {
        let config = ProjectConfig::default();
        assert!(config.allowed_tools.is_empty());
    }

    #[test]
    fn round_trip() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let config = ProjectConfig {
            name: Some("proj".into()),
            provider_scope: ProviderScope::List(vec!["ollama".into()]),
            model_scope: ModelScope::List(vec!["qwen3-coder".into()]),
            auto_apply: true,
            git_access: GitAccess::ReadWrite,
            allowed_tools: vec!["read".into(), "edit".into()],
            hidden: vec!["*.env".into()],
            hardened: vec!["Cargo.lock".into()],
            verify_command: Some("cargo test".into()),
            profile: Some(ProjectProfile {
                summary: "Rust workspace".into(),
                toolchain: "Rust (cargo)".into(),
                build_cmd: Some("cargo build".into()),
                test_cmd: Some("cargo test".into()),
                conventions: vec!["Lint with cargo clippy".into()],
                generated_at: "2026-06-15".into(),
            }),
        };
        config.save(&workspace).unwrap();
        let loaded = ProjectConfig::load(&workspace).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn pre_profile_config_loads_without_profile_fields() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        std::fs::create_dir_all(&workspace.suis_dir).unwrap();
        // A config written before profiles existed has neither field.
        std::fs::write(
            workspace.suis_dir.join("project.json"),
            r#"{"name":"legacy","allowed_tools":["read"]}"#,
        )
        .unwrap();
        let config = ProjectConfig::load(&workspace).unwrap();
        assert_eq!(config.name.as_deref(), Some("legacy"));
        assert_eq!(config.verify_command, None);
        assert_eq!(config.profile, None);
    }
}
