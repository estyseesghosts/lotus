LOTUS portable for Windows x64

Extract the entire ZIP to a writable folder, then double-click LOTUS.cmd.
Sessions live in the sidebar. A primary session keeps its own persistent SD card
(data/<backend>/sessions/cards/<name>); a secondary is a disposable copy whose
writes are discarded when it stops. Use + to start one, then Open file... to pick
your own .3dsx. LOTUS.cmd uses CITnLO. LOTUS-AZAHAR.cmd uses the upstream-tracking
AZAHAR backend. Dragging a .3dsx onto either launcher also lists it in LOTUS.

No installer, administrator access, Rust, compiler or Python is required to run.
Windows 11 x64 is the initial supported host; the GPUI desktop uses Direct3D 11.
Keep all files together. Sessions, logs and saves live in data/<backend>/sessions.
Move the entire folder to move the installation. Select the homebrew again after
moving: the launcher refreshes machine-specific asset/runtime paths each time.
Closing LOTUS terminates its workers. Keep data/ when upgrading to retain runs.

bin/ctr-cli.exe provides headless scenario runs. MCP is served over stdio only,
headless (-Mode MCP) or by the window itself (-Mode WindowedMCP), where you watch
and use the same sessions the agent drives. For a portable MCP command:
powershell.exe -NoProfile -ExecutionPolicy Bypass -File <folder>/launch.ps1 -Mode WindowedMCP -Fixture <homebrew.3dsx>
No network listener.

This initial developer release is unsigned. Software rendering only; audio is
muted. No ROMs, Nintendo firmware, DSP dump, keys or user captures are included.
Visible rendering and mouse/keyboard input were checked by screenshot on one
100% scale display; actual two-DPI acceptance remains unverified. Automated
native lifecycle, isolation, protocol, CLI and MCP checks are documented in docs/.
No hardware accuracy claim is made. Licenses and exact source/build revisions
are in licenses/, SOURCE.md and release-manifest.json. No telemetry or updater.
