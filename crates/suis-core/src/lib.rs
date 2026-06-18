//! suis-core — shared business logic for Suis.
//!
//! Owns configuration, workspace management, permissions, filesystem safety,
//! and project metadata. This crate has no internal dependencies and performs
//! no network I/O.

pub mod config;
pub mod errors;
pub mod filesystem;
pub mod permissions;
pub mod projects;
pub mod workspace;

mod util;

#[cfg(test)]
mod test_util;

pub use config::{GlobalConfig, ProviderConfig, ProviderEntry, Settings};
pub use errors::{ConfigError, Error, FilesystemError, ProviderError, Result, WorkspaceError};
pub use filesystem::TrashEntry;
pub use permissions::{
    CommandPermission, PermissionResult, PermissionScope, PermissionStore, ToolPermission,
};
pub use projects::{
    GitAccess, ModelScope, Plan, PlanStatus, PlanStep, PlanStore, PlanTask, ProjectConfig,
    ProjectProfile, ProviderScope, TaskStatus,
};
pub use workspace::Workspace;
