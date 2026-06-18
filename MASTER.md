THIS IS THE MASTER ARCHITECTURE DOCUMENT FOR SUIS-CLI

This document contains all of the docs in ./docs but in one large file for AI agent use. If you have read this document, you have read all of the documents in ./docs.

===
SECTION 1
---
# PROJECT_PLAN.md

## Vision

Suis is a local-first Rust CLI that enables developers to interact with AI coding agents directly from the terminal.

The primary goal is to provide a polished, lightweight, and highly configurable agent experience that works seamlessly with local AI providers such as Ollama, LM Studio, and llama.cpp.

Suis prioritizes simplicity, transparency, and user control while remaining extensible enough to support future providers, plugins, workflows, and memory systems.

The project is intended to become a daily-driver coding agent for local AI users.

---

# Core Principles

## Local First

Local providers are a first-class feature.

Provider discovery, capability detection, and setup should feel automatic and effortless.

Users should be able to install Suis and immediately begin using available local models with minimal configuration.

---

## User Control

The agent should never silently exceed its permissions.

Users remain in control of:

* filesystem access
* command execution
* git access
* plugin access
* workspace boundaries

Permission decisions should be configurable at multiple scopes.

---

## Lightweight by Design

Suis should avoid unnecessary complexity.

Features should be evaluated based on:

* usefulness
* maintainability
* developer experience
* implementation complexity

---

## Extensible Architecture

Although the MVP is intentionally focused, architecture should support:

* additional providers
* plugins
* workflows
* memory systems
* MCP integrations
* remote APIs

without major redesigns.

---

# Target User

Developers who want:

* local AI coding agents
* simple installation
* strong control over permissions
* terminal-native workflows
* open-source tooling

---

# Success Criteria

Suis succeeds when:

* setup is significantly easier than competing local-first solutions
* the project is actively used by its creator and community
* a stable plugin ecosystem emerges
* users can reliably run coding agents against local models

GitHub metrics are secondary to utility and sustainability.

---

# MVP Goals

## Included

### Interactive CLI

Users launch:

suis

and enter an interactive terminal experience.

---

### Local Provider Discovery

Automatically detect:

* Ollama
* LM Studio
* llama.cpp

Provider discovery should occur during setup and be available on demand.

---

### Capability Detection

Models should be evaluated for capabilities such as:

* chat
* streaming
* tool use
* structured output

Capabilities are stored and reused.

The agent should adapt to model capabilities rather than assuming all models support the same features.

---

### Workspace Security

Workspace boundaries are enforced.

Operations outside the workspace require explicit approval.

Users should clearly understand what resources the agent can access.

---

### File Operations

Agent can:

* read files
* write files
* create files

subject to permission controls.

---

### Command Execution

Agent can execute approved commands.

Permission decisions support:

* once
* session
* project
* always
* deny

---

### Diff-Based Editing

Changes are applied immediately.

Diffs are tracked for:

* undo
* restore
* revert

---

### Session Task Tracking

Agent maintains visible task tracking.

Example:

* analyze codebase
* implement changes
* write tests
* verify results

Task state should be visible to both user and agent.

---

# Explicit Non-Goals

The MVP will not include:

* autonomous multi-hour execution
* cloud-first workflows
* long-term memory systems
* repository indexing
* advanced workflow automation
* complex plugin ecosystems

These may be added later.

---

# Provider Architecture

## Philosophy

Providers are managed separately from models.

Provider discovery and model communication are separate concerns.

Providers represent discovered endpoints.

Transports represent communication protocols.

Multiple providers may share the same transport implementation.

---

## Proposed Structure

~/.config/suis/

providers.json

models/

* ollama.json
* lmstudio.json
* llamacpp.json

---

## Responsibilities

providers.json

Stores:

* provider definitions
* endpoints
* authentication
* enabled state

Provider model files store:

* available models
* detected capabilities
* provider-specific metadata

---

# Transport Philosophy

Suis should communicate with models through transport layers rather than provider-specific integrations whenever possible.

Examples:

- Ollama may use a native transport.
- LM Studio may use an OpenAI-compatible transport.
- Future providers may reuse existing transports.

The runtime should primarily reason about capabilities rather than provider identity.

---

# Permission Architecture

Permissions are capability-based rather than command-based.

Examples:

* file_read
* file_write
* file_create
* command_execute
* git_access

