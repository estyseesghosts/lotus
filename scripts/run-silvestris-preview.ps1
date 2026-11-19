# Visible staging preview (#138). Run from this checkout; never builds main Lotus.
[CmdletBinding()]
param(
    [switch]$NoBuild,
    [switch]$Restart
)
$ErrorActionPreference = 'Stop'
$lotusRoot = Split-Path -Parent $PSScriptRoot
$branch = (& git -C $lotusRoot branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or !$branch -or $branch -in @('main', 'master')) {
    throw 'Use a Lotus staging branch for the Silvestris preview'
}
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force
$pins = Read-Pins -Path (Join-Path $lotusRoot 'config/pins.toml')
$sourceRoot = (Resolve-Path -LiteralPath (Join-Path $lotusRoot $pins.silvestris.path)).Path
& (Join-Path $PSScriptRoot 'verify-silvestris.ps1')
$buildRoot = Join-Path $sourceRoot 'target'
$previewRoot = Join-Path $buildRoot 'lotus-preview'
$previewExe = Join-Path $previewRoot 'lotus-desktop-preview.exe'
$statePath = Join-Path $previewRoot 'process.json'
$preferencesPath = Join-Path $buildRoot 'lotus-preview-desktop.json'
$stdout = Join-Path $previewRoot 'stdout.log'
$stderr = Join-Path $previewRoot 'stderr.log'
$restartProcess = $null

if (Test-Path -LiteralPath $statePath) {
    $previous = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
    $running = Get-Process -Id $previous.pid -ErrorAction SilentlyContinue
    if ($running) {
        # PID alone is insufficient: never terminate an unrelated process that
        # inherited the number, or another manually launched preview instance.
        if ($running.Path -ne $previewExe -or $previous.executable -ne $previewExe -or
            $running.StartTime.ToUniversalTime().Ticks.ToString() -ne [string]$previous.startedUtcTicks) {
            throw 'Preview process record does not match the live process; refusing restart'
        }
        if (!$Restart) {
            Write-Output "Preview already running: PID $($running.Id), $($running.MainWindowTitle). Use -Restart to rebuild/relaunch."
            return
        }
        $restartProcess = $running
    }
}

if (!$NoBuild) {
    Push-Location $lotusRoot
    try {
        & cargo '+1.98.1-x86_64-pc-windows-gnu' build -p lotus-desktop --bin lotus-desktop --features silvestris-ui --offline --locked --target-dir $buildRoot
        if ($LASTEXITCODE -ne 0) { throw 'Lotus preview build failed' }
    } finally { Pop-Location }
}

# Keep the existing window open until its replacement builds successfully.
if ($restartProcess -and !$restartProcess.HasExited) {
    Stop-Process -Id $restartProcess.Id
    if (!$restartProcess.WaitForExit(10000)) { throw 'Previous preview did not exit' }
}
New-Item -ItemType Directory -Path $previewRoot -Force | Out-Null
# Run a copy: Windows locks a running executable, otherwise Cargo cannot relink.
Copy-Item -LiteralPath (Join-Path $buildRoot 'debug/lotus-desktop.exe') -Destination $previewExe -Force
$priorPreferences = $env:LOTUS_DESKTOP_PREFERENCES
try {
    $env:LOTUS_DESKTOP_PREFERENCES = $preferencesPath
    $preview = Start-Process -FilePath $previewExe -WorkingDirectory $lotusRoot -WindowStyle Normal -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
} finally { $env:LOTUS_DESKTOP_PREFERENCES = $priorPreferences }
@{
    pid = $preview.Id
    executable = $previewExe
    startedUtcTicks = $preview.StartTime.ToUniversalTime().Ticks.ToString()
    preferences = $preferencesPath
    branch = $branch
    sha256 = (Get-FileHash -LiteralPath $previewExe -Algorithm SHA256).Hash
} | ConvertTo-Json | Set-Content -LiteralPath $statePath -Encoding UTF8
# The desktop binary has a console subsystem for --mcp stdio, so
# WaitForInputIdle can fail even though GPUI creates a native window.
$windowDeadline = [DateTime]::UtcNow.AddSeconds(10)
do {
    $preview.Refresh()
    if ($preview.HasExited) { throw "Lotus preview exited; inspect $stderr" }
    if ($preview.MainWindowHandle -ne 0) { break }
    Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $windowDeadline)
if ($preview.MainWindowHandle -eq 0) { throw "Preview has no window after 10 seconds; inspect $stderr" }
Write-Output "Visible Lotus preview: PID $($preview.Id), branch $branch"
Write-Output "Preferences: $preferencesPath"
Write-Output "Startup log: $stderr"
