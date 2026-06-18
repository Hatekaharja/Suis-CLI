#!/usr/bin/env bash
# dev.sh — run suis in watch mode for development (rebuild + relaunch on change).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo (the Rust toolchain) was not found on your PATH." >&2
    echo "       Install Rust from https://rustup.rs and re-run this script." >&2
    exit 1
fi

if ! cargo watch --version >/dev/null 2>&1; then
    echo "error: cargo-watch is not installed." >&2
    echo "       Install it with: cargo install cargo-watch" >&2
    exit 1
fi

# Re-run the CLI whenever a source file changes. Extra args are forwarded.
exec cargo watch -x "run -p suis-cli -- $*"
