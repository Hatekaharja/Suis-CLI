# Suggestions

This document captures immediate improvement suggestions from a high-level project review.

Overall, Suis appears to be a strong and coherent project: the positioning is clear, the crate split largely matches the documented architecture, and the codebase appears to have good test coverage around safety-sensitive areas like tools, providers, permissions, and agent behavior.

The highest-value improvements right now are mostly around project polish, documentation freshness, and contributor confidence.

## Priority Suggestions

### 1. Add CI

If CI does not already exist, add a basic workflow that runs the standard Rust checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

This is likely the single highest-value operational improvement. It helps preserve quality as the project grows and gives contributors a clear baseline for pull requests.

### 2. Update docs to reflect plans being implemented

The README roadmap currently presents persistent plans as upcoming work, but the codebase already includes plan-related implementation, including:

- `crates/suis-agent/src/tools/plan.rs`
- plan mode support
- `.suis/plans.json` behavior
- `/implement` references
- plan-backed task persistence

Update the README and related architecture/design docs so users do not think planning is still future-only.

### 3. Clarify the tool count story

The design docs emphasize the six MVP tools:

```text
read
search
edit
bash
git
task
```

The implementation also has a seventh, special-purpose `plan` tool that is exposed only in Plan mode. This is reasonable, but the docs should make the distinction explicit:

> The base agent has six general-purpose tools. Plan mode additionally exposes a special `plan` proposal tool.

This keeps the tool-minimization principle intact while avoiding confusion.

### 4. Add a contributor quick path

Add a small `CONTRIBUTING.md` or a contributor checklist section to the README.

Suggested checklist:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

Also mention key architectural rules, such as:

- dependencies flow inward according to the crate architecture;
- behavior should be capability-driven, not provider-driven;
- safety and explicit user permission are core design constraints;
- new tools should not be added without strong justification.

### 5. Add screenshots or GIFs

Because Suis is a terminal UI project, the README would benefit from visual examples of:

- model selection;
- permission prompts;
- task panel;
- diff viewer;
- plan review;
- provider screen.

This would make the project feel more tangible to new users.

## Additional Suggestions

### 6. Clarify project maturity and known limitations

Add a visible status section near the top of the README.

Example:

```text
Status: early MVP / active development.

Known limitations:
- bash is permission-gated but not sandboxed;
- local model tool-use quality varies;
- remote providers are experimental or planned;
- platform-specific behavior may vary.
```

This helps set expectations for users and contributors.

### 7. Document platform behavior for process cleanup

The bash tool includes Unix process-group cleanup and a fallback child-process kill path elsewhere. This is sensible, but platform behavior should be documented explicitly.

If Windows is supported, document any limitations. If Windows is not supported yet, say so clearly.

### 8. Consider moving historical implementation plans

The `SUIS_PROJECT_PLANS/DONE/` directory appears to contain useful historical implementation notes, but it may distract new contributors.

Possible options:

- keep it where it is, but mention it in the README;
- move it under `docs/archive/`;
- exclude it from release/package artifacts if the project is later packaged or published.

### 9. Add a module-level architecture document

The top-level architecture is clear, but contributors would benefit from a compact `docs/ARCHITECTURE.md` describing module-level responsibilities.

Example:

```text
suis-agent
├── runtime: session loop, events, modes
├── tools: definitions, executor, permission gates
├── context: prompt and context assembly
├── tasks: in-session and plan-backed task views
```

This would make onboarding easier without adding much maintenance burden.

### 10. Watch for docs/code drift

The project has strong documentation, which is a major asset, but the amount of documentation increases the risk of drift.

The planning feature is a current example: parts of the docs still describe plans as future work even though plan-related implementation exists.

Periodically audit at least:

- `README.md`
- `docs/TOOLS.md`
- `docs/DESIGN_PRINCIPLES.md`
- crate-level documentation comments

## Short-Term Action List

Recommended immediate order:

1. Add CI.
2. Update README/docs around plans and the Plan-mode-only `plan` tool.
3. Add `CONTRIBUTING.md` or a contributor checklist.
4. Add screenshots or GIFs.
5. Clarify project maturity and platform support.