Command permissions may additionally maintain remembered decisions.

Example:

* allow once
* allow session
* allow project
* allow always

---

# Workspace Architecture

## Global Configuration

~/.config/suis/

Stores:

* providers
* settings
* plugins
* model metadata

---

## Project Configuration

project/.suis/

Stores:

* project configuration
* permissions
* plugin access
* future project memory

---

## Session State

Stores:

* active conversation
* task tracking
* approvals
* runtime state

Session state is temporary.

---

# Plugin Philosophy

Plugins are expected to become a major extension point.

Potential plugin categories:

* tools
* MCP integrations
* workflows
* prompts
* memory backends

Plugin architecture should influence early design decisions even if plugins arrive after MVP.

---

# Git Philosophy

Git access is considered a privileged capability.

Users should explicitly grant access before:

* reading git history
* creating commits
* creating branches
* generating commit messages

Git functionality is not required for all users.

---

# Future Directions

## Project Memory

Persist knowledge within a project.

Examples:

* coding conventions
* architecture decisions
* user preferences

---

## Multi-Repository Workspaces

Allow agents to operate across multiple repositories within a controlled workspace.

---

## Remote Providers

Support additional providers without changing the core architecture.

Examples:

* OpenAI
* Anthropic
* OpenRouter

---

## Advanced Agent Workflows

Potential future support for:

* extended task execution
* checkpoints to prevent runaway execution loops
* autonomous workflows

---

# Open Questions

## Plugin System

Precise plugin architecture remains undecided.

Questions:

* plugin packaging
* versioning
* sandboxing
* permissions

---

## Capability Detection

Determine how capabilities should be discovered and validated.

Questions:

* static metadata
* provider APIs
* runtime testing

---

## Workspace Awareness

Future evaluation needed regarding:

* repository indexing
* working tree generation
* semantic project understanding

---

## Memory System

Long-term memory architecture remains intentionally deferred until after MVP.

===
SECTION 2
---
# MVP_SCOPE.md

## Purpose

The MVP exists to validate the core Suis experience:

1. Install Suis
2. Automatically discover local AI providers
3. Launch an interactive terminal UI
4. Chat with a coding model
5. Allow the model to inspect and modify a project
6. Maintain strong user control through permissions

If Suis can achieve this workflow reliably, the MVP is successful.

---

# MVP Success Criteria

A developer should be able to:

* install Suis
* run `suis`
* select a discovered local model
* ask the agent to modify code
* approve changes
* review diffs
* continue working

without manually configuring providers.

---

# Included Features

## Interactive Terminal Interface

Primary command:

```bash
suis
```

Launches a full-screen terminal experience.

Required capabilities:

* chat interface
* streaming responses
* task display
* tool activity display
* permission prompts
* model selection

Inspiration:

* OpenCode UI
* Claude Code permission flow

---

## Local Provider Discovery and transport resolution

Supported providers:

### Ollama

Required.

### LM Studio

Required.

### llama.cpp

Required if practical.

May be deferred if implementation complexity is too high.

---

## Provider Configuration

Global configuration directory:

```text
~/.config/suis/
```

Provider endpoints stored in:

```text
providers.json
```

Example:

```json
{
  "providers": [
    {
      "id": "ollama",
      "endpoint": "http://localhost:11434"
      "transport": "ollama"
    },
    {
      "id": "custom",
      "endpoint": "https://work.host:8000",
      "transport": "openai"
    }
  ]
}
```

---

## Transport Resolution

Discovered providers are mapped to transports.

Example:

Ollama
→ Ollama Transport

LM Studio
→ OpenAI Compatible Transport

Future providers should be supported by configuration whenever possible rather than requiring new runtime logic.

---

## Model Discovery

Suis should discover models exposed by providers.

Example:

```text
Ollama
- qwen3-coder:latest
- devstral:latest
- llama3:24b
```

Models become selectable within the UI.

---

## Capability Detection

Capabilities tracked:

* chat
* streaming
* tool_use

Capability information stored under:

```text
models/
```

Example:

```text
models/ollama.json
```

The runtime should adapt based on capabilities.

---

## Workspace Detection

Launching Suis inside a directory automatically creates a workspace context.

Example:

```bash
cd project
suis
```

Workspace root becomes:

```text
project/
```

---

## Workspace Boundary Protection

