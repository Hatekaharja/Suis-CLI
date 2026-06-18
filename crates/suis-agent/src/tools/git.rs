//! The `git` tool: run a git subcommand in the workspace.
//!
//! The level of access (disabled / read-only / read-write) is enforced by the
//! [`ToolExecutor`](super::executor) based on the project's `git_access`
//! setting. The tool itself just shells out to `git` in the workspace root.

use serde_json::{json, Value};

use super::{require_str, run_process, Tool, ToolContext, ToolDefinition, ToolOutcome};

/// Git subcommands that only read repository state. Used by the executor to
/// permit git use under `GitAccess::ReadOnly`.
///
/// `config` is intentionally **excluded**: although it can read values, it also
/// *writes* them, and keys like `core.sshCommand`, `core.pager`, `core.editor`,
/// or a `!`-prefixed alias execute an arbitrary command on the next git
/// operation. Allowing it under "read-only" would be a code-execution path.
pub const READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "ls-files",
    "rev-parse",
    "blame",
    "describe",
    "tag",
    "shortlog",
    "reflog",
    "cat-file",
    "for-each-ref",
];

/// Whether the first token of `args` is a read-only git subcommand.
pub fn is_read_only(args: &str) -> bool {
    let sub = args.split_whitespace().next().unwrap_or("");
    READ_ONLY_SUBCOMMANDS.contains(&sub)
}

/// Runs `git <args>` in the workspace.
pub struct GitTool;

impl Tool for GitTool {
    fn name(&self) -> &'static str {
        "git"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Run a git command in the workspace, e.g. 'status', \
                          'log --oneline -5', or 'diff'."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "The git arguments, without the leading 'git'."
                    }
                },
                "required": ["args"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let git_args = require_str(args, "args")?;
        let split: Vec<String> = git_args.split_whitespace().map(str::to_string).collect();
        if split.is_empty() {
            return Err("git requires a subcommand".to_string());
        }
        run_process("git", &split, &ctx.workspace.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    #[test]
    fn classifies_read_only_subcommands() {
        assert!(is_read_only("status"));
        assert!(is_read_only("log --oneline"));
        assert!(!is_read_only("commit -m x"));
        assert!(!is_read_only("push"));
        // `config` writes repo state (and can set code-executing keys), so it is
        // not read-only and must be blocked under GitAccess::ReadOnly.
        assert!(!is_read_only("config core.sshCommand 'sh -c evil'"));
        assert!(!is_read_only("config --global alias.x '!sh'"));
    }

    #[test]
    fn runs_git_in_workspace() {
        let fx = Fixture::new();
        // Skip cleanly if git is not installed in this environment.
        if run_process("git", &["init".into()], &fx.workspace.root).is_err() {
            return;
        }
        let out = GitTool
            .execute(&json!({ "args": "status --short" }), &fx.ctx())
            .unwrap();
        // Empty repo: `status --short` prints nothing → "(no output)".
        assert!(out == "(no output)" || out.contains("??"));
    }

    #[test]
    fn missing_args_errors() {
        let fx = Fixture::new();
        let err = GitTool.execute(&json!({}), &fx.ctx()).unwrap_err();
        assert!(err.contains("args"));
    }
}
