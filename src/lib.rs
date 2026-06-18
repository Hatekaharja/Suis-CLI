//! The `suis` workspace root package.
//!
//! This crate intentionally contains no logic. It exists only to host the
//! cross-crate integration tests under `tests/integration/`, which exercise
//! `suis-core`, `suis-providers`, and `suis-agent` together. The shipping code
//! lives in the workspace member crates; the user-facing binary is `suis-cli`.
