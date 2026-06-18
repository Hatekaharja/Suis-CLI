//! Workspace detection and boundary enforcement.
//!
//! A [`Workspace`] is rooted at a canonicalized directory. All filesystem
//! access in Suis is checked against this root so the agent cannot read or
//! write outside the project, even via `..` traversal or symlinks.

mod boundary;
mod detect;

use std::path::PathBuf;

/// A detected project workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Canonicalized workspace root.
    pub root: PathBuf,
    /// The `.suis/` directory under the root.
    pub suis_dir: PathBuf,
    /// Whether the root contains a `.git` directory.
    pub is_git: bool,
}
