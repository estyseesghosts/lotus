# 15 — Dependency-ordered implementation tickets

Use one small PR per ticket unless two changes are tightly inseparable. `INT` = `integration` (AZAHAR), `EXP` = `experimental` (CITnLO), `RUST` = LOTUS. Every PR links its regression test and source SHA. Current implementation and remaining acceptance checks are recorded in [25 — Wave status](25-wave-status.md); tickets below retain their original requirements.

## Wave 0 — pin sources and reduce integration risk

### T00 [RUST] Repository skeleton and toolchain lock

**Deliver:** Cargo workspace, empty crates, `rust-toolchain.toml`, source-version manifest, CI format/check. **Check:** `cargo test --workspace` passes; no network downloads at runtime. **Avoid:** implementing service APIs before machine boundary is known.

### T01 [INT] Fork/worktree policy and stock baseline

**Deliver:** `integration` branch from selected upstream SHA, `experimental` from integration, immutable stock checkout; patch inventory; build matrix. **Check:** stock Azahar builds and launches homebrew fixture; revisions recorded. **Avoid:** moving branches as build inputs without pinned hashes.

### T02 [RUST] GPUI screen spike, no emulator

**Deliver:** standalone GPUI app with generated native-resolution top/bottom images, scaling, touch coordinate tests, memory/perf observation. **Check:** 10-minute continuous updates, 4 simulated sessions, two high-DPI scale factors. **Avoid:** native Vulkan interop in this ticket.

### T03 [RUST] Protocol scaffold and supervisor stubs

**Deliver:** envelope schema, handshake, typed status codes, local pipe/socket transport, mock worker, process lifecycle state machine and unit tests. **Check:** invalid messages rejected; worker crash detected; stale epoch rejected; GUI-independent CLI can query mock. **Avoid:** encoding C++ pointer values into protocol.

## Wave 1 — native Azahar worker

### T04 [INT] New CMake worker target

**Files:** root `CMakeLists.txt`, `src/CMakeLists.txt`, new `src/ctr_worker`. **Deliver:** `ENABLE_CTR_WORKER=OFF` default; executable linked to native libs. **Check:** Qt-off/Libretro-off build; stock build behavior unchanged. **Avoid:** modifying `citra_libretro`.

### T05 [INT] Minimal headless frontend

**Files:** `emu_window.h` adapter, worker main, required applet/image callbacks. **Deliver:** boot known `.3dsx`; emit lifecycle and error result. **Check:** no Qt/SDL windows, clean stop/restart; unsupported applets explicit. **Avoid:** assuming `Headless` provides screenshot capture.

### T06 [INT] Native frame output

**Deliver:** software backend raw top/bottom buffers with format/orientation metadata and sequence/ring ownership. **Check:** compare stable images to stock Azahar, test stale leases and source layout; no use-after-free. **Avoid:** scaling images for capture or blocking core waiting on UI.

### T07 [INT] Input/audio adapters

**Deliver:** buttons, touch, scripted-exclusive input channel; bounded audio ring, optional muted output; keyboard/applicant fallback events. **Check:** timed key/touch replay, output hashes, headless audio drain, device disconnect. **Avoid:** mixing live input into deterministic script.

### T08 [INT+RUST] Safe-point command queue + IPC integration

**Files:** `core.cpp` pending work, worker queue, Rust transport client. **Deliver:** load/run/pause/reset/status/stop; bounded read-only register/memory inspection, notification waking. **Check:** concurrent read/control requests while frame limiter sleeps; no mutable-state cross-thread access. **Avoid:** claiming selected-core instruction steps until proven.

### T09 [INT+RUST] Per-instance roots and shutdown

**Deliver:** private settings/save/cache/log paths, unique IPC endpoints, worker termination and recovery. **Check:** two workers simultaneously read same ROM but write separate saves; forced-kill of one does not damage other. **Avoid:** incomplete COW layered filesystem at this stage.

