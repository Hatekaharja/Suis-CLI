//! Permission data types.

use serde::{Deserialize, Serialize};

/// How long a granted permission lasts (or that it is denied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    /// Allow this one invocation only (ephemeral, not persisted as a grant).
    Once,
    /// Allow for the remainder of the session.
    Session,
    /// Allow persistently for this project.
    Project,
    /// Allow persistently across all projects.
    Always,
    /// Explicitly deny.
    Deny,
}

/// A stored permission for a shell command pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPermission {
    /// Exact command or wildcard pattern, e.g. `"cargo test"` or `"cargo *"`.
    pub pattern: String,
    /// The granted scope.
    pub scope: PermissionScope,
}

/// A stored permission for a named tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermission {
    /// Tool name, e.g. `"edit"`.
    pub tool: String,
    /// The granted scope.
    pub scope: PermissionScope,
}

/// The outcome of evaluating a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResult {
    /// Proceed without prompting.
    Allow,
    /// Block outright.
    Deny,
    /// Prompt the user for a decision.
    RequireApproval,
}
