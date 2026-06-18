# PERMISSION_SYSTEM.md

## Purpose

This document defines the permission model used by Suis.

The permission system exists to balance:

* agent autonomy
* user control
* project safety
* workflow efficiency

Suis is designed as a strong local-first coding agent, but the user must always remain in control of:

* filesystem access
* command execution
* git operations
* tool usage
* project boundaries

Permissions should feel powerful without becoming intrusive.

---

# Core Philosophy

## User Owns The Workspace

The AI operates within a workspace.

The workspace belongs to the user.

The AI never owns the workspace.

The AI may suggest actions.

The user ultimately controls what is allowed.

---

## Permissions Are Specific

Permissions should be granted as specifically as possible.

Good:

```text
cargo test
cargo check
git status
```

Bad:

```text
all commands
terminal access
```

Suis should avoid broad permissions whenever possible.

---

## Friction Should Be Optional

Repeated prompts create fatigue.

Users should be able to grant broader permissions when desired.

Example:

```text
Allow cargo test?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

Advanced approval:

```text
Shift + Select
```

expands the permission scope.

Example:

```text
cargo test
```

becomes:

```text
cargo *
```

This allows users to reduce friction while maintaining control.

---

## Dangerous Actions Remain Explicit

Certain actions are considered inherently dangerous.

Dangerous actions should never support persistent approval.

Example:

```text
sudo
chmod
chown
dd
mkfs
```

Approval options:

```text
[1] Once
[2] Deny
```

Only.

---

# Permission Scopes

## Once

Valid for a single action.

After execution the permission expires.

---

## Session

Valid until Suis exits.

Permission is not persisted.

---

## Project

Stored in:

```text
.suis/permissions.json
```

Applies only to the current project.

---

## Always

Stored globally.

Applies across all projects.

Use sparingly.

---

## Deny

Action is rejected.

The agent must choose an alternative approach.

---

# Command Permissions

## Philosophy

Commands are approved individually.

Suis should never expose unrestricted terminal execution.

Example:

```text
cargo test
```

Permission applies to:

```text
cargo test
```

only.

---

## Wildcard Approval

Users may optionally approve command groups.

Example:

```text
cargo test
```

Shift + Approve:

```text
cargo *
```

Possible examples:

```text
cargo *
npm *
pnpm *
git status *
```

Wildcard permissions should be explicit and visible.

---

## Dangerous Commands

Dangerous commands are classified separately.

Examples:

```text
sudo
chmod
chown
dd
mkfs
```

Dangerous commands may only receive:

```text
Once
Deny
```

approval.

They cannot receive:

```text
Session
Project
Always
```

permissions.

---

## Forbidden Commands

Future versions may introduce commands that are never exposed to the agent.

Examples:

```text
shutdown
reboot
poweroff
```

These commands should not be available as tools.

---

# Filesystem Permissions

## Workspace Boundary

The workspace is the project's active root directory.

Example:

```text
/Desktop/project
```

The AI has access to:

```text
/Desktop/project/**
```

by default.

---

## Inside Workspace

Reading files:

```text
Allowed
```

Creating files:

```text
Allowed
```

Editing files:

```text
Allowed
```

Deleting files:

```text
Allowed
```

subject to project rules.

---

## Outside Workspace

Any access outside the workspace requires approval.

Examples:

```text
../
~/Documents
~/Desktop
/etc
```

Prompt:

```text
Agent wants to access:

~/Documents

Allow?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

This behavior should mirror the user experience provided by OpenCode and similar agent systems.

---

# Hidden Files

## Purpose

Hidden files are completely invisible to the AI.

The AI should not:

* read them
* search them
* reference them
* discover their contents

---

## Examples

```json
{
  "hidden_files": [
    ".env",
    ".env.local",
    "secrets.json"
  ]
}
```

---

## Behavior

If the AI attempts access:

```text
Access denied.
```

The existence of file contents should not be revealed.

---

# Hardened Files

## Purpose

Hardened files are visible to the AI.

However:

modification requires explicit approval.

---

## Examples

```json
{
  "hardened_files": [
    "Cargo.toml",
    "package.json",
    ".github/workflows/release.yml"
  ]
}
```

---

## Allowed Operations

Reading:

```text
Allowed
```

Modification:

```text
Permission Required
```

Deletion:

```text
Permission Required
```

---

## Approval Flow

Example:

```text
Agent wants to modify:

Cargo.toml

Allow?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

---

# Delete Behavior

## Philosophy

Accidental deletion should be reversible.

---

## Trash System

Deleting a file should not immediately remove it.

Instead:

```text
.suis/trash/
```

is used.

Example:

```text
src/old.rs
```

becomes:

```text
.suis/trash/src/old.rs
```

---

## Restore

Users should be able to restore files from trash.

---

## Purge

Permanent deletion occurs only through explicit purge operations.

---

# Git Permissions

## Philosophy

Git access is a privileged capability.

Many repositories contain:

* author information
* commit history
* project metadata

Users should decide whether the AI may access git.

---

## Git Read

Examples:

```text
git status
git diff
git log
git show
```

Capability:

```text
git_read
```

Approval options:

```text
Once
Session
Project
Always
Deny
```

---

## Git Write

Examples:

```text
git commit
git checkout
git branch
git merge
git push
```

Capability:

```text
git_write
```

Approval options:

```text
Once
Session
Project
Always
Deny
```

Git write permissions should be requested separately from git read permissions.

---

# Tool Permissions

## Philosophy

Tools are user-controlled.

Tools are not automatically exposed to the AI.

---

## Global Tool Installation

Tools may be installed globally.

Example:

```text
Jira
Linear
Docker
Custom MCP
```

Installed does not mean available.

---

## Project Tool Registration

Projects explicitly choose which tools are available.

Example:

```json
{
  "allowed_tools": [
    "jira",
    "docker"
  ]
}
```

Only registered tools become visible to the AI.

---

## First Use Approval

When a tool is used:

```text
Agent wants to use:

jira.search_issues

Allow?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

---

# Provider Permissions

## Philosophy

Providers are global resources.

Projects determine which providers are visible.

---

## Provider Scope

Example:

```json
{
  "provider_scope": [
    "ollama"
  ]
}
```

Visible:

```text
ollama
```

Hidden:

```text
lmstudio
openrouter
anthropic
```

---

## Model Scope

Example:

```json
{
  "model_scope": [
    "qwen3-coder"
  ]
}
```

Only approved models should appear in project selection menus.

---

## Defaults

Most projects will use:

```json
{
  "provider_scope": "all",
  "model_scope": "all"
}
```

for simplicity.

---

# Permission Storage

## Global Permissions

Stored within:

```text
~/.config/suis/
```

Examples:

```text
always permissions
provider preferences
global defaults
```

---

## Project Permissions

Stored within:

```text
project/.suis/
```

Examples:

```text
permissions.json
project.json
```

Examples:

```text
project permissions
hidden files
hardened files
tool access
provider visibility
model visibility
```

---

# Project Initialization

## Gitignore Import

When `.gitignore` is detected during project setup, Suis may offer to import entries as:

- hidden files
- hardened files

The user must approve the import.

Suis may provide recommendations for common patterns such as:

Hidden:
- .env
- .env.local
- secrets.json

Hardened:
- Cargo.lock
- package-lock.json

Ignored:
- target/
- dist/
- node_modules/

The user remains responsible for the final configuration.

---

# Future Considerations

Potential future permission categories:

```text
network_access
browser_access
mcp_access
memory_access
multi_workspace_access
```

These should follow the same philosophy:

* explicit
* visible
* user-controlled

---

# Anti-Goals

Avoid:

* unrestricted terminal access
* unrestricted filesystem access
* hidden permissions
* automatic tool exposure
* automatic git access
* broad "allow everything" approvals

Users should always understand:

* what the AI can do
* why it can do it
* how to revoke it

Permission systems should remain simple, predictable, and transparent.