## Wave 2 — first useful new emulator

### T10 [RUST] GPUI worker display and session browser

**Deliver:** streamed images, input routing, run control, lifecycle warnings, session switching/docking. **Check:** worker crash does not crash UI; touch mapping correct at zoom/HiDPI; only latest displayed frames consumed. **Avoid:** full-featured GPU panes before stable state snapshots.

### T11 [RUST] Headless CLI and fixture runner

**Deliver:** `create/load/run_until/capture/stop`, scenario format, checkpoints, input lease, report manifest. **Check:** fixed homebrew demo has repeatable result across 3 runs and no unbounded scenario execution. **Avoid:** falsely labeling host timing as hardware time.

### T12 [RUST] Four-worker stress and failure injection

**Deliver:** resource policy and unique artifact roots. **Check:** 4 workers run independent scenarios, one forced crash, no port/cache/saves collision. **Avoid:** unbounded trace memory per worker.

### T13 [RUST] MCP adapter

**Deliver:** small typed tools for session/control/capture/queries, artifact resources, client permissions. **Check:** CLI/MCP parity and no access to arbitrary shell/filesystem. **Avoid:** direct MCP calls into Azahar UDP RPC.

## Wave 3 — comprehensive agentic debugger

Goal, decisions (ADR-008–011), Azahar parity matrix and milestone order: [29](29-wave-3-plan.md). Old 3DS is the default profile; every CPU/kernel/memory check also runs on the New 3DS profile.

### 3.0 Foundations

### T24 [INT+RUST] Settings schema and session profiles

**Deliver:** descriptor table for every `Settings::values` entry and CFG block (name, type, range, default, apply class `boot`/`restart`/`live`/`renderer`/`cfg-savedata`, source location); `settings.list/get/set/diff`; profile fixed at session creation, hash in every manifest/report; worker hard-coded defaults (`frontend.h`) moved into the default profile. **Check:** default profile reproduces Wave 2 screen hashes; `set` of a `boot` setting on a running session is rejected; Old/New 3DS boot test; apply class of each entry cites its consuming source line. **Avoid:** one untyped key/value `set`; changing any stock default in `integration` without its own ADR.

### T25 [EXP+RUST] Stop model, event stream and debug lease

**Deliver:** typed stop reasons (manual, breakpoint, watchpoint, step, exception, `svcBreak`, frame, predicate, PICA event); bounded asynchronous notification channel with loss counters; debug lease arbitrating execution control between desktop, MCP and GDB clients. **Check:** two clients racing pause/continue get deterministic lease errors; event overflow reports loss, worker stays alive. **Avoid:** polling for stop state.

### T45 [INT] Offscreen Vulkan worker

**Deliver:** headless presenter replacing `PresentWindow` surface use; readback of top/bottom screens into the existing frame ring at native and scaled resolution; `physical_device` selection; validation-layer output to the log stream; software renderer remains selectable. **Check:** fixture boots on Vulkan; native-resolution frames compared structurally with software (no byte-identity claim); four Vulkan workers in parallel; device-lost and no-device paths fail cleanly; CI uses a pinned software Vulkan ICD or labels the test opt-in. **Avoid:** GPUI texture interop; window handles in the worker.

### T46 [INT+RUST] Audio output, capture and DSP modes

**Deliver:** audible output for the focused session only (others muted by default); WAV capture of DSP output; HLE, LLE and LLE-multithreaded profiles with user-supplied `dspfirm.cdc` placed in the session root; mic from host device or injected WAV; stretching/realtime/effects-bypass settings; frame+audio dump. **Check:** audio fixture capture hash stable across three runs (HLE); LLE boots with firmware and fails with a typed error without it; switching focus moves audio without glitches in other sessions. **Avoid:** bundling or uploading DSP firmware.

### T26 [RUST+EXP] GDB remote serial protocol server

