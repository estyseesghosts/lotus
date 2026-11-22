LOTUS 0.1.2 adds a desktop command palette for the shared debugger
commands. Inspector requests and results stay with their session and
panel, and queued requests can be canceled.

On a CITnLO session, **Debug → GDB** starts or stops a local GDB listener and
shows when an attached client has exclusive control. The GPU inspector
configures capture, reports retained and dropped draws, compares two
selected captures, and previews textures from frozen capture bytes.

Both native runtimes are unchanged from 0.1.1: AZAHAR `5ecbd982` and CITnLO
`a3994c1d`.

On Windows x64, extract the ZIP and run `LOTUS.cmd` with your own
`.3dsx` homebrew. `LOTUS-AZAHAR.cmd` selects the upstream-tracking
backend. On Linux x86-64, make the AppImage executable and pass it the
path to your homebrew. Both packages include the desktop, headless CLI,
local MCP server, both runtimes, fonts, license notices and build manifests.

Windows requires Windows 11 x64 and Direct3D 11. Linux requires glibc 2.39 or
newer. The packages are unsigned.
See the [release notes](https://github.com/estyseesghosts/lotus/blob/v0.1.2/docs/35-release-notes.md#v012)
for details and linked evidence.
