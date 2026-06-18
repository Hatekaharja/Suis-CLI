//! Persistence for permissions, split across two files by scope:
//!
//! - `.suis/permissions.json` (workspace) — `Project` grants and `Deny` rules.
//! - `~/.config/suis/permissions.json` (global) — `Always` grants, applying
//!   across all projects. Hand-edited `Deny` rules placed here are honored on
//!   load and preserved across saves.
//!
//! `Once` and `Session` scopes are ephemeral: they are never written, and any
//! found on disk (hand-edited, or written by a pre-fix version of Suis) are
//! dropped on load — a session grant must never outlive its session. A
//! project-level `Deny` beats a global grant because evaluation is deny-wins
//! (see [`super::evaluator`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::{CommandPermission, PermissionScope, ToolPermission};
use crate::config::paths;
use crate::errors::{ConfigError, Result};
use crate::util::write_atomic_private;
use crate::workspace::Workspace;

/// The effective set of command and tool permissions for a session: stored
/// global + project entries plus any grants accumulated at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PermissionStore {
    #[serde(default)]
    pub commands: Vec<CommandPermission>,
    #[serde(default)]
    pub tools: Vec<ToolPermission>,
    /// Commands denied for the rest of the session. In-memory only: never
    /// serialized, so a session deny can never outlive its session.
    #[serde(skip)]
    pub session_denies: Vec<String>,
}

/// Whether entries of this scope may be written to disk.
fn is_persistent(scope: PermissionScope) -> bool {
    matches!(
        scope,
        PermissionScope::Project | PermissionScope::Always | PermissionScope::Deny
    )
}

impl PermissionStore {
    fn project_path(workspace: &Workspace) -> PathBuf {
        workspace.suis_dir.join("permissions.json")
    }

    fn global_path(config_dir: &Path) -> PathBuf {
        config_dir.join("permissions.json")
    }

    /// Load the effective permissions for `workspace`: the global file's
    /// entries merged with the project's. Missing files contribute nothing.
    pub fn load(workspace: &Workspace) -> Result<Self> {
        Self::load_from(&paths::config_dir(), workspace)
    }

    /// Persist the store, splitting by scope: `Always` grants to the global
    /// file, `Project` grants and `Deny` rules to the workspace file.
    /// `Once`/`Session` entries are never written.
    pub fn save(&self, workspace: &Workspace) -> Result<()> {
        self.save_to(&paths::config_dir(), workspace)
    }

    /// [`load`](Self::load) with an explicit config directory (used by tests).
    pub fn load_from(config_dir: &Path, workspace: &Workspace) -> Result<Self> {
        let mut store = Self::read_file(&Self::global_path(config_dir))?;
        let project = Self::read_file(&Self::project_path(workspace))?;
        store.commands.extend(project.commands);
        store.tools.extend(project.tools);
        // Ephemeral scopes found on disk must not act as grants.
        store.commands.retain(|c| is_persistent(c.scope));
        store.tools.retain(|t| is_persistent(t.scope));
        Ok(store)
    }

    /// Load the global and project stores *separately*, each filtered to
    /// persistent scopes — for the `/permissions` screen, which groups entries by
    /// where they live and writes each file back independently. Unlike
    /// [`load`](Self::load) the two are not merged, so source is preserved.
    pub fn load_split(workspace: &Workspace) -> Result<(Self, Self)> {
        Self::load_split_from(&paths::config_dir(), workspace)
    }

    /// [`load_split`](Self::load_split) with an explicit config directory (tests).
    pub fn load_split_from(config_dir: &Path, workspace: &Workspace) -> Result<(Self, Self)> {
        let mut global = Self::read_file(&Self::global_path(config_dir))?;
        let mut project = Self::read_file(&Self::project_path(workspace))?;
        for store in [&mut global, &mut project] {
            store.commands.retain(|c| is_persistent(c.scope));
            store.tools.retain(|t| is_persistent(t.scope));
        }
        Ok((global, project))
    }

    /// Write the global and project stores *verbatim* to their files (persistent
    /// scopes only), bypassing the scope-routing merge in [`save`](Self::save).
    /// The `/permissions` screen owns both files in full, so a removed row must
    /// actually disappear — what the screen shows is exactly what is written.
    pub fn save_split(global: &Self, project: &Self, workspace: &Workspace) -> Result<()> {
        Self::save_split_to(&paths::config_dir(), global, project, workspace)
    }

