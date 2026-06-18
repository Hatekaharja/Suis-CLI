//! The `tree` tool: render the workspace's code layout as an indented listing.
//!
//! A dependency-free recursive walk over the workspace — the same guards as
//! `search` (hidden files per the project guard, the `.suis`/`.git` internals,
//! and the build/dependency dirs in [`SKIP_DIRS`]) — but emitting an indented
//! directory tree instead of content matches. It exists so a model can orient
//! itself on the code layout *before* reading or searching, rather than probing
//! files blindly. Names only — never file contents.

use std::path::Path;

use serde_json::{json, Value};

use suis_core::filesystem::guard;

use super::{opt_str, Tool, ToolContext, ToolDefinition, ToolOutcome, SKIP_DIRS};

/// Levels descended when the caller does not specify `depth`. Shallow on
/// purpose: an overview the model can expand into with `path`/`depth`.
const DEFAULT_DEPTH: usize = 3;

/// Hard ceiling on `depth`, so a single call cannot walk an unbounded tree.
const MAX_DEPTH: usize = 6;

/// Cap on entries listed within one directory, with a `… (N more)` marker —
/// one wide directory cannot swamp a small context window.
const MAX_ENTRIES_PER_DIR: usize = 40;

/// Cap on total lines rendered, with a trailing `… (truncated)` marker.
const MAX_LINES: usize = 400;

/// Renders the workspace layout as an indented directory tree.
pub struct TreeTool;

impl Tool for TreeTool {
    fn name(&self) -> &'static str {
        "tree"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Show a directory's layout as an indented tree. The system \
                          prompt already lists the top level, so reach for this only to look \
                          inside a specific directory you need to explore. Directories end \
                          with '/'. Build and dependency directories are listed but not \
                          expanded. A large layout is shown shallower so the whole structure \
                          stays visible; a directory ending in '/ …' has more inside — drill \
                          into it with `path` set to that directory rather than re-running \
                          this. Names only — no file contents."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to scope the tree to (defaults to the workspace root)."
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Optional number of levels to descend (default 3, max 6)."
                    }
                }
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let root_arg = opt_str(args, "path").unwrap_or_else(|| ".".to_string());
        let root = ctx
            .workspace
            .check_boundary(&root_arg)
            .map_err(|e| e.to_string())?;

        if !root.is_dir() {
            return Err(format!("not a directory: {root_arg}"));
        }

        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .map(|d| (d as usize).clamp(1, MAX_DEPTH))
            .unwrap_or(DEFAULT_DEPTH);

        // Fit by reducing depth, not by dropping siblings: render at the
        // requested depth and, while that overflows `MAX_LINES`, retry one level
        // shallower. Every top-level directory then stays named, so the model
        // sees the whole shape and can drill into a `…`-marked subtree — rather
        // than getting a deep dive into the first folder and nothing else.
        // `MAX_ENTRIES_PER_DIR` bounds breadth, so depth 1 always fits and the
        // loop terminates.
        let requested_depth = depth;
        let mut effective_depth = requested_depth;
        let (lines, elided) = loop {
            let mut lines = Vec::new();
            let elided = walk(ctx, &root, 0, effective_depth, &mut lines);
            if lines.len() <= MAX_LINES || effective_depth <= 1 {
                break (lines, elided);
            }
            effective_depth -= 1;
        };

        if lines.is_empty() {
            return Ok("No entries.".to_string());
        }
        let mut out = lines.join("\n");
        if effective_depth < requested_depth {
            out.push_str(&format!(
                "\n… (depth reduced to {effective_depth} to keep the overview whole — \
                 tree a subdirectory for more detail)"
            ));
        } else if elided {
            out.push_str("\n… (truncated — tree a subdirectory for more detail)");
        }
        Ok(out)
    }
}

/// Recursively render `dir` into `out`, indenting by `level`. Descends until
/// `level` reaches `max_depth`. Returns whether anything was elided (a
/// per-directory overflow or an undescended depth/build dir), so the caller can
/// mark the result. Mirrors `search::walk`'s guard order.
fn walk(
    ctx: &ToolContext<'_>,
    dir: &Path,
    level: usize,
    max_depth: usize,
    out: &mut Vec<String>,
) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    let mut truncated = false;
    let indent = "  ".repeat(level);
    let mut shown = 0;

    for path in &paths {
        // Stop one line past the budget so the caller can tell this depth
        // overflowed (and retry shallower) without materializing a huge tree.
        if out.len() > MAX_LINES {
            return true;
        }
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        // Skip Suis/VCS internals and any hidden file — same as search.
        if matches!(name.as_str(), ".suis" | ".git") {
            continue;
        }
        let rel = path.strip_prefix(&ctx.workspace.root).unwrap_or(path);
        if guard::is_hidden(ctx.project, rel) {
            continue;
        }

        if shown >= MAX_ENTRIES_PER_DIR {
            let more = paths.len() - shown;
            out.push(format!("{indent}… ({more} more)"));
            truncated = true;
            break;
        }
        shown += 1;

        let is_dir = path.is_dir();
        if !is_dir {
            out.push(format!("{indent}{name}"));
            continue;
        }

        // Build/dependency dirs are listed but never descended into.
        if SKIP_DIRS.contains(&name.as_str()) {
            out.push(format!("{indent}{name}/ …"));
            truncated = true;
            continue;
        }

        out.push(format!("{indent}{name}/"));
        if level + 1 < max_depth {
            truncated |= walk(ctx, path, level + 1, max_depth, out);
        } else if has_visible_child(ctx, path) {
            // Stopped at the depth limit but there is more below — mark it (like
            // an unexpanded build dir) so the model knows to drill in with
            // `tree <dir>` instead of re-running a broader tree.
            if let Some(last) = out.last_mut() {
                last.push_str(" …");
            }
            truncated = true;
        }
    }
    truncated
}

