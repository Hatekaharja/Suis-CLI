//! The `search` tool: find a literal substring across workspace files.
//!
//! A dependency-free recursive walk over the workspace, skipping hidden files
//! (per the project guard), the `.suis` and `.git` directories, and anything
//! that does not decode as UTF-8. Matches are reported as `path:line: text`.

use std::path::Path;

use serde_json::{json, Value};

use suis_core::filesystem::guard;

use super::{
    access::rel_key, opt_str, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome,
    SKIP_DIRS,
};

/// Upper bound on reported matches, to keep results readable.
const MAX_RESULTS: usize = 200;

/// Lines at or below this length are reported in full; longer ones (e.g.
/// minified code) are collapsed to a window around the match.
const SNIPPET_LINE_LIMIT: usize = 120;

/// Characters of context kept on each side of the match in a collapsed line.
const SNIPPET_CONTEXT: usize = 25;

/// Searches file contents for a literal substring.
pub struct SearchTool;

impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Search the workspace for a literal text substring. \
                          Returns matching lines as 'path:line: text'."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The literal substring to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to scope the search (defaults to the whole workspace)."
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let pattern = require_str(args, "pattern")?;
        if pattern.is_empty() {
            return Err("search pattern must not be empty".to_string());
        }
        let root_arg = opt_str(args, "path").unwrap_or_else(|| ".".to_string());
        let root = ctx
            .workspace
            .check_boundary(&root_arg)
            .map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        if root.is_file() {
            // A search scoped at a single file: record it as searched even if the
            // pattern doesn't match, so the model may then `read_lines` it.
            if let Some(key) = rel_key(ctx.workspace, &root_arg) {
                if let Ok(mut log) = ctx.access.lock() {
                    log.record_searched(key);
                }
            }
            search_file(ctx, &root, &pattern, &mut results);
        } else {
            walk(ctx, &root, &pattern, &mut results);
        }

        if results.is_empty() {
            return Ok("No matches found.".to_string());
        }
        let truncated = results.len() > MAX_RESULTS;
        results.truncate(MAX_RESULTS);
        let mut out = results.join("\n");
        if truncated {
            out.push_str("\n… (results truncated)");
        }
        Ok(out)
    }
}

fn walk(ctx: &ToolContext<'_>, dir: &Path, pattern: &str, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if out.len() >= MAX_RESULTS {
            return;
        }
        let rel = path
            .strip_prefix(&ctx.workspace.root)
            .unwrap_or(&path)
            .to_path_buf();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        // Skip Suis/VCS internals, build/dependency dirs, and any hidden file.
        if matches!(name.as_deref(), Some(".suis") | Some(".git")) {
            continue;
        }
        if path.is_dir() && name.as_deref().is_some_and(|n| SKIP_DIRS.contains(&n)) {
            continue;
        }
        if guard::is_hidden(ctx.project, &rel) {
            continue;
        }

        if path.is_dir() {
            walk(ctx, &path, pattern, out);
        } else {
            search_file(ctx, &path, pattern, out);
            if out.len() >= MAX_RESULTS {
                return;
            }
        }
    }
}

