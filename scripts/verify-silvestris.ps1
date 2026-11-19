# Verify the vendored Silvestris snapshot (#138).
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Pins.psm1') -Force
$lotusRoot = Split-Path -Parent $PSScriptRoot
$pins = Read-Pins -Path (Join-Path $lotusRoot 'config/pins.toml')
$sourceRoot = (Resolve-Path -LiteralPath (Join-Path $lotusRoot $pins.silvestris.path)).Path
$actual = (& git -C $lotusRoot rev-parse "HEAD:$($pins.silvestris.path)").Trim()
if ($LASTEXITCODE -ne 0 -or $actual -ne $pins.silvestris.tree) {
    throw "Silvestris tree expected $($pins.silvestris.tree), found $actual at $sourceRoot"
}
& git -C $lotusRoot diff --quiet HEAD -- $pins.silvestris.path
if ($LASTEXITCODE -ne 0) { throw 'Vendored Silvestris has uncommitted tracked changes' }
$sourcePins = Read-Pins -Path (Join-Path $sourceRoot 'config/pins.toml')
if ($sourcePins.gpui.rev -ne $pins.gpui.rev) { throw 'Lotus and Silvestris GPUI revisions differ' }
if ($sourcePins.rust.channel -ne $pins.rust.channel) { throw 'Lotus and Silvestris Rust channels differ' }
Write-Output "Silvestris $($pins.silvestris.rev) tree $actual verified; GPUI $($pins.gpui.rev)"
