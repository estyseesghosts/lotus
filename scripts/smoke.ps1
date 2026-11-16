<#
.SYNOPSIS
    Runs the fast checks: build, tests, CLI and pin verification.

.DESCRIPTION
    This is the check that a fresh checkout is healthy. It does not need a
    ROM, a GPU or a display.
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Invoke-Step {
    param(
        [string]$Name,
        [scriptblock]$Action
    )

    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0) {
        Write-Error "$Name failed"
    }
}

if (-not $SkipBuild) {
    Invoke-Step -Name 'cargo build' -Action { cargo build --manifest-path (Join-Path $root 'Cargo.toml') }
}

Invoke-Step -Name 'cargo test' -Action { cargo test --manifest-path (Join-Path $root 'Cargo.toml') --workspace }

Invoke-Step -Name 'cargo fmt --check' -Action { cargo fmt --manifest-path (Join-Path $root 'Cargo.toml') --all -- --check }

Invoke-Step -Name 'cargo clippy' -Action { cargo clippy --manifest-path (Join-Path $root 'Cargo.toml') --workspace --all-targets }

Invoke-Step -Name 'ctr-cli version' -Action { & (Join-Path $root 'target\debug\ctr-cli.exe') version }

Invoke-Step -Name 'pin verification' -Action { & (Join-Path $root 'scripts\verify-pins.ps1') }

Write-Host ''
Write-Host 'Smoke checks passed.' -ForegroundColor Green
