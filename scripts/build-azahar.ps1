<# Build the pinned native source with an isolated cache per flavor. #>
[CmdletBinding()]
param(
    [ValidateSet('stock', 'integration', 'experimental')]
    [string]$Branch = 'integration',
    [ValidateSet('Debug', 'Release')]
    [string]$Profile = 'Release',
    [string]$AzaharRoot,
    [string]$BuildDirectory,
    [switch]$Oracle,
    [int]$Parallel = 4
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force
$pins = Read-Pins -Path (Join-Path $root 'config/pins.toml')
$expected = if ($Branch -eq 'stock') { $pins['azahar']['upstream_commit'] } else { $pins['azahar'][$Branch + '_commit'] }
if (-not $AzaharRoot) { $AzaharRoot = Join-Path (Split-Path -Parent $root) ('azahar-' + $Branch) }
$AzaharRoot = (Resolve-Path -LiteralPath $AzaharRoot).Path
$actual = & git -C $AzaharRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $actual -ne $expected) { throw "Source pin mismatch: expected $expected, got $actual" }
$dirty = & git -C $AzaharRoot status --porcelain --untracked-files=normal
if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Build inputs must be committed and clean' }
if (-not $BuildDirectory) { $BuildDirectory = Join-Path $root ('build/azahar-' + $Branch + '-native') }
$BuildDirectory = [System.IO.Path]::GetFullPath($BuildDirectory)
$cache = Join-Path $BuildDirectory 'CMakeCache.txt'
if (Test-Path -LiteralPath $cache) {
    $source = Select-String -LiteralPath $cache -Pattern '^CMAKE_HOME_DIRECTORY:INTERNAL=(.*)$'
    if (-not $source -or [System.IO.Path]::GetFullPath($source.Matches[0].Groups[1].Value) -ne $AzaharRoot) {
        throw "Cache belongs to a different source tree: $cache"
    }
}
$worker = if ($Branch -eq 'stock') { 'OFF' } else { 'ON' }
$flags = @('-G', 'Ninja', "-DCMAKE_BUILD_TYPE=$Profile", "-DENABLE_CTR_WORKER=$worker",
    '-DCMAKE_PROJECT_citra_INCLUDE=',
    '-DENABLE_QT=OFF', '-DENABLE_LIBRETRO=OFF', '-DENABLE_SDL2=OFF',
    '-DENABLE_WEB_SERVICE=OFF', '-DENABLE_SCRIPTING=OFF', '-DENABLE_ROOM=OFF',
    '-DENABLE_ROOM_STANDALONE=OFF', '-DENABLE_OPENAL=OFF', '-DENABLE_CUBEB=OFF',
    '-DENABLE_LIBUSB=OFF', '-DENABLE_SOFTWARE_RENDERER=ON', '-DENABLE_VULKAN=OFF',
    '-DENABLE_OPENGL=OFF', '-DENABLE_LTO=OFF', '-DCITRA_WARNINGS_AS_ERRORS=OFF')
$target = if ($Branch -eq 'stock') { 'citra_core' } else { 'ctr-worker' }
if ($Branch -ne 'stock') {
    $backendName = if ($Branch -eq 'integration') { 'AZAHAR' } else { 'CITnLO' }
    $flags += "-DCTR_BACKEND_NAME=$backendName"
}
if ($Oracle) {
    if ($Branch -ne 'stock') { throw 'The independent oracle must use stock source' }
    $inject = (Join-Path $root 'workers/oracle/inject.cmake').Replace('\', '/')
    $flags += "-DCMAKE_PROJECT_citra_INCLUDE=$inject"
    $target = 'lotus-stock-oracle'
}
& cmake -S $AzaharRoot -B $BuildDirectory @flags
if ($LASTEXITCODE -ne 0) { throw 'Native configuration failed' }
& cmake --build $BuildDirectory --target $target --parallel $Parallel
if ($LASTEXITCODE -ne 0) { throw 'Native build failed' }
@{ source = $actual; branch = $Branch; source_root = $AzaharRoot; target = $target;
   cmake_flags = $flags; compiler = (& g++ --version | Select-Object -First 1);
   host = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
} | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $BuildDirectory 'lotus-build-manifest.json')
