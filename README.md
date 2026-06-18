# Suis

**A local-first AI coding agent for your terminal.**

Suis makes developer workflows simple, safe, and powerful. Rather than relying
on massive context windows or complex autonomous systems, Suis focuses on
structured execution, explicit permissions, and intelligent tooling to help
local models perform at their best — all while preserving a polished terminal
experience inspired by tools like OpenCode and Claude Code.

```bash
cd your-project
suis
```

That's it. Suis discovers your running local providers, detects what each model
can actually do, and drops you into an interactive session — no manual
configuration required.

---

## Why Suis?

**Local first.** Ollama, LM Studio, and llama.cpp are first-class citizens, not
afterthoughts. Discovery is automatic, capability detection is cached, and
every design decision asks: *does this still work well for a local model?*

**You own the workspace.** The agent never silently exceeds its permissions.
Every command, every out-of-bounds file access, every git write goes through an
explicit, scoped approval — and a permission means exactly what its label says.

**Structure over context.** Small models succeed through focused context, not
repository dumps. Suis assembles lean, relevant context per turn, caps tool
output so one verbose build can't poison a 16k context window, and exposes a
deliberately small set of powerful tools.

**Boring architecture wins.** Four crates, one dependency direction, readable
JSON configuration. A contributor should understand the repository in a single
sitting.

---

## Features

- **Interactive terminal UI** — streaming chat, model selection, task panel,
  plan picker, diff viewer, usage popup, and permission prompts, built with
  ratatui.
- **Three runtime modes** — **Plan**, **Agent**, and **Chat**, cycled with
  `Shift+Tab`. Each mode is a structural filter on which tools may run, applied
  *before* any permission gate (read-only modes can't write, no matter what's
  been approved).
- **Automatic provider discovery** — probes Ollama, LM Studio, and llama.cpp on
  their default ports (plus any custom endpoints you configure), with bounded
  probe timeouts so a hung port never stalls startup. Remote providers
  (Anthropic) are added by configuration.
- **Capability detection** — models are checked for chat, streaming, and tool
  use; results are cached and reused. The runtime adapts to what a model can
  do instead of assuming.
- **Eight focused tools** — `read`, `search`, `tree`, `edit`, `bash`, `git`,
  `task`, `plan`. Few tools, predictable behavior, better local-model
  performance.
- **Plans as project artifacts** — Plan mode decomposes work into steps and
  tasks, persisted in `.suis/plans.json`; `/implement` executes a plan (or a
  single step) in a fresh, focused session.
- **Scoped permissions** — approve actions once, for the session, for the
  project, or always; deny with the same precision. Ephemeral grants never
  touch disk.
- **Workspace safety** — boundary enforcement, hidden and hardened files,
  trash-based deletion with restore, and diff tracking for every edit.
- **Context-aware sessions** — per-model context budgets, history pruning,
  `/compact` to summarize a long conversation, and a `/usage` popup that tracks
  token spend per provider.
- **Hardened execution** — commands time out (default 120s) with full
  process-tree cleanup, run off the UI thread, and return bounded output with
  honest truncation markers.

## Quick Start

### Requirements

