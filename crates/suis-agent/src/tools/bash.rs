//! The `bash` tool: run a shell command in the workspace.
//!
//! Permission for the command is decided by the [`ToolExecutor`](super::executor)
//! *before* this runs; the tool itself only executes and reports output. The
//! command runs with the workspace root as its working directory.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    require_str, run_process_with_timeout, truncate_tail, Tool, ToolContext, ToolDefinition,
    ToolOutcome, COMMAND_TIMEOUT, MAX_BASH_OUTPUT_BYTES, MAX_COMMAND_TIMEOUT,
};

/// Executes a shell command via `sh -c`.
pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Run a shell command in the workspace root and return \
                          its combined stdout and stderr. Commands are killed \
                          after 30s by default; for a genuinely long-running \
                          command, set 'timeout' (seconds, up to 900). This tool \
                          does not background processes — never start a server or \
                          other never-returning command with it (it will hang \
                          until the timeout); run such things yourself."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Optional wall-clock limit in seconds (default 30, max 900). \
                                        Raise this only for a command you expect to run long."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let command = require_str(args, "command")?;
        let timeout = resolve_timeout(args);
        // Cap both success output and failure messages to the tail, so a
        // verbose build/test run can't swamp a small local context window.
        run_process_with_timeout(
            "sh",
            &["-c".to_string(), command],
            &ctx.workspace.root,
            timeout,
        )
        .map(|out| truncate_tail(&out, MAX_BASH_OUTPUT_BYTES))
        .map_err(|err| truncate_tail(&err, MAX_BASH_OUTPUT_BYTES))
    }
}

/// The wall-clock limit for a `bash` call: the `timeout` argument (seconds)
/// clamped to `[1s, MAX_COMMAND_TIMEOUT]`, or [`COMMAND_TIMEOUT`] when absent.
/// A zero or non-integer `timeout` falls back to the default rather than
/// erroring — the command still runs, just bounded.
fn resolve_timeout(args: &Value) -> Duration {
    match args.get("timeout").and_then(Value::as_u64) {
        Some(0) | None => COMMAND_TIMEOUT,
        Some(secs) => Duration::from_secs(secs).min(MAX_COMMAND_TIMEOUT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    #[test]
    fn runs_command_in_workspace() {
        let fx = Fixture::new();
        fx.write("marker.txt", "x");
        let out = BashTool
            .execute(&json!({ "command": "ls" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("marker.txt"));
    }

    #[test]
    fn nonzero_exit_is_error() {
        let fx = Fixture::new();
        let err = BashTool
            .execute(&json!({ "command": "exit 3" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("status 3"));
    }

    #[test]
    fn missing_command_errors() {
        let fx = Fixture::new();
        let err = BashTool.execute(&json!({}), &fx.ctx()).unwrap_err();
        assert!(err.contains("command"));
    }

    #[test]
    fn oversized_output_is_truncated_to_the_tail() {
        let fx = Fixture::new();
        // ~30 KiB of output (each line ~8 bytes), over the 16 KiB cap.
        let out = BashTool
            .execute(
                &json!({ "command": "i=0; while [ $i -lt 4000 ]; do echo line$i; i=$((i+1)); done" }),
                &fx.ctx(),
            )
            .unwrap();
        assert!(out.len() <= MAX_BASH_OUTPUT_BYTES + 64);
        assert!(out.starts_with("… (output truncated"));
        assert!(out.contains("line3999"));
    }

    #[test]
    fn small_output_is_not_truncated() {
        let fx = Fixture::new();
        let out = BashTool
            .execute(&json!({ "command": "echo hi" }), &fx.ctx())
            .unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn timeout_defaults_and_clamps() {
        // Absent, zero, or non-integer → the default limit.
        assert_eq!(resolve_timeout(&json!({})), COMMAND_TIMEOUT);
        assert_eq!(resolve_timeout(&json!({ "timeout": 0 })), COMMAND_TIMEOUT);
        assert_eq!(
            resolve_timeout(&json!({ "timeout": "soon" })),
            COMMAND_TIMEOUT
        );
        // A reasonable request is honored.
        assert_eq!(
            resolve_timeout(&json!({ "timeout": 300 })),
            Duration::from_secs(300)
        );
        // Anything over the ceiling is clamped to 15 minutes.
        assert_eq!(
            resolve_timeout(&json!({ "timeout": 99999 })),
            MAX_COMMAND_TIMEOUT
        );
    }

    #[test]
    fn a_command_exceeding_its_timeout_is_killed() {
        let fx = Fixture::new();
        // A 1s timeout on a 30s sleep returns promptly as a timeout error.
        let start = std::time::Instant::now();
        let err = BashTool
            .execute(&json!({ "command": "sleep 30", "timeout": 1 }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(10), "killed promptly");
    }
}
