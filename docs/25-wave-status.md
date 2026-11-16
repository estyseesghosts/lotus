# Wave 0–2 delivery record

2026-11-15, Windows GNU. Project: **LOTUS**. Backends: **AZAHAR** (`integration`),
**CITnLO** (`experimental`), future **LOTUS**. Current immutable native revision
and all build flags are in config/pins.toml/build manifests; upstream control is
`11eceda5e0e682a1f44e396bf954a1b9137097ca`. No hardware accuracy claim is made.

| Ticket | Implemented delivery and evidence |
| --- | --- |
| T00 | Existing pinned Cargo scaffold retained |
| T01 | Clean stock/integration/experimental worktrees, separate caches, stock 60-frame fixture boot, patch/build inventory ([20](20-native-baseline.md)) |
| T02 | Native GPUI synthetic images, four sessions, 10-minute resource observation, geometry at simulated 1.25x/2x DPI ([19](19-screen-spike.md)); actual presentation/DPI interaction deferred |
| T03 | Bounded private pipes, version/identity/epoch checks, typed errors, mock CLI and process failure tests ([18](18-control-pipes.md)) |
| T04 | Default-off native CMake target; Qt/SDL/Libretro off; independent stock control |
| T05 | Native `.3dsx` boot, lifecycle, bounded run, unsupported keyboard/Mii callbacks |
| T06 | Native BGRA screen ring, metadata, copied ownership; stock bytes match, stale slot/load/reset/crash leases rejected |
| T07 | HID buttons/touch, exclusive scenario input; timed replay hashes; bounded muted audio ring and disconnect self-test |
| T08 | Safe-point queue, limiter wake, load/run/pause/reset/status/stop and bounded read-only CPU/memory queries |
| T09 | Private paths/root locks, retained manifests/logs, Windows jobs including descendants; relaunch/isolation/failure tests ([21](21-native-control.md)) |
| T10 | Worker-backed GPUI browser, streaming, controls, keyboard/touch, zoom/docking; native controller tested; visual acceptance deferred ([24](24-desktop.md)) |
| T11 | Bounded CLI scenarios, hashes/checkpoints, three identical fresh-root runs ([22](22-scenario-runner.md)) |
| T12 | Four workers running the same fixture with private roots; one killed during run, siblings continue; four-slot policy and bounded artifacts |
| T13 | Scoped MCP tools/resources, explicit host control permission, native-byte parity and stdio end-to-end checks ([23](23-local-mcp.md)) |

All requested implementation is present. Two visual acceptance checks remain
unverified because the user instructed continuation without Computer Use:
visible GPUI rendering/input and actual two-DPI interaction. Do not treat
mathematical DPI tests or a live process as proof of those checks.

Audio-ring behavior is tested with synthetic PCM; the supplied fixture produced
no audio. Audible output is deliberately unavailable in the initial muted profile.
Two concurrent drawbench guests wrote independent SD CSV files and matched the
stock oracle's screens ([20](20-native-baseline.md)). Other title-save formats,
host platforms and accelerated renderers remain untested. The provided DSP dump
remains external and unused by these HLE fixtures.
No ROM/firmware/log/capture assets are committed or uploaded. Native commits are
local until the owner publishes/transfers them.

Reproduce: `scripts/smoke.ps1`, then `scripts/test-native.ps1` with explicit worker
and fixture paths. Native tests are opt-in, so their ignored default status must
not be counted as execution. The stdio/resource test is
`python tests/test_mcp_native.py <server> <host-config> <asset-handle>`.
Stock/native screen hashes and three-run reports are summarized in documents
20/22; local raw artifacts stay in ignored build directories.

Final checks passed on both AZAHAR and CITnLO: three native session/resource
tests, desktop controller crash/isolation test, MCP native parity and malformed
wire tests. The workspace build/tests, formatting, Clippy with warnings denied
and pin verification passed. GNU linking still reports GPUI's duplicate Windows
resource-manifest warning; visible DPI behavior remains unverified.

Primary source anchors: native System pending-work APIs in
[core.h](../vendor/azahar/src/core/core.h) and [core.cpp](../vendor/azahar/src/core/core.cpp);
software screen layout in
[renderer_software.cpp](../vendor/azahar/src/video_core/renderer_software/renderer_software.cpp);
HID device polling in [hid.cpp](../vendor/azahar/src/core/hle/service/hid/hid.cpp);
audio submission in [dsp_interface.cpp](../vendor/azahar/src/audio_core/dsp_interface.cpp).
The dossier's broader debugger/accuracy APIs remain future targets for Waves 3–4.