    /// [`save_split`](Self::save_split) with an explicit config directory (tests).
    pub fn save_split_to(
        config_dir: &Path,
        global: &Self,
        project: &Self,
        workspace: &Workspace,
    ) -> Result<()> {
        let persistent = |store: &Self| Self {
            commands: store
                .commands
                .iter()
                .filter(|c| is_persistent(c.scope))
                .cloned()
                .collect(),
            tools: store
                .tools
                .iter()
                .filter(|t| is_persistent(t.scope))
                .cloned()
                .collect(),
            session_denies: Vec::new(),
        };
        // Attempt both writes even if the first fails, then report the first error.
        let global_result = Self::write_file(&persistent(global), &Self::global_path(config_dir));
        let project_result = Self::write_file(&persistent(project), &Self::project_path(workspace));
        global_result.and(project_result)
    }

    /// [`save`](Self::save) with an explicit config directory (used by tests).
    pub fn save_to(&self, config_dir: &Path, workspace: &Workspace) -> Result<()> {
        let global_path = Self::global_path(config_dir);
        // The global file receives this store's `Always` grants and keeps any
        // `Deny` rules it already holds (those are hand-edited; the runtime
        // never records denials).
        let existing = Self::read_file(&global_path).unwrap_or_default();
        let global = PermissionStore {
            commands: existing
                .commands
                .iter()
                .filter(|c| c.scope == PermissionScope::Deny)
                .chain(
                    self.commands
                        .iter()
                        .filter(|c| c.scope == PermissionScope::Always),
                )
                .cloned()
                .collect(),
            tools: existing
                .tools
                .iter()
                .filter(|t| t.scope == PermissionScope::Deny)
                .chain(
                    self.tools
                        .iter()
                        .filter(|t| t.scope == PermissionScope::Always),
                )
                .cloned()
                .collect(),
            session_denies: Vec::new(),
        };

        // The project file receives `Project` grants and `Deny` rules — except
        // denies that live in the global file, which stay global instead of
        // being copied into every project that saves.
        let project = PermissionStore {
            commands: self
                .commands
                .iter()
                .filter(|c| match c.scope {
                    PermissionScope::Project => true,
                    PermissionScope::Deny => !existing.commands.contains(*c),
                    _ => false,
                })
                .cloned()
                .collect(),
            tools: self
                .tools
                .iter()
                .filter(|t| match t.scope {
                    PermissionScope::Project => true,
                    PermissionScope::Deny => !existing.tools.contains(*t),
                    _ => false,
                })
                .cloned()
                .collect(),
            session_denies: Vec::new(),
        };

        // Don't create a global file the user never asked for: skip it when it
        // doesn't exist and there is nothing global to record. Attempt both
        // writes even if the first fails, then report the first error.
        let global_result =
            if global_path.exists() || !(global.commands.is_empty() && global.tools.is_empty()) {
                Self::write_file(&global, &global_path)
            } else {
                Ok(())
            };
        let project_result = Self::write_file(&project, &Self::project_path(workspace));
        global_result.and(project_result)
    }

