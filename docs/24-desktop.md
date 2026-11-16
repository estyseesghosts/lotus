# LOTUS desktop

The GPUI desktop consumes the shared native supervisor through a background
controller. UI commands have a 32-entry bound. Complete input states coalesce
separately, so key/touch releases do not depend on command-queue space. Latest
snapshots replace old images; only the selected worker is captured. At most four
sessions retain two GPUI images each, plus the controller's latest selected
screen pair. There is no frame history.

Launch with a local host JSON ([23](23-local-mcp.md)). `New: <asset>` loads an
approved asset paused. Session rows select workers. Run/Pause, Reset and Stop
control the selection. Zoom toggles 1x/1.5x; Dock screens toggles stacked/side-by-side.
Keyboard: Z/X → A/B, A/S → X/Y, arrows, Q/W → L/R, Enter/Backspace → Start/Select.
Click/drag the bottom screen for touch. Focus loss, deactivation, selection and
layout changes release input. GPUI logical coordinates use the image's actual
layout rectangle; simulated 1.25x/2x DPI inversion tests cover native corners.

Worker failures clear displayed pixels and show a lifecycle warning. Other
workers continue in their processes. Closing the controller stops its workers,
with a three-second stop deadline per worker and job termination as fallback.
An already pending native exchange can take up to its 30-second deadline.

**Verified compiled**, pinned GPUI/Windows GNU. **Verified emulator** without UI
automation: `native_controller` tests two sessions, frame streaming, switching,
reset, sibling crash isolation, stop and close cleanup.
Visible rendering, interactive input and actual two-DPI acceptance are deferred
at the user's instruction to continue without Computer Use. Do not count them as
verified. Accelerated rendering, host audio playback and other platforms remain
untested.

Source: `desktop::{desktop_controller,screen_geometry}`, desktop `main.rs`,
`session::{supervisor,native_session}`. Primary GPUI APIs at its pinned source:
`elements/{img,canvas}.rs`, `assets.rs` (`RenderImage` BGRA), `app/context.rs`
(window activation/focus). Rollback: run CLI/MCP instead; no GUI is required for
native scenarios.
