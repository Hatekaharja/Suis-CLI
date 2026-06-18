# TOOLS.md

## Purpose

This document defines the tool system used by Suis.

Tools are the primary mechanism through which an agent interacts with the user's environment.

Tools allow the agent to:

* inspect files
* modify files
* execute commands
* interact with git
* manage tasks

The goal of the tool system is not to expose every possible operation.

The goal is to expose a small number of powerful, predictable tools that work reliably across local and remote models.

---

# Tool Philosophy

## Few Powerful Tools

The number of tools exposed to the model should be minimized.

Prefer:

```text
5 powerful tools
```

over:

```text
50 specialized tools
```

A smaller tool surface:

* improves local model performance
* reduces decision fatigue
* reduces prompt complexity
* improves reliability
* improves compatibility across models

---

## Tools Represent Intent

Tools should represent what the model wants to accomplish.

Not how the implementation works.

Good:

```text
read
search
edit
bash
```

Bad:

```text
read_file
append_file
insert_file
replace_file
search_glob
search_regex
```

Implementation details belong inside Suis.

The model should operate at a higher level of abstraction.

---

## Tool Simplicity Over Tool Count

A tool may perform multiple internal actions.

Example:

```text
search
```

may internally use:

* ripgrep
* glob matching
* recursive directory walking

The model does not need to know this.

The model should only understand the intent of the operation.

---

## Capability Driven Tool Exposure

Tools should only be exposed when a model is capable of using them.

Example:

```rust
if model.capabilities.tool_use {
    expose_tools();
}
```

If a model cannot reliably use tools:

```rust
ToolMode::ChatOnly
```

should be used instead.

Runtime behavior should always be capability-driven rather than provider-driven.

---

## Tool Minimization Is A Feature

The easiest way to make an agent worse is to expose too many tools.

Every additional tool:

* increases prompt size
* increases decision complexity
* increases hallucination opportunities

A new tool should only be introduced when it cannot reasonably be represented by an existing tool.

---

# Tool Modes

Not all models support tools equally well.

Suis should support multiple tool execution modes.

---

## Native Tool Calling

Used when a model supports reliable native tool calls.

Examples:

```text
Qwen3-Coder
Claude
GPT
```

Tools are exposed through the provider's native tool calling mechanism.

---

## Structured Prompt Mode

Used when a model performs poorly with native tool calling.

The model is instructed to emit structured tool requests.

Example:

```text
<tool>
read
src/main.rs
</tool>
```

Suis parses the request and executes the tool.

This mode exists specifically to improve compatibility with local models.

---

## Chat Only Mode

Used when a model cannot reliably use tools.

No tools are exposed.

The model behaves as a traditional assistant.

---

# MVP Tool Set

The MVP should expose only six tools.

```text
read
search
edit
bash
git
task
```

No additional tools should be added without a strong justification.

---

# Tool: read

## Purpose

Read file contents.

---

## Capabilities

Allowed:

```text
Read single file
Read multiple files
Read partial file
```

Not allowed:

```text
Modify files
Delete files
Create files
```

---

## Examples

```text
Read Cargo.toml
```

```text
Read src/main.rs
```

```text
Read the first 200 lines of src/lib.rs
```

---

## Result Format

```text
FILE: src/main.rs

<contents>
```

The output format should remain simple and predictable.

---

# Tool: search

## Purpose

Find information within a workspace.

---

## Capabilities

May search:

* filenames
* symbols
* text
* patterns

May internally use:

* ripgrep
* glob
* walkdir

The implementation is hidden from the model.

---

## Examples

```text
Find all references to Config
```

```text
Find TODO comments
```

```text
Find usages of PermissionScope
```

---

## Result Format

Matches are reported one per line as `path:line: text`:

```text
src/config.rs:12: pub struct Config {
src/main.rs:48:     let config = Config::load()?;
```

Search results should be concise. A long matched line (e.g. minified code) is
collapsed to a window of up to 25 characters on each side of the match, marked
with `…`, so a single line cannot flood the result:

