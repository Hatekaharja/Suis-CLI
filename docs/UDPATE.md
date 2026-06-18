# Architecture Corrections

## Workflow

Step
├── Work Tasks
└── Verify Tasks

## Planning Authority

Planning creates structure.

Implementation follows structure.

Implementation may not modify plans.

## File Editing

Changes are applied immediately.

Diffs are tracked for:
- Undo
- Restore
- Revert

## Progress Checkpoints

Checkpoints exist to prevent runaway execution loops.

They are not a status reporting mechanism.

## Runtime Tasks

Plan Mode:
Persistent Plan Tasks

Agent Mode:
Temporary Runtime Tasks
