# Runs the same fmt/clippy/build/test sequence as .github/workflows/ci.yml,
# in the same order, so a failure shows up locally before it shows up in CI.
$ErrorActionPreference = "Stop"

Set-Location (Join-Path $PSScriptRoot "..")

function Invoke-Step {
    param([string]$Description, [string[]]$Command)
    Write-Output "== $Description =="
    & $Command[0] $Command[1..($Command.Length - 1)]
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed: $Description"
        exit $LASTEXITCODE
    }
}

Invoke-Step "cargo fmt --all --check" @("cargo", "fmt", "--all", "--check")
Invoke-Step "cargo clippy --workspace --all-targets -- -D warnings" @("cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings")
Invoke-Step "cargo build --workspace --bins" @("cargo", "build", "--workspace", "--bins")
Invoke-Step "cargo test --workspace" @("cargo", "test", "--workspace")

Write-Output "All checks passed."
