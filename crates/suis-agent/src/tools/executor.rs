//! Permission-gated tool execution.
//!
//! [`ToolExecutor::execute`] first refuses any tool the session's [`Mode`]
//! disallows (Plan/Chat mode cannot edit or execute — see
//! [`Mode::allows_tool`]), then routes the [`ToolCall`] through the appropriate
//! permission checks before dispatching to the matching [`Tool`]:
//!
//! - `read_lines` / `search` / `edit`: workspace-boundary check. `read_lines`
//!   additionally requires the file to have been `search`ed first, and `edit`
//!   requires an existing file to have been `read_lines`'d first (the
//!   file-tool funnel that keeps a weak model from slurping whole files);
//!   `edit` also gates writes to hardened files.
//! - `bash`: evaluated against the [`PermissionStore`]; unknown or dangerous
//!   commands prompt the user.
//! - `git`: gated by the project's [`GitAccess`] level.
//! - `task`: always permitted (no external effect outside the plan store).
//! - `plan`: reachable only in Plan mode; the draft is routed to the user as an
//!   [`AgentEvent::PlanProposal`] and persisted to `.suis/plans.json` only on
//!   approval — the executor resolves the call itself, the tool body never runs.
//! - any other registered tool: prompts per invocation (fail closed), so a
//!   tool added without an explicit gate never runs silently.
//!
//! When a check needs user input, the executor emits an
//! [`AgentEvent::PermissionRequest`] and awaits a [`PermissionDecision`].
//! Granted command permissions (`Session`/`Project`/`Always`, never dangerous)
//! are recorded so the same command is not re-prompted. `Project` grants are
//! persisted to `.suis/permissions.json` and `Always` grants to the global
//! `~/.config/suis/permissions.json`; `Session` grants live only in memory.
//! Command denials carry a duration too: a session deny is remembered in
//! memory (the command is auto-denied without re-prompting), a project deny
//! persists as a stored `Deny` rule, and a once deny records nothing.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use suis_core::filesystem::guard;
use suis_core::permissions::is_dangerous;
use suis_core::{
    CommandPermission, GitAccess, PermissionResult, PermissionScope, PermissionStore, PlanStore,
    ProjectConfig, Workspace,
};

use super::access::{rel_key, AccessLog};
use super::git::is_read_only;
use super::{opt_str, plan, Tool, ToolContext, ToolResult};
use crate::runtime::events::{AgentEvent, PermissionDecision, PlanDecision};
use crate::runtime::mode::Mode;
use crate::runtime::session::ImplementTarget;
use crate::tasks::TaskStore;

/// Executes tool calls, enforcing permissions and prompting when needed.
pub struct ToolExecutor<'a> {
    workspace: &'a Workspace,
    project: &'a ProjectConfig,
    permissions: &'a mut PermissionStore,
    tasks: &'a Arc<Mutex<TaskStore>>,
    /// The session's file-access log, consulted to enforce search-before-read
    /// and read-before-edit. Shared with the tool bodies that record into it.
    access: &'a Arc<Mutex<AccessLog>>,
    /// The tool set, shared (not borrowed) so a tool body can be moved onto a
    /// blocking thread for execution (see [`ToolExecutor::execute`]).
    tools: Arc<[Box<dyn Tool>]>,
    /// The session's runtime mode; calls to tools the mode disallows are
    /// refused before any permission gate runs.
    mode: Mode,
    /// The active implementation target, if this is an `/implement` session;
    /// passed to tools so `task` operates on the plan step's tasks.
    implement: Option<ImplementTarget>,
    events: &'a mpsc::Sender<AgentEvent>,
}

/// Outcome of a permission gate.
enum Gate {
    /// Proceed with execution.
    Proceed,
    /// Block, with a reason returned to the model.
    Deny(String),
    /// The gate handled the call itself; this is the success content (used by
    /// `plan`, whose approve-then-persist flow lives entirely in the gate).
    Resolved(String),
}

/// Outcome of the full pre-dispatch check ([`ToolExecutor::gate_call`]): either
/// run the tool body, or a final [`ToolResult`] the gate already produced
/// (denied, mode-refused, schema-invalid, unknown, or executor-resolved like
/// `plan`). Separating gating from the body lets a batch gate its calls in
/// order (needs `&mut` permission state) and then run the read-only bodies
/// concurrently (the bodies need only shared state).
pub(crate) enum Gated {
    /// Run the tool body.
    Proceed,
    /// The gate produced the final result; do not run the body.
    Resolved(ToolResult),
}