    /// Read one permissions file; a missing file is an empty store.
    fn read_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::ReadFailure {
            path: path.to_path_buf(),
            source,
        })?;
        let store = serde_json::from_str(&raw).map_err(|source| ConfigError::ParseFailure {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(store)
    }

    /// Write one permissions file atomically.
    fn write_file(store: &PermissionStore, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(store)
            .map_err(|source| ConfigError::SerializeFailure { source })?;
        // Owner-only: this file encodes the user's security policy (grants/denies).
        write_atomic_private(path, &json).map_err(|source| ConfigError::WriteFailure {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::types::PermissionResult;
    use crate::test_util::TempDir;

    fn ws(dir: &TempDir) -> Workspace {
        let workspace = Workspace::detect(dir.path()).unwrap();
        std::fs::create_dir_all(&workspace.suis_dir).unwrap();
        workspace
    }

    fn cmd(pattern: &str, scope: PermissionScope) -> CommandPermission {
        CommandPermission {
            pattern: pattern.into(),
            scope,
        }
    }

    #[test]
    fn missing_files_return_empty() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let store = PermissionStore::load_from(home.path(), &ws(&dir)).unwrap();
        assert_eq!(store, PermissionStore::default());
    }

    #[test]
    fn round_trip() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let store = PermissionStore {
            commands: vec![cmd("cargo *", PermissionScope::Project)],
            tools: vec![ToolPermission {
                tool: "edit".into(),
                scope: PermissionScope::Project,
            }],
            session_denies: Vec::new(),
        };
        store.save_to(home.path(), &workspace).unwrap();
        let loaded = PermissionStore::load_from(home.path(), &workspace).unwrap();
        assert_eq!(store, loaded);
    }

    #[test]
    fn ephemeral_scopes_never_reach_disk() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let store = PermissionStore {
            commands: vec![
                cmd("echo *", PermissionScope::Once),
                cmd("ls *", PermissionScope::Session),
                cmd("cargo *", PermissionScope::Project),
                cmd("git status", PermissionScope::Always),
                cmd("git push", PermissionScope::Deny),
            ],
            tools: vec![ToolPermission {
                tool: "edit".into(),
                scope: PermissionScope::Session,
            }],
            session_denies: vec!["echo hi".to_string()],
        };
        store.save_to(home.path(), &workspace).unwrap();

        // The raw files contain no ephemeral entries.
        let project_raw =
            std::fs::read_to_string(workspace.suis_dir.join("permissions.json")).unwrap();
        let global_raw = std::fs::read_to_string(home.path().join("permissions.json")).unwrap();
        for raw in [&project_raw, &global_raw] {
            assert!(!raw.contains("once"), "{raw}");
            assert!(!raw.contains("session"), "{raw}");
        }

        let loaded = PermissionStore::load_from(home.path(), &workspace).unwrap();
        assert_eq!(loaded.commands.len(), 3);
        assert!(loaded.commands.iter().all(|c| is_persistent(c.scope)));
        assert!(loaded.tools.is_empty());
        assert!(loaded.session_denies.is_empty());
    }

    #[test]
    fn legacy_session_entries_dropped_on_load() {
        // A pre-fix version could write `session` entries; loading self-heals.
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        std::fs::write(
            workspace.suis_dir.join("permissions.json"),
            r#"{"commands":[
                {"pattern":"echo *","scope":"session"},
                {"pattern":"cargo *","scope":"project"}
            ],"tools":[]}"#,
        )
        .unwrap();
        let loaded = PermissionStore::load_from(home.path(), &workspace).unwrap();
        assert_eq!(loaded.commands.len(), 1);
        assert_eq!(loaded.commands[0].pattern, "cargo *");
    }

    #[test]
    fn always_grant_spans_workspaces_project_grant_does_not() {
        let home = TempDir::new();
        let dir_a = TempDir::new();
        let dir_b = TempDir::new();
        let ws_a = ws(&dir_a);
        let ws_b = ws(&dir_b);

        let store = PermissionStore {
            commands: vec![
                cmd("cargo *", PermissionScope::Always),
                cmd("npm *", PermissionScope::Project),
            ],
            tools: Vec::new(),
            session_denies: Vec::new(),
        };
        store.save_to(home.path(), &ws_a).unwrap();

        let in_b = PermissionStore::load_from(home.path(), &ws_b).unwrap();
        assert_eq!(in_b.check_command("cargo test"), PermissionResult::Allow);
        assert_eq!(
            in_b.check_command("npm install"),
            PermissionResult::RequireApproval
        );

        let in_a = PermissionStore::load_from(home.path(), &ws_a).unwrap();
        assert_eq!(in_a.check_command("npm install"), PermissionResult::Allow);
    }

    #[test]
    fn project_deny_beats_global_always() {
        let home = TempDir::new();
        let dir_a = TempDir::new();
        let dir_b = TempDir::new();
        let ws_a = ws(&dir_a);
        let ws_b = ws(&dir_b);

        PermissionStore {
            commands: vec![cmd("cargo *", PermissionScope::Always)],
            tools: Vec::new(),
            session_denies: Vec::new(),
        }
        .save_to(home.path(), &ws_a)
        .unwrap();
        std::fs::write(
            ws_b.suis_dir.join("permissions.json"),
            r#"{"commands":[{"pattern":"cargo *","scope":"deny"}],"tools":[]}"#,
        )
        .unwrap();

        let in_b = PermissionStore::load_from(home.path(), &ws_b).unwrap();
        assert_eq!(in_b.check_command("cargo run"), PermissionResult::Deny);
        let in_a = PermissionStore::load_from(home.path(), &ws_a).unwrap();
        assert_eq!(in_a.check_command("cargo run"), PermissionResult::Allow);
    }

    #[test]
    fn hand_edited_global_deny_survives_save_and_stays_global() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let global_path = home.path().join("permissions.json");
        std::fs::write(
            &global_path,
            r#"{"commands":[{"pattern":"curl *","scope":"deny"}],"tools":[]}"#,
        )
        .unwrap();

        // A normal session: load (picks up the global deny), grant, save.
        let mut store = PermissionStore::load_from(home.path(), &workspace).unwrap();
        store
            .commands
            .push(cmd("git status", PermissionScope::Always));
        store.save_to(home.path(), &workspace).unwrap();

        let global: PermissionStore =
            serde_json::from_str(&std::fs::read_to_string(&global_path).unwrap()).unwrap();
        assert!(global
            .commands
            .iter()
            .any(|c| c.pattern == "curl *" && c.scope == PermissionScope::Deny));
        assert!(global
            .commands
            .iter()
            .any(|c| c.pattern == "git status" && c.scope == PermissionScope::Always));
        // The global deny was not copied into the project file.
        let project: PermissionStore = serde_json::from_str(
            &std::fs::read_to_string(workspace.suis_dir.join("permissions.json")).unwrap(),
        )
        .unwrap();
        assert!(project.commands.is_empty());
    }

    #[test]
    fn save_without_global_content_creates_no_global_file() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        PermissionStore {
            commands: vec![cmd("cargo *", PermissionScope::Project)],
            tools: Vec::new(),
            session_denies: Vec::new(),
        }
        .save_to(home.path(), &workspace)
        .unwrap();
        assert!(!home.path().join("permissions.json").exists());
        assert!(workspace.suis_dir.join("permissions.json").exists());
    }

    #[test]
    fn load_split_keeps_files_separate_and_filters_ephemeral() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        std::fs::write(
            home.path().join("permissions.json"),
            r#"{"commands":[
                {"pattern":"git status","scope":"always"},
                {"pattern":"echo *","scope":"session"}
            ],"tools":[]}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.suis_dir.join("permissions.json"),
            r#"{"commands":[{"pattern":"cargo *","scope":"project"}],"tools":[]}"#,
        )
        .unwrap();

        let (global, project) = PermissionStore::load_split_from(home.path(), &workspace).unwrap();
        assert_eq!(global.commands.len(), 1);
        assert_eq!(global.commands[0].pattern, "git status");
        assert_eq!(project.commands.len(), 1);
        assert_eq!(project.commands[0].pattern, "cargo *");
    }

    #[test]
    fn save_split_writes_verbatim_so_removals_persist() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        // A global file that already holds a hand-edited deny `save` would preserve.
        std::fs::write(
            home.path().join("permissions.json"),
            r#"{"commands":[{"pattern":"curl *","scope":"deny"}],"tools":[]}"#,
        )
        .unwrap();

        // Write back empty stores: save_split deletes the deny rather than keeping it.
        PermissionStore::save_split_to(
            home.path(),
            &PermissionStore::default(),
            &PermissionStore::default(),
            &workspace,
        )
        .unwrap();

        let (global, project) = PermissionStore::load_split_from(home.path(), &workspace).unwrap();
        assert!(global.commands.is_empty(), "global deny was removed");
        assert!(project.commands.is_empty());
    }

    #[test]
    fn save_split_round_trips() {
        let home = TempDir::new();
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let global = PermissionStore {
            commands: vec![cmd("git status", PermissionScope::Always)],
            tools: Vec::new(),
            session_denies: Vec::new(),
        };
        let project = PermissionStore {
            commands: vec![
                cmd("cargo *", PermissionScope::Project),
                cmd("git push", PermissionScope::Deny),
            ],
            tools: vec![ToolPermission {
                tool: "edit".into(),
                scope: PermissionScope::Project,
            }],
            session_denies: Vec::new(),
        };
        PermissionStore::save_split_to(home.path(), &global, &project, &workspace).unwrap();
        let (g, p) = PermissionStore::load_split_from(home.path(), &workspace).unwrap();
        assert_eq!(g, global);
        assert_eq!(p, project);
    }
}
