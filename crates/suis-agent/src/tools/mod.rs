//! The agent's tools: their definitions, execution, and permission gating.
//!
//! Each tool is a zero-sized type implementing [`Tool`]. A tool exposes a JSON
//! Schema [`definition`](Tool::definition) to the model and an
//! [`execute`](Tool::execute) implementation that does the work against a
//! [`ToolContext`]. Execution returns `Result<String, String>`: the `Ok` string
//! is the tool's output, the `Err` string a human-readable failure message. The
//! [`ToolExecutor`] attaches the originating call id and permission decisions,
//! producing the final [`ToolResult`].

pub mod access;
pub mod bash;
pub mod edit;
pub mod executor;
pub mod git;
pub mod plan;
pub mod read;
pub mod search;
pub mod subagent;
pub mod task;
pub mod tree;
pub mod types;

use std::sync::{Arc, Mutex};

use serde_json::Value;

use suis_core::{ProjectConfig, Workspace};

use crate::tasks::TaskStore;

pub use access::AccessLog;
pub use executor::ToolExecutor;
pub use plan::PlanDraft;
pub use types::{ToolCall, ToolDefinition, ToolResult};

/// The result of a tool's own execution: `Ok` output, or `Err` failure message.
pub type ToolOutcome = Result<String, String>;

/// Shared state a tool executes against. Holds borrows of session state plus a
/// handle to the (lockable) task store the `task` tool mutates.
pub struct ToolContext<'a> {
    /// The workspace, for boundary-checked filesystem access.
    pub workspace: &'a Workspace,
    /// The project config, for hidden/hardened and git-access policy.
    pub project: &'a ProjectConfig,
    /// The session task store.
    pub tasks: &'a Arc<Mutex<TaskStore>>,
    /// The active implementation target, if this is an `/implement` session;
    /// while set, the `task` tool operates on the plan step's tasks.
    pub implement: Option<&'a crate::runtime::session::ImplementTarget>,
    /// The session's file-access log: `search` and `read_lines` record into it,
    /// and the executor gate consults it to enforce search-before-read and
    /// read-before-edit. Shared so a tool body on a blocking thread can record.
    pub access: &'a Arc<Mutex<AccessLog>>,
}

/// A tool the agent can expose to a model and execute on its behalf.
pub trait Tool: Send + Sync {
    /// The tool's stable name, matching the model-facing schema.
    fn name(&self) -> &'static str;

    /// The JSON-Schema definition advertised to the model.
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with `args` (the model-supplied arguments object).
    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome;
}

/// The built-in tools, in their canonical order: the read-only orientation and
/// file tools, the write/execute tools, and the Plan-mode-only `plan` tool.
pub fn default_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(tree::TreeTool),
        Box::new(read::ReadLinesTool),
        Box::new(search::SearchTool),
        Box::new(edit::EditTool),
        Box::new(bash::BashTool),
        Box::new(git::GitTool),
        Box::new(task::TaskTool),
        Box::new(plan::PlanTool),
        Box::new(subagent::ExploreTool),
        Box::new(subagent::FindTool),
        Box::new(subagent::DelegateTool),
    ]
}

/// The definitions of [`default_tools`], for context assembly.
pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    default_tools().iter().map(|t| t.definition()).collect()
}

/// Extract a required string argument, or a corrective error message.
pub(crate) fn require_str(args: &Value, key: &str) -> Result<String, String> {
    if let Some(s) = args.get(key).and_then(Value::as_str) {
        return Ok(s.to_string());
    }
    Err(missing_arg_message(args, key))
}

