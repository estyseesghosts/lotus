# 16 — Azahar patch blueprint: source edits, seams and validation

This is an implementation **blueprint, not a verified patch**. Function names and paths marked **inspected** came from Azahar's default source in November 2026. Example hooks/flags are proposed and must be reconciled against a pinned commit, built and tested. Use it to make targeted PRs, not to automate mass changes without a compilation pass.

## `integration` patch series: smallest required native integration

### INT-01: worker executable and CMake target

1. In root `CMakeLists.txt`, add `ENABLE_CTR_WORKER` option defaulting `OFF`. Do not force Qt or Libretro into an alternate combination.
2. `src/CMakeLists.txt`: add `add_subdirectory(ctr_worker)` under the option. Existing `citra_core`, `video_core`, `audio_core`, `input_common`, `network` are already built as static library targets in the source tree.
3. New `src/ctr_worker`: native executable with a thin `main`, `WorkerEmuWindow`, `WorkerInput`, `WorkerFrame`, `WorkerAudio`, `WorkerControl`, `WorkerPaths`.
4. Use build config to toggle software and Vulkan renderer; first prove `ENABLE_QT=OFF` still links and loads a test program.
5. Do not reuse the Libretro `retro_run`/callback object or its global frontend state; our execution is through `Core::System::Load`/`RunLoop`.

**Review gate:** diff of upstream CMake with `ENABLE_CTR_WORKER=OFF` shows no changed behavior. `--target ctr-worker` exists only with option ON. CI builds both worker and stock Qt frontend from separate clean directories.

### INT-02: frontend and applets

Inspected: `src/core/frontend/emu_window.h` defines `EmuWindow`, `WindowSystemType::Headless`, `WindowSystemInfo`, `PollEvents`, touch event methods and framebuffer layout. Implement a real subclass with owned lifetime longer than `Core::System` use, not a borrowed pointer into transient Rust memory. Investigate existing frontend code for additional image, software keyboard, Mii selector, camera and microphone registrations.

**Likely ownership shape:**

```text
WorkerMain
  -> WorkerPaths              choose isolated root BEFORE Azahar init
  -> WorkerEmuWindow          window info/layout/input adapter
  -> WorkerFrontendServices   applet/image/mic callbacks
  -> WorkerSystemController   Core::System::GetInstance()
  -> WorkerVideoSink          raw image ring
  -> WorkerAudioSink          ring/discard playback
  -> WorkerIpcServer          queues commands; no direct guest mutation
```

Do not hide unavailable applet callbacks behind "always accept" unless that is existing Azahar behavior and the run is explicitly compatibility-mode. Make it visible to the test harness.

**Acceptance:** front-end logs initialization → load → first frame → input → quit; CPU and GPU execution continue without any Qt event loop. Touch mapping matches bottom-screen native coordinates at multiple UI scales.

### INT-03: control queue and safe-point dispatcher

Inspected: `Core::System::ProcessPendingWork` handles GDB and existing scripting RPC and can run during frame limiter waits; `NotifyPendingWork` wakes the emulation thread. A minimal addition should drain a **bounded worker command queue** here.

Do not let a socket callback call `system.Memory()` or `system.GPU()` concurrently with run loop. Instead:

```cpp
// Pseudocode only; naming and signatures do NOT exist upstream.
void WorkerControl::Receive(Request req) {
    if (!validate(req) || queue.full()) return reply_error(req);
    queue.enqueue(req);
    system.NotifyPendingWork();
}

void WorkerControl::PumpAtSafePoint() {
    for (unsigned budget=0; budget < kMaxPerSlice && queue.try_pop(req); ++budget) {
        if (req.epoch != epoch) { complete(req, STALE_EPOCH); continue; }
        if (!allowed_in_state(req)) { complete(req, NOT_READY); continue; }
        execute_staged_guest_operation(req); // emulation thread only
    }
}
```

If a command pauses and waits for a host GPU synchronization, split operation into a **safe-point stage** and **asynchronous completion stage**; do not hold locks that block renderer progress. `ProcessPendingWork` is not a blanket guarantee of every renderer thread being idle.

**Acceptance:** concurrent requests while running, during frame limiter waits and under repeated reset; state-only reads consistent; no deadlocks; bounded queue saturation returns typed error.

### INT-04: controlled execution and stable stop reasons

Inspected: `Core::System::SingleStep()` delegates to `RunLoop(false)`; `RunLoop` loops over emulated ARM11 cores and makes scheduling decisions. Distinguish `pause + run_one_emulator_slice` from **exact selected-core instruction step**. In the minimal integration branch, it may be enough to support `run`, `pause`, `resume`, bounded `run_until(frame_count)` and `step_runloop_iteration` (internal) without advertising `step_instruction` if not provable. Put precise per-thread stepping under `experimental`.

Return a stop reason with explicit enum: user pause, breakpoint, watchpoint, exception, frame boundary, guest tick limit, wall timeout, reset, worker fatal. If a command is canceled while running, record the last completed emulation boundary.

**Acceptance:** two guest threads and multiple virtual cores prove what advances on step. The API refuses semantics it cannot honor rather than pretending success.

### INT-05: frame capture and GPU backend proof

Inspected: `WindowSystemType::Headless` exists and Vulkan platform can omit surface extensions. That is **not** an image output path by itself. Use software renderer first, deliberately export the final LCD images into a normalized raw-pixel buffer. Find the renderer-specific point where completed frame data is available; record top/left/right/bottom identity and native dimensions. Some render paths may lazily update framebuffers only for presentation; test this.