/// Whether `dir` has at least one entry the tree would render (used only to
/// decide whether a depth-capped directory should be marked as elided).
fn has_visible_child(ctx: &ToolContext<'_>, dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        match name.as_deref() {
            Some(".suis") | Some(".git") | None => false,
            Some(_) => {
                let rel = path.strip_prefix(&ctx.workspace.root).unwrap_or(&path);
                !guard::is_hidden(ctx.project, rel)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;
    use crate::tools::{ToolContext, ToolDefinition};
    use serde_json::json;

    fn run(fx: &Fixture, args: Value) -> String {
        TreeTool.execute(&args, &fx.ctx()).unwrap()
    }

    #[test]
    fn renders_nested_layout_with_indentation() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "");
        fx.write("src/lib/util.rs", "");
        fx.write("README.md", "");
        let out = run(&fx, json!({}));
        assert!(out.contains("README.md"), "{out}");
        assert!(out.contains("src/"), "{out}");
        assert!(out.contains("  main.rs"), "{out}");
        assert!(out.contains("  lib/"), "{out}");
        assert!(out.contains("    util.rs"), "{out}");
    }

    #[test]
    fn excludes_hidden_paths() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "");
        fx.write(".env", "TOKEN=shh");
        let mut project = fx.project.clone();
        project.hidden.push(".env".into());
        let ctx = ToolContext {
            workspace: &fx.workspace,
            project: &project,
            tasks: &fx.tasks,
            implement: None,
            access: &fx.access,
        };
        let out = TreeTool.execute(&json!({}), &ctx).unwrap();
        assert!(out.contains("src/"), "{out}");
        assert!(!out.contains(".env"), "hidden file must not appear: {out}");
    }

    #[test]
    fn lists_build_dirs_but_does_not_descend() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "");
        fx.write("target/debug/app", "");
        fx.write("node_modules/pkg/index.js", "");
        let out = run(&fx, json!({}));
        assert!(out.contains("target/ …"), "{out}");
        assert!(out.contains("node_modules/ …"), "{out}");
        assert!(
            !out.contains("debug"),
            "build dir must not be expanded: {out}"
        );
        assert!(
            !out.contains("index.js"),
            "deps dir must not be expanded: {out}"
        );
    }

    #[test]
    fn depth_limits_descent_and_marks_truncation() {
        let fx = Fixture::new();
        fx.write("a/b/c/deep.rs", "");
        // depth 1 shows only the top level, and marks `a` as having more inside
        // so the model drills in rather than re-running a broader tree.
        let shallow = run(&fx, json!({ "depth": 1 }));
        assert!(shallow.contains("a/ …"), "{shallow}");
        assert!(
            !shallow.contains("b/"),
            "depth 1 must not descend: {shallow}"
        );
        assert!(shallow.contains("… (truncated"), "{shallow}");
        // A deeper call reaches the file.
        let deep = run(&fx, json!({ "depth": 5 }));
        assert!(deep.contains("deep.rs"), "{deep}");
    }

    #[test]
    fn large_tree_reduces_depth_instead_of_dropping_siblings() {
        let fx = Fixture::new();
        // At the default depth (3) this renders ~480 lines (> MAX_LINES): 15 top
        // dirs + 15 `sub` dirs + 15×30 files. Depth-first tail-truncation would
        // show only the first few top dirs; depth reduction keeps them all.
        for i in 0..15 {
            for j in 0..30 {
                fx.write(&format!("dir{i:02}/sub/f{j:02}.rs"), "");
            }
        }
        let out = run(&fx, json!({}));
        assert!(out.contains("depth reduced"), "{out}");
        // Every top-level directory is still named — no siblings dropped.
        for i in 0..15 {
            assert!(
                out.contains(&format!("dir{i:02}/")),
                "missing dir{i:02}: {out}"
            );
        }
        // The reduced depth stops before the deep file contents.
        assert!(!out.contains("f00.rs"), "{out}");
    }

    #[test]
    fn depth_is_clamped_to_max() {
        let fx = Fixture::new();
        fx.write("a/b/c/d/e/f/g/leaf.rs", "");
        // Far beyond MAX_DEPTH; must not error and must descend at most MAX_DEPTH.
        let out = run(&fx, json!({ "depth": 999 }));
        assert!(out.contains("a/"), "{out}");
        assert!(!out.contains("leaf.rs"), "must stop at MAX_DEPTH: {out}");
    }

    #[test]
    fn path_scopes_into_a_subdirectory() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "");
        fx.write("docs/guide.md", "");
        let out = run(&fx, json!({ "path": "docs" }));
        assert!(out.contains("guide.md"), "{out}");
        assert!(
            !out.contains("main.rs"),
            "scope must exclude siblings: {out}"
        );
    }

    #[test]
    fn non_directory_path_errors() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "");
        let err = TreeTool
            .execute(&json!({ "path": "src/main.rs" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn definition_advertises_optional_path_and_depth() {
        let ToolDefinition {
            name, parameters, ..
        } = TreeTool.definition();
        assert_eq!(name, "tree");
        // Both params are optional: no `required` array.
        assert!(parameters.get("required").is_none(), "{parameters}");
        assert!(parameters["properties"].get("path").is_some());
        assert!(parameters["properties"].get("depth").is_some());
    }
}
