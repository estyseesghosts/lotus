# 35 — Release notes

Changes per published release. Evidence and design live in the linked
documents; this page only says what changed. Labels follow the
[glossary](glossary.md) evidence vocabulary.

## v0.0.7 (unreleased)

- Linux CI for the Rust workspace on Ubuntu 24.04: build, clippy, tests and a
  headless smoke test; GPUI gets its Wayland and X11 backends (#102).
- `worker.lock` is retried for up to 250 ms on Unix while a sibling fork
  still holds an inherited copy (#116).
- Unix workers run in their own process group; abort, the stop kill switch
  and clean-stop cleanup kill the whole group, so descendants no longer
  outlive their worker (#104).
- Linux AppImage launcher: data defaults to `~/.local/lotus` (override with
  `--data-dir` or `LOTUS_DATA_DIR`), `host.json` is regenerated per start, and
  an old `desktop.json` can be imported without overwriting (#105).
- Faster CI: no 2-job cargo cap, dependency-only caching (Swatinem/rust-cache),
  fmt and clippy in a parallel job, mold on Linux; dev builds keep line tables
  only (#117).

## v0.0.6 (2026-11-18)

Pins (config/pins.toml): integration (AZAHAR) `b04d8a6c`, experimental
(CITnLO) `c3422b0a`. The integration pin binds `use_vsync` once (#50).

- One transactional window workspace model shared by every window (#67).
- Host-owned gilrs controller service (#59); keyboard, pointer and
  controllers routed per session (#61); controller pairing policy, identify
  and reconnect (#62). Per-controller routing metrics, session ownership
  denials, and a two-pad soak test with a stalled sibling (#66).
- Input-layout preset format and resolver: stable 3DS action IDs, portable
  keyboard/controller bindings, global → card → secondary overrides; the
  builtin preset reproduces the shipped keys (#57).
- Preset import refuses binding values of the wrong JSON type instead of
  unbinding the action, and returns what it dropped as `warnings` (#114).
- A stopped session's controller is unassigned within 250 ms even while
  another pad streams events; before, cleanup waited for input to go quiet
  (#111).
- An MCP `lotus_workspace single` of a session other than the sidebar
  selection is no longer reverted on the next desktop tick (#112).
- Dragging a split tile to the other edge keeps keyboard focus and each
  session's screen arrangement with the session instead of the slot; outside a
  split the keyboard goes to the shown session, not the sidebar selection (#110).
- Space, `.`, Tab, Esc and modifier keys can no longer be bound to 3DS
  actions: settings reject them and import drops them with a warning. Holding
  Space or `.` no longer leaks the key to the session as a game key (#113).
- Settings window (#53): one non-modal window (gear, Ctrl+, or a card's gear)
  editing global and per-card settings from the registry with their sources,
  and the desktop preferences it replaces the popover with. Text, list and
  binding values are read-only until #54.
- Input mapping profiles (#63): `lotus.input.*` settings layered like worker
  settings and applied live to each session's routing. Per-session mappings
  with Azahar's radial dead zone, sensitivity, inversion and a 0.5 axis
  threshold. Remapping while held releases first. Builtin `default` and
  `xbox-labels` presets, a saved preset library, and `lotus_input_presets`
  (MCP).
- Circle Pad and C-stick carried through the fast input path (`analog_input`
  feature, #60).
- Scenario memory, event and coverage assertions execute (#94).
- Settings registry of every Azahar and Lotus setting with apply class,
  scope and bounds; drift checks against the worker; `ctr-cli settings
  schema` and MCP `lotus_settings_schema` (#50).
- Saved global and per-card settings layers plus secondary runtime
  overrides; `ctr-cli settings show|set|unset|reset`, MCP `lotus_settings`
  and `lotus_settings_change` (#51). Saved changes do not reach a running
  worker; they are listed as pending until the session is recreated.
- The portable launcher takes its data directory from `-DataDir`, a
  `lotus.json` beside it, or `%APPDATA%\LOTUS\lotus.json` (`-SetDataDir`),
  defaulting to `data\` beside the launcher; `desktop.json` moves with it.

Controller paths are verified against tests and the mock worker only; real
controller testing is still pending. Apply-class enforcement for running
sessions (#52) is not in this release.

## v0.0.5 (2026-11-18)

Pins: integration (AZAHAR) `7cd04970`, experimental (CITnLO) `435684c0`,
both pushed to the fork's `integration` and `experimental` branches; the
`vendor/azahar` gitlink is the integration commit (#42). These are the first
pins that contain the native side of the v0.0.4 features below.

Workers list their optional control features in `hello`/`status` `features`,
and the host refuses a request that needs a missing one with a typed
`Unsupported` naming it, instead of sending a field an older worker ignores
(docs/23). Feature matrix of these pins:

| feature | AZAHAR | CITnLO |
| --- | --- | --- |
| `flush_storage` (card copy at one instant, #28) | yes | yes |
| `deterministic_async` setting (#29) | yes, off by default | yes, on by default |
| `preview_nonblocking` (#25) | with the shared-memory preview | with the shared-memory preview |
| `gpu_memory` (#46) | Vulkan builds | Vulkan builds |
| `event_subscriptions` (#26) | no | yes |
| `gpu_capture_levels` (#24) | no | yes |
| `state_hash` (#29) | no | yes |
| `analog_input` (#60, from integration `b2b6f6dd` / experimental `3ef0f937`) | yes | yes |

Release builds run `tests/test_packaged.py` on the assembled ZIP: its
`lotus-mcp` against both of its workers on the debugger fixture (handshake and
pinned source commit, the feature list per backend, GPU counters, stops-only
subscription under a log flood, card copy, preview, and two CITnLO copies
reaching the same `state_hash`). Publication depends on it; the report is
published as `LOTUS-v<version>-packaged-acceptance.json`.

- Vulkan allocation failure and device loss are reported as typed session
  failures instead of aborting the worker, and new Vulkan sessions are
  admitted on device-memory headroom (#46).
- `GeometryEmitter` state, PICA shader uniforms (`Uniforms::f`/`b`/`i`) and HLE
  DSP source frames (`current_frame`, `backup_frame`) are value-initialized on
  both branches. They are serialized but had no initializers, so uninitialized
  heap bytes made two copies of one card differ in `gpu` (about 1 in 8 to 14
  runs) and `dsp` (about 1 in 150) after 30 frames. With all three, 300 of 300
  pairs matched (#42).

## v0.0.4 (2026-11-17)

Changes since v0.0.3 (2026-11-16). Pins are unchanged: integration (AZAHAR)
`b4b9f0c2`, experimental (CITnLO) `559f3f4d`.

**Correction (#42).** The v0.0.4 ZIP was built from those v0.0.3 pins, so its
workers lack the native side of several items below: the stops-only
subscription (they ignore `hello.subscriptions` and push every event), GPU
capture levels (they ignore `capture`), `state_hash`, `flush_storage`, the
non-blocking Vulkan preview, and the paused-wait and stdin latency fixes
(#22). The host side shipped; the worker side ships with the next release
(see Unreleased).

### Concurrent sessions

- Each session is supervised on its own task with its own live-input queue,
  and supervision continues under back-to-back commands.
- A card copy at one guest instant, and a copy that waits for its primary to
  finish starting.
- Workers shut down concurrently within one deadline.
- Worker requests no longer run inside the desktop's publication loop.
- A host policy with interactive and batch budgets.
- Artifacts are retained within a bounded quota with per-session floors.
- Worker binary hashes are cached and shader caches are seeded per worker build.
- GPU capture level is chosen per request.
- The MCP server subscribes to stops only; event delivery is measured under a
  log flood.
- A concurrent-session benchmark runs through the real supervisor
  (`tools/bench_parallel.py`, also run as a CI smoke with mock workers).
- Find the first divergence between two sessions (#29).

### Desktop

- The last preview stays visible while a Vulkan worker has nothing newer.

### Known limits

- Unchanged from v0.0.3. No hardware accuracy is claimed.

## v0.0.3 (2026-11-16)

Changes since v0.0.1 (2026-11-15). No v0.0.2 was published. Pins: integration
(AZAHAR) `b4b9f0c2`, experimental (CITnLO) `559f3f4d`, upstream control
`11eceda5` (`config/pins.toml`). v0.0.1 shipped both runtimes from the same
commit, `05773bbb`.

### Debugger (Wave 3, CITnLO)

The comprehensive agentic debugger from [29](29-wave-3-plan.md), with evidence
in [31](31-wave-3-progress.md) and handoff in [33](33-wave-3-retrospective.md):

- Safe-point CPU, kernel and memory inspection, breakpoints and watchpoints
  ([07](07-debugger-core.md)).
- Rewind with reverse step and continuous reverse continue.
- PICA draw capture with frozen buffer lineage, bounded CiTrace recording and
  replay ([08](08-gpu-and-trace.md)).
- Movies tied to scenarios and a determinism profile ([22](22-scenario-runner.md)).
- Owned DSP, microphone, peripheral and filesystem fixtures; guest cameras
  receive a private BMP.
- 174 settings classified by the code that consumes them (live, renderer,
  boot, cfg-savedata, inert). Non-live settings are refused while running.
- A Rust GDB server, checked with devkitPro GDB 14.1 including Thumb code. It
  refuses to start on AZAHAR.
- Every debugger command is reachable from both `ctr-cli debug` and the MCP
  `lotus_debug` tool ([32](32-debugger-command-parity.md)). That records
  reachability, not backend acceptance.

### Sessions and desktop

- One supervisor drives LOTUS headless or windowed. Primary sessions sit on
  persistent cards; secondaries are disposable ([30](30-sessions-and-interface.md)).
- Every inspector view shows live data. Panes are resizable and snap-collapsible
  and remember their widths. The inspector rail is always visible, and the
  toolbar and gauges adapt to the window width (#13–#17,
  [30](30-sessions-and-interface.md#window)).
- Opt-in Discord Rich Presence (#18, [30](30-sessions-and-interface.md#discord-rich-presence)).
- Sessions prefer Vulkan and fall back to software when Vulkan load is
  unsupported. The fallback is recorded as `renderer_fallback` (#5).
- A desktop started hidden by its parent shows its window once (#3).

### Performance (issues #1–#12, [34](34-performance-and-issue-triage.md))

- Execute breakpoints stay in the JIT. Before, 1 or 64 armed breakpoints hit
  the 20 s deadline; now they cost about 1 % (#8).
- The PICA fast path is restored whenever GPU tools are off (#4).
- Live previews come from a shared-memory ring in both workers, with
  asynchronous Vulkan readback. No preview files are written (#6,
  [21](21-native-control.md#ownership-and-budgets)).
- Rewind checkpoints:
  - Interval of 1–60 frames, and a selectable zstd level.
  - A run-length page-table savestate format that still loads old states.
  - Per-checkpoint cost reporting.
  - On a busy host, serialize time fell from about 720 to 94 ms per Old 3DS
    checkpoint (#9).
- Fixed a reverse-continue bug that landed on the wrong hit after the
  breakpoint set changed (#9).
- Input snapshots are built only when a checkpoint is due. Before, the worker
  made at least 300 heap allocations per frame with rewind off (#7).
- `tools/bench.py` records host and build metadata, rejects Debug builds and
  labels busy hosts. `tools/bench_frontends.py` compares headless with
  windowed (#10).
- `build-azahar.ps1 -Lto` and `scripts/lto-ab.ps1` (#11). LTO stays off until
  the A/B runs on a quiet machine.
- Shutdown counts, closed lifecycle and scratch cleanup (#1). 3DSX titles
  without an NCCH title id load (#2).

### Build and packaging

- lotus-desktop's build script stops MinGW's `default-manifest.o` from being
  linked next to GPUI's manifest (`crates/desktop/build.rs`). Earlier
  executables had two ID-1 manifests in the same language, so it was undefined
  whether Windows applied GPUI's PerMonitorV2 DPI awareness and Common
  Controls v6.
- Protocol: `rewind_configure.compression_level` and
  `rewind_configure.interval_frames`, plus the control method `preview`
  (`specs/debug-v1.schema.json`, [21](21-native-control.md)).

### Known limits

- Timings in [34](34-performance-and-issue-triage.md) come from one busy
  Windows host; the quiet-machine commands are at its end.
- Guest screens can differ between repeats of the same configuration, which
  predates this work and whose cause is not found ([34](34-performance-and-issue-triage.md)).
- Two-DPI interaction, audio playback and non-Windows hosts are unverified
  ([25](25-wave-status.md)). No hardware accuracy is claimed.
