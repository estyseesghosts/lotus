<# Portable desktop/MCP launcher.
   Data directory (host.json, sessions, desktop.json), first match wins:
   -DataDir; `data_dir` in lotus.json beside this script; `data_dir` in
   %APPDATA%\LOTUS\lotus.json; else data/ beside this script. A relative `data_dir`
   resolves against the lotus.json that names it. -SetDataDir writes the
   %APPDATA% file, so the choice survives extracting a new release.
   Desktop: the window; open content from its sidebar or pass/drop a .3dsx.
   MCP: isolated headless MCP on stdio. WindowedMCP: stdio bridge to a shared
   window. SharedDesktop: open that same persistent host from the desktop. #>
[CmdletBinding()]
param(
    [ValidateSet('CITnLO', 'AZAHAR')][string]$Backend = 'CITnLO',
    [ValidateSet('Desktop', 'MCP', 'WindowedMCP', 'SharedDesktop')][string]$Mode = 'Desktop',
    [Parameter(Position = 0)][string]$Fixture,
    [string]$DataDir,
    [string]$SetDataDir,
    [switch]$ConfigureOnly
)
$ErrorActionPreference = 'Stop'
$utf8 = New-Object Text.UTF8Encoding($false)
# Relative paths resolve against the caller's location, not .NET's process directory.
function Get-FullPath([string]$path) { [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).ProviderPath, $path)) }
$userConfig = if ($env:APPDATA) { Join-Path $env:APPDATA 'LOTUS/lotus.json' } else { $null }
if ($SetDataDir) {
    if (-not $userConfig) { throw 'APPDATA is not set; pass -DataDir instead' }
    [IO.Directory]::CreateDirectory((Split-Path $userConfig)) | Out-Null
    $SetDataDir = Get-FullPath $SetDataDir
    [IO.File]::WriteAllText($userConfig, (@{ data_dir = $SetDataDir } | ConvertTo-Json), $utf8)
    Write-Host "LOTUS data directory: $SetDataDir (saved in $userConfig)"
    return
}
function Read-DataDir([string]$file) {
    if (-not $file -or -not (Test-Path -LiteralPath $file -PathType Leaf)) { return $null }
    try { $value = ([IO.File]::ReadAllText($file) | ConvertFrom-Json).data_dir }
    catch { throw "${file}: not valid JSON ($($_.Exception.Message))" }
    if ($null -eq $value) { return $null }
    if ($value -isnot [string] -or $value -eq '') { throw "${file}: data_dir must be a non-empty path string" }
    $value = [Environment]::ExpandEnvironmentVariables($value)
    return [IO.Path]::GetFullPath([IO.Path]::Combine((Split-Path $file), $value))
}
if ($DataDir) { $root = Get-FullPath $DataDir }
else {
    $root = Read-DataDir (Join-Path $PSScriptRoot 'lotus.json')
    if (-not $root) { $root = Read-DataDir $userConfig }
    if (-not $root) { $root = Join-Path $PSScriptRoot 'data' }
}
$assets = @{}
if ($Fixture) {
    $content = (Resolve-Path -LiteralPath $Fixture).Path
    if ([IO.Path]::GetExtension($content) -ine '.3dsx') { throw 'Select a .3dsx homebrew file' }
    $assets['homebrew'] = $content
}
$data = Join-Path $root $Backend.ToLowerInvariant()
[IO.Directory]::CreateDirectory($data) | Out-Null
$config = Join-Path $data 'host.json'
$text = @{
    worker = Join-Path $PSScriptRoot ('runtimes/' + $Backend.ToLowerInvariant() + '/ctr-worker.exe')
    storage_root = Join-Path $data 'sessions'
    assets = $assets
    allow_control = $true
} | ConvertTo-Json -Depth 3
# Windows PowerShell's default UTF-8 includes a BOM; serde_json expects plain UTF-8.
[IO.File]::WriteAllText($config, $text, $utf8)
# Desktop preferences (pane layout, opt-in Discord presence) stay with the data.
$env:LOTUS_DESKTOP_PREFERENCES = Join-Path $root 'desktop.json'
if ($ConfigureOnly) { return }
switch ($Mode) {
    'MCP' { & (Join-Path $PSScriptRoot 'bin/lotus-mcp.exe') $config }
    'WindowedMCP' { & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') --mcp $config }
    'SharedDesktop' { & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') --mcp-host $config }
    default { & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') $config }
}
exit $LASTEXITCODE
