#!/usr/bin/env bash
# install.sh — build the suis binary and install it to ~/.local/bin/suis.
set -euo pipefail

# Resolve the repository root (this script lives in scripts/).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

INSTALL_DIR="${SUIS_INSTALL_DIR:-${HOME}/.local/bin}"
BIN_NAME="suis"

echo "==> Installing suis"

# 1. Require a Rust toolchain.
if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo (the Rust toolchain) was not found on your PATH." >&2
    echo "       Install Rust from https://rustup.rs and re-run this script." >&2
    exit 1
fi

# 2. Build the release binary.
echo "==> Building suis-cli (release)…"
cargo build --release -p suis-cli --manifest-path "${REPO_ROOT}/Cargo.toml"

SRC_BIN="${REPO_ROOT}/target/release/${BIN_NAME}"
if [[ ! -x "${SRC_BIN}" ]]; then
    echo "error: expected binary not found at ${SRC_BIN}" >&2
    exit 1
fi

# 3. Install it.
mkdir -p "${INSTALL_DIR}"
install -m 0755 "${SRC_BIN}" "${INSTALL_DIR}/${BIN_NAME}"
echo "==> Installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"

# 4. PATH reminder.
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*)
        echo "==> ${INSTALL_DIR} is on your PATH. Run 'suis' to start."
        ;;
    *)
        echo "note: ${INSTALL_DIR} is not on your PATH."
        echo "      Add this to your shell profile:"
        echo "          export PATH=\"${INSTALL_DIR}:\$PATH\""
        ;;
esac
