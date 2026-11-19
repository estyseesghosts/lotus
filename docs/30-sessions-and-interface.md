# 30 — Sessions, cards and the LOTUS interface

How LOTUS is driven: **headless** (`lotus-mcp`) or **windowed**
(`lotus-desktop`, optionally `--mcp`). Both drive one shared supervisor with
one control model, so everything an agent can do headless it can do against a
window a person is watching. Builds on [23](23-local-mcp.md) (MCP policy) and
[24](24-desktop.md) (desktop plumbing); terms in the [glossary](glossary.md).

## Primary and secondary sessions

| | Primary | Secondary |
| --- | --- | --- |
| Worker root | `<storage>/cards/<card>/` | `<storage>/scratch/<session>/` |
| Guest SD/NAND | the card's own `sdmc/`, `nand/`, `sysdata/`; real reads/writes that persist | empty, or a copy of one card's guest directories at one guest instant |
| On stop | card kept | root deleted; after a crash or failed stop only logs are kept |
| Concurrency | one primary per card (registry check + the worker's `worker.lock`) | any number, limit permitting; several may copy one card |
| Sidebar | top-level row | nested under the card it copied; a parentless secondary is top-level |

A card is created on first use; names are 1–32 of `a-z 0-9 - _`. The guest
directory names are Azahar's (`SDMC_DIR`, `NAND_DIR`, `SYSDATA_DIR` in
`src/common/common_paths.h`), selected by `FileUtil::SetUserPath(root)` in
`ctr_worker/frontend.h`. Copies over 4 GiB, symlinks and junctions are refused.
On start, a host discards guest data in any unlocked `scratch/` root left by a
previous process. This is a private-copy design, as
[10](10-multi-instance-and-storage.md) prefers over unproven copy-on-write.

### Copying a card in use (#28)

Before #28 a secondary copied a card while its primary kept running, so a copy
could pair files from different moments: a state the guest never had. Now
`Supervisor::clone_card` holds the primary's session for the whole copy:

1. A running primary is paused.
2. `flush_storage` ([21](21-native-control.md)) must succeed. Guest FS calls
   run on host threads (`HLERequestContext::RunAsync`, `kernel/hle_ipc.h`) and
   count as pending until the guest thread wakes; while one is pending, a
   primary that was playing advances one frame and retries (at most 30). A
   primary the user paused is never advanced; the copy is refused with
   `ResourceBusy` ("paused mid-operation, run it briefly and retry").
3. The guest directories are copied into `scratch/<id>.partial` off the async
   runtime and renamed to `scratch/<id>` when complete; a failed or crashed
   copy never exposes a root, and the next host removes `*.partial`.
4. A primary that was playing resumes.

A copy requested while the card's primary is still starting waits for that
creation to commit or fail, at most 30 s (the worker spawn deadline), so a
secondary created together with its primary succeeds
(`native_process.rs` `supervisor_bounds_concurrent_sessions_and_retains_scoped_artifacts`).
A card with no primary here is copied while new primaries on it are refused; a
card locked by another LOTUS process is refused, since its writes cannot be
paused from here. Workers without `flush_storage` are refused with
`Unsupported`. `fork` (CITnLO) also flushes after its savestate, so the copied
card matches the checkpoint.

The copy is one guest instant, not a transaction boundary: a guest that was
mid-way through saving several files is copied mid-way, as a power cut would
leave it. Guest `FSFILE_Write` reaches the OS when the call completes, since
`Service::FS::File::Write` calls `GetSize`, whose seek flushes stdio
(`src/common/file_util.cpp`, `GetSize(FILE*)`); the flush covers other stdio
writers.

Evidence. `crates/session/tests/card_clone.rs` (mock worker whose guest
rewrites a pair of files): 50 direct reads of the running card produce torn
pairs, 20 coordinated copies produce none and the primary keeps playing;
pending FS work advances a playing primary three frames and refuses a paused
one without advancing it; old workers and foreign-locked cards are refused
(**verified compiled**). `tests/test_mcp_card_clone.py` with the owned
filesystem fixture in churn mode (one file rewritten each 4 ms without
`FS_WRITE_FLUSH`) on CITnLO software: a copy of the paused primary, five copies
of the running primary and three forks each hold one whole generation; forks
are byte-identical with identical guest counters; passed 4 of 4 runs
(`runs/clone-28-01`..`04`, **verified emulator**). Reading the card directly at
a frame boundary once showed generation 118 on disk while the guest had
completed 117 (a write still on its host thread). Both workers answer
`flush_storage` as specified (`runs/flush-probe-*`).

### Startup cost (#30)

`NativeSession::spawn` records the worker's SHA-256 in every manifest. It used
to read the whole ~86 MB executable per spawn; hashes are now cached per
process by canonical path and reused only while length, modification and
creation time are unchanged, and stored only if they did not change while
hashing, so a rebuilt worker is rehashed (`cached_hashes_follow_rewrites_of_the_same_length`).
A same-length rewrite within one timestamp tick would go unnoticed.

`"shader_seed": true` (host config, default off) seeds each new root without
`shaders/` from `<storage>/shader-seed/<worker sha256>/shaders/`
(`crates/session/src/shader_seed.rs`). Workers write only their private copy.
After a clean stop the supervisor publishes the root's cache files into the
seed, each through a temporary name and a rename, one stop at a time; a
crashed worker never publishes. Azahar validates the Vulkan pipeline cache
header against vendor and device before use and deletes an invalid copy
(`PipelineCache::IsCacheValid`, `vk_pipeline_cache.cpp`). Homebrew has program
ID 0, so different homebrew share cache file names, as they do in one stock
Azahar user directory. Telemetry reports `shader_seed`
(`seeded|cold|kept|off`) and `startup_ms`. `tests/test_mcp_shader_seed.py` on
CITnLO Vulkan with t03: the first session starts `cold` and publishes the
pipeline cache and four transferable caches; the next starts `seeded` and its
log shows the cache loaded; killing a worker publishes nothing
(`runs/seed-30-01`, **verified emulator**). Startup was 3364 then 816 ms on
this busy host, which also includes the hash cache hit; the default stays off
until `tools/bench_parallel.py --shader-seed` measures it on a quiet host.

Copy-on-write was evaluated and not adopted: Windows block cloning
(`FSCTL_DUPLICATE_EXTENTS_TO_FILE`) needs ReFS, and the default NTFS volume has
none, so private copies remain. Clone latency and bytes for 2/4/8 forks are
measured by `tools/bench_parallel.py` ([34](34-performance-and-issue-triage.md)).

## Shared supervisor

`session::supervisor::Supervisor` is thread-safe. Each session has its own async
lock; the registry lock is never held across a worker exchange. A 600-frame
`run_until` on one session therefore never delays another session's commands,
status or preview. Creation reserves a slot (and the card) before the slow
spawn, so concurrent creations respect `max_sessions` (default 4, the tested
load; configurable to 16).

