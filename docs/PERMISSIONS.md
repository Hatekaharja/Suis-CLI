# PERMISSIONS.md

## Purpose

This document defines the permission system for Suis.

Permissions are responsible for controlling:

* filesystem access
* command execution
* tool usage
* provider visibility
* model visibility
* workspace boundaries

The permission system is a core part of Suis.

It should be:

* predictable
* transparent
* inspectable
* persistent

Permissions are project state, not temporary runtime behavior.

---

# Core Philosophy

## User Owns The Machine

The AI never owns the machine.

The AI may:

* request actions
* suggest actions
* perform approved actions

The user always remains in control.

---

## Explicit Over Implicit

Permissions should be explicit.

Avoid:

```text
Hidden approvals
Automatic escalation
Undocumented behavior
```

Users should always understand:

* what is allowed
* why it is allowed
* where it is configured

---

## Persistent Decisions

Permission decisions should be remembered.

Users should not repeatedly approve the same safe actions.

Examples:

```text
cargo test
cargo check
git status
```

should become frictionless after approval.

---

## Security Through Layers

Permissions are evaluated in layers.

Higher layers always override lower layers.

---

# Permission Hierarchy

Permissions are evaluated in the following order:

```text
Workspace Boundary
↓
Hidden Files
↓
Hardened Files
↓
Tool Permissions
↓
Command Permissions
↓
Provider Permissions
```

Higher levels always win.

Example:

```text
cargo *
```

may be approved.

However:

```text
cargo build ../other-project
```

still requires approval because it crosses the workspace boundary.

---

# Workspace Boundary

The workspace boundary is the primary security mechanism.

Example:

```text
/Desktop/project
```

Everything inside the workspace is considered local project state.

---

## Inside Workspace

The AI may:

```text
Read Files
Edit Files
Create Files
Search Files
```

subject to other permission layers.

---

## Outside Workspace

Access outside the workspace requires approval.

Examples:

```text
../
~/Desktop
~/Documents
~/Projects
```

should trigger permission requests.

---

## Permission Options

Workspace boundary requests support:

```text
Once
Session
Project
Always
Deny
```

---

# Hidden Files

Hidden files are invisible to the AI.

They function similarly to an AI-specific `.gitignore`.

---

## Purpose

Examples:

```text
.env
.env.local
secrets.json
private.key
```

These files should never be exposed to the model.

---

## Behavior

Hidden files:

```text
Read     = Denied
Write    = Denied
Search   = Denied
Discover = Denied
```

The AI should behave as though these files do not exist.

---

## Example

```json
{
  "hidden": [
    ".env",
    ".env.local"
  ]
}
```

---

# Hardened Files

Hardened files are visible but protected.

---

## Purpose

Examples:

```text
Cargo.toml
Cargo.lock
package.json
docker-compose.yml
```

Files that are important but should remain editable when explicitly approved.

---

## Behavior

Hardened files:

```text
Read     = Allowed
Search   = Allowed
Discover = Allowed
Write    = Permission Required
```

---

## Example

```json
{
  "hardened": [
    "Cargo.toml",
    "Cargo.lock"
  ]
}
```

---

# .gitignore Import

During project setup:

```text
.gitignore detected.

Import entries as:

[1] Hidden Files
[2] Hardened Files
[3] Skip
```

This provides a fast project onboarding experience.

---

# Command Permissions

Commands are approved individually.

The AI may request command execution.

The user decides how long that permission should remain valid.

---

## Permission Options

Standard commands support:

```text
Once
Session
Project
Always
Deny
```

---

## Example

```text
Command Requested

cargo test

Allow?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

---

# Wildcard Permissions

Users may approve command groups.

This is intended to reduce permission fatigue.

---

## Exact Match

Example:

```text
cargo test
```

Approval stores:

```text
cargo test
```

only.

---

## Wildcard Match

Using Shift while approving:

```text
cargo test
```

stores:

```text
cargo *
```

instead.

---

## Examples

```text
cargo *
npm *
pnpm *
```

All matching commands inherit the permission.

---

# Destructive Commands

Certain commands are considered destructive.

These commands never support persistent approval.

---

## Examples

```text
rm
rmdir
del
```

Additional commands may be added over time.

---

## Permission Options

Destructive commands support:

```text
Once
Deny
```

only.

Never:

```text
Session
Project
Always
```

---

# Trash System

Destructive file operations should be recoverable.

Instead of permanently deleting files:

```text
rm file.txt
```

the runtime should move files into:

```text
.suis/trash/
```

---

## Goals

Provide:

```text
Recovery
Undo
Safety
```

while still allowing the AI to perform legitimate cleanup tasks.

---

# Tool Permissions

Tools are managed separately from commands.

---

## Registration

Tools are installed globally.

Projects choose which tools are available.

Unregistered tools cannot be used.

---

## First Use

When an AI first requests a tool:

```text
Allow Tool?

[1] Once
[2] Session
[3] Project
[4] Always
[5] Deny
```

---

## Stored Permissions

Tool permissions should behave identically to command permissions.

This creates a consistent user experience.

---

# Git Permissions

Git access is a special capability.

Not all users want AI access to repository history.

---

## Project Setup

During project initialization:

```text
Allow Git Access?

[Y] Yes
[N] No
```

---

## Disabled State

When disabled:

```text
git status
git diff
git log
git blame
```

are unavailable to the AI.

---

# Provider Permissions

Projects may restrict available providers.

This allows different projects to use different AI environments.

---

## Examples

Project A:

```text
Ollama
LM Studio
```

Project B:

```text
OpenAI
```

Project C:

```text
Ollama Only
```

---

## Purpose

Reduce:

```text
Provider Noise
Model Selection Fatigue
Configuration Complexity
```

while preserving flexibility.

---

# Model Permissions

Provider visibility and model visibility are separate concerns.

Projects may choose:

```text
Allowed Providers
Allowed Models
```

independently.

---

## Example

```text
Provider:
Ollama

Allowed Models:
qwen3-coder
devstral
```

Other discovered models remain unavailable.

---

# Permission Storage

Permissions should be persistent and inspectable.

---

## Global

```text
~/.config/suis/
```

Contains:

```text
providers.json
settings.json
plugins/
models/
```

---

## Project

```text
project/.suis/
```

Contains:

```text
project.json
permissions.json
plugins.json
```

Future additions may expand this structure.

---

# Permission Resolution

Before any action executes:

```text
Check Workspace Boundary
↓
Check Hidden Files
↓
Check Hardened Files
↓
Check Tool Permissions
↓
Check Command Permissions
↓
Check Provider Permissions
↓
Execute
```

If any layer denies access:

```text
Execution Stops
```

---

# User Visibility

Users should always be able to inspect:

```text
Allowed Commands
Allowed Tools
Hidden Files
Hardened Files
Provider Access
Model Access
```

through the interface.

Permissions should never become hidden application state.

---

# Anti-Goals

Never allow:

```text
Automatic Permission Escalation
Hidden Tool Access
Invisible Command Execution
Workspace Boundary Bypass
Persistent Destructive Commands
```

Never assume:

```text
User Intent
Safe Commands
Safe Files
```

without explicit permission.

---

# Final Principle

Permissions exist to reduce friction without reducing control.

The ideal Suis experience is:

```text
Safe actions become effortless.

Dangerous actions remain deliberate.
```

