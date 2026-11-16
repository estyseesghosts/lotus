<# Opt-in asset tests. Assets stay external; logs/reports remain local. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Worker,
    [Parameter(Mandatory)][string]$Fixture
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = Split-Path -Parent $PSScriptRoot
$env:LOTUS_NATIVE_WORKER = (Resolve-Path -LiteralPath $Worker).Path
$env:LOTUS_FIXTURE = (Resolve-Path -LiteralPath $Fixture).Path
foreach ($test in @(@('lotus-session', 'native_process'), @('lotus-desktop', 'native_controller'), @('lotus-mcp', 'native_parity'))) {
    & cargo test --manifest-path (Join-Path $root 'Cargo.toml') -p $test[0] --test $test[1] -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Native acceptance failed: $($test[1])" }
}
& python (Join-Path $root 'tests/test_native_protocol.py') $env:LOTUS_NATIVE_WORKER
if ($LASTEXITCODE -ne 0) { throw 'Native wire acceptance failed' }
