LOTUS 0.1.1 applies live setting changes to running cards. Volume and other
live settings reach mounted sessions at once. Boot, renderer and CFG changes
are marked **Restart required** in Settings and apply the next time the card
starts. MCP and CLI setting writes report each change as applied, pending or
rejected.

The new MCP tool `lotus_capture_image` returns both screens as lossless PNG
images, with an optional 1–4× scale. `lotus_capture` still returns raw BGRA
artifacts.

Session policy adds `rewind_memory_mib`, which limits the memory reserved
for rewind recording (default 1536 MiB in interactive mode), and
`snapshot_queue_limit`, which bounds queued snapshot requests (default 16).
Reset and content load now release rewind and GPU-capture slots. Worker
executable hashes are cached by file identity, so a replaced worker is always
rehashed.

Both native runtimes are unchanged from 0.1.0: AZAHAR `5ecbd982` and CITnLO
`a3994c1d`.

On Windows x64, extract the ZIP and run `LOTUS.cmd` with your own
`.3dsx` homebrew. `LOTUS-AZAHAR.cmd` selects the upstream-tracking
backend. On Linux x86-64, make the AppImage executable and pass it the
path to your homebrew. Both packages include the desktop, headless CLI,
local MCP server, both runtimes, fonts, license notices and build manifests.

Windows requires Windows 11 x64 and Direct3D 11. Linux requires glibc 2.39 or
newer. The packages are unsigned.
See [the release notes](https://github.com/estyseesghosts/lotus/blob/v0.1.1/docs/35-release-notes.md)
for details and linked evidence.
