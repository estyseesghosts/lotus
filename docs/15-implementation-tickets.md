# 15 — Dependency-ordered implementation tickets

Use one small PR per ticket unless two changes are tightly inseparable. `INT` = `integration` Azahar branch, `EXP` = `experimental` branch, `RUST` = product repo. Every PR links its regression test and source SHA. **No code below has been implemented as part of this dossier.**

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

## Wave 3 — targeted engineering debugger

### T14 [EXP] CPU/kernel/thread trace

**Deliver:** pause reasons, SVC/IPC and thread events, register/disassembly snapshots, breakpoint tests. **Check:** deterministic crash fixture yields same stop reason/context; perf A/B. **Avoid:** global instruction-by-instruction logging.

### T15 [EXP] GPU event indices and command capture

**Files:** `gpu.cpp`, `pica_core.cpp`; **deliver:** GSP command IDs, PICA command stream, per-draw identity with state/time. **Check:** draw IDs stable for selected test, capture matches actual pre-draw command, bounded loss accounting. **Avoid:** capture of live VRAM long after draw.

### T16 [EXP] Resource lineage and GPU image capture

**Deliver:** texture/surface source versions, transfer events, native pixel/diff images, renderer sync. **Check:** CPU overwrite between two draws distinguishes resource versions, no false identical hashes. **Avoid:** equating host texture format with original 3DS bytes.

### T17 [RUST] Timeline and visual inspector panels

**Deliver:** CPU/kernel/GPU lanes, draw-state diffs, texture/shader tables, snapshot selector, stable source labels. **Check:** 100k-event trace opens without blocking GPUI event loop; resource usage bounded. **Avoid:** expensive polling for hidden panels.

### T18 [RUST+EXP] Differential run analysis

**Deliver:** aligned checkpoint, frame and draw structural diffs; report earliest reliable divergence; corpus runner across stock/integration/experimental. **Check:** synthetic deliberate divergence detected; no nondeterminism misdiagnosed as definite bug. **Avoid:** reliance solely on screenshot visual comparison.

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
