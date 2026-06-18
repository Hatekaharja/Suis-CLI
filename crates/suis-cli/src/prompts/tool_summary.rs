//! Arg-to-subject summaries for tool activity cards.
//!
//! A card shows `⚙ read src/main.rs …` while a tool runs; the part after the
//! tool name is the *subject*, derived here from the tool's primary argument.

use serde_json::Value;

/// Longest subject shown on a card before truncation.
const MAX_LEN: usize = 60;

/// Derive the one-line card subject from a tool's name and its model-supplied
/// args: the tool's primary argument, reduced to one short line. Unknown tools
/// (or missing/odd-typed args) yield an empty subject — the card then shows
/// just the tool name.
pub fn subject(name: &str, args: &Value) -> String {
    let key = match name {
        "read_lines" | "read" | "edit" => "path",
        "bash" => "command",
        "search" => "pattern",
        "git" => "args",
        "task" => "action",
        "explore" | "find" | "delegate" => "objective",
        _ => return String::new(),
    };
    let raw = args.get(key).and_then(Value::as_str).unwrap_or_default();
    let line = raw.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MAX_LEN {
        return line.to_string();
    }
    let truncated: String = line.chars().take(MAX_LEN).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primary_argument_per_tool() {
        assert_eq!(
            subject("read", &json!({ "path": "src/main.rs" })),
            "src/main.rs"
        );
        assert_eq!(
            subject("edit", &json!({ "path": "a.txt", "content": "x" })),
            "a.txt"
        );
        assert_eq!(
            subject("bash", &json!({ "command": "cargo test" })),
            "cargo test"
        );
        assert_eq!(
            subject("search", &json!({ "pattern": "fn main" })),
            "fn main"
        );
        assert_eq!(subject("git", &json!({ "args": "status" })), "status");
        assert_eq!(
            subject("task", &json!({ "action": "add", "title": "t" })),
            "add"
        );
    }

    #[test]
    fn unknown_tool_or_missing_arg_is_empty() {
        assert_eq!(subject("mystery", &json!({ "x": 1 })), "");
        assert_eq!(subject("read", &json!({})), "");
        assert_eq!(subject("read", &Value::Null), "");
        assert_eq!(subject("read", &json!({ "path": 42 })), "");
    }

    #[test]
    fn multiline_and_long_subjects_reduce_to_one_short_line() {
        assert_eq!(
            subject("bash", &json!({ "command": "echo one\necho two" })),
            "echo one"
        );
        let long = "x".repeat(100);
        let got = subject("bash", &json!({ "command": long }));
        assert_eq!(
            got.chars().count(),
            MAX_LEN + 1,
            "60 chars plus the ellipsis"
        );
        assert!(got.ends_with('…'));
    }
}
