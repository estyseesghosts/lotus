<# A/B of Release vs Release+LTO for one native branch (issue #11).

Builds the pinned, clean source twice into fresh build directories through build-azahar.ps1
(same flags apart from ENABLE_LTO), times each build, checks that -flto reaches the compile
and link rules only in the LTO build, records binary size and SHA-256, and then runs
tools/bench.py with both workers on the same ROM. bench.py checks guest identity (ticks and
screen hashes) across the two workers and refuses Debug builds; -RequireIdle passes
--require-idle so a busy host refuses to start. Writes <Output>/lto-ab.json next to the
bench report.

    ./scripts/lto-ab.ps1 -Branch experimental -AzaharRoot <clean pinned source> `
        -Rom <abs .3dsx> -Output runs/<fresh> [-Renderer vulkan,software] [-RequireIdle]
#>
[CmdletBinding()]
param(
    [ValidateSet('integration', 'experimental')]
    [string]$Branch = 'experimental',
    [Parameter(Mandatory)] [string]$Rom,
    [Parameter(Mandatory)] [string]$Output,
    [string]$AzaharRoot,
    [string[]]$Renderer = @('vulkan', 'software'),
    [string[]]$Config = @('off'),
    [ValidateSet('cold', 'warm')]
    [string]$ShaderCache = 'cold',
    [int]$Warmup = 120,
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

function Get-IpoEvidence([string]$Directory, [bool]$Expected) {
    $ninja = Get-Content -LiteralPath (Join-Path $Directory 'build.ninja') -Raw
    $ipoFlag = '(?i)(?:^|\s)(?:-flto(?:=[^\s]+)?|/GL|/LTCG(?::[^\s]+)?)(?=\s|$)'
    $rules = [regex]::Matches($ninja,
        '(?m)^build (?<path>(?:src|externals)/\S+?\.obj|bin/Release/ctr-worker\.exe): (?<rule>\S+)[^\r\n]*(?:\r?\n +[^\r\n]*)*')
    $targets = [ordered]@{}
    foreach ($rule in $rules) {
        $path = $rule.Groups['path'].Value
        $key = if ($path -eq 'bin/Release/ctr-worker.exe') {
            'worker_link'
        } elseif ($path -match '/CMakeFiles/([^/]+)\.dir/') {
            $Matches[1]
        } else {
            continue
        }
        if (-not $targets.Contains($key)) {
            $targets[$key] = [ordered]@{ rules = 0; ipo_rules = 0; flags = @() }
        }
        $entry = $targets[$key]
        $flags = ([regex]::Matches($rule.Value, '(?m)^ +(?:LINK_)?FLAGS = (.*)') |
            ForEach-Object { $_.Groups[1].Value.Trim() }) -join ' '
        $entry.rules++
        if ($flags -match $ipoFlag) { $entry.ipo_rules++ }
        if ($flags -notin $entry.flags) { $entry.flags += $flags }
    }
    foreach ($name in 'ctr-worker', 'worker_link', 'citra_common', 'citra_core', 'video_core',
        'audio_core', 'input_common', 'network') {
        $entry = $targets[$name]
        if (-not $entry -or ($Expected -and $entry.ipo_rules -ne $entry.rules) -or
            (-not $Expected -and $entry.ipo_rules -ne 0)) {
            throw "Unexpected IPO coverage for $name in $Directory (expected=$Expected)"
        }
    }
    return $targets
}

$builds = [ordered]@{}
foreach ($mode in 'default', 'lto') {
    $dir = Join-Path $Output "build-$mode"
    $buildArgs = @{ Branch = $Branch; Profile = 'Release'; BuildDirectory = $dir; Parallel = $Parallel }
    if ($AzaharRoot) { $buildArgs.AzaharRoot = $AzaharRoot }
    if ($mode -eq 'lto') { $buildArgs.Lto = $true }
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    & (Join-Path $PSScriptRoot 'build-azahar.ps1') @buildArgs
    $seconds = $clock.Elapsed.TotalSeconds
    $exe = Join-Path $dir 'bin/Release/ctr-worker.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "No worker at $exe" }
    $ipo = Get-IpoEvidence $dir ($mode -eq 'lto')
    $link = Get-Content -LiteralPath (Join-Path $dir '.ninja_log') |
        Where-Object { ($_ -split "`t")[3] -eq 'bin/Release/ctr-worker.exe' } | Select-Object -Last 1
    if (-not $link) { throw "No worker link entry in $dir/.ninja_log" }
    $linkTimes = $link -split "`t"
    $cache = Get-Content -LiteralPath (Join-Path $dir 'CMakeCache.txt')
    $builds[$mode] = [ordered]@{
        directory = $dir; build_seconds = [math]::Round($seconds, 1); worker = $exe
        bytes = (Get-Item -LiteralPath $exe).Length
        sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLower()
        worker_link_seconds = ([long]$linkTimes[1] - [long]$linkTimes[0]) / 1000.0
        ipo_targets = $ipo
        enable_lto_cache = ($cache | Select-String '^ENABLE_LTO:BOOL=(.*)$').Matches[0].Groups[1].Value
        manifest = Get-Content -LiteralPath (Join-Path $dir 'lotus-build-manifest.json') -Raw | ConvertFrom-Json
    }
}
$report = [ordered]@{ tool = 'scripts/lto-ab.ps1'; branch = $Branch; builds = $builds }
$reportPath = Join-Path $Output 'lto-ab.json'
$report | ConvertTo-Json -Depth 8 | Set-Content $reportPath
if (-not $SkipBench) {
    $bench = Join-Path $Output 'bench'
    $benchArgs = @((Join-Path $root 'tools/bench.py'),
        '--worker', "default=$($builds.default.worker)", '--worker', "lto=$($builds.lto.worker)",
        '--rom', $Rom, '--output', $bench, '--repeats', $Repeats, '--frames', $Frames, '--frame-limit', 0,
        '--warmup', $Warmup, '--shader-cache', $ShaderCache)
    foreach ($r in $Renderer) { $benchArgs += @('--renderer', $r) }
    foreach ($c in $Config) { $benchArgs += @('--config', $c) }
    if ($RequireIdle) { $benchArgs += '--require-idle' }
    & python @benchArgs
    if ($LASTEXITCODE -ne 0) { throw 'bench.py failed' }
    $report.bench = $bench
    $measurements = Get-Content -LiteralPath (Join-Path $bench 'report.json') -Raw | ConvertFrom-Json -AsHashtable
    $invalid = foreach ($r in $Renderer) {
        foreach ($c in $Config) {
            $default = $measurements.results[$r].default[$c]
            $lto = $measurements.results[$r].lto[$c]
            if ($default.completed -ne $Repeats -or $lto.completed -ne $Repeats -or
                $lto.guest_identical_to_default -ne $true -or $null -eq $lto.ratio_vs_default) {
                "$r/$c"
            }
        }
    }
    $report.guest_and_profile_matched = -not $invalid
    $report.host_busy = $measurements.host_busy
    $report | ConvertTo-Json -Depth 8 | Set-Content $reportPath
    if ($invalid) { throw "Incomplete, mismatched guest or mismatched profile: $($invalid -join ', ')" }
    if ($RequireIdle -and $measurements.host_busy) { throw 'Benchmark finished on a busy host' }
}
Write-Host "default $($builds.default.build_seconds) s, $([math]::Round($builds.default.bytes / 1MB, 1)) MiB; lto $($builds.lto.build_seconds) s, $([math]::Round($builds.lto.bytes / 1MB, 1)) MiB"