/// Validate a model's `args` against the tool's top-level JSON schema *before*
/// dispatch: every `required` field must be present, and any present field must
/// match its declared `type`. Returns a corrective, model-facing message naming
/// the offending field and the expected type — the same self-correction spirit
/// as [`missing_arg_message`]/`unknown_tool_message`, so a weak model fixes the
/// call on the next turn instead of consuming an iteration on a hard failure.
///
/// Only the top level is checked: enough to catch the common weak-model
/// mistakes (a missing field, a string sent as a number/object) while leaving
/// deeper structural validation to each tool's own parser. A missing *string*
/// field defers to [`missing_arg_message`] so its synonym hints survive.
pub(crate) fn validate_args(definition: &ToolDefinition, args: &Value) -> Result<(), String> {
    let Some(schema) = definition.parameters.as_object() else {
        return Ok(());
    };
    let props = schema.get("properties").and_then(Value::as_object);

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            let present = args.get(field).map(|v| !v.is_null()).unwrap_or(false);
            if !present {
                let ty = props
                    .and_then(|p| p.get(field))
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str);
                return Err(match ty {
                    Some("string") | None => missing_arg_message(args, field),
                    Some(other) => format!("missing required {other} argument '{field}'"),
                });
            }
        }
    }

    if let (Some(props), Some(obj)) = (props, args.as_object()) {
        for (key, value) in obj {
            if value.is_null() {
                continue;
            }
            let Some(expected) = props
                .get(key)
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !json_type_matches(expected, value) {
                return Err(format!(
                    "argument '{key}' must be {} {expected}, got {}",
                    article(expected),
                    value_kind(value)
                ));
            }
        }
    }
    Ok(())
}

/// Whether `value` satisfies a JSON-Schema `type` keyword. Unknown type names
/// pass (the schema is not second-guessed).
fn json_type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// "a"/"an" for the expected-type word in a corrective message.
fn article(ty: &str) -> &'static str {
    match ty.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// A short noun for the JSON kind actually received, for the corrective message.
fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A corrective error for a missing/mistyped string argument: keeps the stable
/// `missing required string argument '<key>'` prefix, then names the keys
/// actually received and hints at a common synonym, so a weaker model can
/// self-correct on the next turn.
fn missing_arg_message(args: &Value, key: &str) -> String {
    let mut msg = format!("missing required string argument '{key}'");
    match args.as_object() {
        Some(obj) if !obj.is_empty() => {
            let received: Vec<&str> = obj.keys().map(String::as_str).collect();
            msg.push_str(&format!("; received keys: [{}]", received.join(", ")));
            if let Some(synonym) = received.iter().find(|r| is_arg_synonym(key, r)) {
                msg.push_str(&format!("; use '{key}' instead of '{synonym}'"));
            }
        }
        Some(_) => msg.push_str("; no arguments were provided"),
        None => msg.push_str("; arguments were not a JSON object"),
    }
    msg
}

/// Whether `candidate` is a common model-chosen synonym for the real argument
/// `key` (for the corrective hint only — arguments are never rewritten).
fn is_arg_synonym(key: &str, candidate: &str) -> bool {
    let synonyms: &[&str] = match key {
        "path" => &["file", "filename", "filepath", "file_path"],
        "content" => &["text", "contents", "body", "data"],
        "command" => &["cmd", "shell", "script"],
        "pattern" => &["query", "regex", "text", "search"],
        _ => &[],
    };
    synonyms.contains(&candidate)
}

/// Extract an optional string argument.
pub(crate) fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Default wall-clock limit on a single child process. A command that runs
/// longer is killed and reported as timed out, so a model that starts a server
/// (or any never-returning command) without backgrounding it can't wedge the
/// turn forever. The `bash` tool lets a call raise this up to
/// [`MAX_COMMAND_TIMEOUT`] via an explicit `timeout` argument.
pub(crate) const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Ceiling on a `bash` call's `timeout` argument: 15 minutes. A longer request
/// is clamped to this, so even an explicitly long-running command is bounded.
pub(crate) const MAX_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Cap on the `bash` output returned to the model. Large build/test logs are
/// truncated to the **tail** (the failure summary lives at the end) so one
/// verbose command cannot swamp a small local context window.
pub(crate) const MAX_BASH_OUTPUT_BYTES: usize = 16 * 1024;

/// Cap on the `read` output returned to the model. Oversized files are
/// truncated to the **head**, with a marker telling the model to search.
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024;

/// Directory names the file-walking tools (`search`, `tree`) treat as opaque:
/// build outputs and dependency trees that would otherwise dominate a walk
/// (slow) and flood results with artifacts — especially costly for a small
/// local-model context. `search` skips them wholesale; `tree` lists them but
/// does not descend.
pub(crate) const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    "vendor",
    ".next",
];

