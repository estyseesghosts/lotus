LOTUS 0.1.3 expands the desktop debugger. It adds a hex memory viewer with
guarded writes, a breakpoint and watchpoint manager, CPU execution controls,
a process and thread explorer, an event timeline with CiTrace export, rewind,
named checkpoints and movies, memory scanning, and pointer inspection.

Source debugging binds symbols only to the guest build verified in memory,
steps source lines, and reads DWARF variables through typed expressions.
Settings gains search, per-card overrides, controller assignment with live
input preview, and explicit labels for renderer and DSP choices.

On Windows, **WindowedMCP** clients using the same host configuration share
one desktop and its workers. Closing the shared window minimizes it to the
taskbar. Use **Quit Lotus** to stop the host. Named MCP investigations keep
debugging notes and verified source breakpoint definitions across restarts.

Both native runtimes are unchanged from 0.1.2: AZAHAR `5ecbd982` and CITnLO
`a3994c1d`.

On Windows x64, extract the ZIP and run `LOTUS.cmd` with your own
`.3dsx` homebrew. `LOTUS-AZAHAR.cmd` selects the upstream-tracking
backend. On Linux x86-64, make the AppImage executable and pass it the
path to your homebrew. Both packages include the desktop, headless CLI,
local MCP server, both runtimes, fonts, license notices and build manifests.

Windows requires Windows 11 x64 and Direct3D 11. Linux requires glibc 2.39 or
newer. The packages are unsigned.
See the [release notes](https://github.com/estyseesghosts/lotus/blob/v0.1.3/docs/35-release-notes.md#v013)
for details and linked evidence.
