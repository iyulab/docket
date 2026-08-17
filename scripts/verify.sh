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

echo "All checks passed."
