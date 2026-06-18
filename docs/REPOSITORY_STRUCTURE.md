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

