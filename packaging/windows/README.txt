LOTUS portable for Windows x64

Extract the entire ZIP to a writable folder, then double-click LOTUS.cmd.
Select your own .3dsx homebrew. In LOTUS, click New: homebrew, then Run.
LOTUS.cmd uses CITnLO. LOTUS-AZAHAR.cmd uses the upstream-tracking AZAHAR backend.
You can also drag a .3dsx onto either launcher.

No installer, administrator access, Rust, compiler or Python is required to run.
Windows 11 x64 is the initial supported host; the GPUI desktop uses Direct3D 11.
Keep all files together. Sessions, logs and saves live in data/<backend>/sessions.
Move the entire folder to move the installation. Select the homebrew again after
moving: the launcher refreshes machine-specific asset/runtime paths each time.
Closing LOTUS terminates its workers. Keep data/ when upgrading to retain runs.

bin/ctr-cli.exe provides headless scenario runs. bin/lotus-mcp.exe serves local
MCP over stdio using an explicit host JSON. For a portable MCP command:
powershell.exe -NoProfile -ExecutionPolicy Bypass -File <folder>/launch.ps1 -Mode MCP -Fixture <homebrew.3dsx>
MCP and desktop are separate hosts; each owns its sessions. No network listener.

This initial developer release is unsigned. Software rendering only; audio is
muted. No ROMs, Nintendo firmware, DSP dump, keys or user captures are included.
Visible GPUI/input and actual two-DPI acceptance remain unverified. Automated
native lifecycle, isolation, protocol, CLI and MCP checks are documented in docs/.
No hardware accuracy claim is made. Licenses and exact source/build revisions
are in licenses/, SOURCE.md and release-manifest.json. No telemetry or updater.
