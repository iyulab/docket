#!/bin/bash
# Runs the same fmt/clippy/build/test sequence as .github/workflows/ci.yml,
# in the same order, so a failure shows up locally before it shows up in CI.
set -eu

cd "$(dirname "$0")/.."

echo "== cargo fmt --all --check =="
cargo fmt --all --check

echo "== cargo clippy --workspace --all-targets -- -D warnings =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== cargo build --workspace --bins =="
cargo build --workspace --bins

echo "== cargo test --workspace =="
cargo test --workspace

echo "== scripts/test-install.sh =="
bash scripts/test-install.sh

echo "== relative documentation links =="
bash .github/scripts/check-doc-links.sh

echo "== cargo doc --workspace --no-deps =="
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

echo "All checks passed."
