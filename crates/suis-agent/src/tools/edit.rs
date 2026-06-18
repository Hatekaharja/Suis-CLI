//! The `edit` tool: create or modify a workspace file.
//!
//! Two modes:
//! - **replace**: given `old_string` (and optional `new_string`), replace the
//!   single occurrence of `old_string` in an existing file.
//! - **write**: given `content`, write the whole file (creating it if absent).
//!
//! Replace matching is *whitespace-tolerant* (see [`apply_replace`]): a weak
//! local model that reproduces a snippet with the wrong indentation, trailing
//! whitespace, or line endings still lands the edit, instead of looping on
//! "old_string not found". Tolerance never guesses, though — every tier still
//! demands a unique match, so an ambiguous snippet is always an error.
//!
//! The boundary is always enforced here. The hardened-file guard is *not*
//! consulted by the tool: hardened writes are gated by the [`ToolExecutor`],
//! which prompts the user before this runs. The tool returns a unified diff of
//! the change as its output.

use serde_json::{json, Value};

use suis_core::filesystem::guard;

use super::{opt_str, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome};
use crate::diff;

/// Creates or edits a single file, returning a diff of the change.
pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Create or modify a file. Provide either 'content' to \
                          write the whole file, or 'old_string'/'new_string' to \
                          replace a unique snippet in an existing file."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path to the file."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full new contents of the file (write/create mode)."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Existing snippet to replace (replace mode). Match \
                                        the source text; indentation and trailing whitespace \
                                        need not be exact, but it must identify a single place."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement for 'old_string' (defaults to empty)."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = require_str(args, "path")?;
        // `check_write_target` (not `check_boundary`) so a symlink can't redirect
        // the write outside the workspace between the check and the write.
        let resolved = ctx
            .workspace
            .check_write_target(&path)
            .map_err(|e| e.to_string())?;
        let rel_path = resolved
            .strip_prefix(&ctx.workspace.root)
            .unwrap_or(&resolved)
            .to_path_buf();
        let rel = rel_path.to_string_lossy().replace('\\', "/");

        // Hidden files are never writable via `edit`: writing one would also leak
        // its prior contents through the returned diff. The executor gate already
        // denies this for the model; enforce it here too for any direct caller.
        if guard::is_hidden(ctx.project, &rel_path) {
            return Err(format!("cannot edit a hidden file: {rel}"));
        }

        let existing = std::fs::read_to_string(&resolved).ok();

        let new_content = if let Some(old) = opt_str(args, "old_string") {
            let current = existing
                .as_ref()
                .ok_or_else(|| format!("cannot replace in '{rel}': file does not exist"))?;
            let new = opt_str(args, "new_string").unwrap_or_default();
            apply_replace(current, &old, &new).map_err(|e| format!("{e} in '{rel}'"))?
        } else if let Some(content) = opt_str(args, "content") {
            content
        } else {
            return Err("edit requires either 'content' or 'old_string'".to_string());
        };

        let old_content = existing.unwrap_or_default();
        if old_content == new_content {
            return Ok(format!("No changes to '{rel}'."));
        }

        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&resolved, &new_content).map_err(|e| e.to_string())?;

        let diff_text = diff::unified(&old_content, &new_content, &rel);
        Ok(format!("Edited '{rel}'.\n{diff_text}"))
    }
}

/// Where a unique `old_string` match was found within a file.
enum Locate {
    /// A unique whole-line span, as a byte range `[start, end)` of the file.
    Unique(usize, usize),
    /// No match under this tier's tolerance.
    None,
    /// Several matches — too ambiguous to edit safely.
    Ambiguous(usize),
}