**Deliver:** `crates/gdb` RSP server on loopback, one opt-in port per session assigned by the supervisor; packets `qSupported`, `target.xml` (ARM + VFP), `g/G/p/P/m/M/X`, `Z0`–`Z4`, `vCont`, `qXfer:threads/osdata`, `T`, `?`, `D`, `k`, HIO `F` replies, attach-to-next-process; CITnLO built with `ENABLE_GDBSTUB=OFF` and HIO rerouted from `svc.cpp` to the debug channel; skeleton (connection, registers) against the mock worker in 3.0, full in 3.1. Later: non-stop mode, `bs`/`bc` (T38). **Check:** devkitARM `arm-none-eabi-gdb` breaks at `main`, `bt`, `watch`, `stepi`, HIO `printf`, on Old and New 3DS; GDB and MCP attached together obey the lease; no listener on non-loopback interfaces; build test proves the Azahar stub is absent. **Avoid:** reusing Azahar's stub; any non-loopback bind.

### 3.1 CPU, kernel and system

### T14 [EXP] CPU, kernel and thread debugger

Split into separately reviewable PRs; one fixture per PR.

- **T14a execute breakpoints:** exact address, process, ARM/Thumb; JIT halt/invalidate; conditions, hit counts, non-stopping logpoints; invalidated on reset/load.
- **T14b watchpoints:** read/write/access ranges through the memory path; range cap, overhead estimate, loss accounting.
- **T14c stepping:** step, step over/out, run-to, frame advance; documented per-core behavior (New 3DS cores).
- **T14d state editing and disassembly:** register (GPR, CPSR, VFP) and memory writes with JIT invalidation; ARM/Thumb disassembly with raw bytes.
- **T14e kernel inspection:** processes, threads, wait objects, handle tables, memory map, deadlock graph; per-thread and all-thread suspend/resume.
- **T14f SVC/IPC trace:** schema-decoded service commands, raw fallback, guest-tick durations; parity with the IPC recorder.
- **T14g exceptions:** guest exceptions, aborts, `svcBreak`; report bundle with registers, nearby memory, recent IPC/GPU events and backtrace; guest and host failures labeled separately.
- **T14h symbols:** ELF/DWARF from the user's build; symbolized disassembly/backtraces; file:line breakpoints.

**Check:** deterministic crash fixture yields the same stop reason/context on each run and both profiles; robustness list in [07](07-debugger-core.md); perf A/B with debugger idle. **Avoid:** global instruction-by-instruction logging; fabricated source lines.

### T27 [EXP+RUST] Logs and guest debug output

**Deliver:** per-session structured log stream with filter/regex settings, `svcOutputDebugString`, HIO stdout; MCP resources. **Check:** fixture output captured in order; flood hits cap and reports loss. **Avoid:** `printf` in hot paths.

### T28 [EXP+RUST] Savestates and checkpoints

**Deliver:** named save/load via `core/savestate`; compatibility keyed by pin + profile hash; breakpoints and leases behave correctly across load. **Check:** save, run, load, run gives identical hashes; mismatched profile is rejected. **Avoid:** loading states across pins silently.

### T29 [EXP+RUST] Movies and determinism profile

**Deliver:** movie record/playback tied to scenarios, read-only mode; determinism profile (fixed init clock/time/ticks, `deterministic_async_operations` on, async FS off). **Check:** three runs identical on Old and New 3DS. **Avoid:** mixing live input into playback.

### T30 [INT+RUST] Complete input and peripherals

**Deliver:** all buttons incl. Home/Power, C-stick/ZL/ZR, circle pad, 3D/volume sliders, gyro/accelerometer, IR, camera image injection, amiibo, headphone and battery state. **Check:** HID/peripheral fixture reads each value. **Avoid:** live host input during scripted runs.

### T31 [EXP+RUST] System, title and content tools

