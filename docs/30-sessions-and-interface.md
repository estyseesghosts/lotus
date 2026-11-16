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
| Guest SD/NAND | the card's own `sdmc/`, `nand/`, `sysdata/`; real reads/writes that persist | empty, or a point-in-time host copy of one card's guest directories |
| On stop | card kept | root deleted; after a crash or failed stop only logs are kept |
| Concurrency | one primary per card (registry check + the worker's `worker.lock`) | any number, limit permitting; several may copy one card |
| Sidebar | top-level row | nested under the card it copied; a parentless secondary is top-level |

A card is created on first use; names are 1–32 of `a-z 0-9 - _`. The guest
directory names are Azahar's (`SDMC_DIR`, `NAND_DIR`, `SYSDATA_DIR` in
`src/common/common_paths.h`), selected by `FileUtil::SetUserPath(root)` in
`ctr_worker/frontend.h`. A copy is taken while the parent may be running and is
**not** coordinated with its guest writes; copies over 4 GiB, symlinks and
junctions are refused. On start, a host discards guest data in any unlocked
`scratch/` root left by a previous process. This is a private-copy design, as
[10](10-multi-instance-and-storage.md) prefers over unproven copy-on-write.

## Shared supervisor

`session::supervisor::Supervisor` is thread-safe. Each session has its own async
lock; the registry lock is never held across a worker exchange. A 600-frame
`run_until` on one session therefore never delays another session's commands,
status or preview. Creation reserves a slot (and the card) before the slow
spawn, so concurrent creations respect `max_sessions` (default 4, the tested
load; configurable to 16).

The **pump** (every 100 ms in both frontends) polls each idle session's status,
re-issues bounded `run` batches for sessions marked playing, and folds replies
into telemetry. A session held by another command is skipped, not awaited. A
batch that ended with `unsupported_applet` or `core_error` stops playback;
`host_deadline` (slow batch) does not.

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
`frontend: headless|windowed`. New or changed relative to [23](23-local-mcp.md):

| Tool | Behaviour |
| --- | --- |
| `lotus_create_session` | `kind` `primary` (needs `card`) or `secondary` (default; optional `card` to copy); optional `asset` loads paused |
| `lotus_load` | replace content in an owned session; paused |
| `lotus_control` | `run` (continuous, returns at once), `pause`, `reset`, `stop` |
| `lotus_run_until` | exactly 1–600 frames, then paused; ends continuous running |
| `lotus_list_sessions` | ids, kind, card/parent, asset and gauges; never waits on a busy worker |
| `lotus_list_cards` | cards and which session mounts each |
| `lotus_registers` | r0–r15 and CPSR of one core at a safe point |
| `lotus_focus_session` | windowed: show that session to the person; headless: no effect |

Host configuration gains `asset_directories` (handle → directory; each `.3dsx`
directly inside is approved as `<handle>/<file>`, re-listed on demand, so
rebuilt patch variants can be loaded without a restart) and `max_sessions`.
Existing configurations parse unchanged. A desktop user may also open any
`.3dsx` with the file picker; it is approved as `local/<file>` for that process.
MCP clients still cannot name paths.

## Window

- **Sidebar** (Ctrl+B collapses to a rail of state chips): cards with their
  primary, secondaries nested beneath; square dot = primary, round =
  secondary, colour = state. Hover: `+` starts a secondary copying that card
  with the same content; `×` stops (primary data kept, secondary discarded).
  `+` beside SESSIONS opens an existing card, creates the next free card name,
  or starts an empty or copied secondary. Footer: MCP link and slot use.
  Icon buttons and rail chips carry tooltips (compiled; not yet seen on screen,
  since synthetic hover does not reach an inactive window).
- **Toolbar**: name, PRIMARY/SECONDARY badge, card, state, content; Run/Pause,
  Step (one frame), Reset, Load (approved content or Open file…), Stop; Fit /
  1× / 2× zoom; stacked or side-by-side screens; Inspector.
- **Gauge strip** under the screens; **Inspector** (Ctrl+I): session fields and
  core-0 registers, fetched only while paused and open.
- Keys: Z/X A/B, A/S X/Y, Q/W L/R, arrows, Enter/Backspace Start/Select; Space
  run/pause; `.` step; Ctrl+O open file. Desktop loads start running; MCP loads
  stay paused.
- Theme: Geist-derived dark neutrals with colour only for state
  (`desktop::theme`, after the workspace's `vercel.md`). Segoe UI and Consolas,
  since Geist is not assumed installed.

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
the pump, whose status polls skip a session held by a run) and about 4–5 fps
each with three running. The speed gauge reports this (≈ 8 %). It is a property
of the current software renderer, the motivation for ADR-009
([29](29-wave-3-plan.md)), not of this interface.

Unit tests cover card names, copy/discard/sweep, reservations, asset
directories, telemetry windows, tree building, fit scaling and formatting.
Limits: no hardware claim; host audio stays muted; two-DPI and other
platforms untested; no accessibility audit.

Source: `session::{supervisor, card_store, telemetry, process_memory}`,
`mcp::{server, tool_arguments}`, `desktop::{desktop_controller, lotus_window,
session_tree, gauges, theme}`. Rollback: none of this changes the worker or its
protocol; previous behaviour is a headless secondary-only use of the same tools.