- A Rust toolchain ([rustup.rs](https://rustup.rs)) — only needed to build from
  source; prebuilt binaries are attached to each release.
- At least one provider available: a local server such as
  [Ollama](https://ollama.com), [LM Studio](https://lmstudio.ai), or
  [llama.cpp](https://github.com/ggml-org/llama.cpp) (`llama-server`), or a
  remote provider (Anthropic) configured with an API key.

### Install

Grab a prebuilt binary for your platform from the
[releases page](https://github.com/suis/suis/releases) (Linux x86-64, macOS
arm64, macOS x86-64), or build from source:

```bash
git clone <this-repository>
cd suis
./scripts/install.sh
```

This builds the release binary and installs it to `~/.local/bin/suis`
(override with `SUIS_INSTALL_DIR`).

### First run

```bash
cd your-project
suis
```

On first launch in a project, Suis walks you through a short setup:

1. Initialize Suis for this project?
2. If a `.gitignore` exists — import its entries as hidden/hardened files?
   (lock files become *hardened*, secrets become *hidden*)
3. Allow the agent to use git?

Then pick a discovered model and start working.

### Modes

Suis sessions run in one of three modes, cycled with `Shift+Tab` (or set
directly with `/plan`, `/agent`, `/chat`). The mode decides which tools the
model may reach *before* permissions are even consulted:

| Mode | Tools allowed | For |
|---|---|---|
| **Plan** | `read`, `search`, `tree`, `task`, `plan` | Analyzing the codebase and drafting a plan. The only mode that can write a plan. |
| **Agent** | everything except `plan` | Executing work — edits, commands, git. The default. |
| **Chat** | `read`, `search`, `tree`, `task` | Read-only conversation; the model can look but never modify. |

Plan and Chat are read-only by construction: no stored permission can let them
edit, run commands, or touch git. Plans drafted in Plan mode are run later with
`/implement`.

### Slash commands

| Command | Description |
|---|---|
| `/model` | Open the model selection screen |
| `/plan`, `/agent`, `/chat` | Switch the session mode directly |
| `/plans` | List stored plans and their progress |
| `/implement` | Start a focused implementation session for a plan (or one step) |
| `/tasks` | Toggle the task panel |
| `/compact` | Summarize the conversation and replace the history with it |
| `/usage` | Toggle the per-provider token-usage popup |
| `/permissions` | Show current project permissions |
| `/providers` | Enable or disable providers |
| `/clear` | Clear conversation history |
| `/help` | Show available commands |

## Providers

| Provider | Default endpoint | Transport |
|---|---|---|
| Ollama | `http://localhost:11434` | Native (`/api/tags`, `/api/chat`) |
| LM Studio | `http://localhost:1234` | OpenAI-compatible (`/v1`) |
| llama.cpp | `http://localhost:8080` | OpenAI-compatible (`/v1`) |
| Anthropic | `https://api.anthropic.com` | Anthropic Messages API (`/v1`, API key) |

Suis is **transport-centric**: providers are discovered endpoints, transports
are protocols, and the runtime reasons about *capabilities* rather than vendor
identity. A provider on a custom port is a one-line entry in
`~/.config/suis/providers.json`:

```json
{
  "providers": [
    { "id": "llamacpp", "endpoint": "http://192.168.1.10:8080", "transport": "openai", "enabled": true }
  ]
}
```

Any OpenAI-compatible server can be added the same way — no code changes
required.

### Remote providers and API keys

Remote providers are just configuration. An entry can name an environment
variable to read the key from (preferred) or, as a fallback, carry a literal
key — which is redacted from logs and lives only in the owner-readable
`providers.json`:

```json
{
  "providers": [
    { "id": "anthropic", "endpoint": "https://api.anthropic.com", "transport": "anthropic", "enabled": true, "api_key_env": "ANTHROPIC_API_KEY" }
  ]
}
```

Local-first is still the default: remote providers stay disabled until you add
one, and the rest of the runtime — modes, tools, permissions — treats them
identically.

## The Permission Model

Permissions are the heart of Suis. The agent is strong *because* its
boundaries are explicit.

### Scopes

When the agent wants to run a command, you choose:

```text
Allow `cargo test`?

[1] Once   [2] Session   [3] Project   [4] Always   [Enter] Deny session
```

| Scope | Lifetime | Stored in |
|---|---|---|
| **Once** | This invocation only | nowhere |
| **Session** | Until Suis exits | memory only — never written to disk |
| **Project** | This project, persistent | `.suis/permissions.json` |
| **Always** | All projects, persistent | `~/.config/suis/permissions.json` |
| **Deny** | Durable policy | project file |

A project-level **deny always wins** over a global grant. Denials are scoped
too: `Enter` denies for the session, `Shift+Enter` denies for the project,
`Esc` denies just this once.

### Wildcards

Holding `Shift` while approving widens the grant from `cargo test` to
`cargo *` — friction reduction stays a deliberate, visible choice.

### Dangerous commands

`rm`, `sudo`, `chmod`, `chown`, `dd`, `mkfs`, `shutdown`, `reboot`, and
friends never qualify for persistent approval. No stored grant — not even
`Always` — silences the prompt; the only options are *once* or *deny*.

### Git is privileged

Git access is granted per project (disabled / read-only / read-write) during
setup. Read-only mode permits `status`, `log`, `diff`, `show`, and other
inspection subcommands; anything that mutates the repository requires write
access.

### Fail-closed by design

Every registered tool passes an explicit permission gate. A tool the executor
doesn't recognize prompts the user instead of running — the safe default for
anything added in the future.

## Workspace Safety

- **Boundary enforcement** — the workspace root is the agent's world. Any path
  outside it requires explicit approval.
- **Hidden files** — patterns like `.env` are invisible to the model's file
  tools: not readable, not searchable, contents never revealed. If a granted
  bash command references a hidden path, Suis re-prompts anyway. (The bash
  heuristic is token-based and best-effort — it catches the model's honest
  attempts; it is not a sandbox. The enforcement boundary is documented in
  detail in [MASTER.md](MASTER.md).)
- **Hardened files** — visible but write-protected: editing `Cargo.lock` or a
  CI workflow always asks first.
- **Trash, not deletion** — deleted files move to `.suis/trash/` and can be
  restored. Permanent removal only happens through explicit purge.
- **Diffs everywhere** — every edit produces a unified diff you can review;
  `auto_apply` is available per project when you trust the flow.
- **Bounded execution** — commands time out after 120s and the whole process
  group is reaped (no orphaned children). Bash output is capped at 16 KiB
  (keeping the tail, where failures live), file reads at 64 KiB (keeping the
  head, with a marker telling the model to search instead). Truncation never
  splits a UTF-8 codepoint.

## Configuration

Everything is plain, readable JSON — inspectable and hand-editable.

```text
~/.config/suis/              # global: defines what exists
├── providers.json           #   provider endpoints, transports, enabled state,
│                            #   API-key sources for remote providers
├── permissions.json         #   "Always" grants (user-wide)
├── settings.json            #   global preferences: default provider, theme,
│                            #   auto_apply, context budgets & per-model windows
└── models/                  #   cached capability detection per provider
    ├── ollama.json
    └── ...

project/.suis/               # per-project: defines what is visible
├── project.json             #   tool/provider/model scope, hidden & hardened
│                            #   patterns, git access, auto_apply
├── permissions.json         #   project grants and denials
├── plans.json               #   persistent plans, steps, and task progress
└── trash/                   #   recoverable deleted files
```

The ownership model is simple: **global config defines what exists, project
config defines what is visible, runtime permissions define what may happen
right now.**

## Architecture

```text
suis-cli        terminal UI — rendering, input, slash commands
  │
suis-agent      agent runtime — reasoning loop, modes, tools, tasks, plans, context assembly
  │
suis-providers  discovery, capability detection, transports
  │
suis-core       workspace, permissions, filesystem safety, configuration
```

Dependencies flow strictly inward. The CLI owns presentation, the agent owns
decisions, core owns state, providers own connectivity. Cross-crate
integration tests live in [`tests/integration/`](tests/integration).

Design documents live in [`docs/`](docs) (or as one combined file in
[MASTER.md](MASTER.md)): the project plan, design principles, the permission
system, the tool system, and the agent runtime.

## Development

```bash
cargo build --workspace          # build everything
cargo test --workspace           # unit + integration tests
cargo clippy --workspace --all-targets
./scripts/dev.sh                 # development loop helper
```

Tests are hermetic by default — discovery and permission tests run against
local mocks and temp directories, never your real config. The opt-in
end-to-end discovery test runs with `SUIS_TEST_OLLAMA=1` when Ollama is up.

## Roadmap

Suis follows a deliberate MVP-first path. Two earlier roadmap items have since
landed: **plans & steps** (Plan mode plus `/implement`) and **remote providers**
(Anthropic, added by configuration). Planned next, in keeping with the
[design principles](docs/DESIGN_PRINCIPLES.md):

- **More remote transports** — OpenAI / OpenRouter alongside Anthropic, still as
  configuration rather than new architecture.
- **Plugins** — tools, prompts, workflows, and MCP integrations as a stable
  extension surface, inheriting the same permission gates.
- **Project memory** — conventions and decisions that persist across sessions
  without bloating context.

Explicit non-goals: autonomous multi-hour execution, repository indexing,
cloud-first workflows, and a bash sandbox.

## License

MIT
