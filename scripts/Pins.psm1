<#
.SYNOPSIS
    Reads the pinned revision manifest.

.DESCRIPTION
    Minimal TOML reader for config/pins.toml. It understands the subset the
    manifest uses: tables and string values.
#>
function Read-Pins {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $pins = @{}
    $section = ''

    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()

        if ($trimmed -match '^\[(.+)\]$') {
            $section = $Matches[1]
            $pins[$section] = @{}
            continue
        }

        if ($trimmed -match '^([A-Za-z0-9_]+)\s*=\s*"([^"]*)"$' -and $section -ne '') {
            $pins[$section][$Matches[1]] = $Matches[2]
        }
    }

    return $pins
}
