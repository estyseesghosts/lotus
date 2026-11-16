<#
.SYNOPSIS
    Regenerates SHA256SUMS.txt for the dossier files.

.DESCRIPTION
    The manifest covers the human readable dossier: the readme, the agent
    rules, the documents, the proposed specs and the templates. It does not
    cover source code: git already tracks those byte for byte.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$targets = @('AGENTS.md', 'README.md')
$targets += Get-ChildItem -LiteralPath (Join-Path $root 'docs') -Filter '*.md' -File |
    ForEach-Object { 'docs/' + $_.Name }
$targets += Get-ChildItem -LiteralPath (Join-Path $root 'specs') -File -Recurse |
    ForEach-Object { 'specs/' + $_.Name }
$targets += Get-ChildItem -LiteralPath (Join-Path $root 'templates') -File -Recurse |
    ForEach-Object { 'templates/' + $_.Name }

$lines = foreach ($target in $targets) {
    $path = Join-Path $root ($target -replace '/', '\')
    if (Test-Path -LiteralPath $path) {
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $target"
    }
}

$manifest = Join-Path $root 'SHA256SUMS.txt'
$lines | Set-Content -LiteralPath $manifest -Encoding utf8

Write-Host "wrote $($lines.Count) entries to $manifest" -ForegroundColor Green