/// Search a single file, appending `path:line: text` matches to `out`. A file
/// with at least one match is recorded as searched, so the model may then
/// `read_lines` it (the read gate consults this log).
fn search_file(ctx: &ToolContext<'_>, path: &Path, pattern: &str, out: &mut Vec<String>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let rel = path
        .strip_prefix(&ctx.workspace.root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut matched = false;
    for (i, line) in content.lines().enumerate() {
        if line.contains(pattern) {
            matched = true;
            out.push(format!("{}:{}: {}", rel, i + 1, snippet(line, pattern)));
            if out.len() >= MAX_RESULTS {
                break;
            }
        }
    }
    if matched {
        if let Ok(mut log) = ctx.access.lock() {
            log.record_searched(rel);
        }
    }
}

/// Render a matched line for output. Short lines are returned in full; long
/// lines (e.g. minified code) are collapsed to up to [`SNIPPET_CONTEXT`] chars
/// on each side of the first match, marked with `…`, so one line cannot flood
/// the result. Operates on chars (not bytes) to stay UTF-8 safe.
fn snippet(line: &str, pattern: &str) -> String {
    let line = line.trim_end();
    if line.chars().count() <= SNIPPET_LINE_LIMIT {
        return line.to_string();
    }
    let Some(start) = line.find(pattern) else {
        // Match was on the untrimmed line only (trailing whitespace); hard cap.
        return line.chars().take(SNIPPET_LINE_LIMIT).collect();
    };
    let before = &line[..start];
    let matched = &line[start..start + pattern.len()];
    let after = &line[start + pattern.len()..];

    let mut out = String::new();
    let b: Vec<char> = before.chars().collect();
    if b.len() > SNIPPET_CONTEXT {
        out.push('…');
        out.extend(&b[b.len() - SNIPPET_CONTEXT..]);
    } else {
        out.push_str(before);
    }
    out.push_str(matched);
    let a: Vec<char> = after.chars().collect();
    if a.len() > SNIPPET_CONTEXT {
        out.extend(&a[..SNIPPET_CONTEXT]);
        out.push('…');
    } else {
        out.push_str(after);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    #[test]
    fn finds_matches_with_line_numbers() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "fn main() {\n    let needle = 1;\n}\n");
        fx.write("src/other.rs", "// nothing here\n");
        let out = SearchTool
            .execute(&json!({ "pattern": "needle" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("src/main.rs:2:"));
        assert!(out.contains("needle"));
    }

    #[test]
    fn no_matches_reports_clearly() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello\n");
        let out = SearchTool
            .execute(&json!({ "pattern": "zzz" }), &fx.ctx())
            .unwrap();
        assert_eq!(out, "No matches found.");
    }

    #[test]
    fn skips_hidden_files() {
        let fx = Fixture::new();
        fx.write(".env", "TOKEN=needle\n");
        let mut project = fx.project.clone();
        project.hidden.push(".env".into());
        let ctx = ToolContext {
            workspace: &fx.workspace,
            project: &project,
            tasks: &fx.tasks,
            implement: None,
            access: &fx.access,
        };
        let out = SearchTool
            .execute(&json!({ "pattern": "needle" }), &ctx)
            .unwrap();
        assert_eq!(out, "No matches found.");
    }

    #[test]
    fn skips_build_and_dependency_dirs() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "let needle = 1;\n");
        fx.write("target/debug/build.rs", "let needle = 2;\n");
        fx.write("node_modules/pkg/index.js", "const needle = 3;\n");
        let out = SearchTool
            .execute(&json!({ "pattern": "needle" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("src/main.rs"));
        assert!(!out.contains("target/"), "build dir must be skipped: {out}");
        assert!(
            !out.contains("node_modules/"),
            "deps dir must be skipped: {out}"
        );
    }

    #[test]
    fn long_line_is_windowed_around_match() {
        let fx = Fixture::new();
        let long = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        fx.write("min.js", &format!("{long}\n"));
        let out = SearchTool
            .execute(&json!({ "pattern": "needle" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("needle"), "{out}");
        assert!(
            out.contains('…'),
            "long line must be marked as clipped: {out}"
        );
        // Far shorter than the original 406-char line.
        assert!(out.len() < 120, "windowed output should be bounded: {out}");
        assert!(
            !out.contains(&"a".repeat(30)),
            "left context must be clipped: {out}"
        );
        assert!(
            !out.contains(&"b".repeat(30)),
            "right context must be clipped: {out}"
        );
    }

    #[test]
    fn short_line_is_unchanged() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "    let needle = compute();\n");
        let out = SearchTool
            .execute(&json!({ "pattern": "needle" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("    let needle = compute();"), "{out}");
        assert!(!out.contains('…'), "short line must not be clipped: {out}");
    }

    #[test]
    fn empty_pattern_errors() {
        let fx = Fixture::new();
        let err = SearchTool
            .execute(&json!({ "pattern": "" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("empty"));
    }
}
