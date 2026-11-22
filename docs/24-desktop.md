# LOTUS desktop

The GPUI window renders snapshots and sends commands; it never touches a
worker. Session model, layout, gauges and the shared MCP surface are in
[30](30-sessions-and-interface.md); this document covers the plumbing.

`desktop_controller` owns the shared `Supervisor` ([30](30-sessions-and-interface.md))
on a background Tokio runtime. UI commands have a 32-entry bound and each runs
as its own task, so a slow worker never stalls another session. Every key or
touch change submits the complete input state to that session's mailbox
without waiting ([30](30-sessions-and-interface.md#shared-supervisor)), so
releases never depend on command-queue space or another worker.

The controller's main loop never awaits a worker (#21). Every 33 ms (fixed
deadlines; plain `sleep` measured 46.6 ms on Windows) it drains commands and
publishes summaries, catalog and selection. Three tasks do the worker
requests, each one at a time:

- **Preview** of every session the workspace shows: the selected one, or both
  tiles of a split (lotus#65).
- **Inspector**, only while the inspector is open: registers when the paused
  frame changes; live panels every 500 ms; settings and memory map
  (`Panel::live` false) when the view or paused frame changes.
- **Audio focus**, which follows the selection, latest selection wins, and
  retries a failed hand-off every second.

Each request runs as its own task, with at most one per operation and session
in flight (lotus#45). A worker stalled on the previous selection keeps its
exchange, and the newly selected session's first preview, inspection and audio
hand-off start on the next tick. A tick that finds the slot taken is counted as
`saturated`. `frontend_metrics.pending` lists every operation in flight with
its kind, session and age. Audio focus moves to the latest request at once; the
previous holder is muted concurrently and may keep audio until its worker answers.
An exchange in flight is never cancelled, since dropping it would leave its
reply on the pipe. Each result carries the selection generation it started
with and is discarded if the selection, panel or epoch changed meanwhile
(`stale_dropped`). An operation pending over 1 s is named in the toolbar
("waiting: inspection on 3f2a… 2.4 s") instead of freezing other controls.
Loop interval and per-operation latency are published as `frontend_metrics`
in `lotus_status`. `tests/responsive_controller.rs` stalls the selected mock
worker for 4 s: switching to and pausing its sibling completes within 500 ms,
publication continues at about 33 ms, and the stalled worker's late inspector
result is dropped (**verified compiled**, mock workers). A second test stalls only A's capture for 5 s:
B's first preview and inspection arrive in about 70 ms, no kind ever has two
exchanges on A through A→B→A→B switching, and A's late frame is dropped
(**verified**, mock workers).

Window-side counters (lotus#74) are published as `frontend_metrics.ui`:
refresh-task polls started/completed and the heartbeat age, previews installed
and the newest installed sequence (compare `published_preview_sequence`), GPUI
render calls and age, and activation/deactivation counts. They are atomics in
`ui_metrics.rs`, readable over MCP whether or not the window still refreshes. A
bounded `rendered_workspace` snapshot reports the workspace revision and each
tile's session, gauge snapshot epoch and guest frame count, plus its installed
preview epoch, controller sequence and worker sequence. This records the inputs
to GPUI rendering. The OS compositor has its own presentation stage.

A poisoned shared-state mutex is recovered, logged and counted
(`polls_recovered`) instead of leaving the window stale; the poll used to
return silently on poison. `tests/test_focus_cycle.py` cycles focus with real
Alt+Tab, a title-bar click, minimize/restore and rapid Alt+Tab, and names the
first stage that stops. On this machine (2026-11-17, release build, CITnLO and
AZAHAR, t03.3dsx) every stage advanced in every phase: installed trailed
published by at most 2 previews, and renders paused only while minimized. The
Alt+Tab freeze did not reproduce. On 2026-11-22 the strengthened harness passed
with the Windows debug desktop, the packaged CITnLO worker (`a3994c1d`) and
the mock worker. After Alt+Tab back, CITnLO advanced 116 guest frames and
rendered 48 new worker previews over the sampled interval. The guest advanced
116 frames while minimized and preview publication stayed at zero. The
original freeze's cause remains unknown.

Opening the Inspector exposed a crash with Windows accessibility active.
The toolbar and Inspector both registered focus in one GPUI frame because
the shell transferred focus during layout. `b0b05a0` defers that transfer
until the frame is built. `tests/test_focus_cycle.py --inspector-only` uses
Windows UI Automation to locate and click the toolbar button, then checks that
the accessibility subtree opens and closes. The old debug binary panicked
with `set_focus called more than once in a single frame`. The fixed binary
passed with CITnLO and the mock worker. macOS remains unverified.

Only sessions the workspace shows are previewed. `Update::previews` holds the
newest screen pair per shown session; a session that leaves the view is removed
from it, and a late result for it is dropped (`stale_dropped`). In the window the
latest pair replaces that tile's previous one, and replaced images are dropped
from GPUI's sprite atlas. Previews come from the worker's shared-memory ring and fall back to
`capture` on workers without it ([21](21-native-control.md#ownership-and-budgets)).
There is no frame history. With `--mcp` the same runtime serves
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
rail selection, Step, session rows) reach the window; working set held
at ~96.6 MB over 20 s of continuous preview replacement. Actual two-DPI
interaction, host audio and other platforms remain unverified.

Source: `desktop::{desktop_controller, lotus_window, layout, icons,
preferences, presence, session_tree, gauges, theme, screen_geometry}`, `session::supervisor`. Primary GPUI APIs at its pinned
source: `elements/{img,canvas,div}.rs`, `assets.rs` (`RenderImage` BGRA),
`app.rs` (`drop_image`, `prompt_for_paths`), `app/context.rs` (activation/focus).
Rollback: run headless (`lotus-mcp`, `ctr-cli`); no GUI is required.

Preview frames arrive in top-left row-major BGRA8. The desktop validates the
native dimensions, stride, length, and shared epoch and sequence of each
top/bottom pair. It transfers the owned pixel buffers directly to GPUI's
BGRA `RenderImage`. On Windows, GPUI uploads them to a BGRA DirectX atlas.
Raw capture artifacts keep the native channel order and the MCP PNG encoder
converts its own copy to RGBA. [Preview validation](../crates/desktop/src/preview_image.rs)
· [PNG encoder](../crates/mcp/src/screenshot.rs)

**Verified emulator** on Windows with the Release integration worker and
`t03.3dsx`. The [rendered color test](../tests/test_preview_colors.py)
matches native captures against Windows window pixels for both screens in
single and split layouts, with software and Vulkan. Unit tests check row
order, alpha, invalid leases, and that GPUI receives the original allocations.

Wave 3 inspectors (**verified compiled**, controller/model **verified emulator**):
CPU/VFP and disassembly, threads/waits, breakpoints, memory maps, paginated
settings, IPC/logs, frozen GPU captures and shader/texture inspection, audio,
checkpoint selection and event lanes. Boolean settings are clickable; other
commands are available in **Debugger Commands** from the Debug or Inspector
Utilities menu. Search by command or group, then fill in the shared schema’s
argument form. Integer fields accept decimal or `0x` hexadecimal. Structured
fields accept JSON and optional fields can be left blank for their defaults.
Use **Preview JSON** to inspect the payload, **Raw JSON** to edit it directly,
or **Run Clipboard JSON** to send a clipboard command to Result. The palette
shows its session, guest epoch and permission state and returns results inline.
Command input is limited to 64 KiB. Form drafts are kept within one window and
one live guest epoch.
Execution controls share debug leases. Capture/trace
buttons explicitly enable instrumentation; GPU replay requires state restore or
reset before CPU resume.

Inspector data is fetched on the background runtime only while open. Live
panels refresh on a half-second cadence; imported/result views remain fixed.
JSONL imports parse on a blocking worker, bounded by 64 MiB input, 100000 rows
and 32 MiB retained text. Live event lists retain at most 128 rows per request.
Draw lists retain at most 4096 headers per reply. The pinned CITnLO worker
retains eight draws. The virtualized table renders the visible range only.
Snapshot rows restore named states; frozen texture rows decode to bounded image
previews. Selecting successive draws produces bounded structural state diffs.

Inspector actions keep their result and error in the panel where they started.
Use **Show panel** to return to its live data and **Show result** to reopen the
details. Clipboard commands and imported traces open Result. The inspector remembers
each live session's page separately for Settings, IPC, Logs and Timeline.
The GPU panel opens draw details in place and highlights the selected draw
when you return to the list, while that draw is still retained by the worker.
Paging, draw selection and retained results survive a panel change, a split tile
switch and a paused frame advance. Reset, reload, worker/backend replacement or
session removal expires that guest's selections, results and images. Closing the window
clears this state. The saved inspector panel is a window preference.

Polling replies carry the session identity, guest epoch, panel and requested
cursor. Only the current page can supply rows and actions. The panel shows
loading, empty and worker error states separately from explicit command status.

**Cancel** removes a request waiting for the worker or a snapshot slot. An
issued command drains to its worker reply and its result is discarded. Any
effects of an issued mutation can still occur. Desktop capture reads check
for cancellation between chunks. New inspector polls on that worker wait
for explicit commands to finish.

`wave3-native-inspector-01` covers both models through the controller, including
100000-event import and closed manifests. The Windows native inspector test also
checks that a live GDB lease rejects a desktop mutation with a reason, allows a
desktop memory read and restores desktop control on detach.
`wave3-inspector-tests-03` and later
checks cover the bounded view model. **Checked by screenshot**
(`w3c-desktop-01`, 2026-11-16, PrintWindow captures with posted mouse
messages, one 100%-scale display): every inspector panel renders live CITnLO
data from a paused session — CPU registers/disassembly, threads, breakpoints,
memory map, paged settings with apply classes, IPC with its loss counter,
logs, GPU draw, audio regions, snapshots and timeline. Two-DPI interaction
remains unverified.

### Windows idle redraws (#158)

The controller wakes GPUI for changed sessions, previews, layouts and debugger
results. Arrivals within 16 ms share one poll. A 150 ms heartbeat samples device assignments and infrequent host
changes. The window compares the data its controls and gauges display before
invalidating. Restoring a minimized window samples immediately. The controller
keeps its 33 ms supervisor cadence and serializes MCP frontend metrics at most
once every 100 ms. Session rows, card and asset catalogs, and debugger history
share immutable snapshots. Identical inspector replies reuse the parsed view
for that target and query.

**Verified emulator** on Windows 11 with the debug desktop, owned
`t03.3dsx` and Release AZAHAR `5ecbd982a9159fb910f81d8e19fbfbe04facf363`
(Ninja, MinGW, worker and Vulkan compiled in, Qt and SDL2 off, software
renderer selected). The baseline desktop is `c33f8c0`. Each sample is three
seconds after settling. CPU usage is for the desktop process as a percentage
of one logical core. Idle shows one paused tile when sessions exist. Running
shows one tile for one session and two split tiles for two or four sessions.
With four loaded, the other two stay paused.

| Loaded | Window | Renders before/after | Polls before/after | CPU % before/after | Preview installs before/after | Guest frames before/after |
|---:|---|---:|---:|---:|---:|---:|
| 0 | empty | 12/0 | 64/19 | 5.7/2.1 | 0/0 | — |
| 0 | hidden empty | 0/0 | 63/19 | 9.9/0.5 | 0/0 | — |
| 1 | paused | 12/0 | 64/21 | 10.9/1.6 | 0/0 | 0/0 |
| 1 | running | 12/16 | 63/32 | 12.5/12.5 | 19/10 | 16/17 |
| 1 | hidden running | 0/0 | 63/19 | 4.2/0.5 | 0/0 | 17/17 |
| 2 | paused | 12/0 | 63/21 | 12.0/1.6 | 0/0 | 0/0 |
| 2 | running | 12/30 | 64/43 | 18.7/25.5 | 21/23 | 27/29 |
| 2 | hidden running | 0/0 | 63/19 | 2.1/1.6 | 0/0 | 28/26 |
| 4 | paused | 12/0 | 63/25 | 21.3/2.6 | 0/0 | 0/0 |
| 4 | running | 11/26 | 61/37 | 21.3/34.3 | 27/21 | 25/28 |
| 4 | hidden running | 0/0 | 63/20 | 2.1/3.1 | 0/0 | 27/28 |

Each GPUI poll acquires the shared update mutex once. Supervisor ticks stayed
at 89–94 per sample. Metrics serializations went from one per tick to 24–25
per sample. With 0/1/2/4 paused sessions the controller published 0/3/6/13
changed session snapshots, down from roughly 90 assignments per sample.
Idle CPU and render count fell. The short software playback samples varied
in both preview install rate and CPU use. GPUI polls and renders are
application counters. OS wakeups and GPU presentation were not measured.

Run `tests/test_idle_cadence.py <desktop> <worker> <3dsx> <new-output-dir>`.
The harness saves per-phase counters and SHA-256 screen checkpoints.
The two-session checkpoint matched on both screens (`e5540f2e…`, `da0e7076…`).
Playback between samples changes the scene captured by other checkpoints.
`--baseline` records the old idle behavior for comparison.

The Vulkan run used packaged CITnLO `a3994c1da5d2c6e00731a20c578a54a1b6d38fc7`.
It recorded zero settled idle renders with 0/1/2/4 loaded sessions, including
a single paused session with the Inspector open. Two visible sessions
installed 150 previews and advanced 357 guest frames in three seconds.
While minimized they advanced 359 guest frames with zero preview requests.
Restoring advanced the preview sequence on both tiles. The same harness
checks that removing and re-adding a paused tile can reuse the worker's
validated exact-frame slot once. Playback still suppresses repeated slots.

The CITnLO focus-cycle test also passed Windows UI Automation inspector
open/close, Alt+Tab, title-bar activation and minimize/restore. Installed
preview sequence matched the published sequence in every visible sample.