/// Replace `old` with `new` in `current`, tolerating the whitespace a weak model
/// commonly gets wrong. Tiers, most precise first:
/// 0. exact byte match (unchanged behaviour);
/// 1. line-aligned, ignoring each line's *trailing* whitespace and line endings;
/// 2. line-aligned, ignoring *leading indentation* — the replacement is then
///    reindented to the file's actual indentation so the result stays correct.
///
/// Every tier still requires a *unique* match: tolerance widens what counts as
/// "found", never which place gets edited, so an ambiguous snippet is an error
/// rather than a silent edit of the wrong spot.
fn apply_replace(current: &str, old: &str, new: &str) -> Result<String, String> {
    match current.matches(old).count() {
        1 => return Ok(current.replacen(old, new, 1)),
        0 => {}
        n => return Err(format!("old_string is ambiguous ({n} matches)")),
    }
    if old.is_empty() {
        return Err("old_string not found".to_string());
    }

    // Tier 1: trailing-whitespace / line-ending insensitive; indentation kept,
    // so the replacement drops in verbatim.
    match locate_lines(current, old, str::trim_end) {
        Locate::Unique(start, end) => return Ok(splice(current, start, end, new)),
        Locate::Ambiguous(n) => return Err(format!("old_string is ambiguous ({n} matches)")),
        Locate::None => {}
    }

    // Tier 2: indentation insensitive; reindent the replacement onto the file's
    // actual leading whitespace so it doesn't inherit the model's wrong indent.
    match locate_lines(current, old, str::trim) {
        Locate::Unique(start, end) => {
            let file_indent = leading_ws(current[start..].lines().next().unwrap_or(""));
            let old_indent = leading_ws(old.lines().next().unwrap_or(""));
            let adjusted = reindent(new, old_indent, file_indent);
            Ok(splice(current, start, end, &adjusted))
        }
        Locate::Ambiguous(n) => Err(format!("old_string is ambiguous ({n} matches)")),
        Locate::None => Err("old_string not found".to_string()),
    }
}

/// Byte offset where each line of `s` starts, plus a trailing sentinel equal to
/// `s.len()`, so line `k` spans `s[offsets[k]..offsets[k + 1]]`.
fn line_offsets(s: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    if *offsets.last().unwrap() != s.len() {
        offsets.push(s.len());
    }
    offsets
}

/// Find the unique run of whole lines in `current` whose normalized form equals
/// `old`'s lines normalized by `norm`. Returns the byte span covering those
/// lines (including the trailing newline of the last one).
fn locate_lines(current: &str, old: &str, norm: fn(&str) -> &str) -> Locate {
    let offsets = line_offsets(current);
    let line_count = offsets.len() - 1;
    let old_lines: Vec<&str> = old.lines().collect();
    let len = old_lines.len();
    if len == 0 || len > line_count {
        return Locate::None;
    }
    let target: Vec<&str> = old_lines.iter().map(|line| norm(line)).collect();

    let mut first: Option<(usize, usize)> = None;
    let mut count = 0usize;
    for i in 0..=(line_count - len) {
        let matched = (0..len).all(|k| {
            let line = current[offsets[i + k]..offsets[i + k + 1]]
                .trim_end_matches('\n')
                .trim_end_matches('\r');
            norm(line) == target[k]
        });
        if matched {
            count += 1;
            first.get_or_insert((offsets[i], offsets[i + len]));
        }
    }
    match (count, first) {
        (1, Some((start, end))) => Locate::Unique(start, end),
        (0, _) => Locate::None,
        (n, _) => Locate::Ambiguous(n),
    }
}

/// Splice `new` into `current` over the byte range `[start, end)`, preserving
/// the replaced span's trailing-newline state so surrounding lines stay intact.
fn splice(current: &str, start: usize, end: usize, new: &str) -> String {
    let mut piece = new.to_string();
    let span_has_newline = end > start && current.as_bytes()[end - 1] == b'\n';
    if span_has_newline && !piece.is_empty() && !piece.ends_with('\n') {
        piece.push('\n');
    }
    let mut out = String::with_capacity(start + piece.len() + (current.len() - end));
    out.push_str(&current[..start]);
    out.push_str(&piece);
    out.push_str(&current[end..]);
    out
}