impl<'a> ToolExecutor<'a> {
    /// Construct an executor borrowing the session state it gates against.
    #[allow(clippy::too_many_arguments)] // mirrors the session state it borrows
    pub fn new(
        workspace: &'a Workspace,
        project: &'a ProjectConfig,
        permissions: &'a mut PermissionStore,
        tasks: &'a Arc<Mutex<TaskStore>>,
        access: &'a Arc<Mutex<AccessLog>>,
        tools: Arc<[Box<dyn Tool>]>,
        mode: Mode,
        implement: Option<ImplementTarget>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Self {
        ToolExecutor {
            workspace,
            project,
            permissions,
            tasks,
            access,
            tools,
            mode,
            implement,
            events,
        }
    }

    /// Run a single tool call through its permission gate and implementation.
    pub async fn execute(&mut self, call: &super::ToolCall) -> ToolResult {
        match self.gate_call(call).await {
            Gated::Resolved(result) => result,
            Gated::Proceed => {
                run_tool_body(
                    self.workspace,
                    self.project,
                    self.tasks,
                    self.access,
                    &self.tools,
                    self.implement.as_ref(),
                    call,
                )
                .await
            }
        }
    }

    /// Run every pre-dispatch check for `call` — existence, mode, argument
    /// schema, then the permission gate — returning [`Gated::Proceed`] to run
    /// the body or [`Gated::Resolved`] with the final result the checks already
    /// produced. Splitting this out of [`execute`] lets a batch gate its calls
    /// in order (this needs `&mut self`) and then run the read-only bodies
    /// concurrently with shared state (see [`run_tool_body`]).
    pub(crate) async fn gate_call(&mut self, call: &super::ToolCall) -> Gated {
        let Some(tool) = self.tools.iter().find(|t| t.name() == call.name) else {
            let names: Vec<&str> = self.tools.iter().map(|t| t.name()).collect();
            return Gated::Resolved(ToolResult::error(
                &call.id,
                unknown_tool_message(&call.name, &names),
            ));
        };

        // Mode enforcement comes before any permission gate: in Plan/Chat mode
        // a write/execute tool is refused outright (no prompt) even if the
        // model hallucinates a call the assembler never advertised. The error
        // is a normal tool result the model can react to.
        if !self.mode.allows_tool(&call.name) {
            return Gated::Resolved(ToolResult::error(
                &call.id,
                format!(
                    "tool '{}' is not available in {} mode",
                    call.name, self.mode
                ),
            ));
        }

        // Schema validation before the permission gate: a malformed call is a
        // corrective result the model can fix next turn, not a prompt or a hard
        // failure.
        if let Err(message) = super::validate_args(&tool.definition(), &call.arguments) {
            return Gated::Resolved(ToolResult::error(&call.id, message));
        }

        match self.gate(&call.name, &call.arguments).await {
            Gate::Deny(reason) => Gated::Resolved(ToolResult::error(&call.id, reason)),
            Gate::Resolved(content) => Gated::Resolved(ToolResult::ok(&call.id, content)),
            Gate::Proceed => Gated::Proceed,
        }
    }

    /// Decide whether `name`/`args` may proceed, prompting the user if needed.
    async fn gate(&mut self, name: &str, args: &Value) -> Gate {
        match name {
            "search" | "tree" => self.gate_path(args, "access", false).await,
            "read_lines" => match self.gate_path(args, "access", false).await {
                Gate::Proceed => self.gate_read_lines(args),
                other => other,
            },
            "edit" => match self.gate_path(args, "modify", true).await {
                Gate::Proceed => self.gate_edit_funnel(args),
                other => other,
            },
            "bash" => self.gate_bash(args).await,
            "git" => self.gate_git(args),
            // The task tool only mutates the task list (in-session, or the
            // active plan step's states during `/implement`) — so it is
            // deliberately ungated.
            "task" => Gate::Proceed,
            // Only reachable in Plan mode (the mode check above); the whole
            // draft → approval → persist flow lives here.
            "plan" => self.gate_plan(args).await,
            // Fail closed: a registered tool without an explicit gate prompts
            // per invocation rather than running silently.
            _ => {
                let decision = self.request(format!("run tool: {name}")).await;
                if decision.is_denied() {
                    Gate::Deny("Permission denied.".to_string())
                } else {
                    Gate::Proceed
                }
            }
        }
    }

    /// Gate a filesystem path: deny outside-workspace access unless approved,
    /// and (when `check_hardened`) prompt before modifying a hardened file.
    async fn gate_path(&mut self, args: &Value, verb: &str, check_hardened: bool) -> Gate {
        // `search` may omit `path` (defaults to the whole workspace).
        let Some(path) = opt_str(args, "path") else {
            return Gate::Proceed;
        };

        if !self.workspace.contains(&path) {
            let decision = self
                .request(format!("{verb} path outside workspace: {path}"))
                .await;
            if decision.is_denied() {
                return Gate::Deny("Permission denied.".to_string());
            }
            return Gate::Proceed;
        }

        if check_hardened {
            if let Ok(resolved) = self.workspace.check_boundary(&path) {
                let rel = resolved
                    .strip_prefix(&self.workspace.root)
                    .unwrap_or(&resolved)
                    .to_path_buf();
                // Hidden files are never writable: editing one would also leak its
                // prior contents through the diff. Deny outright (no prompt) —
                // this mirrors the read/search/list hidden guard for writes.
                if guard::is_hidden(self.project, &rel) {
                    return Gate::Deny(format!("cannot edit a hidden file: {path}"));
                }
                if guard::is_hardened(self.project, &rel) {
                    let decision = self.request(format!("modify hardened file: {path}")).await;
                    if decision.is_denied() {
                        return Gate::Deny("Permission denied.".to_string());
                    }
                }
            }
        }
        Gate::Proceed
    }

    /// Funnel gate for `read_lines`: the file must have been `search`ed this
    /// session first. Keeps a weak model from blindly reading files it never
    /// located — it must search to find the lines worth reading. Paths outside
    /// the workspace (already approved by [`gate_path`]) are exempt: they can't
    /// appear in a workspace search.
    fn gate_read_lines(&self, args: &Value) -> Gate {
        let Some(path) = opt_str(args, "path") else {
            return Gate::Proceed;
        };
        if !self.workspace.contains(&path) {
            return Gate::Proceed;
        }
        let Some(key) = rel_key(self.workspace, &path) else {
            return Gate::Proceed;
        };
        let searched = self
            .access
            .lock()
            .map(|log| log.was_searched(&key))
            .unwrap_or(true);
        if searched {
            Gate::Proceed
        } else {
            Gate::Deny(format!(
                "search '{path}' before reading it: run the search tool (scoped to the file, \
                 or one that surfaces it) so you read only the lines you need."
            ))
        }
    }

    /// Funnel gate for `edit`: an *existing* file must have been `read_lines`'d
    /// this session first, so the model edits from the real current contents
    /// rather than a guess. Creating a new file is exempt (there's nothing to
    /// read), as are paths outside the workspace (already approved).
    fn gate_edit_funnel(&self, args: &Value) -> Gate {
        let Some(path) = opt_str(args, "path") else {
            return Gate::Proceed;
        };
        if !self.workspace.contains(&path) {
            return Gate::Proceed;
        }
        // A brand-new file has nothing to read first.
        let exists = self
            .workspace
            .check_boundary(&path)
            .map(|resolved| resolved.exists())
            .unwrap_or(false);
        if !exists {
            return Gate::Proceed;
        }
        let Some(key) = rel_key(self.workspace, &path) else {
            return Gate::Proceed;
        };
        let read = self
            .access
            .lock()
            .map(|log| log.was_read(&key))
            .unwrap_or(true);
        if read {
            Gate::Proceed
        } else {
            Gate::Deny(format!(
                "read_lines '{path}' before editing it: read the lines you intend to change \
                 so the edit matches the file's current contents."
            ))
        }
    }

    /// Gate a shell command against stored permissions, prompting when required.
    async fn gate_bash(&mut self, args: &Value) -> Gate {
        let Some(command) = opt_str(args, "command") else {
            // Missing argument: let the tool surface the error.
            return Gate::Proceed;
        };

        match self.permissions.check_command(&command) {
            PermissionResult::Allow => {
                // A stored grant authorizes the command, but `bash` is gated by
                // approval, not by the filesystem guards that bind the model's
                // file tools. If the command names a hidden path, re-prompt even
                // when granted — a stored grant never silences this, mirroring
                // the dangerous-command rule. Best-effort and token-based (see
                // `hidden_token`).
                if let Some(token) = self.hidden_token(&command) {
                    let decision = self
                        .request(format!("command references hidden file: {token}"))
                        .await;
                    if decision.is_denied() {
                        return Gate::Deny("Permission denied.".to_string());
                    }
                }
                Gate::Proceed
            }
            PermissionResult::Deny => Gate::Deny("Command denied by policy.".to_string()),
            PermissionResult::RequireApproval => {
                let decision = self.request(format!("run command: {command}")).await;
                self.record_command_decision(&command, decision);
                if decision.is_denied() {
                    return Gate::Deny("Permission denied.".to_string());
                }
                Gate::Proceed
            }
        }
    }

    /// The first whitespace-separated token of `command` that names a hidden
    /// path (per the project's hidden patterns), if any. Token-based and
    /// bypassable by construction (e.g. `cat $(echo .env)`); its job is catching
    /// the model's honest attempts, not sandboxing.
    fn hidden_token(&self, command: &str) -> Option<String> {
        command
            .split_whitespace()
            .find(|tok| guard::is_hidden(self.project, std::path::Path::new(tok)))
            .map(str::to_string)
    }

    /// Gate a git invocation by the project's access level.
    fn gate_git(&self, args: &Value) -> Gate {
        let git_args = opt_str(args, "args").unwrap_or_default();
        match self.project.git_access {
            GitAccess::Disabled => {
                Gate::Deny("Git access is disabled for this project.".to_string())
            }
            GitAccess::ReadOnly => {
                if is_read_only(&git_args) {
                    Gate::Proceed
                } else {
                    Gate::Deny(
                        "Git access is read-only; this command would modify the repository."
                            .to_string(),
                    )
                }
            }
            GitAccess::ReadWrite => Gate::Proceed,
        }
    }

    /// Gate (and fully resolve) a `plan` call: validate the draft, route it to
    /// the user as an [`AgentEvent::PlanProposal`], and persist to
    /// `.suis/plans.json` only on approval. Nothing is written before the user
    /// approves; a dropped channel (no UI listening) is a rejection.
    async fn gate_plan(&self, args: &Value) -> Gate {
        let draft = match plan::parse_draft(args) {
            Ok(draft) => draft,
            Err(message) => return Gate::Deny(message),
        };

        let mut store = match PlanStore::load(self.workspace) {
            Ok(store) => store,
            Err(e) => return Gate::Deny(format!("could not load plans: {e}")),
        };
        // A revision of a plan that doesn't exist is the model's mistake, not
        // a question for the user — fail before prompting.
        if let Some(id) = &draft.revises {
            if store.get(id).is_none() {
                return Gate::Deny(format!("no plan with id '{id}'"));
            }
        }

        let (tx, rx) = oneshot::channel();
        let proposal = AgentEvent::PlanProposal {
            draft: draft.clone(),
            sender: tx,
        };
        if self.events.send(proposal).await.is_err() {
            return Gate::Deny("Plan rejected by user.".to_string());
        }
        let decision = rx.await.unwrap_or(PlanDecision::Reject);
        if decision == PlanDecision::Reject {
            return Gate::Deny("Plan rejected by user.".to_string());
        }

        let id = match &draft.revises {
            Some(id) => {
                let existing = store.get_mut(id).expect("existence checked above");
                existing.title = draft.title;
                existing.description = draft.description;
                existing.steps = draft.steps;
                id.clone()
            }
            None => store.insert(draft.title, draft.description, draft.steps),
        };
        match store.save(self.workspace) {
            Ok(()) => Gate::Resolved(format!("Plan saved as '{id}'.")),
            Err(e) => Gate::Deny(format!("failed to save plan: {e}")),
        }
    }

    /// Record a command decision so it applies beyond this invocation.
    ///
    /// Grants are recorded at their scope (never for dangerous commands);
    /// denials are recorded at their duration — session denies stay in memory,
    /// project denies persist as a stored `Deny` rule. `Once` decisions in
    /// either direction record nothing.
    fn record_command_decision(&mut self, command: &str, decision: PermissionDecision) {
        match decision {
            PermissionDecision::Grant { scope, wildcard } => {
                if is_dangerous(command) {
                    return;
                }
                let persist = match scope {
                    PermissionScope::Session => false,
                    PermissionScope::Project | PermissionScope::Always => true,
                    PermissionScope::Once | PermissionScope::Deny => return,
                };

                let pattern = if wildcard {
                    let head = command.split_whitespace().next().unwrap_or(command);
                    format!("{head} *")
                } else {
                    command.trim().to_string()
                };

                self.permissions
                    .commands
                    .push(CommandPermission { pattern, scope });
                if persist {
                    // Best-effort persistence; a failure must not abort the turn.
                    let _ = self.permissions.save(self.workspace);
                }
            }
            PermissionDecision::DenyOnce => {}
            PermissionDecision::DenySession => {
                self.permissions
                    .session_denies
                    .push(command.trim().to_string());
            }
            PermissionDecision::DenyProject => {
                self.permissions.commands.push(CommandPermission {
                    pattern: command.trim().to_string(),
                    scope: PermissionScope::Deny,
                });
                // Best-effort persistence; a failure must not abort the turn.
                let _ = self.permissions.save(self.workspace);
            }
        }
    }

    /// Emit a [`AgentEvent::PermissionRequest`] and await the decision. A
    /// dropped channel (no UI listening) is treated as a denial.
    async fn request(&self, action: String) -> PermissionDecision {
        let (tx, rx) = oneshot::channel();
        if self
            .events
            .send(AgentEvent::PermissionRequest { action, sender: tx })
            .await
            .is_err()
        {
            return PermissionDecision::deny();
        }
        rx.await.unwrap_or_else(|_| PermissionDecision::deny())
    }
}

/// Run an already-gated tool body on the blocking pool, producing its
/// [`ToolResult`]. Takes only shared state (no `&mut`), so a batch of read-only
/// calls can run these concurrently after gating. Tool bodies are synchronous
/// and may block (a long command, a big file); running them on
/// `spawn_blocking` keeps a slow tool from parking an async worker or freezing
/// the TUI. The pieces the context needs are cheap to clone / share.
pub(crate) async fn run_tool_body(
    workspace: &Workspace,
    project: &ProjectConfig,
    tasks: &Arc<Mutex<TaskStore>>,
    access: &Arc<Mutex<AccessLog>>,
    tools: &Arc<[Box<dyn Tool>]>,
    implement: Option<&ImplementTarget>,
    call: &super::ToolCall,
) -> ToolResult {
    let tools = Arc::clone(tools);
    let workspace = workspace.clone();
    let project = project.clone();
    let tasks = Arc::clone(tasks);
    let access = Arc::clone(access);
    let implement = implement.cloned();
    let name = call.name.clone();
    let args = call.arguments.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        let ctx = ToolContext {
            workspace: &workspace,
            project: &project,
            tasks: &tasks,
            implement: implement.as_ref(),
            access: &access,
        };
        let tool = tools
            .iter()
            .find(|t| t.name() == name)
            .expect("tool presence checked by gate_call");
        tool.execute(&args, &ctx)
    })
    .await;

    match outcome {
        Ok(Ok(content)) => ToolResult::ok(&call.id, content),
        Ok(Err(message)) => ToolResult::error(&call.id, message),
        Err(join_err) => ToolResult::error(&call.id, format!("tool execution failed: {join_err}")),
    }
}