**Deliver:** CIA install, system file setup, HOME Menu boot, title list, NAND/SDMC browse confined to session roots, console ID/MAC regeneration, console unique data as session assets, Gateway cheats, plugin loader, ROM compress/decompress. **Check:** operations cannot escape the session root. **Avoid:** downloading system files on the user's behalf without consent.

### T32 [EXP+RUST] Performance stats and profiler

**Deliver:** perf stats, frame times, microprofile scopes; host time and guest ticks reported separately. **Check:** overhead A/B. **Avoid:** presenting host speed as hardware speed.

### 3.2 GPU

### T33 [EXP] PICA event breakpoints

**Deliver:** stop on each `debug_utils` event (command processed, primitive batch in/out, vertex shader invocation, display transfer, GSP command, buffer swap) with draw/command identity. **Check:** GPU fixture stops at expected counts on software and Vulkan. **Avoid:** stalling the renderer thread outside safe points.

### T15 [EXP] GPU event indices and command capture

**Files:** `gpu.cpp`, `pica_core.cpp`; **deliver:** GSP command IDs, PICA command stream, per-draw identity with state/time. **Check:** draw IDs stable for selected test, capture matches actual pre-draw command, bounded loss accounting. **Avoid:** capture of live VRAM long after draw.

### T34 [EXP+RUST] Command list and shader inspection

**Deliver:** command list viewer data, register/uniform state at a draw, vertex shader per-vertex inputs/outputs and instruction trace; `dump_command_buffers`. **Check:** shader trace matches the shader interpreter output for the fixture. **Avoid:** tracing all vertices by default.

### T16 [EXP] Resource lineage and GPU image capture

**Deliver:** texture/surface source versions, transfer events, native pixel/diff images, renderer sync. **Check:** CPU overwrite between two draws distinguishes resource versions, no false identical hashes. **Avoid:** equating host texture format with original 3DS bytes.

### T35 [EXP+RUST] Surface and texture viewer

**Deliver:** decode any address/format/size; texture dump; custom texture settings. **Check:** decoded fixture textures match known bytes. **Avoid:** reading GPU-backed pages without recording cache sync.

### T36 [EXP+RUST] CiTrace record and replay

**Deliver:** record/replay through `core/tracer`. **Check:** replayed trace reproduces captured frame. **Avoid:** unbounded trace files.

### T37 [EXP] GPU timing and shader settings

**Deliver:** `simulate_3ds_gpu_timings`, `delay_game_render_thread_us`, `shaders_accurate_mul`, shader JIT/interpreter, hardware shaders and caches exposed with time-domain labels. **Check:** each setting changes the documented observable on a fixture. **Avoid:** any accuracy claim without hardware evidence.

### 3.3 Beyond Azahar

### T38 [RUST+EXP] Rewind

**Deliver:** snapshot ring plus input log; reverse step/continue; GDB `bs`/`bc`. **Check:** reverse-continue to a watchpoint reaches the same write as a forward run. **Avoid:** unbounded snapshot memory.

### T39 [RUST] Fork sessions from a savestate

**Deliver:** N sibling workers from one state with independent roots and optional profile overrides. **Check:** forks diverge only by injected differences. **Avoid:** sharing writable storage between forks.

### T40 [RUST+EXP] `run_until` predicates

**Deliver:** PC reached, memory condition, SVC, log regex, frame N, draw ID; bounded with timeouts. **Check:** each predicate stops the fixture at the expected point. **Avoid:** unbounded waits.

### T18 [RUST+EXP] Differential run analysis

**Deliver:** aligned checkpoint, frame and draw structural diffs; report earliest reliable divergence; corpus runner across stock/integration/experimental. **Check:** synthetic deliberate divergence detected; no nondeterminism misdiagnosed as definite bug. **Avoid:** reliance solely on screenshot visual comparison.

### T41 [RUST] Profile matrix runs

