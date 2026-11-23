LOTUS 0.1.4 records live play as replayable scenario tests, from MCP, the
CLI or **Debug → Record Test** on Windows. Windows investigations gain
editing, evidence archives, retained regression suites and guarded first-bad
searches. Both native workers apply live input at frame boundaries and bound
WAV capture, GPU memory sampling and control-request backlog.

Native runtimes: AZAHAR `7b45415d` and CITnLO `64883d29`.

On Windows x64, extract the ZIP and run `LOTUS.cmd` with your own
`.3dsx` homebrew. `LOTUS-AZAHAR.cmd` selects the upstream-tracking
backend. On Linux x86-64, make the AppImage executable and pass it the
path to your homebrew. Both packages include the desktop, headless CLI,
local MCP server, both runtimes, fonts, license notices and build manifests.

Windows requires Windows 11 x64 and Direct3D 11. Linux requires glibc 2.39 or
newer. The packages are unsigned.
See the [release notes](https://github.com/estyseesghosts/lotus/blob/v0.1.4/docs/35-release-notes.md#v014)
for details and linked evidence.