Agent cannot access files outside workspace root without approval.

Examples:

Allowed:

```text
project/src
project/tests
project/docs
```

Blocked:

```text
~/.ssh
~/.config
~/Documents
```

Requires user approval.

---

## File Tools

Required tools:

### Read File

Read contents of files.

### Write File

Modify existing files.

### Create File

Create new files.

### List Directory

Inspect project structure.

---

## Command Execution

Required capability:

```text
command_execute
```

Permission flow:

* once
* session
* project
* always
* deny

Commands should be visible before execution.

---

## Diff Viewer

All file modifications should generate diffs.

Default behavior:

```text
Show diff
→ User approves
→ Apply change
```

Optional project setting:

```text
auto_apply = true
```

---

## Session Task Tracking

Agent can maintain tasks.

Example:

□ Analyze repository

□ Implement feature

□ Add tests

□ Verify behavior

Tasks are visible to users.

Tasks only exist for the current session.

---

## Project Configuration

Workspace directory:

```text
project/.suis/
```

Contains:

```text
project.json
permissions.json
```

Only files required for MVP should exist.

---

# Deferred Features

The following features are intentionally excluded.

---

## Plugins

Reason:

Large architectural surface area.

Design now.

Implement later.

---

## MCP Integration

Reason:

Not required to validate core experience.

---

## Long-Term Memory

Reason:

Session memory is sufficient for MVP.

---

## Multi-Repository Workspaces

Reason:

Single repository support validates core workflows first.

---

## Remote Providers

Reason:

Local-first positioning should be validated before expanding provider support.

---

## Git Operations

Reason:

Can be implemented after permission system stabilizes.

MVP may detect git repositories.

Agent should not create commits.

Agent should not create branches.

---

## Repository Indexing

Reason:

Adds complexity and startup cost.

Agent should load files on demand.

---

## Autonomous Execution

Reason:

Requires checkpointing, recovery, and advanced context management.

Not needed for MVP.

---

# MVP Technical Priorities

Priority 1

Provider discovery.

Without discovery there is no differentiator.

---

Priority 2

Permission system.

Without permissions there is no trust.

---

Priority 3

Terminal experience.

Without a polished UI the project loses its primary advantage.

---

Priority 4

Agent runtime.

Reliable file editing and command execution.

---

Priority 5

Capability detection.

Allows future provider expansion.

---

# MVP Deliverables

A release is considered MVP-complete when:

✓ Install script exists

✓ Suis launches successfully

✓ Ollama detection works

✓ LM Studio detection works

✓ Models can be selected

✓ Chat works

✓ File editing works

✓ Diffs work

✓ Permissions work

✓ Workspace protection works

✓ Task tracking works

✓ Configuration files are generated automatically

✓ Real projects can be modified successfully

Anything beyond this is version 1 territory.

===
SECTION 3
---
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

===
SECTION 4
---
# REPOSITORY_STRUCTURE.md

## Purpose

This document defines the repository layout, crate boundaries, ownership rules, and module responsibilities for Suis.

The structure should:

* support rapid MVP development
* remain maintainable long-term
* support future plugin systems
* support future provider expansion
* avoid unnecessary crate fragmentation

---

# Guiding Principles

## Clear Ownership

Every subsystem should have a single owner.

Examples:

* provider discovery belongs to the provider layer
* permissions belong to the core layer
* UI belongs to the CLI layer

Avoid shared ownership whenever possible.

---

## Dependency Direction

Dependencies should always flow inward.

Example:

```text
CLI
→ Agent
→ Core
```

Never:

```text
Core
→ CLI
```

---

## Future Plugin Compatibility

The plugin system is not part of MVP.

However:

* transport interfaces
* tool interfaces
* memory interfaces

should be designed to allow future extension.

---

## Transport-Centric Design

Suis is transport-centric rather than provider-centric.

The runtime should primarily care about capabilities rather than vendor identity.

Examples:

* chat support
* streaming support
* tool support
* structured output support

rather than:

* Ollama
* LM Studio
* OpenAI
* Anthropic

Providers, transports, and models are separate concepts.

Examples:

* Ollama is a provider
* LM Studio is a provider
* OpenAI-Compatible is a transport
* qwen3-coder is a model

The runtime should never contain model-specific logic unless absolutely required for protocol compatibility.

Good:

