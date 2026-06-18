# CONTEXT_ASSEMBLY.md
# First Implementation/Version (MVP)

The architecture described in this document represents the long-term direction of Suis.

The first implementation should intentionally use a much simpler context assembly strategy.

The goal of the MVP is:

```text
Get an agent running.

Validate workflows.

Validate permissions.

Validate tooling.
```

before investing in advanced context management.

---

## MVP Context Layout

Every model request should contain:

```text
System Prompt
↓
Available Tools
↓
Current Task
↓
Chat History
↓
User Message
```

Nothing more.

---

## System Prompt

Contains:

```text
Agent Identity
Behavior Rules
Tool Usage Instructions
Permission Awareness
```

This remains static throughout the session.

---

## Available Tools

The runtime provides the model with:

```text
Available Tool Names
Tool Descriptions
Tool Parameters
```

Only tools enabled for the current project should be exposed.

---

## Current Task

When a task exists:

```text
Current Objective
Current Task
```

should be injected.

Example:

```text
Objective:
Implement login functionality

Current Task:
Add login endpoint
```

If no task exists:

```text
User Request
```

acts as the current objective.

---

## Chat History

The MVP should simply inject conversation history.

Example:

```text
User
Assistant
User
Assistant
```

No summarization.

No session state.

No structured memory.

No context compression.

---

## Why Use Chat History Initially

Chat history is:

```text
Simple
Predictable
Easy To Debug
```

While inefficient, it dramatically reduces implementation complexity.

The primary goal of the MVP is validating agent behavior rather than optimizing context usage.

---

## Explicit Non-Goals

The first implementation should not include:

```text
Session State
Session Summaries
Context Compression
Project Memory
Structured Memory
Dynamic Context Assembly
Adaptive Context Budgets
```

These systems can be added incrementally after the core runtime is proven.

---

## Migration Path

The MVP architecture should be implemented in a way that allows future replacement.

Eventually:

```text
Chat History
```

will become:

```text
Session State
+
Recent Messages
+
Runtime Context
+
Task Context
```

without requiring major runtime redesign.

---

## MVP Principle

The first implementation should optimize for:

```text
Simplicity
Reliability
Iteration Speed
```

rather than:

```text
Maximum Context Efficiency
```

A working agent with naive context assembly is more valuable than a sophisticated context system that delays development.


---
# Future MVP Context (Eventual goal)
## Purpose

This document defines how Suis constructs context for AI models.

Context Assembly is responsible for:

* preparing model inputs
* managing context windows
* maintaining session continuity
* exposing runtime statef
* providing task information
* controlling memory usage

The primary goal is:

```text
Provide the right context.

Not the most context.
```

Suis is designed primarily for local models.

Local models often perform better when given:

```text
Clear Structure
Focused Objectives
Relevant Information
```

rather than large amounts of conversation history.

---

# Core Philosophy

## Better Context Beats Bigger Context

Many agent systems attempt to improve performance by increasing context.

Suis takes the opposite approach.

Instead of:

```text
Entire Repository
Entire Conversation
Entire Project History
```

Suis prefers:

```text
Current Goal
Current Task
Current State
Relevant Files
```

---

## Context Is A Resource

Context should be treated as limited.

Every token added should justify its existence.

Avoid:

```text
Duplicate Information
Unused Information
Historical Noise
```

---

## Structure Over Intelligence

The runtime should help the model succeed.

Do not assume the model will:

```text
Track Progress
Remember Objectives
Maintain Focus
```

across long sessions.

Instead:

```text
Provide Structure
Provide State
Provide Boundaries
```

---

# Context Layers

Every model invocation should be assembled from layers.

```text
System Prompt
↓
Runtime Context
↓
Session State
↓
Plan / Task Context
↓
Recent Messages
↓
Requested Files
↓
User Message
```

Each layer serves a different purpose.

---

# System Prompt

The system prompt defines:

```text
Agent Identity
Behavior Rules
Tool Usage Rules
Permission Awareness
```

The system prompt should remain relatively stable.

It should not contain:

```text
Project Information
Session Information
Task Information
```

---

# Runtime Context

Runtime Context represents the current environment.

It is generated dynamically.

---

## Example

```text
Mode: Agent

Workspace:
~/Desktop/project

Current Provider:
ollama

Current Model:
qwen3-coder

Git Access:
Enabled

Available Tools:
- read_file
- write_file
- list_files
```

---

## Purpose

Runtime Context answers:

```text
Where am I?
What can I do?
What tools exist?
```

without requiring the model to infer those details.

---

# Session State

Session State is structured memory.

It is generated by Suis.

It is not conversation history.

---

## Purpose

Session State provides continuity across a session.

Examples:

```text
Current Objective
Completed Tasks
Modified Files
Important Decisions
```