/// A corrective error for a call to a tool that doesn't exist: names a likely
/// intended tool (synonym table first, then nearest by edit distance) and lists
/// the real tools, so a weaker model can retry correctly.
fn unknown_tool_message(name: &str, available: &[&str]) -> String {
    let suggestion = tool_synonym(name)
        .filter(|s| available.contains(s))
        .or_else(|| nearest_name(name, available));
    let mut msg = format!("unknown tool '{name}'.");
    if let Some(s) = suggestion {
        msg.push_str(&format!(" Did you mean '{s}'?"));
    }
    msg.push_str(&format!(" Available tools: {}.", available.join(", ")));
    msg
}

/// Map common model-invented tool names onto the real tool.
fn tool_synonym(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let mapped = match n.as_str() {
        "read_file" | "open_file" | "openfile" | "cat" | "view" | "open" | "get_file" | "read" => {
            "read_lines"
        }
        "write_file" | "writefile" | "create_file" | "write" | "apply_patch" | "str_replace"
        | "edit_file" => "edit",
        "shell" | "run" | "exec" | "execute" | "run_command" | "terminal" | "command" => "bash",
        "grep" | "find" | "ripgrep" | "rg" | "search_files" | "find_in_files" => "search",
        "ls" | "list" | "list_files" | "list_dir" | "dir" | "layout" | "map" | "lsr"
        | "list_directory" => "tree",
        "todo" | "tasks" | "task_list" | "todos" => "task",
        _ => return None,
    };
    Some(mapped)
}

