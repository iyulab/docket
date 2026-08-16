# Installs the docket-mcp and docket-cc launchers for this machine. Downloads
# the latest docket-mcp-launcher and docket-cc-launcher release assets and
# places them locally as "docket-mcp.exe"/"docket-cc.exe" - your MCP client
# config and Claude Code hook config point at these files; each launcher
# checks GitHub Releases for its own worker on every run.
$ErrorActionPreference = "Stop"

$Repo = "iyulab/docket"
$InstallDir = if ($env:DOCKET_INSTALL_DIR) { $env:DOCKET_INSTALL_DIR } else { "$env:LOCALAPPDATA\docket\bin" }

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$Workers = @{
    "docket-mcp" = "docket-mcp-launcher-x86_64-pc-windows-msvc.exe"
    "docket-cc"  = "docket-cc-launcher-x86_64-pc-windows-msvc.exe"
}
foreach ($worker in $Workers.Keys) {
    $asset = $Workers[$worker]
    $url = "https://github.com/$Repo/releases/latest/download/$asset"
    Write-Output "Downloading $asset..."
    Invoke-WebRequest -Uri $url -OutFile "$InstallDir\$worker.exe"
    Write-Output "Installed to $InstallDir\$worker.exe"
}

Write-Output "Make sure $InstallDir is on your PATH. Point your MCP client's `"command`" at"
Write-Output "`"docket-mcp.exe`" (with DOCKET_CORE_URL set) and any Claude Code hook at `"docket-cc.exe`"."
