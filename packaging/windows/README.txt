LOTUS portable for Windows x64

Extract the entire ZIP to a writable folder, then double-click LOTUS.cmd.
Sessions live in the sidebar. A primary session keeps its own persistent SD card
(data/<backend>/sessions/cards/<name> by default); a secondary is a disposable copy whose
writes are discarded when it stops. Use + to start one, then Open file... to pick
your own .3dsx. LOTUS.cmd uses CITnLO. LOTUS-AZAHAR.cmd uses the upstream-tracking
AZAHAR backend. Dragging a .3dsx onto either launcher also lists it in LOTUS.

No installer, administrator access, Rust, compiler or Python is required to run.
Windows 11 x64 is the initial supported host; the GPUI desktop uses Direct3D 11.
Keep all files together. Sessions, logs and saves live in data/<backend>/sessions;
desktop preferences in data/desktop.json.
Move the entire folder to move the installation. Select the homebrew again after
moving: the launcher refreshes machine-specific asset/runtime paths each time.
Closing LOTUS terminates its workers. Keep data/ when upgrading to retain runs.
To keep data elsewhere: LOTUS.cmd -SetDataDir "D:\LOTUS data" (saved per Windows
user in %APPDATA%\LOTUS\lotus.json), or a lotus.json beside launch.ps1 with
{"data_dir": "<path>"}. Paths above under data/ then live in that directory.

bin/ctr-cli.exe provides headless scenario runs. MCP is served over stdio only,
headless (-Mode MCP) or by the window itself (-Mode WindowedMCP), where you watch
and use the same sessions the agent drives. For a portable MCP command:
powershell.exe -NoProfile -ExecutionPolicy Bypass -File <folder>/launch.ps1 -Mode WindowedMCP -Fixture <homebrew.3dsx>
No network listener.

This developer release is unsigned. Sessions use Vulkan and fall back to
software rendering when the worker refuses Vulkan setup; audio is muted.
Full user guide: docs/37-user-guide.md in the source ZIP or on GitHub. No ROMs, Nintendo firmware, DSP dump, keys or user captures are included.
Visible rendering and mouse/keyboard input were checked by screenshot on one
100% scale display; actual two-DPI acceptance remains unverified. Automated
native lifecycle, isolation, protocol, CLI and MCP checks are documented in docs/.
No hardware accuracy claim is made. Licenses and exact source/build revisions
are in licenses/, SOURCE.md and release-manifest.json. No telemetry or updater.
