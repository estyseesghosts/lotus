<# Portable desktop/MCP launcher. All runtime files stay beside this script. #>
[CmdletBinding()]
param(
    [ValidateSet('CITnLO', 'AZAHAR')][string]$Backend = 'CITnLO',
    [ValidateSet('Desktop', 'MCP')][string]$Mode = 'Desktop',
    [Parameter(Position = 0)][string]$Fixture,
    [switch]$ConfigureOnly
)
$ErrorActionPreference = 'Stop'
if (-not $Fixture) {
    if ($Mode -eq 'MCP' -or $ConfigureOnly) { throw 'Supply -Fixture with a local .3dsx path' }
    Add-Type -AssemblyName System.Windows.Forms
    $dialog = New-Object System.Windows.Forms.OpenFileDialog
    $dialog.Title = 'Open homebrew in LOTUS'
    $dialog.Filter = '3DS homebrew (*.3dsx)|*.3dsx'
    try {
        if ($dialog.ShowDialog() -ne [System.Windows.Forms.DialogResult]::OK) { exit 0 }
        $Fixture = $dialog.FileName
    } finally { $dialog.Dispose() }
}
$content = (Resolve-Path -LiteralPath $Fixture).Path
if ([IO.Path]::GetExtension($content) -ine '.3dsx') { throw 'Select a .3dsx homebrew file' }
$data = Join-Path $PSScriptRoot ('data/' + $Backend.ToLowerInvariant())
[IO.Directory]::CreateDirectory($data) | Out-Null
$config = Join-Path $data 'host.json'
$text = @{
    worker = Join-Path $PSScriptRoot ('runtimes/' + $Backend.ToLowerInvariant() + '/ctr-worker.exe')
    storage_root = Join-Path $data 'sessions'
    assets = @{ homebrew = $content }
    allow_control = $true
} | ConvertTo-Json -Depth 3
# Windows PowerShell's default UTF-8 includes a BOM; serde_json expects plain UTF-8.
[IO.File]::WriteAllText($config, $text, (New-Object Text.UTF8Encoding($false)))
if ($ConfigureOnly) { return }
if ($Mode -eq 'MCP') {
    & (Join-Path $PSScriptRoot 'bin/lotus-mcp.exe') $config
} else {
    & (Join-Path $PSScriptRoot 'bin/lotus-desktop.exe') $config
}
exit $LASTEXITCODE
