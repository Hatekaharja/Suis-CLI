# WORKFLOW_MODEL.md

## Purpose

This document defines the workflow hierarchy used by Suis.

The workflow model is responsible for organizing:

* planning
* implementation
* task tracking
* future memory systems

Suis intentionally separates planning from execution.

This improves:

* local model performance
* context management
* long-term project organization
* user visibility

---

# Core Philosophy

## Planning Is A First-Class Concept

Most coding agents treat planning as temporary runtime state.

Suis treats planning as a project artifact.

Plans may persist for:

* days
* weeks
* months

A plan should remain available even after individual implementation sessions end.

---

## Execution Is Temporary

Implementation sessions are temporary.

A session may:

* execute tasks
* update plans
* complete work

When a session ends:

* conversation history may disappear
* runtime context may disappear

Project state remains.

---

## Tasks Are State Units

Tasks are the smallest unit of trackable work.

A task represents:

```text
Something that can be completed.
```

Examples:

```text
Add login endpoint
Create migration
Write tests
Fix bug
```

Tasks are intentionally simple.

---

## Steps Organize Tasks

A step is a collection of related tasks.

Examples:

```text
Database Layer
Backend API
Frontend UI
Testing
```

A step groups work into manageable sections.

---

# Hierarchy

The workflow hierarchy is:

```text
Project
│
├── Plans
│   └── Steps
│       └── Tasks
│
└── Sessions
    └── Steps
        └── Tasks
```

---

# Project

A project represents the current workspace.

Example:

```text
~/Desktop/suis
```

Project state is stored inside:

```text
.suis/
```

A project owns:

* plans
* permissions
* configuration
* future memories

---

# Plans

## Purpose

Plans represent long-lived objectives.

Examples:

```text
Implement Authentication
Add Plugin System
Refactor Provider Layer
Migrate To Axum
```

Plans should survive:

* restarts
* sessions
* context resets

---

## Plan Structure

A plan contains:

```text
Plan
└── Steps
```

Example:

```text
Authentication System

├── Database
├── Backend API
├── Frontend UI
└── Testing
```

---

## Plan Completion

A plan is complete when all steps are complete.

---

# Sessions

## Purpose

Sessions represent active implementation work.

Examples:

```text
Fix failing tests
Implement login endpoint
Investigate bug
```

Sessions are temporary.

Sessions do not replace plans.

---

## Session Structure

A session contains:

```text
Session
└── Steps
```

Example:

```text
Fix Login Bug

├── Investigation
├── Fix
└── Verification
```

---

## Session Lifetime

Sessions exist only while implementation work is active.

Session history may eventually be summarized and stored.

Conversation history should not be treated as permanent project memory.

---

# Steps

## Purpose

Steps divide work into meaningful sections.

A step should be:

* understandable
* self-contained
* implementable

Examples:

```text
Database Layer
Backend API
Frontend UI
Testing
```

---

## Structure

A step contains:

```text
Step
├── Work Tasks
└── Verify Tasks
```

---

## Completion

A step is complete when all tasks are complete.

---

# Tasks

## Purpose

Tasks represent actionable work.

Tasks are the smallest workflow unit.

Examples:

```text
Create users table
Add login endpoint
Implement middleware
Write integration tests
```

---

## States

Tasks may be:

```text
todo
doing
done
blocked
```

Additional states may be added later.

---

# Plan Mode

## Purpose

Plan mode is used to create and manage plans.

Examples:

```text
Plan migration to Axum
Design plugin architecture
Plan authentication system
```

Plan mode focuses on:

* analysis
* roadmap creation
* decomposition

Plan mode does not implement changes.

---

# Agent Mode

## Purpose

Agent mode executes work.

Examples:

```text
Implement login endpoint
Fix failing tests
Refactor provider layer
```

Agent mode focuses on:

* execution
* implementation
* verification

Agent mode may update:

* tasks
* steps
* plans

---

# Chat Mode

## Purpose

Chat mode provides normal AI interaction.

Examples:

```text
Explain this code
Review architecture
Suggest improvements
```

Chat mode does not require plans or implementation.

---

# Implement Workflow

## Purpose

Implementation should operate on existing project structure whenever possible.

---

## Implement Plan

User:

```text
/implement
```

Select:

```text
Plan
```

The agent receives:

* plan summary
* completed steps
* remaining steps

The agent may execute multiple steps.

---

## Implement Step

User:

```text
/implement
```

Select:

```text
Step
```

The agent receives:

* current step
* related tasks
* plan summary

The implementation session becomes focused on a specific area of work.

---

# Future Memory Integration

Future memory systems should integrate with workflow entities.

Examples:

```text
Memory
├── Plans
├── Steps
└── Tasks
```

Memory should reference workflow objects rather than duplicate them.

---

# Storage

Initial project storage may contain:

```text
.suis/

project.json
permissions.json
plans.json
```

Future additions may include:

```text
steps.json
tasks.json
sessions.json
memory.json
```

Storage format should remain implementation-specific.

This document defines relationships, not persistence details.

---

# Design Goals

The workflow model exists to:

* reduce context bloat
* improve local model performance
* separate planning from execution
* support future memory systems
* provide user-visible project structure

The workflow hierarchy should remain simple, understandable, and scalable.

