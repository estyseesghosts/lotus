LOTUS 0.1.0 uses Silvestris for the desktop and Settings. New profiles use
Aqua Light. Settings → General switches Style and Appearance without closing
your sessions. Saved themes, pane sizes and visibility are retained.

Both native runtimes keep continuous Vulkan previews nonblocking between run
batches and report Windows adapter memory pressure for session admission.
CITnLO supports background rewind compression with a configurable queue.

On Windows x64, extract the ZIP and run `LOTUS.cmd` with your own
`.3dsx` homebrew. `LOTUS-AZAHAR.cmd` selects the upstream-tracking
backend. On Linux x86-64, make the AppImage executable and pass it the
path to your homebrew. Both packages include the desktop, headless CLI,
local MCP server, both runtimes, fonts, license notices and build manifests.

Windows requires Windows 11 x64 and Direct3D 11. Linux requires glibc 2.39 or
newer. The packages are unsigned.
See [the integration evidence](https://github.com/estyseesghosts/lotus/blob/v0.1.0/docs/38-silvestris-adoption.md)
for the tested desktop, input, worker and package paths.
