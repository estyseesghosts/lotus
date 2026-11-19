<#
.SYNOPSIS
    Verifies that every pinned dependency still points at the recorded revision.

.DESCRIPTION
    Checks four things:

      1. The vendored Azahar checkout is at the pinned integration commit.
      2. The submodule pointer recorded in the index matches that commit.
      3. Cargo.lock resolves GPUI to the pinned upstream commit.
      4. The active Rust toolchain is the pinned channel.

    Exits with 1 when any check fails.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Continue'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force

$root = Split-Path -Parent $PSScriptRoot
$pinsPath = Join-Path $root 'config\pins.toml'

if (-not (Test-Path -LiteralPath $pinsPath)) {
    Write-Error "missing $pinsPath"
    exit 1
}

$pins = Read-Pins -Path $pinsPath
$failures = 0

function Test-Pin {
    param(
        [string]$Name,
        [bool]$Ok,
        [string]$Detail
    )

    if ($Ok) {
        Write-Host ("PASS  {0}: {1}" -f $Name, $Detail) -ForegroundColor Green
    } else {
        Write-Host ("FAIL  {0}: {1}" -f $Name, $Detail) -ForegroundColor Red
        $script:failures++
    }
}

# 1. Vendored Azahar checkout.
$azaharRoot = Join-Path $root $pins['azahar']['vendor_path']
$expectedAzahar = $pins['azahar']['integration_commit']

if (Test-Path -LiteralPath (Join-Path $azaharRoot '.git')) {
    $actualAzahar = (& git -C $azaharRoot rev-parse HEAD).Trim()
    Test-Pin -Name 'azahar checkout' -Ok ($actualAzahar -eq $expectedAzahar) `
        -Detail ("expected {0}, found {1}" -f $expectedAzahar, $actualAzahar)
} else {
    Test-Pin -Name 'azahar checkout' -Ok $false -Detail "no checkout at $azaharRoot"
}

# 2. Submodule pointer in the index.
$hasHead = $null -ne (& git -C $root rev-parse --verify HEAD 2>$null)
if ($hasHead) {
    $pointer = (& git -C $root ls-tree HEAD $pins['azahar']['vendor_path'])
    if ($pointer -match '^160000 commit ([0-9a-f]{40})') {
        Test-Pin -Name 'azahar submodule pointer' -Ok ($Matches[1] -eq $expectedAzahar) `
            -Detail ("index records {0}" -f $Matches[1])
    } else {
        Test-Pin -Name 'azahar submodule pointer' -Ok $false -Detail 'no submodule pointer in the index'
    }
} else {
    Test-Pin -Name 'azahar submodule pointer' -Ok $false -Detail 'the repository has no commit yet'
}

# 3. GPUI revision in Cargo.lock.
$lockPath = Join-Path $root 'Cargo.lock'
$expectedGpui = $pins['gpui']['rev']

if (Test-Path -LiteralPath $lockPath) {
    $lock = Get-Content -LiteralPath $lockPath -Raw
    $gpuiLocked = $lock -match [regex]::Escape("rev=$expectedGpui")
    Test-Pin -Name 'gpui lock entry' -Ok $gpuiLocked `
        -Detail ("expected rev {0} in Cargo.lock" -f $expectedGpui)
} else {
    Test-Pin -Name 'gpui lock entry' -Ok $false -Detail 'Cargo.lock is missing'
}

# 4. Rust toolchain.
$expectedRust = $pins['rust']['channel']
$rustcVersion = (& rustc --version)
Test-Pin -Name 'rust toolchain' -Ok ($rustcVersion -like "*$expectedRust*") `
    -Detail ("expected {0}, found {1}" -f $expectedRust, $rustcVersion.Trim())
$expectedHost = $pins['rust']['host']
$rustcHost = ((& rustc -vV) | Where-Object { $_ -like 'host: *' }) -replace '^host: ', ''
Test-Pin -Name 'rust host' -Ok ($rustcHost -eq $expectedHost) `
    -Detail ("expected {0}, found {1}" -f $expectedHost, $rustcHost)

if ($failures -gt 0) {
    Write-Host ''
    Write-Host "$failures pin check(s) failed." -ForegroundColor Red
    exit 1
}

Write-Host ''
Write-Host 'All pins verified.' -ForegroundColor Green
exit 0