/// The leading whitespace (indentation) of a line.
fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Re-base `new`'s indentation from `old_indent` to `file_indent`: strip the
/// snippet's assumed leading indent from each line and apply the file's real
/// one, preserving relative indentation inside the block. Blank lines stay
/// blank; a trailing newline is preserved.
fn reindent(new: &str, old_indent: &str, file_indent: &str) -> String {
    if old_indent == file_indent || new.is_empty() {
        return new.to_string();
    }
    let mut out = String::new();
    for (i, line) in new.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        match line.strip_prefix(old_indent) {
            Some(rest) => {
                out.push_str(file_indent);
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    if new.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    #[test]
    fn writes_new_file_with_content() {
        let fx = Fixture::new();
        let out = EditTool
            .execute(&json!({ "path": "a.txt", "content": "hi\n" }), &fx.ctx())
            .unwrap();
        assert_eq!(fx.read("a.txt"), "hi\n");
        assert!(out.contains("+hi"));
    }

    #[test]
    fn replaces_unique_snippet() {
        let fx = Fixture::new();
        fx.write("src/main.rs", "let x = 1;\nlet y = 2;\n");
        EditTool
            .execute(
                &json!({ "path": "src/main.rs", "old_string": "let x = 1;", "new_string": "let x = 9;" }),
                &fx.ctx(),
            )
            .unwrap();
        assert_eq!(fx.read("src/main.rs"), "let x = 9;\nlet y = 2;\n");
    }

    #[test]
    fn replace_tolerates_trailing_whitespace_and_crlf() {
        let fx = Fixture::new();
        // File has trailing spaces and CRLF endings the model won't reproduce.
        fx.write("a.rs", "fn main() {  \r\n    go();\r\n}\r\n");
        EditTool
            .execute(
                &json!({
                    "path": "a.rs",
                    "old_string": "fn main() {\n    go();",
                    "new_string": "fn main() {\n    run();",
                }),
                &fx.ctx(),
            )
            .unwrap();
        let after = fx.read("a.rs");
        assert!(after.contains("run();"), "{after}");
        assert!(!after.contains("go();"), "{after}");
    }

    #[test]
    fn replace_tolerates_indentation_and_reindents() {
        let fx = Fixture::new();
        // The block is indented 8 spaces in the file; the model supplies it at 0.
        fx.write(
            "a.rs",
            "mod m {\n    fn f() {\n        let x = 1;\n    }\n}\n",
        );
        EditTool
            .execute(
                &json!({
                    "path": "a.rs",
                    "old_string": "let x = 1;",
                    "new_string": "let x = 2;",
                }),
                &fx.ctx(),
            )
            .unwrap();
        // The replacement keeps the file's original 8-space indentation.
        assert!(
            fx.read("a.rs").contains("        let x = 2;"),
            "{}",
            fx.read("a.rs")
        );
    }

    #[test]
    fn replace_reindents_a_multi_line_block() {
        let fx = Fixture::new();
        fx.write("a.rs", "fn f() {\n    a();\n    b();\n}\n");
        // Model gives the block unindented; relative indentation is preserved and
        // the file's 4-space base is restored.
        EditTool
            .execute(
                &json!({
                    "path": "a.rs",
                    "old_string": "a();\nb();",
                    "new_string": "a();\nif t {\n    c();\n}",
                }),
                &fx.ctx(),
            )
            .unwrap();
        let after = fx.read("a.rs");
        assert!(after.contains("    a();"), "{after}");
        assert!(after.contains("    if t {"), "{after}");
        assert!(after.contains("        c();"), "{after}");
    }

    #[test]
    fn tolerant_match_still_rejects_ambiguity() {
        let fx = Fixture::new();
        // Two trim-equal occurrences (different indentation): no unique target.
        fx.write("a.rs", "    foo();\n        foo();\n");
        let err = EditTool
            .execute(
                &json!({ "path": "a.rs", "old_string": "foo();", "new_string": "bar();" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        assert_eq!(fx.read("a.rs"), "    foo();\n        foo();\n");
    }

    #[test]
    fn ambiguous_old_string_errors() {
        let fx = Fixture::new();
        fx.write("a.txt", "dup\ndup\n");
        let err = EditTool
            .execute(&json!({ "path": "a.txt", "old_string": "dup" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("ambiguous"));
        // File untouched.
        assert_eq!(fx.read("a.txt"), "dup\ndup\n");
    }

    #[test]
    fn missing_old_string_errors() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello\n");
        let err = EditTool
            .execute(&json!({ "path": "a.txt", "old_string": "nope" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn requires_content_or_old_string() {
        let fx = Fixture::new();
        let err = EditTool
            .execute(&json!({ "path": "a.txt" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("requires"));
    }

    #[test]
    fn hidden_file_edit_is_denied_without_leaking_contents() {
        let fx = Fixture::new();
        fx.write(".env", "SECRET=1\n");
        let mut project = fx.project.clone();
        project.hidden.push(".env".into());
        let ctx = ToolContext {
            workspace: &fx.workspace,
            project: &project,
            tasks: &fx.tasks,
            implement: None,
            access: &fx.access,
        };
        let err = EditTool
            .execute(&json!({ "path": ".env", "content": "SECRET=2\n" }), &ctx)
            .unwrap_err();
        assert!(err.contains("hidden"));
        // The secret must not appear in the error, and the file is untouched.
        assert!(!err.contains("SECRET"));
        assert_eq!(fx.read(".env"), "SECRET=1\n");
    }

    #[test]
    fn outside_workspace_is_denied() {
        let fx = Fixture::new();
        let err = EditTool
            .execute(
                &json!({ "path": "../escape.txt", "content": "x" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("boundary") || err.contains("permission"));
    }
}