Each session has its own **supervision task** and **input task**
(`supervisor/supervision.rs`), started when its creation commits and ended when
it stops. There is no host-wide pump or barrier: a worker whose pipe stalls
delays only its own tasks (#19, #20).

- Supervision polls status every 100 ms (1 s, without IPC, once the worker has
  stopped or crashed), re-issues bounded `run` batches for a playing session,
  and folds replies into telemetry. A session held by another command is
  counted busy and skipped, but after 4 skips in a row the poll queues for the
  session lock. Without that, back-to-back client commands on one session
  starved it: 0 polls and 0 continuations in 8 s, so its run sat paused at the
  600-frame budget (`tools/bench_parallel.py`, `runs/bp-real-01`). When a
  running batch is expected to reach its budget within one period, the next
  poll is due 1 ms after that (at least 5 ms), so an unthrottled session does
  not idle up to 100 ms per batch. The next poll is scheduled from
  completion, so a slow exchange never causes catch-up bursts. A batch that
  ended with a
  debugger stop, `unsupported_applet` or `core_error` stops playback;
  `host_deadline` (slow batch) does not.
- `submit_input` queues a complete input state and returns. The input task
  waits for the session, then sends the newest state. A newer state replaces
  the queued tail unless that would revert a button or touch transition the
  tail carries, so a press and release shorter than one exchange both arrive;
  at 8 queued states the tail is overwritten and counted. Load, reset, a
  direct `input` call, an owner change and stop discard queued states.
- Exchanges on one worker are serialized by its mutex, which is FIFO: a pause
  or debugger command queued behind input waits for at most one exchange.
- A timed-out exchange is never dropped mid-reply (the reply would reach the
  next request); the worker's 30 s exchange deadline ends the connection.

Per-session `telemetry.supervision` (polls, busy and late polls, lateness and
status latency, failures, continuations) and `telemetry.input` (submitted,
delivered, coalesced, kept and dropped transitions, cancelled, failed, pending,
queue age, p95 and max delivery latency) appear in every session summary.
`crates/session/tests/supervision.rs` checks against `lotus-mock-native` that a
worker stalled for 3 s stops none of three siblings' polls, continuations or
input (p95 under 500 ms, every tap delivered, no stuck key), that a manual
pause is never resumed, and that 2.5 s of back-to-back commands on one session
still leaves at least 3 polls and a continuation (fails with the skip-only
poll) (**verified compiled**, mock workers).

### Stopping and shutdown

`stop` marks the session closing (a second stop is refused), then waits at
most 3 s for the session lock and the worker's clean exit. If a stalled
exchange or command still holds the session, the supervisor terminates the
worker's job through a kill switch that needs no lock (`KillSwitch` in
`controlled_worker.rs`, a weak reference to the job). The in-flight exchange
then fails on the closed pipe within the 2 s release deadline, and the stop
reports `TimedOut`. A secondary that ended abnormally keeps only its logs.

