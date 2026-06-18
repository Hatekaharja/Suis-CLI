#!/usr/bin/env bash
# release.sh — build a stripped release binary and package it as a tarball
# under dist/ for distribution.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo (the Rust toolchain) was not found on your PATH." >&2
    exit 1
fi

BIN_NAME="suis"
VERSION="$(cargo pkgid -p suis-cli | sed 's/.*[@#]//')"
TARGET_TRIPLE="$(rustc -vV | sed -n 's/host: //p')"
DIST_DIR="${REPO_ROOT}/dist"

echo "==> Building ${BIN_NAME} ${VERSION} for ${TARGET_TRIPLE} (release)…"
cargo build --release -p suis-cli

SRC_BIN="${REPO_ROOT}/target/release/${BIN_NAME}"
if [[ ! -x "${SRC_BIN}" ]]; then
    echo "error: expected binary not found at ${SRC_BIN}" >&2
    exit 1
fi

# Strip symbols if the tool is available (smaller artifact, non-fatal if not).
if command -v strip >/dev/null 2>&1; then
    strip "${SRC_BIN}" || true
fi

mkdir -p "${DIST_DIR}"
ARCHIVE="${DIST_DIR}/${BIN_NAME}-${VERSION}-${TARGET_TRIPLE}.tar.gz"
tar -czf "${ARCHIVE}" -C "${REPO_ROOT}/target/release" "${BIN_NAME}"
echo "==> Wrote ${ARCHIVE}"