/// Run a child process in `cwd`, capturing stdout+stderr. Returns the combined
/// output on success (exit 0) and an error string (output plus exit status) on
/// failure or spawn error. A command exceeding [`COMMAND_TIMEOUT`] is killed
/// and reported as timed out.
pub(crate) fn run_process(program: &str, args: &[String], cwd: &std::path::Path) -> ToolOutcome {
    run_process_with_timeout(program, args, cwd, COMMAND_TIMEOUT)
}

/// [`run_process`] with an explicit deadline (the `bash` tool passes a
/// caller-chosen timeout, and the seam tests inject a short one).
pub(crate) fn run_process_with_timeout(
    program: &str,
    args: &[String],
    cwd: &std::path::Path,
    timeout: std::time::Duration,
) -> ToolOutcome {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put the child in its own process group so a timeout can kill the whole
    // tree (the shell and anything it spawned), not just the leader.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to run {program}: {e}"))?;

    // Drain stdout/stderr on threads so a chatty child can't deadlock against a
    // full pipe buffer while we poll for exit.
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = child_stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = child_stderr.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                kill_process_tree(&mut child);
                let _ = child.wait();
                return Err(format!("failed to run {program}: {e}"));
            }
        }
    };

    if timed_out {
        // Return as soon as the deadline fires. The process-group kill normally
        // closes the pipes, but a command can deliberately detach a descendant
        // into another process group/session while leaving stdout/stderr
        // inherited. Waiting for the reader threads in that case extends a 30s
        // timeout until the detached process exits. Dropping the handles
        // detaches those short-lived readers; they finish when the inherited
        // descriptors eventually close, while the tool reports the timeout on
        // time.
        return Err(format!("command timed out after {}s", timeout.as_secs()));
    }

    // The child exited normally, so its pipes are closed and these joins return
    // promptly with the captured output.
    let stdout_buf = out_reader.join().unwrap_or_default();
    let stderr_buf = err_reader.join().unwrap_or_default();

    let status = status.expect("non-timeout path always has an exit status");

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&stdout_buf));
    let stderr = String::from_utf8_lossy(&stderr_buf);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    let combined = combined.trim_end().to_string();

    if status.success() {
        Ok(if combined.is_empty() {
            "(no output)".to_string()
        } else {
            combined
        })
    } else {
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        Err(format!("exited with status {code}\n{combined}"))
    }
}

/// Best-effort kill of a child and its process group.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child leads its own group (see `process_group(0)` at spawn), so
        // its pid is the group id; a negative pid signals the whole group.
        let pgid = child.id() as i32;
        // SAFETY: `kill(2)` with a constant signal and no borrowed state;
        // result ignored because this is best-effort cleanup.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    // Always also signal the leader directly (covers non-unix and the race
    // where the group kill missed a just-spawned member).
    let _ = child.kill();
}

/// Truncate `s` to at most `max` bytes, keeping the **tail** with a leading
/// marker. Used for command output, where the meaningful summary is at the end.
/// Snaps to a line boundary where possible and never splits a UTF-8 codepoint.
pub(crate) fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    let kept = &s[start..];
    // Prefer to begin at the start of a line for readability.
    let kept = match kept.find('\n') {
        Some(i) if i + 1 < kept.len() => &kept[i + 1..],
        _ => kept,
    };
    format!(
        "… (output truncated, showing last {} bytes)\n{kept}",
        kept.len()
    )
}