```text
dist/bundle.js:1: …,t){return t.Config=}function r(e,…
```

The model can request file contents separately through `read`.

---

# Tool: edit

## Purpose

Modify workspace files.

---

## Philosophy

All edits should be diff-based.

The model should never directly overwrite files.

The user should always be able to inspect what changed.

---

## Capabilities

May:

```text
Create files
Modify files
Rename files
Delete files
```

subject to permission rules.

---

## Delete Behavior

Deletes should move files into:

```text
.suis/trash/
```

instead of permanently removing them.

This allows recovery from mistakes.

---

## Edit Modes

### Suggested

Default mode.

Show diff.

Request approval.

---

### Auto Apply

Project configurable.

Changes are applied automatically after approval rules are satisfied.

---

## Result Format

```diff
--- old
+++ new

- old line
+ new line
```

Diff output should remain human-readable.

---

# Tool: bash

## Purpose

Execute terminal commands.

---

## Philosophy

The AI never receives unrestricted shell access.

All command execution is governed by the permission system.

---

## Capabilities

Examples:

```text
cargo test
cargo check
cargo build

npm run build

pnpm install
```

---

## Permission Integration

Command permissions are evaluated before execution.

Possible outcomes:

```text
Allow Once
Allow Session
Allow Project
Allow Always
Deny
```

Dangerous commands use a separate approval flow.

---

## Dangerous Commands

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
Allow Once
Deny
```

No persistent approvals are permitted.

---

## Result Format

```text
Exit Code: 0

stdout:
...

stderr:
...
```

Command results should remain simple and readable.

---

# Tool: git

## Purpose

Interact with repository history and status.

---

## Philosophy

Git access is a privileged capability.

Users should explicitly decide whether the AI may use git.

---

## Git Read

Examples:

```text
status
diff
log
show
```

Requires:

```text
git_read
```

permission.

---

## Git Write

Examples:

```text
commit
branch
checkout
merge
push
```

Requires:

```text
git_write
```

permission.

---

## Result Format

Git output should be summarized when appropriate.

The model should not need to parse unnecessary git internals.

---

# Tool: task

## Purpose

Track agent progress.

---

## Philosophy

Tasks are visible to both:

* user
* agent

Tasks provide lightweight shared state during a session.

---

## Examples

Create task:

```text
Investigate failing tests
```

Update task:

```text
Working on permission system
```

Complete task:

```text
Implement provider discovery
```

---

## MVP Scope

Simple status tracking only.

```text
Todo
Doing
Done
```

No:

* dependency graphs
* project management systems
* workflow engines

---

## Future Potential

Tasks may eventually become part of project memory.

This is explicitly out of scope for MVP.

---

# Tool Visibility

Tools are not automatically exposed.

Visibility is controlled by project configuration.

Example:

```json
{
  "allowed_tools": [
    "read",
    "search",
    "edit",
    "bash"
  ]
}
```

Only visible tools should be presented to the model.

---

# Tool Permissions

Visibility and execution are separate concerns.

Example:

```text
Tool Visible
≠
Tool Allowed
```

A tool may be visible to the model but still require approval before use.

Example:

```text
jira.search
```

may require:

```text
Allow Once
Allow Session
Allow Project
Allow Always
Deny
```

before execution.

---

# Future Tool Categories

Potential future additions:

```text
browser
memory
mcp
workspace
network
```

These should only be added when a real requirement emerges.

The default assumption should be:

```text
Can an existing tool solve this problem?
```

before introducing a new one.

---

# Anti-Goals

Avoid:

* dozens of tools
* provider-specific tools
* transport-specific tools
* exposing implementation details
* unrestricted shell access
* overlapping tools

The model should see a simple, stable interface.

Complexity belongs inside Suis.

Not inside the prompt.

---

# Summary

Suis follows a tool-minimization philosophy.

The MVP exposes:

```text
read
search
edit
bash
git
task
```

These tools should be powerful enough to perform real software development tasks while remaining understandable to both users and local models.

The goal is not to expose more tools.

The goal is to expose the right tools.

