#!/bin/bash
# Installs the docket-mcp and docket-cc launchers for this machine. Downloads
# the latest docket-mcp-launcher and docket-cc-launcher release assets and
# places them locally as "docket-mcp"/"docket-cc" - your MCP client config
# and Claude Code hook config point at these files; each launcher checks
# GitHub Releases for its own worker on every run.
set -eu

REPO="iyulab/docket"
INSTALL_DIR="${DOCKET_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux) platform="unknown-linux-gnu" ;;
  Darwin) platform="apple-darwin" ;;
  *) echo "Unsupported OS: $os" >&2; exit 1 ;;
esac

case "$arch" in
  x86_64) platform_arch="x86_64" ;;
  arm64|aarch64) platform_arch="aarch64" ;;
  *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac

if [ "$os" = "Linux" ] && [ "$platform_arch" = "aarch64" ]; then
  echo "Unsupported combination: Linux on aarch64 (no release build exists yet)" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"

for worker in docket-mcp docket-cc; do
  asset="${worker}-launcher-${platform_arch}-${platform}"
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
  echo "Downloading ${asset}..."
  curl -fsSL -o "${INSTALL_DIR}/${worker}" "$url"
  chmod +x "${INSTALL_DIR}/${worker}"
  echo "Installed to ${INSTALL_DIR}/${worker}"
done

echo "Make sure ${INSTALL_DIR} is on your PATH. Point your MCP client's \"command\" at"
echo "\"docket-mcp\" (with DOCKET_CORE_URL set) and any Claude Code hook at \"docket-cc\"."