/// The available name within edit distance 2 of `name`, if any (closest wins).
fn nearest_name<'a>(name: &str, available: &[&'a str]) -> Option<&'a str> {
    available
        .iter()
        .map(|cand| (*cand, levenshtein(name, cand)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(cand, _)| cand)
}

/// Plain iterative Levenshtein distance over chars.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;
    use crate::tools::{default_tools, ToolCall};
    use serde_json::json;

    /// Drive the executor for one call while a background task answers any
    /// permission prompt with `decision`.
    async fn run_with_decision(
        fx: &Fixture,
        permissions: &mut PermissionStore,
        call: ToolCall,
        decision: Option<PermissionDecision>,
    ) -> ToolResult {
        run_with_tools(fx, permissions, default_tools(), call, decision).await
    }

    /// Like [`run_with_decision`] but with an explicit tool set.
    async fn run_with_tools(
        fx: &Fixture,
        permissions: &mut PermissionStore,
        tools: Vec<Box<dyn Tool>>,
        call: ToolCall,
        decision: Option<PermissionDecision>,
    ) -> ToolResult {
        run_in_mode(fx, permissions, tools, Mode::Agent, call, decision).await
    }

    /// Like [`run_with_tools`] but in an explicit [`Mode`].
    async fn run_in_mode(
        fx: &Fixture,
        permissions: &mut PermissionStore,
        tools: Vec<Box<dyn Tool>>,
        mode: Mode,
        call: ToolCall,
        decision: Option<PermissionDecision>,
    ) -> ToolResult {
        let access = Arc::new(Mutex::new(AccessLog::default()));
        run_in_mode_access(fx, permissions, &access, tools, mode, call, decision).await
    }

    /// Like [`run_in_mode`] but with a caller-supplied access log, so a test can
    /// pre-seed the search/read funnel before the call.
    #[allow(clippy::too_many_arguments)]
    async fn run_in_mode_access(
        fx: &Fixture,
        permissions: &mut PermissionStore,
        access: &Arc<Mutex<AccessLog>>,
        tools: Vec<Box<dyn Tool>>,
        mode: Mode,
        call: ToolCall,
        decision: Option<PermissionDecision>,
    ) -> ToolResult {
        let (tx, mut rx) = mpsc::channel(16);

        let responder = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(ev) = rx.recv().await {
                if let AgentEvent::PermissionRequest { action, sender } = ev {
                    let d = decision.expect("unexpected permission prompt");
                    let _ = sender.send(d);
                    events.push(action);
                }
            }
            events
        });

        let tools: Arc<[Box<dyn Tool>]> = tools.into();
        let result = {
            let mut ex = ToolExecutor::new(
                &fx.workspace,
                &fx.project,
                permissions,
                &fx.tasks,
                access,
                tools,
                mode,
                None,
                &tx,
            );
            ex.execute(&call).await
        };
        drop(tx);
        let _ = responder.await;
        result
    }

    /// An access log with `paths` pre-recorded as both searched and read, for
    /// tests that exercise behavior past the funnel.
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

    /// Drive one `plan` call in Plan mode while a background task answers the
    /// proposal with `decision` (`None` panics if a proposal is emitted). Any
    /// permission prompt also panics: the plan flow must never ask for one.
    async fn run_plan_call(
        fx: &Fixture,
        args: Value,
        decision: Option<PlanDecision>,
    ) -> ToolResult {
        let (tx, mut rx) = mpsc::channel(16);

        let responder = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::PlanProposal { sender, .. } => {
                        let d = decision.expect("unexpected plan proposal");
                        let _ = sender.send(d);
                    }
                    AgentEvent::PermissionRequest { .. } => {
                        panic!("plan flow must not emit a permission prompt")
                    }
                    _ => {}
                }
            }
        });

        let mut perms = PermissionStore::default();
        let access = Arc::new(Mutex::new(AccessLog::default()));
        let tools: Arc<[Box<dyn Tool>]> = default_tools().into();
        let result = {
            let mut ex = ToolExecutor::new(
                &fx.workspace,
                &fx.project,
                &mut perms,
                &fx.tasks,
                &access,
                tools,
                Mode::Plan,
                None,
                &tx,
            );
            ex.execute(&call("plan", args)).await
        };
        drop(tx);
        let _ = responder.await;
        result
    }

    fn draft_args() -> Value {
        json!({
            "action": "propose",
            "title": "Auth System",
            "description": "Add JWT auth",
            "steps": [
                { "title": "Tokens", "work_tasks": ["login route"], "verify_tasks": ["auth tests"] }
            ]
        })
    }

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn read_lines_after_search_executes() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello");
        let mut perms = PermissionStore::default();
        let access = seeded_access(&["a.txt"]);
        let result = run_in_mode_access(
            &fx,
            &mut perms,
            &access,
            default_tools(),
            Mode::Agent,
            call("read_lines", json!({ "path": "a.txt" })),
            None,
        )
        .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("hello"), "{}", result.content);
    }

    #[tokio::test]
    async fn read_lines_without_prior_search_is_denied() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello");
        let mut perms = PermissionStore::default();
        // No search recorded: the funnel blocks the read with a corrective hint.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("read_lines", json!({ "path": "a.txt" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("search"), "{}", result.content);
    }

    #[tokio::test]
    async fn read_outside_workspace_prompts_then_denies() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("read_lines", json!({ "path": "../../etc/passwd" })),
            Some(PermissionDecision::deny()),
        )
        .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn edit_existing_file_without_prior_read_is_denied() {
        let fx = Fixture::new();
        fx.write("a.txt", "old\n");
        let mut perms = PermissionStore::default();
        // No read recorded: editing an existing file is blocked.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("edit", json!({ "path": "a.txt", "content": "new\n" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("read_lines"), "{}", result.content);
        // The file is untouched.
        assert_eq!(fx.read("a.txt"), "old\n");
    }

    #[tokio::test]
    async fn edit_existing_file_after_read_executes() {
        let fx = Fixture::new();
        fx.write("a.txt", "old\n");
        let mut perms = PermissionStore::default();
        let access = seeded_access(&["a.txt"]);
        let result = run_in_mode_access(
            &fx,
            &mut perms,
            &access,
            default_tools(),
            Mode::Agent,
            call("edit", json!({ "path": "a.txt", "content": "new\n" })),
            None,
        )
        .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fx.read("a.txt"), "new\n");
    }

    #[tokio::test]
    async fn edit_creating_a_new_file_needs_no_prior_read() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        // A brand-new file has nothing to read first: the funnel lets it through.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("edit", json!({ "path": "new.txt", "content": "hi\n" })),
            None,
        )
        .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fx.read("new.txt"), "hi\n");
    }

    #[tokio::test]
    async fn bash_with_project_grant_runs_without_prompt() {
        let fx = Fixture::new();
        let mut perms = PermissionStore {
            commands: vec![CommandPermission {
                pattern: "echo *".into(),
                scope: PermissionScope::Project,
            }],
            tools: vec![],
            session_denies: vec![],
        };
        // `None` decision => the responder panics if a prompt is emitted.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "echo hi" })),
            None,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hi"));
    }

    #[tokio::test]
    async fn dangerous_command_prompts_even_if_stored() {
        let fx = Fixture::new();
        // Stored as Session, but `rm` is dangerous → still prompts.
        let mut perms = PermissionStore {
            commands: vec![CommandPermission {
                pattern: "rm *".into(),
                scope: PermissionScope::Session,
            }],
            tools: vec![],
            session_denies: vec![],
        };
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "rm -rf build" })),
            Some(PermissionDecision::deny()),
        )
        .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn session_grant_is_recorded_in_memory() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "echo hi" })),
            Some(PermissionDecision::grant(PermissionScope::Session, true)),
        )
        .await;
        assert!(!result.is_error);
        // A wildcard session grant should now be present.
        assert!(perms.commands.iter().any(|c| c.pattern == "echo *"));
    }

    #[tokio::test]
    async fn edit_hidden_file_is_denied_without_prompt() {
        let mut fx = Fixture::new();
        fx.write(".env", "SECRET=1\n");
        fx.project.hidden.push(".env".into());
        let mut perms = PermissionStore::default();
        // A `None` decision panics the responder if any prompt is emitted: the
        // hidden-edit denial must be outright, never an approval prompt.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("edit", json!({ "path": ".env", "content": "SECRET=2\n" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("hidden"), "{}", result.content);
        // The secret never leaks and the file is untouched.
        assert!(!result.content.contains("SECRET"));
        assert_eq!(fx.read(".env"), "SECRET=1\n");
    }

    #[tokio::test]
    async fn git_disabled_is_denied() {
        let mut fx = Fixture::new();
        fx.project.git_access = GitAccess::Disabled;
        let mut perms = PermissionStore::default();
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("git", json!({ "args": "status" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("disabled"));
    }

    #[tokio::test]
    async fn git_readonly_blocks_writes() {
        let mut fx = Fixture::new();
        fx.project.git_access = GitAccess::ReadOnly;
        let mut perms = PermissionStore::default();
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("git", json!({ "args": "commit -m x" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("read-only"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let result = run_with_decision(&fx, &mut perms, call("frobnicate", json!({})), None).await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn unknown_tool_suggests_a_real_tool_and_lists_them() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        // A common model-invented name maps to `read_lines` via the synonym table.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("read_file", json!({ "path": "a.txt" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Did you mean 'read_lines'?"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("Available tools:"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn malformed_arguments_yield_a_corrective_result_not_a_failure() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        // `edit` requires `path` (a string); sending a number is a corrective
        // result the model can fix, surfaced before any permission gate.
        let result =
            run_with_decision(&fx, &mut perms, call("edit", json!({ "path": 7 })), None).await;
        assert!(result.is_error);
        assert!(result.content.contains("'path'"), "{}", result.content);
        // No write happened.
        assert!(!fx.workspace.root.join("7").exists());
    }

    #[test]
    fn nearest_name_catches_a_typo() {
        let available = ["read", "search", "edit", "bash"];
        assert_eq!(nearest_name("raed", &available), Some("read"));
        assert_eq!(nearest_name("xyzzy", &available), None);
    }

    /// A registered tool the gate has no explicit arm for.
    struct NoopTool;

    impl Tool for NoopTool {
        fn name(&self) -> &'static str {
            "custom"
        }

        fn definition(&self) -> crate::tools::ToolDefinition {
            crate::tools::ToolDefinition {
                name: self.name().to_string(),
                description: "test tool".to_string(),
                parameters: json!({ "type": "object", "properties": {} }),
            }
        }

        fn execute(&self, _args: &Value, _ctx: &ToolContext<'_>) -> crate::tools::ToolOutcome {
            Ok("ran".to_string())
        }
    }

    #[tokio::test]
    async fn ungated_tool_prompts_and_denial_blocks() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let mut tools = default_tools();
        tools.push(Box::new(NoopTool));
        let result = run_with_tools(
            &fx,
            &mut perms,
            tools,
            call("custom", json!({})),
            Some(PermissionDecision::deny()),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("Permission denied"));
    }

    #[tokio::test]
    async fn ungated_tool_runs_after_approval() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let mut tools = default_tools();
        tools.push(Box::new(NoopTool));
        let result = run_with_tools(
            &fx,
            &mut perms,
            tools,
            call("custom", json!({})),
            Some(PermissionDecision::once()),
        )
        .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "ran");
    }

    /// Serializes the tests that point `SUIS_CONFIG_DIR` at a scratch dir, so
    /// persistence-triggering tests never read or write the real user config.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn session_grant_does_not_survive_reload_project_grant_does() {
        let _guard = ENV_LOCK.lock().await;
        let config_dir = crate::test_util::TempDir::new();
        std::env::set_var("SUIS_CONFIG_DIR", config_dir.path());

        let fx = Fixture::new();
        let mut perms = PermissionStore::default();

        let first = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "echo hi" })),
            Some(PermissionDecision::grant(PermissionScope::Session, false)),
        )
        .await;
        assert!(!first.is_error);
        let second = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "ls" })),
            Some(PermissionDecision::grant(PermissionScope::Project, false)),
        )
        .await;
        assert!(!second.is_error);

        // Both grants work for the rest of this session…
        assert_eq!(perms.check_command("echo hi"), PermissionResult::Allow);
        assert_eq!(perms.check_command("ls"), PermissionResult::Allow);

        // …but a fresh session sees only the project grant.
        let reloaded = PermissionStore::load(&fx.workspace).unwrap();
        assert_eq!(
            reloaded.check_command("echo hi"),
            PermissionResult::RequireApproval
        );
        assert_eq!(reloaded.check_command("ls"), PermissionResult::Allow);
    }

    #[tokio::test]
    async fn session_deny_blocks_repeat_without_prompt() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        let first = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "echo hi" })),
            Some(PermissionDecision::DenySession),
        )
        .await;
        assert!(first.is_error);

        // The same command again: auto-denied by policy, no prompt emitted
        // (a `None` decision panics the responder if one appears).
        let second = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "echo hi" })),
            None,
        )
        .await;
        assert!(second.is_error);
        assert!(second.content.contains("denied by policy"));
    }

    #[tokio::test]
    async fn granted_command_referencing_hidden_file_reprompts() {
        let mut fx = Fixture::new();
        fx.project.hidden.push(".env".into());
        // `cat *` is granted for the project, yet `cat .env` must still prompt.
        let mut perms = PermissionStore {
            commands: vec![CommandPermission {
                pattern: "cat *".into(),
                scope: PermissionScope::Project,
            }],
            tools: vec![],
            session_denies: vec![],
        };
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "cat .env" })),
            Some(PermissionDecision::deny()),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("Permission denied"));
    }

    #[tokio::test]
    async fn granted_command_without_hidden_token_does_not_reprompt() {
        let fx = Fixture::new();
        fx.write("notes.txt", "hello");
        // No hidden patterns configured; a granted `cat` must run unprompted
        // (a `None` decision panics the responder if a prompt appears).
        let mut perms = PermissionStore {
            commands: vec![CommandPermission {
                pattern: "cat *".into(),
                scope: PermissionScope::Project,
            }],
            tools: vec![],
            session_denies: vec![],
        };
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "cat notes.txt" })),
            None,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn project_deny_is_persisted() {
        let _guard = ENV_LOCK.lock().await;
        let config_dir = crate::test_util::TempDir::new();
        std::env::set_var("SUIS_CONFIG_DIR", config_dir.path());

        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        // Dangerous command: grants are never recorded for these, but a
        // project deny is.
        let result = run_with_decision(
            &fx,
            &mut perms,
            call("bash", json!({ "command": "rm -rf build" })),
            Some(PermissionDecision::DenyProject),
        )
        .await;
        assert!(result.is_error);

        let reloaded = PermissionStore::load(&fx.workspace).unwrap();
        assert_eq!(
            reloaded.check_command("rm -rf build"),
            PermissionResult::Deny
        );
    }

    #[tokio::test]
    async fn plan_mode_refuses_edit_without_prompt() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        // A `None` decision panics the responder if any prompt is emitted: the
        // mode refusal must happen before every permission gate.
        let result = run_in_mode(
            &fx,
            &mut perms,
            default_tools(),
            Mode::Plan,
            call("edit", json!({ "path": "a.txt", "content": "x" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not available in plan mode"));
        assert!(
            !fx.workspace.root.join("a.txt").exists(),
            "no write happened"
        );
    }

    #[tokio::test]
    async fn chat_mode_refuses_bash_even_when_granted() {
        let fx = Fixture::new();
        // A stored project grant must not override the mode boundary.
        let mut perms = PermissionStore {
            commands: vec![CommandPermission {
                pattern: "echo *".into(),
                scope: PermissionScope::Project,
            }],
            tools: vec![],
            session_denies: vec![],
        };
        let result = run_in_mode(
            &fx,
            &mut perms,
            default_tools(),
            Mode::Chat,
            call("bash", json!({ "command": "echo hi" })),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not available in chat mode"));
    }

    #[tokio::test]
    async fn plan_mode_still_reads_and_tracks_tasks() {
        let fx = Fixture::new();
        fx.write("a.txt", "hello");
        let mut perms = PermissionStore::default();
        let access = seeded_access(&["a.txt"]);
        let read = run_in_mode_access(
            &fx,
            &mut perms,
            &access,
            default_tools(),
            Mode::Plan,
            call("read_lines", json!({ "path": "a.txt" })),
            None,
        )
        .await;
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("hello"), "{}", read.content);

        let task = run_in_mode(
            &fx,
            &mut perms,
            default_tools(),
            Mode::Plan,
            call("task", json!({ "action": "create", "title": "analyze" })),
            None,
        )
        .await;
        assert!(!task.is_error);
    }

    #[tokio::test]
    async fn approved_plan_proposal_is_persisted() {
        let fx = Fixture::new();
        let result = run_plan_call(&fx, draft_args(), Some(PlanDecision::Approve)).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("auth-system"), "{}", result.content);

        let store = PlanStore::load(&fx.workspace).unwrap();
        let plan = store.get("auth-system").expect("plan saved");
        assert_eq!(plan.title, "Auth System");
        assert_eq!(plan.steps[0].work_tasks[0].title, "login route");
    }

    #[tokio::test]
    async fn rejected_plan_proposal_writes_nothing() {
        let fx = Fixture::new();
        let result = run_plan_call(&fx, draft_args(), Some(PlanDecision::Reject)).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("rejected by user"),
            "{}",
            result.content
        );
        assert!(
            !fx.workspace.suis_dir.join("plans.json").exists(),
            "rejection must not touch disk"
        );
    }

    #[tokio::test]
    async fn invalid_draft_errors_without_proposing() {
        let fx = Fixture::new();
        // `None` decision => the responder panics if a proposal is emitted.
        let result = run_plan_call(
            &fx,
            json!({ "action": "propose", "title": "T", "steps": [] }),
            None,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("at least one step"));
    }

    #[tokio::test]
    async fn revising_unknown_plan_errors_without_proposing() {
        let fx = Fixture::new();
        let mut args = draft_args();
        args["action"] = json!("revise");
        args["id"] = json!("nope");
        let result = run_plan_call(&fx, args, None).await;
        assert!(result.is_error);
        assert!(result.content.contains("no plan with id 'nope'"));
    }

    #[tokio::test]
    async fn revision_replaces_structure_keeping_the_id() {
        let fx = Fixture::new();
        let first = run_plan_call(&fx, draft_args(), Some(PlanDecision::Approve)).await;
        assert!(!first.is_error);

        let mut args = draft_args();
        args["action"] = json!("revise");
        args["id"] = json!("auth-system");
        args["title"] = json!("Auth System v2");
        let second = run_plan_call(&fx, args, Some(PlanDecision::Approve)).await;
        assert!(!second.is_error);
        assert!(second.content.contains("auth-system"));

        let store = PlanStore::load(&fx.workspace).unwrap();
        assert_eq!(store.plans.len(), 1, "revise must not add a second plan");
        assert_eq!(store.get("auth-system").unwrap().title, "Auth System v2");
    }

    #[tokio::test]
    async fn plan_tool_is_refused_outside_plan_mode() {
        let fx = Fixture::new();
        let mut perms = PermissionStore::default();
        for (mode, marker) in [(Mode::Agent, "agent"), (Mode::Chat, "chat")] {
            let result = run_in_mode(
                &fx,
                &mut perms,
                default_tools(),
                mode,
                call("plan", draft_args()),
                None,
            )
            .await;
            assert!(result.is_error);
            assert!(
                result
                    .content
                    .contains(&format!("not available in {marker} mode")),
                "{}",
                result.content
            );
        }
        assert!(!fx.workspace.suis_dir.join("plans.json").exists());
    }
}
