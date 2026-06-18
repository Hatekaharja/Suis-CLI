# AGENT_RUNTIME.md

## Purpose

This document defines the runtime behavior of Suis.

The runtime is responsible for:

* session management
* context assembly
* tool execution
* permission handling
* task progression
* implementation workflows

The runtime is intentionally designed around local models.

Suis should help smaller local models succeed through structure rather than relying on large context windows.

---

# Core Philosophy

## Planning Creates Structure

Planning is responsible for:

* understanding goals
* creating plans
* creating steps
* creating tasks

Planning should not implement code.

---

## Implementation Follows Structure

Implementation is responsible for:

* reading files
* modifying files
* executing tools
* completing tasks

Implementation should not redesign plans.

---

## Context Is A Resource

Context should be treated as scarce.

The runtime should provide:

* relevant information
* focused objectives
* current work

The runtime should avoid:

* entire repository dumps
* excessive conversation history
* unrelated files

---

# Runtime Modes

Suis contains three modes.

---

## Plan Mode

Purpose:

```text
Analyze
Plan
Decompose
Structure
```

Capabilities:

```text
✓ Read Files
✓ Search Files

✗ Edit Files
✗ Execute Commands
✗ Use Git
✗ Modify Project State
```

Output:

```text
Plans
Steps
Tasks
```

Plan mode never implements code.

---

## Agent Mode

Purpose:

```text
Implement
Execute
Verify
```

Capabilities:

```text
✓ Read Files
✓ Edit Files
✓ Execute Allowed Tools
✓ Update Tasks
✓ Verify Work
```

Agent mode performs project work.

---

## Chat Mode

Purpose:

```text
Discuss
Review
Explore
```

Capabilities:

```text
✓ Read Files
✓ Search Files
```

Chat mode behaves similarly to Plan mode.

Chat mode does not modify project state.

---

# Session Lifecycle

A session represents active work.

A session owns:

```text
Current Objective
Current Context
Current Runtime State
```

Sessions are temporary.

Project state persists beyond sessions.

---

# Planning Runtime

Plan mode creates:

```text
Plan
└── Step
    ├── Work Tasks
    └── Verify Tasks
```

Example:

```text
Authentication

Backend API

Work Tasks
□ Add login endpoint
□ Add logout endpoint

Verify Tasks
□ Run integration tests
□ Verify login flow
```

Plans are stored as project artifacts.

---

# Direct Agent Runtime

Direct agent mode occurs when no plan exists.

Example:

```text
Fix failing tests
```

The runtime may create temporary session tasks if work becomes complex.

Simple requests should not automatically create task structures.

Examples:

```text
Fix typo
Rename variable
Update comment
```

should remain lightweight.

---

# Plan Implementation Runtime

Plan implementation begins when a user selects:

```text
/implement
```

and chooses:

```text
Plan
Step
```

The runtime assembles implementation context.

The implementation agent receives:

```text
Plan Summary
Current Step
Current Work Tasks
Current Verify Tasks
Relevant Files
Project Snapshot
```

---

## Plan Authority

Plans are considered authoritative.

The implementation agent should follow the existing plan structure.

If additional work is discovered:

```text
Missing Tasks
Additional Steps
New Requirements
```

the runtime should notify the user.

The user may then choose how to update project planning.

Implementation should not silently rewrite plans.

---

# Task Ownership

The implementation agent owns task progression.

The runtime stores state.

Examples:

```text
todo
doing
done
blocked
```

The runtime should not automatically infer task completion.

Task state should remain explicit.

---

# Verification Workflow

Each step contains:

```text
Work Tasks
Verify Tasks
```

---

## Work Phase

The agent completes:

```text
Work Tasks
```

first.

---

## Verification Prompt

After work tasks complete:

```text
All work tasks are complete.

Begin verification?

[Y] Yes
[N] No
```

Verification begins only after user approval.

---

## Verification Phase

The agent executes:

```text
Verify Tasks
```

Examples:

```text
cargo test
cargo check
integration tests
manual verification instructions
```

---

## Step Completion

A step becomes complete when:

```text
Work Tasks Complete
+
Verify Tasks Complete
```

---

# Context Assembly

The runtime should construct focused context.

The implementation agent should not receive:

```text
Entire Repository
Entire Conversation History
Entire Plan Collection
```

unless explicitly required.

---

## Project Snapshot

The runtime may generate:

```text
Project Root
├── src/
├── tests/
├── Cargo.toml
└── README.md
```

This provides navigation without excessive context usage.

---

## Relevant Files

Files should be loaded on demand.

The runtime should prefer:

```text
Discover
Read
Expand
```

rather than:

```text
Load Everything
```

---

# Work Package

A Work Package is a runtime concept.

It is not persisted.

A Work Package represents the focused information provided to the implementation agent.

Example:

```text
Plan Summary
Current Step
Current Tasks
Relevant Files
Project Snapshot
```

Work Packages exist to improve local model performance.

---

# Interruptions

Users may interrupt execution at any time.

Examples:

```text
Use Tokio instead
Stop current approach
Focus on testing
```

Interruptions should be handled immediately.

---

# Queued Messages

Shift+Enter may queue messages.

Queued messages should be processed after the current operation completes.

This behavior remains implementation-defined.

---

# Progress Checkpoints

The runtime may detect long-running execution sessions.

Examples:

```text
High Tool Usage
Long Execution Time
Repeated Actions
```

The runtime may offer the user:

```text
Request Update
Continue
Stop Execution
```

The purpose of checkpoints is preventing runaway execution loops.

Progress visibility should primarily be handled through the user interface.

---

# File Modification Workflow

File changes should be applied immediately.

The runtime should record:

```text
Diffs
Changes
Modification History
```

Users should be able to:

```text
Review
Undo
Restore
Revert
```

changes through the interface.

---

# Permission Integration

The runtime must respect:

```text
Workspace Boundaries
Command Permissions
Tool Permissions
File Permissions
Provider Permissions
```

Permission handling is defined separately.

The runtime should never bypass permission decisions.

---

# Session Completion

A session may complete when:

```text
Tasks Complete
+
No Remaining Work Detected
```

Verification should remain user-controlled.

---

# Runtime Anti-Goals

Avoid:

* autonomous plan rewriting
* repository-wide context loading
* hidden execution
* infinite agent loops
* automatic permission escalation
* tool execution without visibility

The runtime should remain predictable, transparent, and controllable.

Most importantly:

Planning creates structure.

Implementation follows structure.

