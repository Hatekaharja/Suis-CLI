# Next Big Improvement: Agent Flow & Capabilities Roadmap

## Context

Suis is a mature, cleanly-architected local-first coding agent (~32k LOC, 4 crates,
strong test coverage, no critical bugs). The single weakest area relative to its
ambition is the **agent runtime itself**: the loop is well-engineered for *plumbing*
(streaming, interrupts, `<think>` splitting, text-tool-call recovery, mechanical
pruning, the `/implement` task-by-task driver), but the agent is **capability-thin**:

1. **Cold start every session.** The system prompt is fully static and project-blind
   ([system_prompt.rs:7](../crates/suis-agent/src/context/system_prompt.rs), marked "MVP:
   static"). The model knows nothing about the project's language, build/test commands,
   or layout and must burn turns re-running `tree`/`search` to rediscover them.
2. **No self-verification in Agent mode.** The disciplined work→verify gate exists only
   inside `/implement` sessions ([agent.rs:540](../crates/suis-agent/src/runtime/agent.rs)).
   A plain Agent turn loops until the model *stops calling tools* — nothing makes it
   confirm the code compiles or tests pass, so it can declare success on broken code.
3. **No delegation.** One flat loop with a 24-iteration ceiling
   ([agent.rs:34](../crates/suis-agent/src/runtime/agent.rs)); no way to hand a
   self-contained subtask to a fresh lean context and fold back only a summary.
4. **Strictly sequential tools.** The prompt forces one tool call per round-trip
   ([system_prompt.rs:25](../crates/suis-agent/src/context/system_prompt.rs)); every
   read/search is a separate model call.

This plan delivers **all four** as a sequenced roadmap. Order matters: Phase 1
establishes the project profile (incl. the check command) that Phase 2 verifies
against; Phases 1–2 produce the lean-context machinery Phase 4 reuses. Phase 3 is an
independent flow win slotted where it best accelerates the verify→fix cycle.

Guiding constraint, inherited from the project's design principles: **structure over
context, local-first, user owns the workspace.** Every new capability must work on a
small local model and pass through the existing mode + permission gates unchanged.

---

## Phase 1 — Warm-start project awareness (foundation)

**Goal:** Replace the static system prompt with a project-aware one, so every session
opens already knowing what the project is and how to build/test it.

**Approach**
- Add an optional, cached **project profile** to `ProjectConfig`
  ([crates/suis-core/src/projects/config.rs](../crates/suis-core/src/projects/config.rs)).
  The struct already uses `#[serde(default)]`, so new optional fields are
  backward-compatible with existing `.suis/project.json` files:
  - `verify_command: Option<String>` — the project's check (e.g. `cargo test`,
    `npm test`). Drives Phase 2.
  - `profile: Option<ProjectProfile>` — `{ summary, toolchain, build_cmd, test_cmd,
    conventions: Vec<String>, generated_at }`. A compact, cached brief.
- **Detection helper** (new `crates/suis-agent/src/context/profile.rs`): a deterministic,
  offline first pass that infers toolchain + likely build/test commands from manifest
  files (`Cargo.toml` → cargo, `package.json` scripts → npm/pnpm, `pyproject.toml`,
  `go.mod`, `Makefile`). Reuse the directory walk + hidden-pattern filtering already in
  [work_package.rs `snapshot`/`list_dir`](../crates/suis-agent/src/context/work_package.rs)
  rather than re-rolling tree traversal.
- **Inject into the prompt.** In
  [assembler.rs `build`](../crates/suis-agent/src/context/assembler.rs) (the
  `format!("{SYSTEM_PROMPT}\n\n{}", mode_prompt(mode))` site), append a rendered
  profile block when present. Keep it terse (it is pinned and counts against budget):
  a `render_profile()` in `system_prompt.rs` mirroring the style of
  `work_package::render_ledger`. The two-level layout snapshot is included once here so
  ordinary sessions stop cold-starting on `tree`.
- **First-run + refresh UX.** Populate `verify_command`/`profile` during the existing
  project-init flow ([crates/suis-cli/src/screens/project_init.rs](../crates/suis-cli/src/screens/project_init.rs))
  using the detection helper's guess (user can edit/confirm). Add a `/profile` slash
  command ([commands/parser.rs](../crates/suis-cli/src/commands/parser.rs) +
  `handlers.rs`) to view/regenerate it.

