# T02 native screen spike

**Verified compiled**: Windows GNU, pinned GPUI
`6b720973027999bae402df23d469026f4cced988`, Rust 1.98.1.
Native Azahar baseline remains `11eceda5e0e682a1f44e396bf954a1b9137097ca`;
this ticket does not link it or change native behavior.

Run `cargo run -p lotus-desktop --bin lotus-screen-spike`.
The separate executable continuously replaces eight native BGRA images for four
synthetic sessions. It displays one session, changes selection every five host
seconds and alternates 1x/1.5x display scale every twenty seconds. Hidden sessions
have no image elements. Only the latest eight CPU images are retained; no native
Vulkan interop or emulation claims are made. Arc is needed by GPUI's Render image
source API. Source-inspected API: GPUI `assets.rs`, `RenderImage::new`, BGRA data.

`cargo test -p lotus-desktop --lib` exercises native geometry, changing image
contents, outside clicks, invalid transforms, four zoom levels, and physical
coordinate inversion at 1.25x/2x DPI. These are simulated DPI tests, not evidence
of two actual Windows display configurations. TouchTransform uses the actual
bottom-image rectangle, excluding letterbox space, and never emits coordinates
for outside presses.

The process was launched for observation on this host. Initial samples were
92,753,920 bytes working set / 66,912,256 private bytes / 872 handles; a later
sample was 92,733,440 / 66,834,432 / 868. These are host resource observations,
not guest timings or proof of visible presentation. Generation time is displayed
in host microseconds. The computer-use tool rejected access to the executable;
visual inspection and actual two-DPI interaction acceptance remain pending.
At ten minutes (19:50:06–20:00:06 local, 2026-11-15), working set was
93,011,968 bytes, private memory 66,850,816 bytes and handle count 868.
CPU time was 332.8 host seconds. Live touch interaction and actual two-DPI
presentation remain acceptance gaps. Existing .rsrc manifest linker warning remains visible.

Visual and actual two-DPI interaction checks are deferred at the user's request
to continue without Computer Use. Rollback: disable the separate spike executable;
its geometry module is now also used by the native desktop. Linux/macOS remain untested.