`close` (every frontend on exit) refuses new sessions and stops all sessions
concurrently, each with its own deadline, so shutdown takes one worker's
deadline rather than one per session; a stop still unfinished after 6 s is
abandoned and reported. It returns every session's outcome; `lotus-mcp` prints
the failures. `close_is_bounded_by_one_deadline_with_hung_workers` stops 16
mock workers, 8 of them hung in a status reply, in 3.03 s on this host, and
checks that no worker process survives, hung roots keep only logs and clean
roots are removed (**verified compiled**, #31). Off Windows the kill switch
does nothing (untested platform).

## Gauges

Per session, from status replies (worker fields in [21](21-native-control.md)):

| Gauge | Meaning |
| --- | --- |
| FPS + sparkline | presented guest frames per **host** second over ≥ 500 ms windows |
| Speed | FPS ÷ 59.8312 Hz = `BASE_CLOCK_RATE_ARM11 / 4481136` (`core/core_timing.h:42`) |
| Frame / epoch | presented frame counter; epoch increments on load/reset |
| Audio | guest DSP samples per host second and cumulative drops (output stays muted) |
| Worker | working set via the supervisor's own process handle (never by PID), PID, uptime |
| Control | last mutating actor (`you`, `agent`, `host`), its operation, input owner |

These are host-observed throughput figures, not guest-tick or hardware timing.

## MCP surface

Headless and windowed servers expose the same tools; `lotus_status` reports
`frontend: headless|windowed` and, windowed, the desktop loop's
`frontend_metrics` ([24](24-desktop.md)). New or changed relative to [23](23-local-mcp.md):

| Tool | Behaviour |
| --- | --- |
| `lotus_create_session` | `kind` `primary` (needs `card`) or `secondary` (default; optional `card` to copy); optional `asset` loads paused |
| `lotus_load` | replace content in an owned session; paused |
| `lotus_control` | `run` (continuous, returns at once), `pause`, `reset`, `stop` |
| `lotus_run_until` | exactly 1–600 frames, then paused; ends continuous running |
| `lotus_list_sessions` | ids, kind, card/parent, asset and gauges; never waits on a busy worker |
| `lotus_list_cards` | cards and which session mounts each |
| `lotus_registers` | r0–r15 and CPSR of one core at a safe point |
| `lotus_focus_session` | windowed: inspect that session, and show it unless the workspace is split; headless: no effect |
| `lotus_workspace` | `get` the layout, or `tile`, `single`, `untile`, `swap`, `set_axis`, `set_ratio`, `set_screen_layout`, `focus_viewport` with optional `expected_revision`; changes need control and a window ([workspace](#workspace-67)) |
| `lotus_settings_schema` | the settings registry ([below](#settings-registry-50)), optionally one scope; no host needed |
| `lotus_settings` | effective worker settings of `global`, a `card` or a `session`, with sources and problems ([layers](#settings-layers-51)) |
| `lotus_settings_change` | `set`, `unset` or `reset` on the same targets; needs control |
| `lotus_input_presets` | input-layout presets: `list`, `export`, `import`, `duplicate`, `rename`, `delete`; changes need control ([input layouts](#input-layouts-57-63)) |

Host configuration gains `asset_directories` (handle → directory; each `.3dsx`
directly inside is approved as `<handle>/<file>`, re-listed on demand, so
rebuilt patch variants can be loaded without a restart) and `max_sessions`.
A directory that does not exist yet is not fatal: it lists nothing, appears in
`lotus_status.unavailable_asset_directories` with the reason, and is listed once
created. `--asset-dir handle=path` (repeatable, `lotus-mcp` and `lotus-desktop`)
adds or replaces a handle from the command line. Other missing paths (`worker`,
`workers`, `assets`) still fail startup, with the key and path in the error.
Existing configurations parse unchanged. A desktop user may also open any
`.3dsx` with the file picker; it is approved as `local/<file>` for that process.
MCP clients still cannot name paths.

## Window

Layout is computed in logical pixels by `desktop::layout` (pure, unit-tested):

- **Panes**: session sidebar 200–440 px (default 264, rail 52) and inspector
  300–760 px (default 420, rail 44); the centre keeps at least 320 px. Drag a
  pane's inner edge to resize. Dragging past half its minimum width snaps it to
  its rail and keeps the previous width (the snap rule from VS Code's
  `splitview.ts`, `halfSize = viewMinimumSize / 2`); dragging the rail edge outward
  reopens it. When the window is too narrow for both, panes shrink to their minimums,
  less recently used first, and then that pane is drawn as its rail without
  changing its saved state. Ctrl+B / Ctrl+I toggle the same state.
- **Sidebar**: cards with their primary and secondaries nested beneath; square
  dot = primary, round = secondary, colour = state. Hover `+` copies a card,
  `×` stops. A card row's gear opens its settings. Footer: MCP link, slot use,
  and the Settings gear. Long names
  truncate with an ellipsis; the rail shows one chip per session.
- **Toolbar** by centre width: ≥ 780 px labelled buttons, ≥ 520 px icons with
  tooltips, otherwise Run/Pause and Step plus a ⋯ menu holding Reset, Load,
  Stop, zoom and screen layout. Run/Pause and Step are always visible.
- **Inspector rail** (always visible): CPU, breakpoints, threads | memory |
  GPU | logs | snapshots, a More menu (IPC, timeline, audio, settings,
  result), and Run/Pause and Step at the bottom. Choosing a view opens the inspector. The current
  view is marked by its surface and a bar, not only by colour. The expanded
  inspector shows the view's icon, name and group (EXECUTION, MEMORY, MEDIA,
  TRACE, SESSION), its own commands (Back, reverse stop and replay are styled
  as dangerous) and a ⋯ menu with Open JSONL, Run clipboard JSON and the keys.
- **Gauges**: one row of cards when the centre is at least 7 × 112 px wide and the window
  is at least 700 px tall; otherwise FPS, speed and frame as one-line pills,
  and a chevron that shows the rest. Each pill shows the full detail in its tooltip.
- **Keys**: Z/X A/B, A/S X/Y, Q/W L/R, arrows, Enter/Backspace Start/Select
  (only while the screen has focus); Space run/pause; `.` step; Ctrl+O open
  file; Ctrl+, Settings; Ctrl+1–7 rail views; Ctrl+[ / Ctrl+] cycle views; Tab / Shift+Tab move
  focus; Esc closes menus.
- **Icons** are original 16 × 16 stroke SVGs (`desktop::icons`), tinted by
  text colour, so they stay sharp at any scale.
- **Theme**: Geist-derived dark neutrals with colour only for state
  (`desktop::theme`, after the workspace's `vercel.md`). Segoe UI and Consolas.

Pane widths, open states, Discord options and the Settings window bounds persist in `desktop.json`
(`desktop::preferences`): `LOTUS_DESKTOP_PREFERENCES`, else
`%APPDATA%\LOTUS\desktop.json`, else `$XDG_CONFIG_HOME/lotus/desktop.json`.
The portable launcher keeps it in `data/`. If the file exists but does not parse,
it is never overwritten, and the Settings window says so.

## Workspace (#67)

The workspace is what a windowed host shows: `empty`, `single {session}` or
`split {first, second, axis, ratio, screens}` (`lotus_session::workspace`, pure and
unit-tested). The supervisor owns the one instance, so the desktop and MCP change the
same state through `Supervisor::update_workspace`; neither keeps its own layout.

- Sessions are stored by session ID. `horizontal` = first left, second right;
  `vertical` = first top, second bottom; `auto` leaves the choice to the window.
  `ratio` is the first tile's share, 0.2–0.8 (a policy bound, not a hardware one).
  `screens` is each tile's top/bottom arrangement: `auto`, `stacked`, `side_by_side`.
- Operations: `tile`, `single`, `untile {keep}`, `swap`, `set_axis`, `set_ratio`,
  `set_screen_layout`, `focus_viewport`. Each is validated whole (existing,
  distinct sessions, ratio bounds, split-only operations) and commits all or
  nothing. `revision` increments only when the state changes; a repeated
  operation is a no-op. `expected_revision` refuses a stale request
  (`ResourceBusy`), so an agent cannot overwrite a newer drag.
- Separate from the workspace, and never changed by it: the selection
  (`Update::selected`), controller input, audio focus and worker execution.
  Outside a split, selecting a session in the sidebar or with
  `lotus_focus_session` shows it via `single`; selection never collapses a split.
- A stopped session leaves the workspace when it is unregistered: a split
  collapses to the other tile, whose worker keeps running; a single view becomes
  empty. A crashed session stays registered and keeps its tile.
- Headless hosts never enable it: `lotus_status.workspace.available` is false and
  every change returns `Unsupported`. MCP changes need control permission.
- `lotus_workspace` (#72) is the MCP view of the same model: `get` returns the
  snapshot, a change returns `{committed, workspace}` with the new revision, and
  stale, unknown-session and headless refusals come back as tool errors with the
  supervisor's message. It never selects, moves controllers or starts workers.
- `desktop.json` keeps `split_axis` and `split_ratio`, the defaults for a `tile`
  that omits them. Tiled sessions are not saved: session IDs end with the host,
  and restoring a layout never starts a worker.

Evidence: `workspace::tests` (transitions, idempotence, invalid and stale
requests leave the state unchanged, removal fallback) and
`supervision.rs` `workspace_arranges_without_touching_workers` /
`workspace_writes_from_mcp_need_control` against `lotus-mock-native` workers
(**verified compiled**). The MCP tool's read and refusal paths:
`server::tests::workspace_tool_reads_headless_and_refuses_changes_it_cannot_commit`
(**verified compiled**).

### Two visible sessions (#65)

A split shows both sessions at once, each tile with its own preview, images,
touch mapping and header (name, paired controller names, `Keyboard` on the
focused tile, screen arrangement toggle, return-to-sidebar; the first tile also
cycles the axis auto → left/right → top/bottom).

- Entering a split: the tile icon on a hidden, running, loaded session's sidebar
  row tiles it beside the single view, or replaces the unfocused tile of a split.
  Return-to-sidebar (`untile {keep}`) also selects the kept session, because
  outside a split the window shows the selection. No worker starts or stops.
- Previews: the controller requests one per shown session per tick, each with its
  own in-flight slot (#45), so a stalled tile never delays the other.
- Input: keyboard goes to the focused viewport (a click on a tile sends
  `focus_viewport`); what it held is released when it moves. Pointer touch goes to
  the tile it started on. Controllers follow their assignment (#62), never focus,
  tile order or selection. Selecting a session in the sidebar changes the
  inspector, audio focus and gauges, never the tiles or keyboard.
- Geometry (#69): `split_plan` in `lotus_window.rs`, one pure function used for
  both drawing and the touch rectangles. An explicit axis or per-tile screen
  arrangement is kept. `auto` takes whichever gives the smaller tile the larger
  scale and holds that choice until the alternative is 5% larger
  (`AUTO_HYSTERESIS`), so a resize or divider drag near a crossover does not flip
  back and forth. An even split (ratio 0.5) shows both consoles at one common
  scale; a moved divider gives each tile its own fit. Sizes never go negative;
  below the 0.5 scale floor the tile clips and the stage scrolls. The solver works
  in GPUI logical pixels, so a DPI change alters only the device pixel ratio.
  Resizing never touches a worker or renderer setting.
- `lotus_status.workspace.resolved` / `lotus_workspace get` report what the window
  drew for the current revision: chosen `axis`, per-tile `screens` and `scale`
  (host pixels per guest pixel, after zoom). It is `null` before the window lays
  the split out, outside a split, or while the report is for an older revision,
  and reporting it never bumps the revision.
- Gauges (#70): in a split each console has its own gauges on its outer edge
  and the bottom strip is hidden. Left/right: a 112 px column left of the first
  tile and right of the second. Top/bottom: a pill row above the first and below
  the second. `split_plan` takes that edge out of each slot before fitting the
  screens, so gauges never cover a screen or touch area. Pills: state, FPS, speed,
  frame (audio, renderer, worker and control in its tooltip), then one `PAD` per
  controller assigned to that session: green when connected, amber when
  disconnected but remembered by the router (#62), `none` otherwise. Pairing is
  read from the assignment state only (`pairing`), never from pad order, tile
  order or keyboard focus. Gauges follow the session, so a swap moves them with
  it. Single view keeps the bottom strip unchanged.
- Inspector (#71): a split removes the inspector rail and its width
  (`layout::plan_with` with no inspector). Each tile header has an inspect
  button ("Inspect <name>"); it opens a floating inspector for that tile's
  session, on the tile's side of the stage below the toolbar, so neither tile
  shrinks. It reuses the docked panels, views and debug commands, resizable from
  its inner edge (width not saved). Ctrl+I and choosing a view open it for the
  keyboard-focused tile. The target is `Command::Inspect`, separate from the
  selection: `Update::inspected` is that tile while it stays visible in a split,
  otherwise `selected`. The controller clears it when the session leaves the
  split, stops or the split ends, and inspector replies are dropped by a
  generation counter bumped on every `inspected` change, so a swap keeps the
  overlay on its session and an untile never shows the other worker's data.
  Opening it changes neither selection, audio focus, controller assignment nor
  whether a worker runs. The saved single-view inspector open state and width
  are not written while split and return on untile.

Evidence: `responsive_controller.rs`
`tiled_sessions_preview_independently_while_one_stalls` (mock workers): with A's
capture stalled 5 s, B's preview advances while A's does not, selecting A leaves
the split intact, and after `untile` A's late frame is dropped with both workers
still running. `split_auto_picks_the_axis_with_larger_screens_and_keeps_explicit_choices`,
`split_bounds_never_overlap_and_screens_fit_their_tiles` (minimum, portrait,
landscape, 4K at 200%, ultrawide and 21:9 content areas at ratios 0.2 to 0.8),
`auto_never_picks_a_smaller_layout_than_the_alternatives` and
`auto_does_not_oscillate_around_the_crossover` (fails with the hysteresis set to
0) cover the geometry; `supervision.rs` checks that `resolved` is only reported
for the current split revision (**verified compiled**). Release build with two
CITnLO `t03` sessions on Vulkan (**emulator**): at 1700x950 `auto` resolved
left/right stacked at 1.35 on both tiles; at 900x1400 it went top/bottom at 0.81;
back at 1700x950 it returned to left/right; `set_axis vertical` gave side-by-side
screens at 1.24, all without restarting a worker.

Inspector evidence: `tile_inspector_targets_its_own_session_and_never_changes_selection`
(mock workers: ignored outside a split, targets B while `selected` stays A,
follows B through a swap, falls back to A on untile, workers keep playing) and
the split case in `layout.rs` (**verified compiled**). Release build, two CITnLO
`t03` sessions (**emulator**): tiling removed the rail; B's inspect button opened
"Inspecting content 2a756c" on the right with B's registers while the toolbar
stayed on A; after `swap` it moved left with B, survived a status call, 30 s
idle and a second swap; `untile` restored the docked rail showing A's registers.
In one earlier run the overlay was gone after a swap and was not reproduced
since. Keyboard activation of the buttons and macOS were not tested.

Gauge evidence: `tile_pairing_comes_from_assignments_only` and
`split_reserves_each_tiles_outer_gauge_edge` (**verified compiled**). Release
build, two CITnLO `t03` sessions (**emulator**): left/right put A's gauges far
left and B's far right; after `swap` and `set_axis vertical` A sat at the bottom
with its gauges below it; killing A's worker turned only A's gauges to `crashed`,
FPS 0.0, while B's kept updating at 60 fps. Physical controllers were not
connected, so `PAD` was only seen as `none`; connected and remembered pads are
covered by the unit test alone.

### Drag tiling (#68)

`tiling.rs` holds every drag decision as pure functions; the window draws the
preview and commits the operation from the same `Target`, so what is shown is
what is dropped.

- Sources: a sidebar row (loaded, running sessions only) or a tile header.
  Dragging starts after GPUI's 2 px threshold and shows a ghost with the
  session label. The revision at drag start travels with it and the drop is
  sent as `update_workspace(op, Some(revision))`, so a drop over a layout an
  agent changed meanwhile is rejected instead of applied to the wrong tiles.
  A drop for a session that no longer exists is ignored.
- Stage targets: the stage's diagonals split it into left, right, top and
  bottom edges with no dead zones. A row onto an empty stage shows it alone; a
  row onto a single view tiles it on the dropped edge (left/right horizontal,
  top/bottom vertical); a row onto a split replaces only the tile under the
  pointer, and the displaced session keeps running in the sidebar. A tile onto
  an edge reorders or reorients; a drop that matches the drawn layout commits
  nothing, so `auto` is kept. The preview is a bordered half (or tile) at 12%
  fill.
- Sidebar target: dropping a tile on the sidebar ("Return to sidebar (keeps
  running)") untiles and keeps the other session. Workers are never stopped or
  paused by any drag.
- Divider: replaces the gap between tiles. Dragging it previews the ratio
  locally (clamped 0.2–0.8) and commits one `SetRatio` on release;
  double-click sets 0.5.
- Cancel: Escape or a drop outside a target ends the drag with no operation.
- Keyboard, in a split: Ctrl+Shift+H left/right, Ctrl+Shift+V top/bottom,
  Ctrl+Shift+R swap, Ctrl+Shift+W untile (keeps the keyboard-focused tile and
  selects it), Ctrl+Shift+Left/Up and Right/Down move the ratio by 0.05,
  Ctrl+Shift+D resets it to 0.5. Entering a split without a pointer uses the
  row's tile button. X/E/U/S/0 were tried first and dropped: on the test
  machine Ctrl+Shift with those keys never reached the window.

Drag evidence: `tiling.rs` tests check that every stage point maps to the edge
its preview draws, row-onto-single and row-onto-split results, swap/reorder/
reorient without no-op commits, and the divider ratio round trip and bounds
(**verified compiled**). Release build, two CITnLO `t03` sessions, synthetic
mouse and key input (**emulator**): dragging B's row to the left edge previewed
the left half and tiled B left; Escape during a tile drag left the revision
unchanged; dragging the divider set the ratio to about 0.3; dragging the first
tile to the bottom edge gave top/bottom with B on the bottom; dragging a tile
to the sidebar left single A with both workers playing. Shortcuts, read back
through `lotus_status`: R swapped (revision 2→3, focus followed A), Left twice
gave 0.45 then 0.40, D gave 0.5, W left single A. Touch input, DPI scaling
other than 100%, mouse capture loss mid-drag and macOS were not tested; one
tile-header drag failed to start on the first attempt and worked on retry,
likely an artefact of the driver's synthetic input.

## Live input state (#60)

`native_session::InputState` is the one fast input state for the desktop,
`submit_input`/`input`, MCP `lotus_input`, the scenario runner and the worker's
`input` method: `buttons` (Azahar `Settings::NativeButton` bits), touch `x`/`y`
in native bottom-screen pixels with `touch`, and `circle_pad`/`c_stick` as
`[x, y]`, each -1..=1 with x right and y up. That is what Azahar's
`Input::AnalogDevice::GetStatus` hands HID (`ctr_worker/input.h` `Analog`, which
also clamps the vector to length 1), and it matches gilrs' stick convention
(`lotus-input` `Axis`), so no sign change is needed between them.

- **Worker** (integration `b2b6f6dd`, experimental `3ef0f937`): `input` takes
  optional `circle_pad`/`c_stick` and changes only the fields it is sent. Before
  this it replaced the whole state, so every button or touch update zeroed the
  sticks and the accelerometer/gyroscope set by `input_state`. Motion, the 3D
  slider and volume stay outside the fast state and keep their `input_state`
  values. Out-of-range or malformed sticks reply `invalid_argument`. Both
  workers list `analog_input`.
- **Host**: `InputState::validate` refuses non-finite or out-of-range axes and
  off-screen touch before queueing. A worker without `analog_input` gets no
  stick fields, and a stick off centre is refused with `Unsupported`. This
  happens in `submit_input` too, so the desktop learns it rather than
  telemetry. The mailbox treats stick positions like touch coordinates: newest
  value wins, never a transition. Button and touch edges are still kept, and
  owner hand-off (#48) is unchanged.
- **Evidence**: `tools/analog_input_acceptance.py <worker> <peripherals.3dsx>
  <out>` is **verified emulator** on both workers. It runs two workers on the
  peripherals fixture at once, drives opposite stick directions through `input`
  and decodes HID/IR shared memory. Neutral/left/right/up/down each gave the
  expected signs and circle direction bits in both sessions: circle ±154,
  C-stick ±156. A button-only `input` kept the sticks and the accelerometer
  (128, -256, 512). `cargo test -p lotus-session` covers per-session delivery
  through the mock and refusal by a worker without the feature
  (`analog_input_is_per_session_and_refused_by_old_workers`). It also covers
  stick coalescing that keeps a tap
  (`stick_moves_coalesce_but_keep_button_edges`).
- **Not yet**: controllers drive the sticks through routing (#61) with the
  default layout; remapping is #63. ZL/ZR (bits 14/15) and Home (16) travel as buttons; Home reaches APT
  only with HOME Menu registered (tools/peripheral_acceptance.py).

## Controllers (#59)

`lotus-input` owns physical gamepads. `lotus-desktop` starts one
`ControllerService` per process, on its own `lotus-input` thread, so polling is
independent of GPUI rendering and of which session is selected. Workers and GPUI
never enumerate devices. Routing to sessions is the next section; devices,
counters and assignments are in `lotus_status.frontend_metrics.controllers`.

- **Backend**: gilrs `=0.11.2` behind `ControllerBackend`. Feature `wgi`
  (default) uses Windows Gaming Input; `xinput` uses XInput
  (`--no-default-features --features xinput`). gilrs-core refuses both at once
  (`platform/mod.rs`). gilrs' README: WGI "requires an in focus window to be
  associated with the process to receive events". gilrs' default filters
  (deadzone, jitter, D-pad axes as buttons) stay on.
- **Identity**: `DeviceId` (`pad-N`) is one connection during one host run,
  never reused; a reconnect is a new ID. The backend model UUID is reported as
  `model_uuid` and is not identity, since identical pads share it.
- **Events**: `connected {info, state}` carries everything held at connect, so
  a consumer resynchronizes; `disconnected` means release everything that
  device held; `button {button, pressed}` (repeated edges are dropped) and
  `axis {axis, value}` in -1..1, sticks positive right/up. Buttons are
  positional (south/east/north/west, SDL layout).
- **Bounds**: 1024 queued events, then drops counted in `dropped`. The thread
  blocks in gilrs' `next_event_blocking` for at most 50 ms (both Windows
  backends wait on a channel, `recv_timeout`), so shutdown joins within that.
  A connect for a slot still listed retires the old connection first.

Evidence (**verified compiled**): `lotus-input` unit tests with the synthetic
backend (distinct IDs for identical pads, reconnect resync, duplicate edges,
missed disconnect, full queue, shutdown). Both backends build with MinGW and
start: `lotus-pads 2` reports `running: true`, no devices (no controller is
attached to the build machine). Idle: 0.047 s CPU in 9 s, including process
start and WGI initialisation (release, Windows 11).

**Unverified**, needs physical controllers: two pads listed with live input
from each, repeated hotplug, and the focus matrix (focused, Alt+Tab away,
minimized; WGI and XInput) — run `lotus-pads 30` from each build. macOS and
Linux are unbuilt.

## Input routing (#61)

`lotus_input::router::Router` keeps what each source holds and which session
it targets, and composes one complete guest state per session. Sources are the
keyboard, the pointer and each controller connection. `desktop::input_routing`
submits the composed states with owner `live` and keeps the router in step with
the device service and the session list. A `lotus-input-router` thread applies
controller events, so routing does not wait on GPUI frames.

- **Targets**: keyboard and pointer follow the selected session while the
  window has keyboard focus. `release_input` (blur, Tab, selection change)
  releases only their contribution. Controllers follow explicit assignment
  (next section). Selection, focus, Settings and layout never move or release
  them. An unassigned controller routes nowhere.
- **Composition**: buttons are ORed over sources and touch comes from the
  pointer only. Each stick takes the source with the largest deflection; ties go
  to the keyboard, then the pointer, then the lowest connection ID. A state is
  submitted only when the composition changes, so a keyboard release never
  sends a zero that drops a held controller button.
- **Default controller layout**: Azahar's `xinput_to_3ds_mapping`
  (`input_common/sdl/sdl_impl.cpp`). East is A, south is B, north is X and west
  is Y. The D-pad maps to the D-pad, the shoulders to L/R, Start to Start, Back
  to Select and Guide to Home. That table leaves ZL/ZR out; Azahar binds them to
  the trigger axes at 0.5 instead (`BuildButtonParamPackageForButton`). Here both
  the trigger buttons and the trigger axes press them. The left stick is the
  Circle Pad and the right stick the C-stick. This is the builtin `default`
  layout; sessions can use others (see Input layouts).
- **Reassignment and disconnect**: assigning a controller first republishes its
  old session without it. Buttons held at that moment are ignored on the new
  session until released, and sticks until centred, so an old hold never
  arrives there. A disconnect removes only that connection's contribution.
- **Repair**: when the service's `dropped` counter rises, queued events are
  applied and then every controller's state is replaced from the service
  snapshot. After 250 ms without events, connections missing from the device
  list are removed and unknown ones are added unassigned. Sessions that ended
  are dropped from every source.
- **Refusals**: if a submission is refused for a session that still exists, the
  next change sends the full composed state. A worker without `analog_input`
  gets the same state without sticks (`analog_dropped`). The worker refuses live
  input while a scenario owns it (#48), and that input is never replayed.
  `submit_input` only queues to the per-session mailbox, so a stalled worker
  delays no other session.
- **Status**: `frontend_metrics.controllers.routing` reports:
  - `assignments`: device ID → session, or null.
  - `router`: `published`, `unknown_device`, `resyncs`.
  - `submission`: `refused`, `analog_dropped`.

Evidence (**verified compiled**, mock workers):

- The `lotus-input` router unit tests cover a keyboard on A beside a pad on B,
  ORing and separate release, and the stick policy. They also cover pointer
  retargeting, reassignment suppression, identical pads, session removal, lost
  events and change-only publishing.
- `crates/desktop/tests/input_routing.rs` runs the real supervisor with two
  `lotus-mock-native` workers and the scripted backend:
  - The keyboard and pad 1 on A never reach B, which has pad 2.
  - Losing keyboard focus keeps pad 1's button held.
  - Unplugging pad 2 releases only B.
  - Reassignment neutralizes A and leaks nothing to B.
  - B's input arrives within 1 s while A stalls for 3 s.
  - Live input under a scenario owner is refused and not replayed after reset.
  - Stopping B unassigns its pad.

**Unverified**, needs physical controllers: the same run with two real pads,
and controller input while Settings or another window has focus. WGI needs a
focused window of this process (see the backend note above).

## Controller pairing (#62)

Pairs are made in the sidebar's CONTROLLERS list. Each connected controller
shows its name, connection ID (`pad-N`) and the session it drives. **Assign**
gives it to the selected session, **Replace** does so when that session already
has a controller, and **Unassign** frees it. **Identify** marks the next
controller to press a button, so two identical pads can be told apart. The press
itself is routed as usual. Assignments name running session IDs, primary or
secondary. Selecting another session only changes which one the buttons act
on.

- **Policy** (`Router::assign`): one controller per session and one session per
  controller. At most `MAX_ASSIGNED` = 2 pairs exist; every connected controller
  is listed. Refusals name the reason: the session already has `pad-N`, two are
  assigned, the controller is gone, or the session is not running. Moving an
  assigned controller to a free session is allowed. Before a controller joins
  its new session, its old one is republished without it, with the held-input
  suppression from #61.
- **Reconnect**: a disconnect remembers the controller's name, model UUID and
  session. A new connection takes that session back only when the match is
  unambiguous. Exactly one remembered entry must have its model, and no other
  unassigned controller of that model may be connected. The session must also
  be running and still free, within the cap. Otherwise it stays unassigned and
  the list shows "NAME disconnected; was on SESSION" until someone chooses. Two
  identical pads share a model UUID (#59), so they never resolve on their own.
  An entry ends when its session ends or is given another controller.
- **Teardown**: stopping, crashing or expiring a session unassigns its
  controller and drops its remembered entry within one 250 ms reconcile.
- **Status**: `frontend_metrics.controllers.routing` adds `max_assigned`,
  `remembered` and `identify` (`armed`, `last`, `count`).

Audio focus is unchanged by pairing; it follows the selected session as before.

Evidence (**verified compiled**, mock workers):

- Router unit tests cover the cap and occupancy, replace and move, identify, and
  reconnect. The reconnect cases are a unique model, identical pads, an unassigned
  twin, an ended session and a session given another pad.
- `controller_pairing_policy_against_running_sessions` in
  `crates/desktop/tests/input_routing.rs` runs three `lotus-mock-native`
  sessions and three scripted pads, two of them identical. It checks:
  - Identify names the right pad.
  - The third pad is refused an occupied session, a third pair, and a stopped
    session.
  - Holding pad 1's button and stick on A while tapping pad 2 three times on B
    delivers each only to its own worker.
  - Unplugging pad 2 releases only B.
  - Pad 2's reconnect waits for a choice while its twin is unassigned.
  - The unique pad reconnects to A on its own.
  - Stopping A and B clears both pairings.

The sidebar list is **verified compiled** only: no screenshot run.
**Unverified** with physical pads: two real controllers on two sessions, and
hotplug.

## Controller qualification (#66)

- **Per-device metrics**: `frontend_metrics.controllers.routing.devices` lists
  each connection. Disconnected ones stay listed, but only the last 8.
  - Event counts:
    - `received`: events from the device service.
    - `submitted`: changed the session's composed state.
    - `unchanged`: a duplicate edge, a stick in the dead zone, or a button
      another source already holds.
    - `unrouted`: a button or axis event with no session assigned.
    - `refused`: the supervisor refused the submission.
  - Latency `latency_p50_ms`/`p95`/`max` (last 256 submissions): from the
    backend event timestamp to the supervisor accepting the state into the
    mailbox.
  - `busy_ms`: router-thread time in `InputRouting::event`. This is wall time
    and includes lock waits. It does not include gilrs polling on the
    `lotus-input` thread.
  - `last_event_age_ms`: time since the device's last event.
- **Session metrics**: mailbox-to-worker delivery is the session's
  `telemetry.input` (#20/#48): `delivered`, `coalesced`, `edges_dropped`,
  latency. `denied` counts deliveries the worker refused because a scenario
  owns it; these are also counted in `failed`.
  - `native_session::finish` now maps the worker's `permission_denied` reply to
    `io::ErrorKind::PermissionDenied`. It was `Other`, which made ownership
    refusals indistinguishable from other failures.
- **Replay**: controllers reach a worker only as the normalized `InputState`
  with owner `live` (#60/#61). Scenario and movie input carries its own owner,
  and the worker refuses `live` input while that owner holds it (#48). Replays
  therefore do not see gilrs timing or device enumeration. A worker without
  `analog_input` gets the state without sticks (#60).

Evidence (**verified compiled**, mock workers, scripted backend):
`two_pads_soak_without_crossover_or_stuck_input` in
`crates/desktop/tests/input_routing.rs`. It runs two `lotus-mock-native`
sessions. Pad 1 drives A with South/East and a left-leaning stick, alongside a
held keyboard L. Pad 2 drives B with North/West and a right-leaning stick. Every
1 ms step toggles a button and moves the stick through a diagonal with drift.
The phases are pad 1 alone, then both pads, then both pads while A's worker
stalls for 2 s. `LOTUS_INPUT_SOAK_SECS` sets the length (default 6).

The test asserts:
- Neither worker's accepted input ever contains the other pad's bits or stick
  sign.
- B accepts more than 100 states during A's stall.
- Both workers end neutral.
- No device events are dropped, and there are no refusals or unrouted events.
- Each device's routing p95 is under 25 ms.
- The second pad keeps pad 1's p95 within 3× + 2 ms of its baseline, and the
  per-event router cost within 3× + 20 µs.
- B's `edges_dropped` is 0, and `denied` is 0 on both sessions.

The 60 s run on Windows 11 (debug build, 2026-11-18) gave:

| | one pad | two pads |
|---|---|---|
| routing p95 | 0.135 ms | 0.125 / 0.160 ms |
| router busy | 4.31% (22.5 µs/event) | 5.66% (14.0 µs/event) |

Other results from the same run:
- About 200k device events, 0 dropped.
- B's delivery p95 was 1.8 ms and its max 9.7 ms.
- A's delivery max was 1.76 s: the stall.
- A dropped 492 edges during the stall, where the mailbox keeps only the newest
  transitions (#48). B dropped 0.

The other #66 cases are the #61/#62 tests above: reassignment, identical models,
disconnect, scenario takeover, keyboard focus loss and keyboard/pad overlap.
`crates/session/tests/supervision.rs` covers reset cancelling pending input and
refusal by older workers.

Not covered yet:
- Mapping changes during play wait for profiles (#63) and the Settings controls
  (#64).
- CPU for gilrs polling is unmeasured: the scripted backend replaces it.

Hardware matrix (all **unverified**; no controller is attached to the build
machine):

| Platform | Backend | Two pads, two sessions | Focus: main / Settings / other app / minimized | Reconnect |
|---|---|---|---|---|
| Windows 11 | gilrs WGI (default) | unverified | unverified; WGI needs a focused window of this process (gilrs README) | unverified |
| Windows 11 | gilrs XInput (`--features xinput`) | unverified | unverified | unverified |
| macOS | gilrs IOKit | unbuilt | unbuilt | unbuilt |

To qualify a pad, record its model, USB or Bluetooth connection and the OS
build. Then follow the user guide's pairing steps
([37](37-user-guide.md#game-controllers)) and read `routing.devices`.

## Input layouts (#57, #63)

`crates/input/src/layout.rs` defines the preset format, the resolver and the
compiled per-session `Mapping` the router applies. The layout is chosen with
`lotus.input.*` settings in the same files and layers as worker settings (#51),
and the desktop applies it to running sessions. The editor is #64.

- **Actions.** Stable IDs: `a b x y up down left right l r zl zr start select
  home`, `circle_pad_{up,down,left,right}`, `c_stick_{up,down,left,right}`.
  Touch is not an action; it follows the pointer on the bottom screen.
- **Bindings.** `{"device":"keyboard","key":"z"}` (GPUI key names, the strings
  the window receives, not scancodes), `{"device":"button","button":"east"}`
  and `{"device":"axis","axis":"left_x","direction":"negative"}`, using the
  positional `Button`/`Axis` names (SDL layout) rather than backend IDs or
  serials. The `device` tag is the capability a binding needs. A binding whose
  device is absent stays in the preset and never fires.
- **Preset file.** `{"version":1,"name":"…","bindings":{"a":[…],…}}`, which is
  also the export format. On import, unknown action IDs and unparseable
  bindings are dropped and reported, and duplicates within an action collapse.
  Actions the file omits (an older preset) take the builtin bindings. A missing,
  zero or newer `version`, an empty name, a `bindings` that is not an object, or
  a known action whose value is not a list (`null`, `{}`, a string…) refuses
  the file (#114): only `[]` unbinds. A stored `lotus.input.bind.*` value that
  is not a list is reported and skipped, so the layer below applies.
- **Builtin presets** (read-only). `default` reproduces the shipped controls:
  the keyboard table under Window, and for controllers `default_button`, both
  sticks, and the trigger axes `left_z`/`right_z` as ZL/ZR past the threshold.
  The trigger axes are Azahar's own SDL default (`sdl_impl.cpp`
  `BuildButtonParamPackageForButton`); gilrs on Windows reports triggers as
  buttons, which `default_button` already binds. `default` is positional (3DS A
  on the right face button, as Nintendo pads print it).
  `xbox-labels` moves A/B/X/Y to the printed Xbox labels (A at the bottom).
  DualSense buttons are printed with symbols, so position is their only meaning.
- **Settings** (owner `input`, group Controls, every scope, apply class
  `routing`):

  | Key | Value |
  | --- | --- |
  | `lotus.input.preset` | preset name; null is `default` |
  | `lotus.input.bind.<action>` | binding list replacing the preset's; `[]` unbinds; null keeps the preset's |
  | `lotus.input.{circle_pad,c_stick}.deadzone` | 0–0.99, default 0 |
  | `lotus.input.{circle_pad,c_stick}.sensitivity` | 0.1–2, default 1 |
  | `lotus.input.{circle_pad,c_stick}.invert_y` | default false |
  | `lotus.input.threshold` | 0.01–0.99, default 0.5 |

  Binding lists are checked with the layout parser at every target (at most 8
  per action). These keys are never sent to a worker (`Resolution::profile`
  leaves them out), are never `pending`, and are refused in the host `profile`.
- **Precedence.** Per key, as #51: global, then the card (a primary's own, a
  secondary's parent), then a secondary's runtime override. A global edit
  therefore reaches every card that does not override that key, and a card's
  explicit value stays when the global one changes. `resolve` starts from the
  effective preset (builtin first, then the library) and replaces each
  overridden action's whole binding list. A selected preset that no longer
  exists falls back to `default` with a `MissingPreset` problem.
- **Live.** Unlike worker settings, layouts resolve live
  (`Supervisor::input_layout`): saved global and card edits reach running
  sessions, primary or secondary. A secondary's runtime overrides live in its
  session and vanish when it stops. Every settings or preset change bumps
  `Supervisor::settings_revision`. `InputRouting` re-resolves when the revision
  changes (checked at least every 250 ms) and before routing to a session it
  has no layout for. File reads happen outside the router lock.
- **Library.** Saved presets are `<storage_root>/settings/input-presets/<name>.json`,
  each an exported preset. Names follow card-name rules, and builtin names are
  reserved. The operations are save (create, or replace), import (a taken name
  becomes `name-2`, `name-3`, ...), duplicate (also from a builtin), rename and
  delete, all under `settings/settings.lock`. A file with a bad name or content
  is listed as a problem and skipped. Renaming or deleting a preset leaves
  selections of the old name to fall back to `default`.
  `lotus_input_presets` (MCP) does list, export, import, duplicate, rename and
  delete; a change returns `{"name", "warnings"}`, the warnings being what an
  import dropped or filled from the builtin. Saving over a preset file this
  build cannot read (newer version, malformed) is refused.
- **Conflicts and gaps.** One binding on several actions presses all of them.
  `Resolved::conflicts` lists them and `Resolved::unbound` lists actions with no
  binding; neither is refused. Both, with each session's preset and problems,
  are in `frontend_metrics.controllers.routing.layouts`, for the editor to
  offer choices.
- **Mapping.** `Mapping::new(&Resolved, Profile)` compiles fixed tables indexed
  by button, so a sample allocates nothing. An axis bound to a stick's right/up
  action (or left/down, sign flipped) drives that component in analog. Every
  other axis binding is digital, pressed past `threshold` (default 0.5,
  Azahar's ZL/ZR value). Buttons, keys and digital axes bound to stick
  directions push that stick fully.
- **Stick shaping** (per 3DS stick). Radial dead zone (0–0.99, default 0) with
  Azahar's rescaling `x / r * (r - dz) / (1 - dz)` (`sdl_impl.cpp`
  `SDLAnalog::GetStatus`), applied on top of gilrs' own deadzone filter; then
  sensitivity (default 1) with a unit-circle clamp, then optional Y inversion.
  Digital directions are added after shaping, so the dead zone never eats a
  button edge.
- **Routing.** The router keeps one mapping per session (`set_mapping`), so the
  same controller can mean different things in two sessions. A mapping change
  suppresses what that session's sources hold: the old action is released at
  once and the new one needs a fresh press or a re-centred axis. Keyboard keys
  held when the keyboard's target changes are suppressed the same way. The
  window sends held key names (`keyboard_keys`) and the router maps them with
  the target session's layout. The session input-owner protocol (#48) is
  unchanged.

**Editor interaction (#64, not built).** The Controls view sits inside the
standalone Settings window. It shows a 3DS outline with every action. Selecting
one lists its bindings with the layer each came from, and offers add (capture
the next key or controller input), remove, and reset to inherited. A scope
picker chooses global, card or the selected secondary. The view has preset
create/rename/duplicate/delete and import/export; builtin presets are
read-only. Conflicting bindings are marked on both actions, and unbound actions
are listed. Keyboard and controller bindings are listed separately. Bindings for
absent devices are greyed out.

Evidence: **verified compiled**. Unit tests in `layout.rs` cover:

- builtin keyboard and controller defaults;
- the import/export round trip, refusal and dropped entries;
- overrides and unbinding;
- library and missing presets;
- conflicts;
- Xbox, DualSense and Nintendo pad-family fixtures, including triggers as
  buttons and as axes;
- dead zone, sensitivity and inversion, and digital input surviving the dead
  zone;
- axis rebinds.

`router.rs` tests cover per-session mappings without leaks, remapping while
held, and keyboard keys following the target. Store, registry, supervisor and
MCP coverage:

- `settings_store.rs`: input keys layer per action and stay out of worker
  profiles; the preset library round trip (save, replace, refusals, import
  suffixes, duplicate, rename, delete, bad files).
- `lotus-settings` `tests.rs`: binding validation at every scope.
- `settings_layers.rs` (**verified emulator**, mock worker): global edits reach
  running primaries, card differences hold, a secondary's override stays out of
  its worker and vanishes with it, a deleted preset falls back.
- `lotus-desktop` `input_routing.rs` (**verified emulator**): a runtime override
  remaps a held pad button without a stuck key, and the keyboard follows each
  session's layout.
- `lotus-mcp`: the preset tool.

No physical controller was used. The window was not re-run with the key-name
path.

## Settings registry (#50)

`lotus-settings` describes every setting a person or client can see: the 173
Azahar worker settings, the Lotus host (`lotus.host.*`, `HostConfig`) and
desktop (`lotus.desktop.*`, `desktop.json`) settings, and the input layout
(`lotus.input.*`, [input layouts](#input-layouts-57-63)). Each descriptor has a
stable key, owner, intent group and subgroup, label, description, type with
bounds or choices, default, scopes (`global`, `primary`,
`secondary_runtime`), apply class, availability and source citations.

- **Azahar half.** Generated from `crates/settings/azahar-inventory.json`,
  which `tools/settings_inventory.py` dumps from both pinned workers'
  `settings_list` (it records each worker's SHA-256 and pin). Types, ranges,
  choices, defaults, apply classes and consumer citations come from the
  worker; groups are an explicit table in `azahar.rs`. One key exists only on
  CITnLO and is marked `branch_only`.
- **Exclusions.** Excluded settings are listed with a reason and settable at no
  scope: keys the worker refuses (`use_gdbstub`, `gdbstub_port`,
  `enable_rpc_server`, `use_custom_storage`, `output_type`), `inert` settings,
  and settings whose cited consumers are all in `renderer_opengl` (Lotus runs
  software or Vulkan) or all in frontend layout code (captures use
  `frames.h` `ScreenLayout`, single-screen layouts with 3D off). The generator
  keeps at most six citations per setting, so a setting with six is never
  excluded on that basis: further consumers may exist.
- **Lotus half.** `lotus.rs`. Defaults are checked against `HostConfig::new`,
  `Policy::default`, `ArtifactQuota::default` and `Preferences::default`, and
  the width/ratio bounds against `layout.rs` and `workspace.rs`, by tests in
  the owning crates. Where the code checks only a lower bound, the descriptor
  has no upper bound.
- **Host profile.** `Supervisor::new` refuses a `profile` key that is unknown,
  excluded, a Lotus key, or has an invalid value, with the reason. The
  configured workers' branch is unknown before they start, so a CITnLO-only key
  passes here and an AZAHAR worker refuses it at `settings_set`. A
  configuration that previously set an excluded or misspelled key now fails
  startup instead of being ignored or refused per session.
- **Drift.** At session start, after `settings_list`, a worker whose
  `backend_name` is `AZAHAR` or `CITnLO` is compared with the registry.
  `settings_drift` in `native-manifest.json` lists keys the registry lacks and
  registry keys the worker no longer binds; a non-empty result is logged.
  Other workers (the mock) record `null`.
- **Discovery.** `ctr-cli settings schema [global|primary|secondary]` and
  `lotus_settings_schema` print the same JSON.

The registry drives the Settings window ([below](#settings-window-53)).

## Settings window (#53)

`desktop::settings_window`: one separate, resizable, non-modal window per
process ("LOTUS Settings", 860 × 620, minimum 640 × 420). The sidebar gear,
Ctrl+, and a card row's gear (offline cards and running primaries) open it, or
focus it and switch its target if it is open. It replaces the preferences
popover.

- **Targets.** Global (saved defaults) and each card (its saved overrides,
  scope `primary`), whether or not the primary runs. A secondary has no saved
  settings and is not a target; its temporary overrides stay CLI and MCP only.
- **Rows.** The left column picks the target and a registry group; groups with
  no row at that scope are hidden. Each row shows the label, description, value
  and source badge (`default`, `global`, `card`, `managed`, `desktop.json`,
  `host.json`). Worker and input values come from `Supervisor::settings` and
  are written with `Supervisor::set_setting` (`Actor::Desktop`); a save error
  is shown in the header.
- **Controls.** Booleans toggle, choices cycle, integers step by 1; floats by
  1/100 of their range rounded up to 1, 2 or 5 × 10ⁿ (0.1 when unbounded),
  clamped to the bounds. Reset
  removes the target's value ("Reset to Global" on a card, "Reset to default"
  on global). Text, JSON and binding values are shown read-only until #54.
- **Read-only.** Host settings (`host.json`, read at start), values from the
  host `profile` (`managed`), and every worker or input row when the host has
  no `allow_control` or no host configuration. The tooltip says why.
- **Desktop preferences** are the `lotus.desktop.*` rows, validated against
  the registry and saved to `desktop.json` at once. Discord Rich Presence
  cannot be turned on without a usable application ID; the Discord group
  shows the connection state. If `desktop.json` has no location or did not
  parse, the header says changes are not saved.
- **Refresh.** Values reload on target switch, after each edit, when
  `Supervisor::settings_revision` changes (checked every 250 ms; MCP edits in
  the same process) and on activation (edits saved by `ctr-cli` meanwhile).
- **Window state.** Closing it any way (title bar, Esc, the main window
  closing) saves `gpui::Window::window_bounds` to `settings_bounds`; it reopens
  there if the saved size is at least the minimum, otherwise centred. The main
  window closes it. Keys typed in it never reach a guest: it has its own focus,
  and the main window maps keys only while its screens are focused.

`tests/test_settings_window.py` (**verified emulator**, mock worker, real key
events, 2026-11-18): Ctrl+, three times leaves one Settings window, Esc closes
it and saves bounds, it reopens at the same bounds, and closing the main
window closes it. Unit tests cover the steps, every target having rows, and
the bounds minimum. The reload on revision change and on activation is
**source inspected** only. macOS and Linux are not built or run. `--mcp`
mode keeps the process alive after its window closes, with or without
Settings; that predates #53.

## Settings layers (#51)

Worker settings resolve, lowest to highest: registry default (the worker's own
default) → saved global defaults → saved card overrides → a secondary's runtime
overrides → host configuration `profile`. `crates/session/src/settings_store.rs`.

| Layer | Stored in | Written by |
| --- | --- | --- |
| global | `<storage_root>/settings/global.json` | `global` target |
| card | `cards/<card>/lotus-settings.json` | `card` target |
| runtime | memory of the secondary's session | `session` target (secondaries only) |
| managed | host configuration `profile` | the host JSON; read-only here |

- **Sparse files.** `{"version": 1, "values": {...}}` with only explicit entries;
  `unset` removes one and inheritance resumes, `reset` empties the file. The card
  file sits beside `sdmc/`, `nand/` and `sysdata/`; secondaries copy only those
  (`GUEST_DIRS`), so it never reaches scratch.
- **Sessions.** A session resolves its settings at creation: a primary from
  global + its card, a cloned secondary from global + its parent card, a
  parentless secondary from global. Saved changes never touch a running worker.
  A primary's report lists them under `pending` until its card next starts. A
  secondary keeps its creation snapshot. Its runtime changes are applied to the
  worker immediately (the worker refuses non-live classes while running) and
  vanish with it; `unset` re-applies the creation value. A primary session is
  changed through its card. Checkpoint-restored sessions run with the
  checkpoint's settings and refuse changes.
- **Managed.** Host `profile` keys win everywhere and refuse edits at every
  target. A saved entry they shadow is reported as a problem and kept.
  `lotus.host.*` stay in the host JSON and `lotus.desktop.*` in `desktop.json`;
  the stores refuse both. Existing host JSON, CLI flags and `desktop.json` load
  unchanged; there is nothing to migrate, because no worker setting was saved
  before.
- **Robustness.** Each write holds `settings/settings.lock` (OS file lock, so
  other Lotus processes on the storage root wait), re-reads the file, changes
  one entry and renames a complete temporary file over it. Entries this build
  cannot use (unknown, excluded, invalid) are reported and kept. A file that
  does not parse or has another version is reported, listed in `read_only`,
  and never written.
- **Frontends.** `lotus_settings` and `lotus_settings_change` (MCP);
  `ctr-cli settings show|set|unset|reset <host-config> global|card:<name>`.
  Changes need `allow_control`. Portable builds keep `storage_root` at
  `<data>/<backend>/sessions` (`packaging/windows/launch.ps1`), so both files
  stay in the data directory (`data/` in the release folder by default),
  separately per backend.

Evidence: **verified emulator** (mock worker) in
`crates/session/tests/settings_layers.rs`, unit tests in `settings_store.rs`,
plus CLI and MCP tests. `debug settings_set` still writes a worker directly
and is not recorded as a runtime override. Portable packaging was not rebuilt
for this change (source inspected). The MCP group has no settings until automation settings (#56) land. Input
layout keys share these files and layers but never reach a worker.

## Discord Rich Presence

Opt-in (Settings → Discord → Discord Rich Presence; off by default). Owned by
the desktop only; `lotus-mcp` and `ctr-cli` never connect. Policy
(`desktop::presence::derive`): sessions that are Running or Paused with content
count. 0 clears the activity, 1 shows `Playing NAME...`, 2+ with the same
content show `Debugging NAME...`, otherwise `Debugging multiple games`; `state`
reads `N sessions`. Content is compared by the canonical path of the
approved asset, so two files called `game.3dsx` differ. Only the sanitised file
name is sent. "Hide game titles" sends `Playing a game` / `Debugging sessions`.

Transport: `discord-rich-presence` 1.1.0 (MIT) over Discord's local IPC pipe,
on one dedicated thread. The supervisor loop publishes the latest content and
the thread only wakes when it changes. Every SET_ACTIVITY reply is read, so a
dead pipe or an ERROR event is noticed. Reconnects back off from 5 s to 60 s,
with one warning per outage. Shutdown clears the activity (bounded at 2 s).
Application ID: preference `discord_application_id`, else build-time
`LOTUS_DISCORD_APPLICATION_ID`, else the LOTUS application `1572001204404363274`.

Replaced frames are dropped from GPUI's sprite atlas immediately
(`App::drop_image`) so streaming stays bounded.

## Evidence

**Verified emulator** (Windows GNU, AZAHAR and CITnLO at the native pin in
`config/pins.toml`, fixture `t03.3dsx`, 2026-11-16):

- `session native_process`: four sessions created concurrently (two primaries,
  two secondaries), a fifth refused before launch, two advancing at once,
  secondaries' roots deleted and cards kept on stop.
- `desktop native_controller`: primary streams and plays; a second primary on
  the same card is refused; a copied secondary sees the card's file, diverges
  without touching it, runs concurrently; an MCP server sharing the
  controller's supervisor lists both, advances the secondary and moves the
  window's selection; crash isolation; crashed secondary discarded with logs
  kept; primary card persists after stop.
- `tests/test_windowed_mcp.py`: an MCP client launches `lotus-desktop --mcp`
  and drives primary + secondary concurrently, independently and
  asynchronously; no desktop or worker process remains afterwards.
- Unchanged clients: `native_parity` and `tests/test_mcp_native.py` pass with
  their original create calls (kind defaults to secondary).

**Checked by screenshot** (one 100%-scale display): see [24](24-desktop.md).
Paused registers at frame 25 were identical across two separate runs.

**Observed speed**: on this host the software-rendered worker runs `t03.3dsx`
at about 7 guest fps alone (60 frames in 8.7 host s via `run_until`, without
supervision polls, which skip a session held by a run) and about 4–5 fps
each with three running. The speed gauge reports this (≈ 8 %). It is a property
of the current software renderer, the motivation for ADR-009
([29](29-wave-3-plan.md)), not of this interface.

**Layout and presence** (2026-11-16, one 100%-scale 1920 × 1080 display):

- Unit tests: pane plans at 1600/1000/900/720 px, snap at half the minimum, toolbar
  densities, pill sizes, rail/overflow coverage, the preferences round trip,
  and presence policy and thread. The thread tests use a scripted transport
  that covers disabled-never-connects, backoff while Discord is absent,
  reconnect after a failed send, deduplication, and clearing on the last
  session, on disable and on shutdown.
- **Checked by screenshot** (`tools/desktop_shots.py`, window-only
  PrintWindow with posted mouse messages, `runs/ui-shots-02`): 22 scenarios
  covering both panes at 1280/1000/900/720 px, squeeze, every toolbar density
  and menu, compact and expanded gauges, drag snap/reopen/resize for both
  panes (saved widths read back from `desktop.json`), four sessions with
  32-character card names and long asset handles, and the logical sizes that
  125/150/200% scaling gives.
- **Verified against Discord** (the user's running client): `tests/discord_live.rs`
  sees each state confirmed by Discord's reply (none → Playing → Debugging
  same → multiple → hidden → none → off). The full window path (MCP session
  changes, status line read from the preferences menu) shows the same.

Unverified: keyboard paths (Tab order, Ctrl shortcuts; no input was
synthesised outside the window), a real non-100% display scale, Discord
quitting and restarting (covered only by the scripted transport, and by
`tests/test_discord_presence.py`, which needs Discord closed), and other
platforms. No accessibility audit. Host audio stays muted.

Source: `session::{supervisor, card_store, telemetry, process_memory}`,
`mcp::{server, tool_arguments}`, `desktop::{desktop_controller, lotus_window,
session_tree, gauges, theme}`. Rollback: none of this changes the worker or its
protocol; previous behaviour is a headless secondary-only use of the same tools.