---

## Example

```json
{
  "objective": "Implement login endpoint",

  "completed_tasks": [
    "Create auth route",
    "Add JWT middleware"
  ],

  "modified_files": [
    "src/auth.rs",
    "src/routes.rs"
  ],

  "important_decisions": [
    "JWT authentication",
    "15 minute token expiration"
  ]
}
```

---

## Goals

Allow the model to understand:

```text
What has happened
What is complete
What decisions matter
```

without replaying the entire conversation.

---

# Session Log

The Session Log stores raw conversation history.

Example:

```text
User
Assistant
User
Assistant
```

---

## Important

Session Logs are storage.

They are not context.

The runtime should not inject entire session logs into model requests.

---

# Recent Messages

A small window of recent conversation should be provided.

---

## Purpose

Recent Messages preserve short-term conversational flow.

Examples:

```text
Last User Message
Last Assistant Message

or

Last Few Exchanges
```

---

## Goals

Allow the model to understand:

```text
Recent Instructions
Recent Clarifications
Recent Decisions
```

without exposing the full conversation.

---

# Plan Context

When operating on a plan:

```text
Plan
↓
Step
↓
Task
```

the runtime should inject only the information necessary for the current work.

---

## Current Step

Example:

```text
Authentication

Current Step:
Backend API
```

---

## Current Work Task

Example:

```text
Add login endpoint
```

---

## Current Verify Task

Example:

```text
Run integration tests
```

Only injected during verification.

---

# Future Tasks

Future tasks should not be injected.

Example:

```text
Add login endpoint
```

should not automatically expose:

```text
Add frontend UI
Add profile page
```

---

## Reasoning

Future tasks encourage:

```text
Task Skipping
Scope Expansion
Plan Drift
```

particularly in smaller local models.

The runtime should own sequencing.

The model should own execution.

---

# Direct Agent Context

When no plan exists:

```text
User Request
↓
Agent Session
```

The runtime may create temporary runtime tasks.

---

## Runtime Tasks

Examples:

```text
Fix compilation error
Rename module
Update tests
```

These tasks exist only for the current session.

They are not project artifacts.

---

# File Context

Files are loaded on demand.

---

## Request Model

The runtime should prefer:

```text
Discover
↓
Read
↓
Expand
```

rather than:

```text
Load Entire Repository
```

---

## Example

Model requests:

```text
Read src/main.rs
```

Runtime provides:

```text
src/main.rs
```

and nothing more.

Additional files should only be loaded when requested.

---

# Repository Discovery

Repository structure should be discovered through tools.

Examples:

```text
list_files
search_files
read_file
```

The runtime should avoid automatically injecting repository trees.

---

# Context Expansion

Context should grow only when required.

Examples:

```text
Need Additional File
→ Load File

Need Test
→ Load Test

Need Configuration
→ Load Configuration
```

Avoid speculative context injection.

---

# Memory Model

MVP memory consists of two systems.

---

## Session Log

Raw conversation storage.

Example:

```text
Messages
Responses
Tool Calls
```

Stored for history and future features.

---

## Session State

Structured memory.

Example:

```text
Objective
Completed Tasks
Modified Files
Important Decisions
```

Used directly by the runtime.

---

# Project Memory

Project Memory is not part of MVP.

Future implementations may include:

```text
Long-Term Decisions
Project Conventions
Architectural Rules
```

stored independently from sessions.

---

# Context Budgeting

Context assembly should remain dynamic.

Avoid fixed allocations.

Example:

```text
20% Files
20% History
20% Tasks
```

should not be hardcoded.

---

## Reasoning

Different requests require different context.

Examples:

```text
Debugging
```

needs more file context.

```text
Planning
```

needs more project context.

---

# Small Context Models

Smaller context models require more aggressive optimization.

The runtime may adjust instructions.

Examples:

```text
Be concise.
Prefer tool usage.
Avoid long explanations.
```

---

## Goal

Encourage:

```text
Tool Usage
Execution
Iteration
```

instead of long reasoning chains.

---

# Large Context Models

Large context models may receive:

```text
More Files
More History
More Session State
```

when beneficial.

The assembly strategy should remain adaptive.

---

# Context Ownership

The runtime owns context assembly.

The model does not choose:

```text
Session State
Runtime Context
Task State
```

The model only requests additional information through tools.

---

# Anti-Goals

Avoid:

* injecting entire repositories
* injecting entire conversations
* injecting future tasks
* speculative file loading
* hidden context generation
* context assembly based on guesses

---

# Final Principle

The purpose of Context Assembly is not to make the model smarter.

The purpose is to make the model's job simpler.

Suis should not overwhelm the model with information.

Suis should provide:

```text
The right information

at the right time

for the current task.
```

