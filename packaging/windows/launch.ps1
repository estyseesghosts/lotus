<# Portable desktop/MCP launcher. All runtime files stay beside this script.
   Desktop: the window; open content from its sidebar or pass/drop a .3dsx.
   MCP: headless MCP on stdio. WindowedMCP: the window, also serving MCP on stdio. #>
[CmdletBinding()]
param(
    [ValidateSet('CITnLO', 'AZAHAR')][string]$Backend = 'CITnLO',
    [ValidateSet('Desktop', 'MCP', 'WindowedMCP')][string]$Mode = 'Desktop',
    [Parameter(Position = 0)][string]$Fixture,
    [switch]$ConfigureOnly
)
$ErrorActionPreference = 'Stop'
$assets = @{}
if ($Fixture) {
    $content = (Resolve-Path -LiteralPath $Fixture).Path
    if ([IO.Path]::GetExtension($content) -ine '.3dsx') { throw 'Select a .3dsx homebrew file' }
    $assets['homebrew'] = $content
}
$data = Join-Path $PSScriptRoot ('data/' + $Backend.ToLowerInvariant())
[IO.Directory]::CreateDirectory($data) | Out-Null
$config = Join-Path $data 'host.json'
$text = @{
    worker = Join-Path $PSScriptRoot ('runtimes/' + $Backend.ToLowerInvariant() + '/ctr-worker.exe')
    storage_root = Join-Path $data 'sessions'
    assets = $assets
    allow_control = $true
} | ConvertTo-Json -Depth 3
# Windows PowerShell's default UTF-8 includes a BOM; serde_json expects plain UTF-8.
[IO.File]::WriteAllText($config, $text, (New-Object Text.UTF8Encoding($false)))
# Desktop preferences (pane layout, opt-in Discord presence) stay in the release folder.
$env:LOTUS_DESKTOP_PREFERENCES = Join-Path $PSScriptRoot 'data/desktop.json'
if ($ConfigureOnly) { return }
switch ($Mode) {
    'MCP' { & (Join-Path $PSScriptRoot 'bin/lotus-mcp.exe') $config }
    'WindowedMCP' { & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') --mcp $config }
    default { & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') $config }
}
exit $LASTEXITCODE