For Vulkan, implement offscreen capture with explicit synchronization and staged readback; time the operation. Capture engine and UI presentation use separate buffers so a slow editor panel cannot block guest rendering. A raw framebuffer may need rotation or format conversion before GPUI; record whether conversion occurred.

**Acceptance:** render simple moving geometry, verify per-frame hashes change when expected, compare stock Azahar reference screenshot at identical guest state, test repeated/single-eye frames, verify no half-frame reads.

### INT-06: storage and settings isolation

Inspected: `Core::System::Load` interacts with `FileUtil::SetCurrentRomPath`, and global settings are used throughout the emulator. Introduce an explicit user-directory root **before** core initialization; audit every call site writing NAND/SD, shaders, cheats, movie recordings, custom textures, config, logs and dump paths. A separate OS process solves globals but **does not** isolate filesystem paths by itself.

**Acceptance:** file watcher records every write for two instances; nothing writes outside its assigned root (except explicitly allowed shared read-only files). Crash one worker mid-run; second produces unaffected saves.

### INT-07: strip conflicting servers

Inspected: `src/core/rpc/udp_server.cpp` binds `45987`. Do not use its default server for MCP/worker control. Disable existing RPC by worker build config; preserve optional per-worker GDB stub only with unique port and controlled access. Existing `rpc_server.cpp` illustrates memory/screenshot handling and synchronization caveats, but is not our public protocol.

**Acceptance:** launch 4 workers and confirm no socket collision and no unintended LAN UDP listener.

## `experimental` patch series: accuracy and debugger upgrades

### EXP-01: structured native event sink

Introduce a small optional event recording interface at critical boundaries (`CPU`, `SVC/IPC`, `GPU`, `DSP`, `Timing`, memory/cache). Default no-op in integrated baseline; implement bounded ring in experimental, with compile-time/runtime filters, sampled vs lossless modes and drop counts.

**Do not** put a heavy polymorphic callback on every register write without profiling. Batch lightweight records, publish after instruction slice or host GPU submission as practical. Each event includes an identity and clock domain.

### EXP-02: PICA command/capture hooks

Inspected: `GPU::Execute` handles GSP commands, `PicaCore::ProcessCmdList` decodes register writes, trigger registers execute draws; `RasterizerInterface` has outputs and renderer-specific backends. Hook at three semantic levels:

```text
GSP command submit (source process and buffer address)
  -> PICA command parser: raw bytes and decoded register deltas
     -> draw trigger: freeze/identify state and resource versions
        -> rasterizer: host render resource results/cache interactions
           -> guest GPU completion IRQ and VBlank/display transfer
```

**Acceptance:** command captures include unknown raw writes, state at draw time and resource fingerprints at or before consumption. Add negative test where game overwrites VRAM after draw; captured earlier version must not change.

### EXP-03: HLE kernel and CPU trace

Capture SVC dispatch, service IPC, context switch/wait/wake and exception events from existing HLE boundaries. Add real per-core/per-thread exact stepping only when validated. Use sampling for CPU hot paths; instruction-trace mode only for very short bounded windows.

### EXP-04: timing observability

`CoreTiming` uses virtual events; `GPU` has throughput-based delay estimates. First **expose the assumptions** in traces and control panels. Store old/new model results alongside actual tests without changing guest scheduling. A model experiment behind a flag should have an unmodified Azahar reference mode and produce exact comparable runs.

### EXP-05: independent implementation seam

Once GPU fixture corpus works, allow a second reference rasterizer/shader engine for a *subset* of PICA commands, driven by captured exact input state. Validate functionality before inserting it into the realtime emulation pipeline. With proper conformance tests, evolve from offline differential component to selectable runtime engine.

## Source points requiring a fresh audit at pinned SHA

1. `Core::System` initialization/destruction and applet registrations: ensure startup-order assumptions.
2. Exact software frame export pathway: find who owns the final native screen surfaces and frame mailbox.
3. `RasterizerInterface` and GPU resource cache: find coherent way to freeze resource state without altering guest execution.
4. CPU breakpoints/JIT cache invalidation: avoid stale code execution after debugger patches.
5. CMake dependency graph with Qt OFF and Libretro OFF: renderer, audio and backend selection may have transitive frontend assumptions.
6. Audio frontend API and frame limiter: ensure a muted worker still keeps emulated DSP timing and doesn't sleep on missing sound device.
7. Savestate pending operations: explicit failure rather than forcing serialization mid-async task.
8. Global file path helpers, settings, GDB and RPC ports: process isolation is incomplete without per-root config.

## Conflict and removal policy

Every nontrivial change gets a small comment explaining the worker need and a `patch-inventory` entry. If upstream adds a compatible headless feature, **remove** our duplicate integration patch rather than preserve it forever. Experimental patches should attach near stable semantic events; when upstream rewrites a subsystem, revalidate event interpretation even when compilation succeeds.

## Demonstration tasks for integration branch acceptance

1. Worker executes a known synthetic 3DS program to a moving triangle, sends top/bottom raw frames, and accepts touch/button input.
2. Worker accepts `pause` while frame-limited, returns coherent CPU snapshot, resumes and stops cleanly.
3. Four workers run isolated 3DSX copies with unique settings/log/SD directories, without using Libretro or opening Qt.
4. Killing one worker leaves other workers and GPUI alive, with partial artifact marked as failed.
5. Stock Azahar at same SHA and integrated branch produce expected matching state/image checks on test fixtures (acknowledging host renderer timing differences).
