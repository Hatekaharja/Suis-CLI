# DESIGN_PRINCIPLES.md

## Purpose

This document defines the core design principles of Suis.

These principles act as the architectural compass for the project.

When introducing new features, systems, plugins, providers, tools, or workflows, contributors should evaluate whether the change aligns with these principles.

If a proposed feature conflicts with these principles, the feature should be reconsidered before implementation.

---

# Principle 1: User Owns The Workspace

The AI operates within the user's environment.

The AI never owns the environment.

The AI may:

* suggest actions
* perform approved actions
* automate workflows

The user ultimately controls:

* files
* commands
* tools
* permissions
* providers
* models

Suis should never remove ownership from the user.

---

# Principle 2: Local First

Suis is designed for local AI usage first.

The default experience should work with:

* Ollama
* LM Studio
* llama.cpp
* future local providers

Remote providers are supported as an extension of the system, not as the primary design target.

Every architectural decision should ask:

> Does this still work well for local models?

---

# Principle 3: Capability Driven

Runtime behavior should be determined by capabilities rather than vendor-specific logic.

Good:

```rust
if model.capabilities.tool_use {
    // enable tools
}
```

Bad:

```rust
if provider == "ollama" {
    // special behavior
}
```

Capabilities define what a model can do.

Provider identity should rarely affect runtime behavior.

---

# Principle 4: Explicit Visibility

Resources must be visible before they can be used.

Examples:

* providers
* models
* tools
* files

The AI should never automatically gain access to resources simply because they exist.

Visibility is always controlled by the user.

---

# Principle 5: Safe By Default

Safety should be the default state.

Examples:

* workspace boundaries
* hidden files
* hardened files
* trash instead of permanent deletion
* dangerous command restrictions

Users may choose to reduce restrictions.

The system should never assume elevated access by default.

---

# Principle 6: Smart Suggestions, Not Smart Decisions

Suis should make helpful recommendations.

Suis should not make important decisions on behalf of the user.

Examples:

Good:

```text
Detected Ollama

Would you like to add it?
```

Good:

```text
Detected .gitignore

Import hidden files?
```

Bad:

```text
Automatically imported hidden files.
```

Bad:

```text
Automatically granted permissions.
```

The user remains responsible for final decisions.

---

# Principle 7: Reduce Friction Without Sacrificing Control

Agent systems fail when every action requires approval.

Agent systems also fail when approvals become meaningless.

Suis should reduce repetitive prompts through:

* session permissions
* project permissions
* wildcard approvals
* remembered preferences

while ensuring users always understand what is being approved.

---

# Principle 8: Project Isolation

Projects should remain isolated from one another.

A project defines:

* permissions
* tool visibility
* model visibility
* provider visibility
* project-specific configuration

Actions taken in one project should not unexpectedly affect another.

---

# Principle 9: Open And Inspectable

Users should be able to understand how Suis works.

Configuration should be:

* readable
* editable
* inspectable

Permission decisions should be visible.

Configuration files should remain simple and predictable.

Avoid hidden behavior whenever possible.

---

# Principle 10: Boring Architecture Wins

Complexity should only be introduced when justified by real requirements.

Prefer:

```text
Simple
Predictable
Understandable
```

over:

```text
Flexible
Abstract
Theoretically scalable
```

Architecture should remain approachable to contributors.

A developer should be able to understand the repository structure in a single sitting.

---

# Principle 11: Strong Agent, Strong Boundaries

Suis aims to be a strong coding agent.

The AI should be capable of:

* reading files
* editing files
* creating files
* running commands
* using tools

within approved boundaries.

The goal is not to limit the agent.

The goal is to clearly define where the agent may operate.

Strong capabilities require strong boundaries.

---

# Principle 12: Configuration Defines Reality

Suis follows a simple ownership model:

Global Configuration

```text
Defines what exists.
```

Examples:

* providers
* models
* global preferences

Project Configuration

```text
Defines what is visible.
```

Examples:

* provider scope
* model scope
* tool visibility
* hidden files
* hardened files

Runtime Permissions

```text
Defines what may happen right now.
```

Examples:

* command approvals
* tool approvals
* git permissions

This separation should remain consistent throughout the project.

---

# Decision Framework

When evaluating a new feature, ask:

1. Does this preserve user ownership?
2. Does this remain local-first?
3. Does this reduce or increase complexity?
4. Does this introduce hidden behavior?
5. Does this respect project isolation?
6. Does this maintain explicit visibility?
7. Would a new contributor understand this quickly?

If the answer to multiple questions is "no", the feature should be reconsidered.

---

# Summary

Suis should remain:

* Local First
* Open Source
* Provider Agnostic
* Capability Driven
* User Controlled
* Safe By Default
* Simple To Understand

The project should prioritize clarity, ownership, and usability over unnecessary complexity.

