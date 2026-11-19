# Local staging dependency gate (#138). Does not modify either repository.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force
$lotusRoot = Split-Path -Parent $PSScriptRoot
$pins = Read-Pins -Path (Join-Path $lotusRoot 'config/pins.toml')
$sourceRoot = (Resolve-Path -LiteralPath (Join-Path $lotusRoot $pins.silvestris.path)).Path
$actual = (& git -C $sourceRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $actual -ne $pins.silvestris.rev) {
    throw "Silvestris expected $($pins.silvestris.rev), found $actual at $sourceRoot"
}
& git -C $sourceRoot diff --quiet
if ($LASTEXITCODE -ne 0) { throw 'Silvestris has uncommitted tracked source changes' }
& git -C $sourceRoot diff --cached --quiet
if ($LASTEXITCODE -ne 0) { throw 'Silvestris has staged source changes' }
$sourcePins = Read-Pins -Path (Join-Path $sourceRoot 'config/pins.toml')
if ($sourcePins.gpui.rev -ne $pins.gpui.rev) { throw 'Lotus and Silvestris GPUI revisions differ' }
if ($sourcePins.rust.channel -ne $pins.rust.channel) { throw 'Lotus and Silvestris Rust channels differ' }
Write-Output "Silvestris $actual verified; GPUI $($pins.gpui.rev)"
