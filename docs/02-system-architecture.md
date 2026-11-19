# 02 — Architecture and ownership boundaries

## Components and process graph

```text
GPUI desktop ──────┐
CLI (Rust) ─────────┼──> Session supervisor (Rust) ──> per-worker control socket
MCP (Rust) ─────────┘            │                       ├─ worker 1 (native C++/Azahar)
                                │                       ├─ worker 2 (native C++/Azahar)
                           artifact store              └─ worker N
                                │                         │
                         traces / reports         shared-memory image ring
                                └─────────────────────────┘

Physical Old 3DS experiments ──> evidence database and comparison runner
```

### Product control plane (Rust)

- `session`: owns child process creation, lifecycle, directory isolation, permissions, lock files, start/restart policy, device assignment, resource quotas.
- `protocol`: versioned request/response and async event schemas; type validation; no Azahar C++ internals exposed.
- `trace`: framed files with bounded event buffers, index/chunks, dropped-event markers, source metadata.
- `runner`: scripted sequences, input ownership, capture rules, comparison and failure shrinking.
- `debugger`: stateful orchestration above raw commands: CPU stepping, symbolication, watchpoint budgeting, kernel inspection.
- `desktop`: GPUI tabs, layouts, display, inspector panes, user actions; never accesses native Azahar internals.
- `mcp`: small stable tool surface and explicit instance handles; no unrestricted process control.
- `accuracy`: fixture reader, mathematical tolerances, physical-console datasets, confidence/evidence labels.

### Data plane

Small control commands travel over local IPC. Large, high-frequency data never does: native images through shared-memory rings; large memory snapshots/trace exports through on-disk artifacts with content hashes, optional shared segments for one-shot transfers. The supervisor publishes short metadata and reference handles, not megabytes as JSON.

### Worker (one process per emulated console)

- Native C++ executable links `citra_core`, `video_core`, `audio_core`, platform dependencies and a thin worker adapter.
- Worker owns one `Core::System`, one emulation thread, renderer resources, input bridge, audio producer and per-instance files.
- Rust supervisor is *not* linked to `Core::System` directly. Worker exit cannot take down GPUI or other emulator instances.
- Emulated-state changes should be accepted through a worker control queue, applied on the core's emulation thread at safe points.
- Backend present stages: software renderer (initial reference), Vulkan offscreen (performance) and optional later cross-API GPU texture interop.

## Boundaries that must remain stable

**A. Worker protocol:** session create/load/pause/resume/step/read/trace/capture/shutdown, result/error semantics, events and state version. Adding a Rust renderer later should not change this contract.

**B. Artifact formats:** images, binary frame captures, CPU snapshots, trace streams and metadata need versioned schemas. A capture from experimental backend should still open in UI after future upgrades.

**C. Hardware test definition:** the same test program + binary hash + seed + input script can execute on a physical unit and in both workers; differences are comparable.

**D. Debugger model:** breakpoint/watchpoint concepts, PC/core/thread identity, memory views and event timestamps independent of backend internals.

## Where not to over-abstract

Do not define a C ABI for every GPU register in phase 1. The C++ worker can issue high-level snapshot/capture responses while the experimental branch adds internal events. Do not fabricate an interchangeable Rust CPU and GPU interface until a second implementation and conformance tests exist. At first, only the **worker** is replaceable; hardware units are not.

## Coherency and serialization

A debugger snapshot spans CPU registers, virtual memory, GPU pending work and potentially a host GPU cache. "Paused CPU" ≠ "all mutable state coherent." Define `snapshot_kind`:

- `cpu_safe_point`: guest execution stopped, registers/thread/kernel state stable, memory observations through the emulated memory API.
- `gpu_sync`: requires named framebuffer/texture resources made visible through the selected renderer; may stall host GPU.
- `full_checkpoint`: snapshot of all supported serializable state at a validated boundary, refuses when pending unsupported async work exists.
- `telemetry`: non-blocking samples, explicitly stale/approximate and cannot substitute for a checkpoint.

Do not promise atomic physical-bus state in an HLE machine that never modeled such a bus. State the limits of the snapshot's meaning.

## Time domains

Store separately:
- `guest_ticks`: current Azahar timing ticks, monotonic within a run but not necessarily correct hardware cycles.
- `host_monotonic_ns`: profiler overhead and transport latency; never compare directly to 3DS cycles.
- `guest_frame_seq`: presentation sequence and boundary events.
- `hardware_measured_ns` and `hardware_counter_*`: physical-console experiments with clock/model/version metadata.
- `model_estimate_ns`: experimental timing estimates with confidence and model revision.

When traces from different clocks are merged, preserve original time domain and an explicit synchronization relation; never simply sort all numbers as if they share a clock.

## Failure domains

- Crash: `worker.exited` event with exit code, captured stderr/log path; invalid shared-memory handles; session enters failed state.
- GPU hangs: watchdog sees no frame; do not automatically claim guest hang because pause, shader compilation or blocked debug capture can produce similar symptoms. Keep control channel responsive if possible.
- UI crash: workers have supervisor-defined lifetime policy (terminate by default; optional headless continuation explicitly enabled).
- IPC loss: command IDs deduplicate responses where supported; unknown state-changing command result yields `status=unknown`, not false success.
- Invalid save/write: do not mutate base NAND, rollback private overlay as needed.

## Threat model

ROMs, saves, NAND and firmware are untrusted binaries/data, sometimes personal. Worker parser bugs and MCP command abuse can be dangerous. Start with local pipes scoped by OS permissions, no LAN listener, deny arbitrary file path traversal from MCP, normalize paths, enforce byte/time limits and treat captures as user-owned potentially sensitive artifacts. OS process isolation is useful but is **not an enforced sandbox** by itself.

## Dependency policy

The Rust workspace and native Azahar build are independently locked. One script resolves pinned `integration` commit, compiler, CMake generator, CMake flags, GPUI Git SHA and Rust version; a manifest reports them all. The worker protocol version is independent of the C++ commit and must be tested for compatibility at handshake.

The desktop-only, opt-in Silvestris adapter is mapped in
[38](38-silvestris-adoption.md): ui_framework.rs initializes the shared theme
and fonts, theme.rs selects bundled font roles, and settings_window.rs mounts
the first shared control. The controller/worker boundary above is unchanged.
