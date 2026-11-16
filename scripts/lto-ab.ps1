<# A/B of Release vs Release+LTO for one native branch (issue #11).

Builds the pinned, clean source twice into fresh build directories through build-azahar.ps1
(same flags apart from ENABLE_LTO), times each build, checks that -flto reaches the compile
and link rules only in the LTO build, records binary size and SHA-256, and then runs
tools/bench.py with both workers on the same ROM. bench.py checks guest identity (ticks and
screen hashes) across the two workers and refuses Debug builds; -RequireIdle passes
--require-idle so a busy host refuses to start. Writes <Output>/lto-ab.json next to the
bench report.

    pwsh scripts/lto-ab.ps1 -Branch experimental -Rom <abs .3dsx> -Output runs/<fresh> `
        [-Renderer vulkan,software] [-Repeats 5] [-Frames 600] [-RequireIdle]
#>
[CmdletBinding()]
param(
    [ValidateSet('integration', 'experimental')]
    [string]$Branch = 'experimental',
    [Parameter(Mandatory)] [string]$Rom,
    [Parameter(Mandatory)] [string]$Output,
    [string[]]$Renderer = @('vulkan', 'software'),
    [string[]]$Config = @('off'),
    [int]$Repeats = 5,
    [int]$Frames = 600,
    [int]$Parallel = 4,
    [switch]$RequireIdle,
    [switch]$SkipBench
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = Split-Path -Parent $PSScriptRoot
$Output = [System.IO.Path]::GetFullPath($Output)
if (Test-Path -LiteralPath $Output) { throw "Output exists: $Output" }
New-Item -ItemType Directory -Path $Output | Out-Null
$Rom = (Resolve-Path -LiteralPath $Rom).Path

$builds = [ordered]@{}
foreach ($mode in 'default', 'lto') {
    $dir = Join-Path $Output "build-$mode"
    $buildArgs = @{ Branch = $Branch; Profile = 'Release'; BuildDirectory = $dir; Parallel = $Parallel }
    if ($mode -eq 'lto') { $buildArgs.Lto = $true }
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    & (Join-Path $PSScriptRoot 'build-azahar.ps1') @buildArgs
    $seconds = $clock.Elapsed.TotalSeconds
    $exe = Join-Path $dir 'bin/Release/ctr-worker.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "No worker at $exe" }
    # CMake IPO with GCC adds -flto=auto to the target's compile and link rules (build.ninja).
    $ninja = Get-Content -LiteralPath (Join-Path $dir 'build.ninja') -Raw
    $flto = $ninja -match '-flto'
    if ($flto -ne ($mode -eq 'lto')) { throw "$mode build: -flto present=$flto in build.ninja" }
    $cache = Get-Content -LiteralPath (Join-Path $dir 'CMakeCache.txt')
    $builds[$mode] = [ordered]@{
        directory = $dir; build_seconds = [math]::Round($seconds, 1); worker = $exe
        bytes = (Get-Item -LiteralPath $exe).Length
        sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLower()
        flto_in_build_ninja = $flto
        enable_lto_cache = ($cache | Select-String '^ENABLE_LTO:BOOL=(.*)$').Matches[0].Groups[1].Value
        manifest = Get-Content -LiteralPath (Join-Path $dir 'lotus-build-manifest.json') -Raw | ConvertFrom-Json
    }
}
$report = [ordered]@{ tool = 'scripts/lto-ab.ps1'; branch = $Branch; builds = $builds }
if (-not $SkipBench) {
    $bench = Join-Path $Output 'bench'
    $benchArgs = @((Join-Path $root 'tools/bench.py'),
        '--worker', "default=$($builds.default.worker)", '--worker', "lto=$($builds.lto.worker)",
        '--rom', $Rom, '--output', $bench, '--repeats', $Repeats, '--frames', $Frames)
    foreach ($r in $Renderer) { $benchArgs += @('--renderer', $r) }
    foreach ($c in $Config) { $benchArgs += @('--config', $c) }
    if ($RequireIdle) { $benchArgs += '--require-idle' }
    & python @benchArgs
    if ($LASTEXITCODE -ne 0) { throw 'bench.py failed' }
    $report.bench = $bench
}
$report | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $Output 'lto-ab.json')
Write-Host "default $($builds.default.build_seconds) s, $([math]::Round($builds.default.bytes / 1MB, 1)) MiB; lto $($builds.lto.build_seconds) s, $([math]::Round($builds.lto.bytes / 1MB, 1)) MiB"
