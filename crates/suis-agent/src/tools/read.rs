//! The `read_lines` tool: return a bounded range of lines from a workspace file.
//!
//! There is deliberately no "read the whole file" tool: a weak local model that
//! slurps entire files exhausts its context window fast. Instead the model
//! `search`es to find the lines it cares about (the executor gate even requires
//! a prior search of the file), then reads a window around them. Every read is
//! capped to [`MAX_LINES`] lines and [`MAX_READ_BYTES`] bytes.

use serde_json::{json, Value};

use suis_core::filesystem::ops;

use super::{
    access::rel_key, require_str, truncate_head, Tool, ToolContext, ToolDefinition, ToolOutcome,
    MAX_READ_BYTES,
};

/// Lines returned when only a `start_line` (or nothing) is given.
const DEFAULT_LINES: usize = 200;

/// Hard cap on the lines a single `read_lines` returns, however wide the
/// requested range — a backstop against reading a whole file by asking for
/// `1..100000`.
const MAX_LINES: usize = 400;

/// Reads a bounded range of lines from a single file, honoring the workspace
/// boundary and the hidden-file guard (both enforced by `suis-core`).
pub struct ReadLinesTool;

impl Tool for ReadLinesTool {
    fn name(&self) -> &'static str {
        "read_lines"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Read a range of lines from a file in the workspace. Search the file \
                          first (line numbers come from search results), then read a window \
                          around the lines you need. Returns at most 400 lines per call."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path to the file to read."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to return, 1-based (default 1)."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line to return, 1-based and inclusive (default start_line + 199)."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = require_str(args, "path")?;
        let content = ops::read(ctx.workspace, ctx.project, &path).map_err(|e| e.to_string())?;

        let total = content.lines().count();
        let start = opt_usize(args, "start_line").unwrap_or(1).max(1);
        let end = match opt_usize(args, "end_line") {
            Some(e) => e.max(start),
            None => start + DEFAULT_LINES - 1,
        };
        // Clamp the window to the file and to the per-call line cap.
        let end = end.min(start + MAX_LINES - 1);

        if total == 0 {
            return Ok(format!("(empty file: {path})"));
        }
        if start > total {
            return Err(format!(
                "start_line {start} is past the end of '{path}' ({total} lines)"
            ));
        }
        let last = end.min(total);

        // Collect the 1-based inclusive range [start, last].
        let body: String = content
            .lines()
            .skip(start - 1)
            .take(last - start + 1)
            .collect::<Vec<_>>()
            .join("\n");

        // Record the read so a later `edit` of this file is allowed.
        if let Some(key) = rel_key(ctx.workspace, &path) {
            if let Ok(mut log) = ctx.access.lock() {
                log.record_read(key);
            }
        }

        let header = format!("{path} lines {start}-{last} of {total}:\n");
        Ok(truncate_head(&format!("{header}{body}"), MAX_READ_BYTES))
    }
}

/// Extract an optional non-negative integer argument.
fn opt_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    fn body(out: &str) -> &str {
        // Strip the "path lines a-b of n:\n" header for content assertions.
        out.split_once('\n').map(|(_, b)| b).unwrap_or(out)
    }

    #[test]
    fn reads_a_line_range() {
        let fx = Fixture::new();
        fx.write("a.txt", "one\ntwo\nthree\nfour\n");
        let out = ReadLinesTool
            .execute(
                &json!({ "path": "a.txt", "start_line": 2, "end_line": 3 }),
                &fx.ctx(),
            )
            .unwrap();
        assert!(out.starts_with("a.txt lines 2-3 of 4:"));
        assert_eq!(body(&out), "two\nthree");
    }

    #[test]
    fn defaults_to_the_head_window() {
        let fx = Fixture::new();
        let text: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fx.write("a.txt", &text);
        let out = ReadLinesTool
            .execute(&json!({ "path": "a.txt" }), &fx.ctx())
            .unwrap();
        // Whole short file fits in the default window.
        assert!(body(&out).starts_with("line1\n"));
        assert!(body(&out).ends_with("line10"));
    }

    #[test]
    fn caps_a_huge_range_to_the_line_limit() {
        let fx = Fixture::new();
        let text: String = (1..=1000).map(|i| format!("line{i}\n")).collect();
        fx.write("big.txt", &text);
        let out = ReadLinesTool
            .execute(
                &json!({ "path": "big.txt", "start_line": 1, "end_line": 100000 }),
                &fx.ctx(),
            )
            .unwrap();
        // Header reports the clamped end, not the requested one.
        assert!(out.starts_with(&format!("big.txt lines 1-{MAX_LINES} of 1000:")));
        assert!(body(&out).contains(&format!("line{MAX_LINES}")));
        assert!(!body(&out).contains(&format!("line{}", MAX_LINES + 1)));
    }

    #[test]
    fn start_past_end_errors() {
        let fx = Fixture::new();
        fx.write("a.txt", "one\ntwo\n");
        let err = ReadLinesTool
            .execute(&json!({ "path": "a.txt", "start_line": 99 }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("past the end"), "{err}");
    }

    #[test]
    fn records_the_read_in_the_access_log() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello\n");
        ReadLinesTool
            .execute(&json!({ "path": "a.txt" }), &fx.ctx())
            .unwrap();
        assert!(fx.access.lock().unwrap().was_read("a.txt"));
    }

    #[test]
    fn missing_path_argument_errors() {
        let fx = Fixture::new();
        let err = ReadLinesTool.execute(&json!({}), &fx.ctx()).unwrap_err();
        assert!(err.contains("path"));
    }

    #[test]
    fn hidden_file_is_denied() {
        let fx = Fixture::new();
        fx.write(".env", "SECRET=1");
        let mut project = fx.project.clone();
        project.hidden.push(".env".into());
        let ctx = ToolContext {
            workspace: &fx.workspace,
            project: &project,
            tasks: &fx.tasks,
            implement: None,
            access: &fx.access,
        };
        let err = ReadLinesTool
            .execute(&json!({ "path": ".env" }), &ctx)
            .unwrap_err();
        assert!(!err.contains("SECRET"));
    }

    #[test]
    fn definition_is_valid_object_schema() {
        let def = ReadLinesTool.definition();
        assert_eq!(def.name, "read_lines");
        assert_eq!(def.parameters["type"], "object");
        assert!(def.parameters["properties"]["path"].is_object());
    }
}