/// Truncate `s` to at most `max` bytes, keeping the **head** with a trailing
/// marker naming the full size. Used for file reads, so the model knows to
/// search rather than re-read. Snaps to a line boundary where possible and
/// never splits a UTF-8 codepoint.
pub(crate) fn truncate_head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let total = s.len();
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];
    // Prefer to end at a line boundary.
    let head = match head.rfind('\n') {
        Some(i) => &head[..=i],
        None => head,
    };
    format!(
        "{head}… (file truncated, showing first {} of {total} bytes; use search to find more)",
        head.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn def(parameters: Value) -> ToolDefinition {
        ToolDefinition {
            name: "t".into(),
            description: "test".into(),
            parameters,
        }
    }

    #[test]
    fn validate_args_passes_a_well_formed_call() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "n": { "type": "integer" } },
            "required": ["path"]
        });
        assert!(validate_args(&def(schema), &json!({ "path": "a.rs", "n": 3 })).is_ok());
    }

    #[test]
    fn validate_args_flags_a_missing_required_string_with_synonyms() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        // A missing string field reuses the synonym-aware message.
        let err = validate_args(&def(schema), &json!({ "file": "a.rs" })).unwrap_err();
        assert!(
            err.contains("missing required string argument 'path'"),
            "{err}"
        );
        assert!(err.contains("use 'path' instead of 'file'"), "{err}");
    }

    #[test]
    fn validate_args_flags_a_missing_required_nonstring() {
        let schema = json!({
            "type": "object",
            "properties": { "steps": { "type": "array" } },
            "required": ["steps"]
        });
        let err = validate_args(&def(schema), &json!({})).unwrap_err();
        assert_eq!(err, "missing required array argument 'steps'");
    }

    #[test]
    fn validate_args_flags_a_present_field_of_the_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        });
        let err = validate_args(&def(schema), &json!({ "command": 42 })).unwrap_err();
        assert_eq!(err, "argument 'command' must be a string, got a number");
    }

    #[test]
    fn validate_args_ignores_undeclared_and_untyped_fields() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" } }
        });
        // Extra keys and fields without a declared type are left alone.
        assert!(validate_args(&def(schema), &json!({ "path": "a", "extra": 1 })).is_ok());
    }

    #[test]
    fn run_process_succeeds_and_captures_output() {
        let out = run_process(
            "sh",
            &["-c".into(), "echo hello".into()],
            &std::env::temp_dir(),
        )
        .unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn run_process_times_out_and_reaps_child() {
        let start = std::time::Instant::now();
        let err = run_process_with_timeout(
            "sh",
            &["-c".into(), "sleep 30".into()],
            &std::env::temp_dir(),
            Duration::from_millis(200),
        )
        .unwrap_err();
        // Returned at (roughly) the deadline, not after the full sleep.
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "did not time out promptly"
        );
        assert!(err.contains("timed out"), "unexpected error: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_process_timeout_does_not_wait_for_detached_stdout_holder() {
        let start = std::time::Instant::now();
        let err = run_process_with_timeout(
            "sh",
            &[
                "-c".into(),
                // `setsid` moves the grandchild out of the shell's process
                // group, so our timeout kill cannot reap it. It still inherits
                // stdout; the timeout path must not wait for output reader
                // threads until this sleep exits.
                "setsid sh -c 'sleep 2' & sleep 30".into(),
            ],
            &std::env::temp_dir(),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "timeout waited for detached stdout holder"
        );
        assert!(err.contains("timed out"), "unexpected error: {err}");
    }

    #[test]
    fn truncate_tail_keeps_end_with_marker() {
        let body: String = (0..1000).map(|i| format!("line{i}\n")).collect();
        let out = truncate_tail(&body, 100);
        assert!(out.starts_with("… (output truncated"));
        assert!(out.contains("line999"));
        assert!(!out.contains("line0\n"));
    }

    #[test]
    fn truncate_head_keeps_start_with_marker() {
        let body: String = (0..1000).map(|i| format!("line{i}\n")).collect();
        let out = truncate_head(&body, 100);
        assert!(out.starts_with("line0\n"));
        assert!(out.contains("file truncated"));
        assert!(out.contains(&format!("of {} bytes", body.len())));
        assert!(!out.contains("line999"));
    }

    #[test]
    fn truncation_is_a_noop_under_the_cap() {
        let s = "small output";
        assert_eq!(truncate_tail(s, 1024), s);
        assert_eq!(truncate_head(s, 1024), s);
    }

    #[test]
    fn truncation_never_splits_a_codepoint() {
        // Multibyte chars straddling the cut point must not panic or corrupt.
        let s = "αβγδε".repeat(100); // each char is 2 bytes
        let tail = truncate_tail(&s, 11);
        let head = truncate_head(&s, 11);
        // Both must be valid UTF-8 (guaranteed by `String`) and non-empty.
        assert!(!tail.is_empty());
        assert!(!head.is_empty());
    }
}
