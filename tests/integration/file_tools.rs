//! Integration: file tools through the agent executor, plus the core trash
//! round-trip. Exercises `suis-agent` (tool dispatch + permission gating) on top
//! of `suis-core` (boundary checks, filesystem ops) against a real temp
//! workspace.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::mpsc;

use suis_agent::tools::default_tools;
use suis_agent::{AccessLog, Mode, TaskStore, ToolCall, ToolExecutor, ToolResult};
use suis_core::filesystem::{ops, trash};
use suis_core::{PermissionStore, ProjectConfig, Workspace};

use common::TempDir;

/// Run one tool call through a fresh executor against `ws`, sharing `access`
/// across calls so the search→read→edit funnel carries between them. The event
/// receiver is dropped immediately, so the in-workspace calls run unprompted
/// while any permission request (e.g. an out-of-workspace path) resolves to a
/// denial — the executor's documented "no UI listening" behavior.
async fn run_tool(
    ws: &Workspace,
    project: &ProjectConfig,
    perms: &mut PermissionStore,
    access: &Arc<Mutex<AccessLog>>,
    name: &str,
    args: serde_json::Value,
) -> ToolResult {
    let tools: Arc<[_]> = default_tools().into();
    let tasks = Arc::new(Mutex::new(TaskStore::new()));
    let (tx, rx) = mpsc::channel(16);
    drop(rx);
    let call = ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments: args,
    };
    let mut executor = ToolExecutor::new(
        ws,
        project,
        perms,
        &tasks,
        access,
        tools,
        Mode::Agent,
        None,
        &tx,
    );
    executor.execute(&call).await
}

#[tokio::test]
async fn create_search_read_and_edit_a_file() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    let project = ProjectConfig::default();
    let mut perms = PermissionStore::default();
    let access = Arc::new(Mutex::new(AccessLog::default()));

    // Create a file via the edit tool (write mode). A brand-new file needs no
    // prior read, so the funnel lets the create through.
    let created = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "edit",
        json!({ "path": "notes.txt", "content": "first line\n" }),
    )
    .await;
    assert!(!created.is_error, "edit/create failed: {}", created.content);
    assert_eq!(dir.read("notes.txt"), "first line\n");

    // Search the file (the funnel requires this before a read).
    let searched = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "search",
        json!({ "pattern": "first", "path": "notes.txt" }),
    )
    .await;
    assert!(!searched.is_error, "search failed: {}", searched.content);

    // Read it back through read_lines.
    let read = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "read_lines",
        json!({ "path": "notes.txt" }),
    )
    .await;
    assert!(!read.is_error, "read failed: {}", read.content);
    assert!(read.content.contains("first line"), "{}", read.content);

    // Modify it via the edit tool (replace mode) — allowed now the file is read.
    let edited = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "edit",
        json!({ "path": "notes.txt", "old_string": "first", "new_string": "second" }),
    )
    .await;
    assert!(!edited.is_error, "edit/replace failed: {}", edited.content);
    assert_eq!(dir.read("notes.txt"), "second line\n");

    // The on-disk change is reflected on the next read.
    let reread = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "read_lines",
        json!({ "path": "notes.txt" }),
    )
    .await;
    assert!(reread.content.contains("second line"), "{}", reread.content);
}

#[tokio::test]
async fn edit_outside_workspace_is_denied() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    let project = ProjectConfig::default();
    let mut perms = PermissionStore::default();
    let access = Arc::new(Mutex::new(AccessLog::default()));

    // No responder is listening, so the boundary prompt resolves to a denial.
    let result = run_tool(
        &ws,
        &project,
        &mut perms,
        &access,
        "edit",
        json!({ "path": "../escape.txt", "content": "nope" }),
    )
    .await;
    assert!(result.is_error);
    assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
}

#[test]
fn delete_moves_to_trash_and_restores() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    dir.write("data.txt", "keep me");

    let entry = ops::delete(&ws, "data.txt").expect("delete to trash");
    assert!(!dir.exists("data.txt"), "original should be gone");
    assert!(entry.location.exists(), "trash copy should exist");
    assert!(trash::trash_root(&ws).exists(), "trash root should exist");

    trash::restore(&entry).expect("restore");
    assert!(dir.exists("data.txt"), "original should be restored");
    assert_eq!(dir.read("data.txt"), "keep me");
}
