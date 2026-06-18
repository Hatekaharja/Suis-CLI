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

