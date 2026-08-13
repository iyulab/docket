# Installs the docket-mcp launcher for this machine. Downloads the latest
# docket-mcp-launcher release asset and places it locally as "docket-mcp.exe" -
# your MCP client config points at that file; the launcher itself checks
# GitHub Releases for the actual docket-mcp worker on every run.
$ErrorActionPreference = "Stop"

$Repo = "iyulab/docket"
$InstallDir = if ($env:DOCKET_INSTALL_DIR) { $env:DOCKET_INSTALL_DIR } else { "$env:LOCALAPPDATA\docket\bin" }
$Asset = "docket-mcp-launcher-x86_64-pc-windows-msvc.exe"
$Url = "https://github.com/$Repo/releases/latest/download/$Asset"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Write-Output "Downloading $Asset..."
Invoke-WebRequest -Uri $Url -OutFile "$InstallDir\docket-mcp.exe"

Write-Output "Installed to $InstallDir\docket-mcp.exe"
Write-Output "Make sure $InstallDir is on your PATH, then point your MCP client's"
Write-Output "`"command`" at `"docket-mcp.exe`" with DOCKET_CORE_URL set to your docket-core host."