**Reuse:** `work_package::snapshot`/`list_dir`, `guard::is_hidden`, `ProjectConfig`
serde-default load/save, `budget::estimate_tokens` to keep the block within a small cap.

**Test:** profile detection per manifest type (unit, hermetic via `Fixture`); assembler
test asserting the profile block appears after the system prompt and is dropped from the
*prunable* region only as a last resort (it is part of the pinned prefix). Confirm an
empty profile reproduces today's byte-identical prompt (no regression).

---

## Phase 2 — Self-verification loop in Agent mode

**Goal:** After the agent makes edits in Agent mode, automatically run the project's
`verify_command`, feed the result back, and let the model self-correct before the turn
settles — generalizing `/implement`'s work→verify discipline to every turn.

**Approach**
- In [agent.rs `run_turn_step`](../crates/suis-agent/src/runtime/agent.rs), when the model
  settles (`tool_calls.is_empty()` at the `return TurnOutcome::Completed` site) **and**
  this turn edited files **and** `project.verify_command` is set **and** mode is Agent:
  run a single verification step instead of returning immediately.
  - Detect "edited files this turn" by reusing the existing `touched_paths` logic
    ([agent.rs:721](../crates/suis-agent/src/runtime/agent.rs)) over the turn's messages.
  - Run the command through the **existing permission path**: synthesize a `bash` tool
    call for `verify_command` and route it through `ToolExecutor`
    ([executor.rs](../crates/suis-agent/src/tools/executor.rs)) so it inherits the command
    gate, dangerous-command rules, and timeout. No new execution path, no bypass.
  - On non-zero exit / failing output: push the captured result as a `Tool`/`System`
    message ("verification failed: …; fix and continue") and **loop** (counts against
    `MAX_ITERATIONS`). On success: emit a `VerifyPassed` marker and settle.
- **Guardrails:** verify at most once per *settle* (not per tool call) and cap
  verify→fix rounds (e.g. a small `MAX_VERIFY_ROUNDS`) so a perpetually-failing build
  can't burn the whole iteration budget; after the cap, settle and report honestly.
- **Opt-out:** no `verify_command` ⇒ behavior is exactly today's (zero-risk default).
  A project `auto_apply: false` flow still shows diffs first — verification runs after
  edits are applied, unchanged.
- **Events:** add `VerifyStarted { command }` and `VerifyResult { passed, summary }` to
  [AgentEvent](../crates/suis-agent/src/runtime/events.rs); render a thin status line in
  the CLI (mirror the `TaskCompacted` marker treatment).

**Reuse:** `touched_paths`, `ToolExecutor::execute`, the `bash` tool + its output
capping/timeout, `PermissionStore` gating.

**Test:** mock transport scripts an edit then settles; assert a verify `bash` call is
issued, a failing result loops the model, a passing result settles with `VerifyPassed`;
assert no verify runs when `verify_command` is unset, when no edits occurred, or in
Plan/Chat mode; assert the round cap halts a always-failing verify.

---

## Phase 3 — Parallel read-only tool batching + schema-validated tool errors

**Goal:** Cut round-trips and wasted iterations. The loop *already* iterates over a
`Vec<ToolCall>` ([agent.rs:343](../crates/suis-agent/src/runtime/agent.rs)) — the only
thing forcing serialization is the prompt instruction.

**Approach**
- **Allow batched read-only calls.** Update [system_prompt.rs](../crates/suis-agent/src/context/system_prompt.rs)
  to permit emitting several *read-only* tool calls (`read`/`search`/`tree`) in one
  response while keeping write/execute calls one-at-a-time. Execute the read-only subset
  of a batch concurrently (`futures::future::join_all` over `ToolExecutor::execute`,
  which already runs bodies on `spawn_blocking`); execute any write/execute calls in the
  batch sequentially after, preserving ordering guarantees and the interrupt-skip
  semantics already implemented at [agent.rs:343-396](../crates/suis-agent/src/runtime/agent.rs).
- **Schema-validated, retry-friendly errors.** Before dispatch, validate the model's
  arguments against the tool's JSON-Schema `definition().parameters` and, on mismatch,
  return a corrective message in the same spirit as the existing
  `missing_arg_message`/`unknown_tool_message` helpers
  ([tools/mod.rs:95](../crates/suis-agent/src/tools/mod.rs),
  [executor.rs:421](../crates/suis-agent/src/tools/executor.rs)) — naming the offending
  field and expected type so a weak model self-corrects on the next turn instead of
  consuming an iteration on a hard failure.