**Deliver:** one scenario across JIT/interpreter, Old/New 3DS, accurate-mul, GPU timings, software/Vulkan and backends; earliest divergence per pair via T18. **Check:** a known JIT-sensitive fixture is flagged. **Avoid:** treating a divergence as a bug without a reference.

### T42 [RUST+EXP] Memory scanning

**Deliver:** narrowing value search (exact, changed, unchanged, range), typed views, pointer chains. **Check:** fixture variable located in bounded passes. **Avoid:** scanning MMIO.

### T47 [EXP+RUST] DSP inspector

**Deliver:** HLE DSP source/voice/mixer state snapshots. **Check:** fixture voice parameters read back. **Avoid:** claims about LLE internals.

### T43 [RUST] Agent debugging test suite

**Deliver:** devkitARM fixtures with known bugs (null dereference, deadlock, New-3DS-only race, wrong texture, heap overwrite, HIO output). **Check:** each is diagnosed through MCP alone, with the transcript retained. **Avoid:** fixtures that depend on commercial assets.

### 3.4 Surfaces

### T17 [RUST] Timeline and visual inspector panels

**Deliver:** CPU/kernel/GPU lanes, draw-state diffs, texture/shader tables, snapshot selector, stable source labels; panels for settings, breakpoints, threads, IPC, logs and audio. **Check:** 100k-event trace opens without blocking GPUI event loop; resource usage bounded. **Avoid:** expensive polling for hidden panels.

### T44 [RUST] Surface parity gate

**Deliver:** test that every protocol method has CLI and MCP surfaces; parity matrix in [29](29-wave-3-plan.md) generated from it. **Check:** CI fails on a deliberately unexposed method. **Avoid:** hand-maintained parity lists.

## Wave 4 — accuracy research

### T19 [RUST/hardware] Coffeepot 3DS microbenchmark integration

**Deliver:** physical-console harness/suite, metadata parser, timer overhead calibration, raw fixture corpus. **Check:** first 3 physical experiments reproduced in separate sessions and compared to Azahar. **Avoid:** crediting unrun code as hardware-verified.

### T20 [EXP] Expose timing assumptions

**Deliver:** list of virtual tick/transfer completion assumptions, tracing of source parameters and output event times, configured deterministic ticks. **Check:** trace accounts for timing events with correct domains; regression suite unaffected when disabled. **Avoid:** replacing model with game-specific per-title waits.

### T21 [RUST/EXP] Independent PICA register/shader reference

**Deliver:** Rust component able to parse selected command lists and execute a minimal shader/reference draw; differential fixtures. **Check:** known hardware outputs match, with errors cataloged; feature-gated and doesn't break integrated backend. **Avoid:** promising full PICA rasterizer on first PR.

### T22 [EXP] First measured GPU timing correction

**Deliver:** one documented timing behavior modeled against isolated and held-out physical tests. **Check:** improved physical error on target cases without known regressions, release flag and confidence labels. **Avoid:** accuracy claims from host profiler measurements.

### T23 [RUST] CPU and memory reference emulator research track

**Deliver:** small ARM11 interpreter/test harness, architectural conformance tests and cache/timing experiment plan. **Check:** instruction results match hardware cases; no requirement to boot games yet. **Avoid:** replacing working Dynarmic prematurely.

## Architectural decisions still to research

- Exact worker ownership of separate renderer/audio threads; benchmark best race-free safe-point boundary.
- Whether real renderer exports native screen buffers conveniently without render-to-surface changes.
- Whether a shared immutable shader cache is safe across worker processes.
- GPUI texture import API viability; postpone until native-frame pipeline has measured limitations.
- Complete save-state fidelity across worker revisions; scripted checkpoint remains fallback.
- Independent native firmware boot path and distribution constraints.

## PR review form

For each ticket include: `source SHA`, `branch`, `touched native files`, `why implementation belongs here`, `feature flag`, `before/after command or screenshot artifact`, `tests performed`, `hardware verification status`, `host performance A/B`, `known risk`, `rollback`. See templates.
