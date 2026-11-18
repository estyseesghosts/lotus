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
  `×` stops. Footer: MCP link, slot use, and the preferences gear. Long names
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
  file; Ctrl+1–7 rail views; Ctrl+[ / Ctrl+] cycle views; Tab / Shift+Tab move
  focus; Esc closes menus.
- **Icons** are original 16 × 16 stroke SVGs (`desktop::icons`), tinted by
  text colour, so they stay sharp at any scale.
- **Theme**: Geist-derived dark neutrals with colour only for state
  (`desktop::theme`, after the workspace's `vercel.md`). Segoe UI and Consolas.

Pane widths, open states and Discord options persist in `desktop.json`
(`desktop::preferences`): `LOTUS_DESKTOP_PREFERENCES`, else
`%APPDATA%\LOTUS\desktop.json`, else `$XDG_CONFIG_HOME/lotus/desktop.json`.
The portable launcher keeps it in `data/`. If the file exists but does not parse,
it is never overwritten, and the preferences menu says so.

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
- Separate from the workspace, and never changed by it: the inspector target
  (`Update::selected`), controller input, audio focus and worker execution.
  Outside a split, selecting a session in the sidebar or with
  `lotus_focus_session` shows it via `single`; selection never collapses a split.
- A stopped session leaves the workspace when it is unregistered: a split
  collapses to the other tile, whose worker keeps running; a single view becomes
  empty. A crashed session stays registered and keeps its tile.
- Headless hosts never enable it: `lotus_status.workspace.available` is false and
  every change returns `Unsupported`. MCP changes need control permission.
- `desktop.json` keeps `split_axis` and `split_ratio`, the defaults for a `tile`
  that omits them. Tiled sessions are not saved: session IDs end with the host,
  and restoring a layout never starts a worker.

Evidence: `workspace::tests` (transitions, idempotence, invalid and stale
requests leave the state unchanged, removal fallback) and
`supervision.rs` `workspace_arranges_without_touching_workers` /
`workspace_writes_from_mcp_need_control` against `lotus-mock-native` workers
(**verified compiled**).

## Controllers (#59)

`lotus-input` owns physical gamepads. `lotus-desktop` starts one
`ControllerService` per process, on its own `lotus-input` thread, so polling is
independent of GPUI rendering and of which session is selected. Workers and GPUI
never enumerate devices. Events are not routed to sessions yet (#61); the
desktop drains them and reports devices and counters in
`lotus_status.frontend_metrics.controllers`.

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

## Discord Rich Presence

Opt-in (preferences gear → Discord Rich Presence; off by default). Owned by
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
