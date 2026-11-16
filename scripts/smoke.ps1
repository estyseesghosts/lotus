<#
.SYNOPSIS
    Runs the fast checks: build, tests, formatter, clippy, CLI and pins.

.DESCRIPTION
    This is the check that a fresh checkout is healthy. It does not need a
    ROM, a GPU or a display.
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

# Native tools write progress to stderr; that must not be treated as a
# terminating error. Failures are detected through the exit codes.
$ErrorActionPreference = 'Continue'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
$root = Split-Path -Parent $PSScriptRoot

function Invoke-Step {
    param(
        [string]$Name,
        [scriptblock]$Action
    )

    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name" -ForegroundColor Red
        exit 1
    }
}

if (-not $SkipBuild) {
    Invoke-Step -Name 'cargo build' -Action {
        cargo build --manifest-path (Join-Path $root 'Cargo.toml') 2>&1 | Out-Host
    }
}

Invoke-Step -Name 'cargo test' -Action {
    cargo test --manifest-path (Join-Path $root 'Cargo.toml') --workspace 2>&1 | Out-Host
}

Invoke-Step -Name 'cargo fmt --check' -Action {
    cargo fmt --manifest-path (Join-Path $root 'Cargo.toml') --all -- --check 2>&1 | Out-Host
}

Invoke-Step -Name 'cargo clippy' -Action {
    cargo clippy --manifest-path (Join-Path $root 'Cargo.toml') --workspace --all-targets 2>&1 | Out-Host
}

Invoke-Step -Name 'ctr-cli version' -Action {
    & (Join-Path $root 'target\debug\ctr-cli.exe') version 2>&1 | Out-Host
}

Invoke-Step -Name 'pin verification' -Action {
    & (Join-Path $root 'scripts\verify-pins.ps1') 2>&1 | Out-Host
}

Write-Host ''
Write-Host 'Smoke checks passed.' -ForegroundColor Green
exit 0
