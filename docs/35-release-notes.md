# 35 — Release notes

Changes per published release. Evidence and design live in the linked
documents; this page only says what changed. Labels follow the
[glossary](glossary.md) evidence vocabulary.

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
