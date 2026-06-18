#!/usr/bin/env bash
# install-suis.sh
# Installs the latest suis release binary.

set -euo pipefail

INSTALL_DIR="${SUIS_INSTALL_DIR:-${HOME}/.local/bin}"
BIN_NAME="suis"

echo "==> Installing ${BIN_NAME}"

# Ensure curl exists.
if ! command -v curl >/dev/null 2>&1; then
    echo "error: curl is required but was not found on PATH." >&2
    exit 1
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
    Darwin-arm64|Darwin-aarch64)
        ASSET="suis-macos-arm64"
        ;;
    Darwin-x86_64)
        ASSET="suis-macos-amd64"
        ;;
    Linux-x86_64)
        ASSET="suis-linux-amd64"
        ;;
    *)
        echo "error: unsupported platform: ${OS} ${ARCH}" >&2
        exit 1
        ;;
esac

DOWNLOAD_URL="https://github.com/Hatekaharja/Suis-CLI/releases/latest/download/${ASSET}"

mkdir -p "${INSTALL_DIR}"

TMP_FILE="$(mktemp)"

echo "==> Downloading ${ASSET}"
curl -fsSL "${DOWNLOAD_URL}" -o "${TMP_FILE}"

chmod 0755 "${TMP_FILE}"
mv "${TMP_FILE}" "${INSTALL_DIR}/${BIN_NAME}"

echo "==> Installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*)
        echo "==> ${INSTALL_DIR} is on your PATH."
        echo "==> Run '${BIN_NAME}' to start."
        ;;
    *)
        echo
        echo "note: ${INSTALL_DIR} is not on your PATH."
        echo "Add the following to your shell profile:"
        echo
        echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
        echo
        ;;
esac
