//! Integration: the permission gates around tool execution — workspace
//! boundary, hidden-file blocking, and hardened-file approval — driven through
//! the agent's `ToolExecutor` against a real temp workspace.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::mpsc;

use suis_agent::tools::default_tools;
use suis_agent::{
    AccessLog, AgentEvent, Mode, PermissionDecision, TaskStore, ToolCall, ToolExecutor, ToolResult,
};
use suis_core::{PermissionStore, ProjectConfig, Workspace};

use common::TempDir;

/// An access log with `paths` pre-recorded as searched and read, so a test can
/// reach the permission gates past the search→read→edit funnel.
fn seeded_access(paths: &[&str]) -> Arc<Mutex<AccessLog>> {
    let access = Arc::new(Mutex::new(AccessLog::default()));
    {
        let mut log = access.lock().unwrap();
        for p in paths {
            log.record_searched(p.to_string());
            log.record_read(p.to_string());
        }
    }
    access
}

/// Execute one tool call, answering every emitted permission prompt with
/// `decision`. Returns the result and the list of prompt descriptions seen.
/// A `None` decision asserts that no prompt is emitted. `access` seeds the
/// file-tool funnel so the call under test reaches the permission gate.
async fn run_with_decision(
    ws: &Workspace,
    project: &ProjectConfig,
    perms: &mut PermissionStore,
    access: &Arc<Mutex<AccessLog>>,
    name: &str,
    args: serde_json::Value,
    decision: Option<PermissionDecision>,
) -> (ToolResult, Vec<String>) {
    let tools: Arc<[_]> = default_tools().into();
    let tasks = Arc::new(Mutex::new(TaskStore::new()));
    let (tx, mut rx) = mpsc::channel(16);

    let responder = tokio::spawn(async move {
        let mut prompts = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::PermissionRequest { action, sender } = ev {
                let d = decision.expect("unexpected permission prompt");
                let _ = sender.send(d);
                prompts.push(action);
            }
        }
        prompts
    });

    let call = ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments: args,
    };
    let result = {
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
    };
    drop(tx);
    let prompts = responder.await.unwrap();
    (result, prompts)
}

#[tokio::test]
async fn workspace_boundary_is_enforced() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    let project = ProjectConfig::default();
    let mut perms = PermissionStore::default();

    let (result, prompts) = run_with_decision(
        &ws,
        &project,
        &mut perms,
        &seeded_access(&[]),
        "read_lines",
        json!({ "path": "../../etc/passwd" }),
        Some(PermissionDecision::deny()),
    )
    .await;

    assert!(result.is_error, "out-of-workspace read should be denied");
    assert_eq!(prompts.len(), 1, "exactly one boundary prompt expected");
    assert!(prompts[0].contains("outside workspace"));
}

#[tokio::test]
async fn hidden_file_is_blocked_without_prompt() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    dir.write(".env", "SECRET=top-secret");
    let mut project = ProjectConfig::default();
    project.hidden.push(".env".into());
    let mut perms = PermissionStore::default();

    // Hidden files are blocked inside the read tool, not via a prompt: a `None`
    // decision asserts no prompt is emitted. The funnel is pre-seeded so the
    // hidden guard (not the search-first gate) is what blocks the read.
    let (result, prompts) = run_with_decision(
        &ws,
        &project,
        &mut perms,
        &seeded_access(&[".env"]),
        "read_lines",
        json!({ "path": ".env" }),
        None,
    )
    .await;

    assert!(result.is_error, "hidden file read should fail");
    assert!(prompts.is_empty(), "hidden block should not prompt");
    assert!(
        !result.content.contains("top-secret"),
        "hidden file contents must not leak"
    );
}

#[tokio::test]
async fn hardened_file_requires_approval() {
    let dir = TempDir::new();
    let ws = Workspace::detect(dir.path()).unwrap();
    dir.write("Cargo.lock", "version = 1\n");
    let mut project = ProjectConfig::default();
    project.hardened.push("Cargo.lock".into());
    let mut perms = PermissionStore::default();
    // Existing file: pre-seed the funnel so the hardened-approval gate (not the
    // read-before-edit gate) is what's under test.
    let access = seeded_access(&["Cargo.lock"]);

    // Denied: the file is left untouched.
    let (denied, prompts) = run_with_decision(
        &ws,
        &project,
        &mut perms,
        &access,
        "edit",
        json!({ "path": "Cargo.lock", "content": "version = 2\n" }),
        Some(PermissionDecision::deny()),
    )
    .await;
    assert!(denied.is_error, "hardened edit should be deniable");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("hardened"));
    assert_eq!(dir.read("Cargo.lock"), "version = 1\n", "file unchanged");

    // Approved (once): the edit goes through.
    let (approved, prompts) = run_with_decision(
        &ws,
        &project,
        &mut perms,
        &access,
        "edit",
        json!({ "path": "Cargo.lock", "content": "version = 2\n" }),
        Some(PermissionDecision::once()),
    )
    .await;
    assert!(!approved.is_error, "approved hardened edit should succeed");
    assert_eq!(prompts.len(), 1);
    assert_eq!(dir.read("Cargo.lock"), "version = 2\n", "file updated");
}
