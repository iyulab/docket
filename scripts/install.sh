#!/bin/bash
# Installs the docket-mcp launcher for this machine. Downloads the latest
# docket-mcp-launcher release asset and places it locally as "docket-mcp" —
# your MCP client config points at that file; the launcher itself checks
# GitHub Releases for the actual docket-mcp worker on every run.
set -euo pipefail

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

asset="docket-mcp-launcher-${platform_arch}-${platform}"
url="https://github.com/${REPO}/releases/latest/download/${asset}"

mkdir -p "$INSTALL_DIR"
echo "Downloading ${asset}..."
curl -fsSL -o "${INSTALL_DIR}/docket-mcp" "$url"
chmod +x "${INSTALL_DIR}/docket-mcp"

echo "Installed to ${INSTALL_DIR}/docket-mcp"
echo "Make sure ${INSTALL_DIR} is on your PATH, then point your MCP client's"
echo "\"command\" at \"docket-mcp\" with DOCKET_CORE_URL set to your docket-core host."