```rust
if model.capabilities.tool_use {
    // enable tools
}
```

Bad:

```rust
if model.name == "qwen3-coder" {
    // special behavior
}
```

---

# Workspace Structure

```text
suis/

├── Cargo.toml
├── Cargo.lock
│
├── crates/
│   ├── suis-cli/
│   ├── suis-agent/
│   ├── suis-core/
│   └── suis-providers/
│
├── docs/
├── scripts/
├── assets/
└── tests/
```

---

# Crate Overview

## suis-cli

Purpose:

Terminal user interface.

Responsibilities:

* application startup
* terminal rendering
* chat UI
* task display
* permission prompts
* diff rendering
* slash commands

Owns:

* ratatui integration
* keyboard handling
* UI state

Does NOT own:

* model communication
* tool execution
* permissions
* agent behavior

---

## suis-agent

Purpose:

Agent orchestration layer.

Responsibilities:

* conversation management
* tool invocation
* task tracking
* reasoning loop
* context assembly

Owns:

* agent runtime
* tool lifecycle
* conversation state

Depends on:

* suis-core
* suis-providers

---

## suis-core

Purpose:

Shared business logic.

Responsibilities:

* configuration
* permissions
* workspace management
* filesystem operations
* project metadata
* shared domain logic

Should contain:

* minimal dependencies
* reusable logic
* persistent state management

This crate becomes the foundation of the system.

---

## suis-providers

Purpose:

Provider discovery and model communication.

Responsibilities:

* provider discovery
* transport selection
* model communication
* capability detection
* model metadata management

The crate should remain lightweight and avoid provider-specific runtime behavior wherever possible.

Provider-specific logic should primarily exist in discovery and transport layers.

---

# Dependency Graph

```text
suis-cli
│
├── suis-agent
│
├── suis-providers
│
└── suis-core
```

Agent depends on:

```text
suis-agent
│
├── suis-providers
└── suis-core
```

Provider layer depends on:

```text
suis-providers
│
└── suis-core
```

Core depends on nothing internal.

---

# Documentation Structure

```text
docs/

├── PROJECT_PLAN.md
├── MVP_SCOPE.md
├── REPOSITORY_STRUCTURE.md
│
├── architecture/
├── providers/
├── permissions/
├── agent/
├── ui/
└── plugins/
```

Future design documents should live inside domain-specific folders.

---

# Root Directory Layout

```text
suis/

├── crates/
├── docs/
├── assets/
├── scripts/
└── tests/
```

---

## assets/

Contains:

```text
assets/

├── logos/
├── themes/
└── examples/
```

---

## scripts/

Contains:

```text
scripts/

├── install.sh
├── release.sh
└── dev.sh
```

Install experience is a first-class concern.

Scripts should remain visible and easy to audit.

---

## tests/

Contains:

```text
tests/

├── integration/
├── fixtures/
└── snapshots/
```

Cross-crate testing belongs here.

---

# Internal Module Layout

## suis-core

```text
suis-core/

src/

├── config/
├── workspace/
├── permissions/
├── filesystem/
├── projects/
└── errors/
```

Responsibilities:

* configuration loading
* workspace management
* permission persistence
* filesystem safety
* shared domain types

---

## suis-agent

```text
suis-agent/

src/

├── runtime/
├── conversation/
├── tools/
├── tasks/
├── context/
└── prompts/
```

Responsibilities:

* agent execution
* task management
* context assembly
* tool orchestration

---

## suis-providers

```text
suis-providers/

src/

├── discovery/
│   ├── ollama.rs
│   ├── lmstudio.rs
│   └── llamacpp.rs
│
├── transport/
│   ├── openai.rs
│   └── ollama.rs
│
├── provider.rs
├── model.rs
├── capability.rs
└── lib.rs
```

---

## discovery/

Responsible for locating providers.

Examples:

* detect Ollama
* detect LM Studio
* detect llama.cpp

Discovery should answer:

* Is the provider running?
* Where is it running?
* What models are available?

Discovery should not own inference logic.

Discovery should return provider information that can be persisted to configuration.

---

## transport/

Responsible for model communication.

Examples:

* chat completions
* streaming responses
* tool execution
* structured output

Transports should be reusable across providers whenever possible.

Examples:

```text
LM Studio
→ OpenAI-Compatible Transport

Future Provider
→ OpenAI-Compatible Transport
```

