# Installs the docket-mcp and docket-cc launchers for this machine. Downloads
# the latest docket-mcp-launcher and docket-cc-launcher release assets and
# places them locally as "docket-mcp.exe"/"docket-cc.exe" - your MCP client
# config and Claude Code hook config point at these files; each launcher
# checks GitHub Releases for its own worker on every run.
$ErrorActionPreference = "Stop"

$Repo = "iyulab/docket"
$InstallDir = if ($env:DOCKET_INSTALL_DIR) { $env:DOCKET_INSTALL_DIR } else { "$env:LOCALAPPDATA\docket\bin" }

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$Workers = [ordered]@{
    "docket-mcp" = "docket-mcp-launcher-x86_64-pc-windows-msvc.exe"
    "docket-cc"  = "docket-cc-launcher-x86_64-pc-windows-msvc.exe"
}

# Download every launcher to a temp file next to its destination first and move
# them into place only once all of them arrived, so a download that fails part
# way leaves the existing install as it was instead of updating one launcher
# and leaving the other on an older release.
$Staged = @{}
try {
    foreach ($worker in $Workers.Keys) {
        $asset = $Workers[$worker]
        $url = "https://github.com/$Repo/releases/latest/download/$asset"
        $tmp = Join-Path $InstallDir ".$worker.exe.new"
        $Staged[$worker] = $tmp
        Write-Output "Downloading $asset..."
        Invoke-WebRequest -Uri $url -OutFile $tmp
    }
    foreach ($worker in $Workers.Keys) {
        Move-Item -Force -Path $Staged[$worker] -Destination "$InstallDir\$worker.exe"
        $Staged.Remove($worker)
        Write-Output "Installed to $InstallDir\$worker.exe"
    }
}
finally {
    foreach ($tmp in $Staged.Values) {
        Remove-Item -Force -ErrorAction SilentlyContinue -Path $tmp
    }
}

Write-Output "Make sure $InstallDir is on your PATH. Point your MCP client's `"command`" at"
Write-Output "`"docket-mcp.exe`" (with DOCKET_CORE_URL set) and any Claude Code hook at `"docket-cc.exe`"."
