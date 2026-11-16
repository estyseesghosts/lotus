<#
.SYNOPSIS
    Configures and builds the native worker against a pinned Azahar tree.

.DESCRIPTION
    One build directory per branch. The integration and the experimental
    branch must never share a configured CMake cache.

    Example:
      scripts\build-azahar.ps1 -Branch integration
      scripts\build-azahar.ps1 -Branch experimental
#>
[CmdletBinding()]
param(
    [ValidateSet('integration', 'experimental')]
    [string]$Branch = 'integration',

    [ValidateSet('Debug', 'Release')]
    [string]$Profile = 'Release',

    [string]$AzaharRoot,
    [string]$BuildDirectory
)

$ErrorActionPreference = 'Continue'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force

$pins = Read-Pins -Path (Join-Path $root 'config\pins.toml')

if (-not $AzaharRoot) {
    $AzaharRoot = Join-Path $root $pins['azahar']['vendor_path']
}

if (-not $BuildDirectory) {
    $BuildDirectory = Join-Path $root ("build\azahar-" + $Branch)
}

if (-not (Test-Path -LiteralPath $AzaharRoot)) {
    Write-Error "no Azahar tree at $AzaharRoot. Run scripts\bootstrap.ps1 first."
}

Write-Host "==> Configuring the $Branch worker" -ForegroundColor Cyan
Write-Host "    azahar tree : $AzaharRoot"
Write-Host "    build dir   : $BuildDirectory"

& cmake -S (Join-Path $root 'workers\azahar') -B $BuildDirectory `
    -DLOTUS_AZAHAR_ROOT=$AzaharRoot `
    "-DCMAKE_BUILD_TYPE=$Profile"
if ($LASTEXITCODE -ne 0) {
    Write-Error 'CMake configuration failed'
}

Write-Host '==> Building' -ForegroundColor Cyan
& cmake --build $BuildDirectory --config $Profile
if ($LASTEXITCODE -ne 0) {
    Write-Error 'the build failed'
}

Write-Host ''
Write-Host "worker built in $BuildDirectory" -ForegroundColor Green