**Reuse:** existing concurrency primitive (`spawn_blocking` per tool), the corrective-
message pattern, `ToolDefinition.parameters`.

**Test:** a response with two `read` calls + one `edit` executes the reads concurrently
then the edit; interrupt mid-batch still skips not-yet-started calls with synthetic
results; a malformed-arg call yields a corrective error result (not a thrown error) and
the turn continues.

---

## Phase 4 — Sub-agent delegation

**Goal:** Let the agent delegate a self-contained subtask to a fresh, lean sub-context
and fold back only a dense summary — the biggest leverage for larger tasks on small
context windows. This generalizes the `/implement` ledger machinery that already exists.

**Approach**
- **New `delegate` tool** (`crates/suis-agent/src/tools/delegate.rs`, registered in
  [tools/mod.rs `default_tools`](../crates/suis-agent/src/tools/mod.rs)), available in Agent
  mode only (extend [Mode::allows_tool](../crates/suis-agent/src/runtime/mode.rs)). Args:
  `{ objective, context_hint? }`.
- Because tool bodies are synchronous, the executor cannot itself drive a model loop.
  Handle `delegate` like `plan` is handled — as an **executor-resolved gate** that hands
  control back to the `Agent`: emit a new `AgentEvent`/internal signal, and have the
  `Agent` run a **nested turn** with a fresh `Session`-like sub-context seeded by the
  Phase-1 project profile + the parent's relevant ledger, bounded by its own (smaller)
  iteration ceiling. Reuse the lean-seed pattern from
  [`seed_implement_context`](../crates/suis-agent/src/runtime/agent.rs) and the silent
  [`summarize`](../crates/suis-agent/src/runtime/agent.rs) helper to produce the handoff
  note returned to the parent as the tool result.
- **Safety:** the sub-agent shares the parent's `PermissionStore` and mode (it cannot
  exceed Agent-mode capabilities), inherits the same workspace boundary, and forbids
  re-entrant `delegate` (depth 1) to bound cost. Interrupts propagate via the existing
  `watch` signal.
- **Events:** `SubAgentStarted { objective }` / `SubAgentFinished { summary }` for a
  collapsible UI block, reusing the `TaskCompacted` rendering style.

**Reuse:** `summarize`, `seed_implement_context`/`render_ledger`, the `plan`-style
executor-resolved gate, the interrupt `watch` plumbing, `LedgerEntry`.

**Test:** mock transport drives a parent turn that calls `delegate`; assert a nested
turn runs against a fresh history, the parent receives only the summary (not the
sub-transcript), depth-2 delegation is refused, and an interrupt during the sub-turn
unwinds cleanly.

---

## Sequencing & risk

| Phase | Depends on | Risk | Default-safe when… |
|---|---|---|---|
| 1 Warm-start | — | Low | empty profile ⇒ identical prompt |
| 2 Self-verify | 1 (`verify_command`) | Medium | no `verify_command` ⇒ today's behavior |
| 3 Parallel/errors | — (independent) | Low–Med | reads batch only when model opts in |
| 4 Sub-agents | 1, reuses 2's lean-context | High | tool absent ⇒ no change |

Land them in order; each phase is independently shippable and independently testable.
Phases 1–3 are safe-by-default (no behavior change without new config / model opt-in);
Phase 4 is purely additive (a new tool).

---

## Verification (end-to-end)

- **Per phase:** `cargo test --workspace` and `cargo clippy --workspace --all-targets`
  (the project's stated dev loop, README "Development"). New logic is unit-tested with
  the existing `MockTransport` (agent.rs) and `Fixture`/`TempDir` (test_util) harnesses —
  no network, fully hermetic.
- **Live smoke (`/run` or manual):** in a real Rust/JS project with a local provider
  (Ollama), confirm:
  1. Phase 1 — first turn references the project layout/commands without first calling
     `tree`; `/profile` shows the brief.
  2. Phase 2 — ask for an edit that breaks the build; observe the agent auto-run the
     verify command, see the failure, and fix it before settling.
  3. Phase 3 — a "read these three files" request issues one batched response; a
     deliberately malformed tool call produces a corrective message, not a dead turn.
  4. Phase 4 — a multi-part request triggers `delegate`; the main transcript stays lean
     and shows only the sub-agent summary.
- **Regression:** confirm `/implement`, `/compact`, interrupts, and Plan/Chat read-only
  enforcement are unchanged (existing tests in agent.rs / executor.rs must stay green).
