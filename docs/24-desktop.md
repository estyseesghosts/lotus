# LOTUS desktop

The GPUI window renders snapshots and sends commands; it never touches a
worker. Session model, layout, gauges and the shared MCP surface are in
[30](30-sessions-and-interface.md); this document covers the plumbing.

`desktop_controller` owns the shared `Supervisor` ([30](30-sessions-and-interface.md))
on a background Tokio runtime. UI commands have a 32-entry bound and each runs
as its own task, so a slow worker never stalls another session. Complete input
states coalesce separately (8 ms flush), so key/touch releases never depend on
command-queue space. Only the selected session is previewed; the latest screen
pair replaces the previous one, and replaced images are dropped from GPUI's
sprite atlas. There is no frame history. With `--mcp` the same runtime serves
MCP on stdio over the same supervisor.

Launch: `lotus-desktop [--mcp] host.json` ([23](23-local-mcp.md) host config).
Without a configuration the window explains how to start one. Desktop loads
start running; MCP loads stay paused. Click/drag the bottom screen for touch;
focus loss, deactivation, selection and layout changes release input. Touch maps
through the bottom image's actual layout rectangle; simulated 1.25x/2x DPI
inversion tests cover native corners (`screen_geometry`).

Worker failures clear displayed pixels and show a crashed state; other workers
continue in their processes. Closing the window stops every session (3 s
deadline each, job termination as fallback), discarding secondary data; an
already pending native exchange can take up to its 30 s deadline.

**Verified emulator**: `native_controller` (primary/secondary, concurrency,
crash isolation, shared MCP, persistence; see [30](30-sessions-and-interface.md)).
**Checked by screenshot** (Windows 11, one 100%-scale display, debug build,
2026-11-16): native frames render; keyboard (Ctrl+I) and mouse (menu, collapse,
rail selection, Step, Inspect, session rows) reach the window; working set held
at ~96.6 MB over 20 s of continuous preview replacement. Actual two-DPI
interaction, host audio and other platforms remain unverified.

Source: `desktop::{desktop_controller, lotus_window, session_tree, gauges,
theme, screen_geometry}`, `session::supervisor`. Primary GPUI APIs at its pinned
source: `elements/{img,canvas,div}.rs`, `assets.rs` (`RenderImage` BGRA),
`app.rs` (`drop_image`, `prompt_for_paths`), `app/context.rs` (activation/focus).
Rollback: run headless (`lotus-mcp`, `ctr-cli`); no GUI is required.
