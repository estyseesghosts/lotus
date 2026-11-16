<#
.SYNOPSIS
    Prepares a checkout for development.

.DESCRIPTION
    Installs the pinned Rust toolchain, fetches the pinned vendored sources
    and the Cargo dependencies, and verifies that every pin still matches.

    Safe to run repeatedly.
#>
[CmdletBinding()]
param(
    [switch]$SkipSubmodules,
    [switch]$SkipFetch
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force

$pins = Read-Pins -Path (Join-Path $root 'config\pins.toml')

Write-Host '==> Rust toolchain' -ForegroundColor Cyan
$channel = $pins['rust']['channel']
$toolchain = (& rustup toolchain list) -join "`n"
if ($toolchain -notmatch [regex]::Escape($channel)) {
    & rustup toolchain install $channel --profile minimal --component rustfmt --component clippy
    if ($LASTEXITCODE -ne 0) {
        Write-Error "cannot install the pinned toolchain $channel"
    }
} else {
    Write-Host "toolchain $channel is installed"
}

if (-not $SkipSubmodules) {
    Write-Host '==> Vendored sources' -ForegroundColor Cyan
    & git -C $root submodule update --init --recursive
    if ($LASTEXITCODE -ne 0) {
        Write-Error 'cannot update the git submodules'
    }
}

if (-not $SkipFetch) {
    Write-Host '==> Cargo dependencies' -ForegroundColor Cyan
    & cargo fetch --manifest-path (Join-Path $root 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) {
        Write-Error 'cannot fetch the Cargo dependencies'
    }
}

Write-Host '==> Pin verification' -ForegroundColor Cyan
& (Join-Path $PSScriptRoot 'verify-pins.ps1')
exit $LASTEXITCODE
