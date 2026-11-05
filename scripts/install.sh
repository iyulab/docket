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

# Every launcher is downloaded to a temp file next to its destination first and
# only then renamed into place, for two reasons:
#  * An installed executable is never rewritten in place. macOS caches a
#    binary's code signature per file, so overwriting a launcher that has run
#    before makes its next start die with "Code Signature Invalid"; a rename
#    gives the path a new file instead.
#  * A download that fails part way leaves the existing install as it was,
#    rather than updating one launcher and leaving the other on an older release.
workers="docket-mcp docket-cc"
staged_files=""
cleanup() {
  for f in $staged_files; do rm -f "$f"; done
}
trap cleanup EXIT

for worker in $workers; do
  asset="${worker}-launcher-${platform_arch}-${platform}"
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
  tmp="${INSTALL_DIR}/.${worker}.new.$$"
  staged_files="$staged_files $tmp"
  echo "Downloading ${asset}..."
  curl -fsSL --retry 3 --retry-delay 2 -o "$tmp" "$url"
  chmod +x "$tmp"
done

for worker in $workers; do
  mv -f "${INSTALL_DIR}/.${worker}.new.$$" "${INSTALL_DIR}/${worker}"
  echo "Installed to ${INSTALL_DIR}/${worker}"
done

echo "Make sure ${INSTALL_DIR} is on your PATH. Point your MCP client's \"command\" at"
echo "\"docket-mcp\" (with DOCKET_CORE_URL set) and any Claude Code hook at \"docket-cc\"."