without requiring new runtime logic.

---

## provider.rs

Defines provider-related types.

Example responsibilities:

* provider configuration
* endpoint information
* transport assignment

Example:

```rust
pub struct Provider {
    pub id: String,
    pub endpoint: String,
    pub transport: TransportType,
}
```

Provider data should be loaded from:

```text
providers.json
```

---

## model.rs

Defines model-related types.

Example responsibilities:

* model metadata
* model selection state
* runtime model information

Example:

```rust
pub struct Model {
    pub provider_id: String,
    pub model_id: String,
    pub capabilities: Capabilities,
}
```

Model data should be loaded from:

```text
models/<provider>.json
```

The runtime should not contain model-specific logic.

---

## capability.rs

Defines model capabilities.

Examples:

* tool use
* streaming
* thinking
* structured output

Example:

```rust
pub struct Capabilities {
    pub tool_use: bool,
    pub streaming: bool,
    pub thinking: bool,
}
```

Capabilities should drive runtime behavior.

---

## suis-cli

```text
suis-cli/

src/

├── app/
├── screens/
├── widgets/
├── prompts/
├── commands/
└── state/
```

Responsibilities:

* rendering
* keyboard input
* slash commands
* user interaction

The CLI should not contain business logic.

---

# Configuration Layout

Global configuration:

```text
~/.config/suis/
```

Structure:

```text
~/.config/suis/

providers.json

models/
├── ollama.json
├── lmstudio.json
└── llamacpp.json

settings.json

plugins/
```

---

## providers.json

Stores:

* provider definitions
* endpoints
* transport assignment
* enabled state

Example:

```json
{
  "providers": [
    {
      "id": "ollama",
      "endpoint": "http://localhost:11434",
      "transport": "ollama"
    },
    {
      "id": "lmstudio",
      "endpoint": "http://localhost:1234",
      "transport": "openai"
    }
  ]
}
```

---

## models/

Stores:

* discovered models
* cached capabilities
* model metadata

Example:

```text
models/

ollama.json
lmstudio.json
```

These files should not contain endpoint configuration.

The files are data storage only and must not imply provider-specific runtime logic.

---

# Workspace Layout

Project-local directory:

```text
project/.suis/
```

Structure:

```text
.suis/

project.json
permissions.json
plugins.json
```

Future additions:

```text
memory.json
tasks.json
```

These should not be added until required.

---

# Ownership Rules

## CLI Owns Presentation

If it renders to the terminal:

`suis-cli` owns it.

---

## Agent Owns Decisions

If it determines what happens next:

`suis-agent` owns it.

---

## Core Owns State

If it persists configuration or permissions:

`suis-core` owns it.

---

## Providers Own Connectivity

If it discovers providers or communicates with models:

`suis-providers` owns it.

---

# Deferred Crates

These should NOT exist during MVP.

Potential future crates:

```text
suis-plugins
suis-memory
suis-mcp
```

Create them only when real requirements emerge.

---

# Anti-Goals

Avoid:

* micro-crates
* circular dependencies
* provider-specific logic in CLI
* permission logic in UI
* plugin-specific code in core
* model-specific runtime logic
* unnecessary abstraction layers

Architecture should remain boring, predictable, and easy to navigate.

===
SECTION 5
---
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

## Enforcement Boundary

The hidden-file guard binds the model's **file tools** (`read`, `search`,
`edit`): these consult the project's hidden patterns and refuse hidden paths
outright.

The `bash` tool is different. A shell command is opaque, so `bash` is bound by
**per-command approval**, not by the filesystem guard — the same trade-off
Claude Code makes (a sandbox is out of scope). As a best-effort safeguard, if a
command's tokens reference a hidden path (e.g. `cat .env` with `.env` hidden),
Suis re-prompts even when the command pattern was previously granted; a stored
grant never silences this. The heuristic is token-based and bypassable by
construction (e.g. `cat $(echo .env)`); its job is catching the model's honest
attempts, not sandboxing the shell. Treat `bash` grants accordingly.

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

===
SECTION 6
---
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

* temporary runtime tasks
* session state

Agent mode may not modify plans.

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

===
SECTION 7
---
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

===
SECTION 8
---
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

```text
src/config.rs:12
src/main.rs:48
src/lib.rs:102
```

Search results should be concise.

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


