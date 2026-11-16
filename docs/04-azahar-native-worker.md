# 04 — Building the native Azahar worker (Option B)

**Scope:** the actual native Azahar core. Explicitly **not** `src/citra_libretro`. The goal is a small, maintainable C++ executable with a portable IPC boundary, not an FFI binding of every C++ class into Rust.

## Source facts verified against current Azahar master (2026-11-15)

- `src/core/core.cpp` declares `Core::System System::s_instance` and contains `System::RunLoop`, `Load`, `SingleStep`, `ProcessPendingWork`, `ProcessPendingWorkUntil` and `NotifyPendingWork`.
- `src/core/core.h` exposes `Load(Frontend::EmuWindow&, filepath, secondary_window)`, `RunLoop(bool tight_loop=true)`, `SingleStep`, `RequestShutdown`, `Memory`, `Kernel`, `GPU`, `CoreTiming`.
- `src/core/frontend/emu_window.h`: `WindowSystemType::Headless`; virtual `PollEvents`; `CreateSharedContext`; window info with `render_surface=nullptr`; touch methods; framebuffer layout and context lifecycle.
- `src/video_core/renderer_vulkan/vk_platform.cpp` recognizes headless window type. **Does not guarantee end-to-end GPU screenshots or offscreen output**; test it.
- `src/core/core_timing.cpp` schedules events and virtual CPU timers, with configurable tick base and CPU clock percentage.
- `src/video_core/gpu.cpp`: `GPU::Execute` receives GSP requests, `SubmitCmdList` triggers the PICA command processor, VBlank events are registered, DMA/display transfers are handled.
- `src/video_core/pica/pica_core.cpp`: `PicaCore` owns internal registers, shader engine, geometry pipeline, trigger-draw handling.
- `src/core/rpc/rpc_server.cpp`: existing RPC includes memory access and screenshots; `src/core/rpc/udp_server.cpp` uses a fixed UDP port `45987` (unsuitable as-is for independent workers).
- `src/CMakeLists.txt` adds core, video, audio, network and input libraries, then Qt/Libretro by options. `src/core/CMakeLists.txt` builds `citra_core`.

These are integration leads, not claims that an `EmuWindow` subclass can be implemented without further missing interfaces.

## Native worker responsibilities

1. Parse per-worker config from supervisor: root dir, file path, selected graphics backend, initial clock/3DS model, debug mode, IPC endpoint, requested test determinism, audio on/off.
2. Create native file/path environment and register frontend applets, microphone/image callbacks, video and input bridges.
3. Initialize `Core::System::GetInstance()`; `Load` selected executable; report structured error/status.
4. Own the *single* emulation thread. Execute `RunLoop(true)` for running mode; `SingleStep()` only when correct CPU stepping semantics are confirmed.
5. Drain only explicitly allowed control requests at `ProcessPendingWork` safe points; allow quiescent pause commands to be applied even when frame limiting is sleeping.
6. Route screen/audio from backend to bounded shared-memory/output queues; never block the emulation thread indefinitely waiting on a slow GPUI client.
7. On exit, flush/close storage, complete or reject pending control messages, stop video/audio threads, destroy renderer, then unload and report final state.

## First implementation spike: choose software renderer

A software renderer avoids the GPU surface and Vulkan readback uncertainties during earliest UI bring-up. Disable Qt; compile worker target with video software backend and required dependencies. Once images+input+audio work, add offscreen Vulkan.

**Build configuration sketch (must be validated on chosen source SHA):**

```bash
cmake -S vendor/azahar -B build/azahar-worker \
  -DENABLE_QT=OFF \
  -DENABLE_LIBRETRO=OFF \
  -DENABLE_CTR_WORKER=ON \
  -DENABLE_SOFTWARE_RENDERER=ON \
  -DENABLE_VULKAN=ON \
  -DENABLE_SCRIPTING=OFF \
  -DENABLE_WEB_SERVICE=OFF \
  -DENABLE_ROOM=OFF \
  -DENABLE_ROOM_STANDALONE=OFF
cmake --build build/azahar-worker --target ctr-worker --config RelWithDebInfo
```

`ENABLE_CTR_WORKER` and `ctr-worker` are **new proposed names** and do not exist upstream yet. `RelWithDebInfo` handling and toolchain configuration depend on the chosen CMake generator. Some existing conditionals are tied to `ENABLE_QT`; inventory and remove accidental dependencies before declaring `Qt OFF` functional. Keep stock Azahar build independently testable.

## CMake changes (integration branch)

Minimal, additive changes:

- Root `CMakeLists.txt`: `option(ENABLE_CTR_WORKER "Build external emulation worker" OFF)`, set any mutual exclusions deliberately; don't repurpose Libretro flags.
- `src/CMakeLists.txt`: `if(ENABLE_CTR_WORKER) add_subdirectory(ctr_worker) endif()` near other frontend targets.
- `src/ctr_worker/CMakeLists.txt`: add executable with `worker_main.cpp`, `worker_frontend.cpp`, `worker_control.cpp`, `worker_capture.cpp`, `worker_input.cpp`, `worker_audio.cpp`, `worker_storage.cpp`. Link existing Azahar `citra_core`, `video_core`, `audio_core`, other dependencies verified by target graph. Do **not** duplicate core source compilation.
- Explicitly choose a toolchain configuration and avoid linking existing Qt application objects. Keep native worker code in a small directory, with platform-specific socket/shared-memory code isolated.

A hard requirement is that `ENABLE_CTR_WORKER=OFF` leaves upstream build behavior unchanged.

## Frontend construction

Create `WorkerEmuWindow final : public Frontend::EmuWindow` with:
- `WindowSystemInfo` set to `Headless` and null surface initially.
- `PollEvents` forwards input/config messages gathered by the worker (no native GUI event loop).
- `GetFramebufferLayout` uses an explicit top/bottom layout; touch coordinates are mapped from GPUI to bottom-screen pixel coordinates and forwarded to `TouchPressed/Moved/Released`.
- `CreateSharedContext` initially returns null where supported; if renderer requires one, implement a renderer-specific offscreen context, not a fake pointer.
- Configure min/layout sizes before `Core::System::Load` and keep their lifetime valid through shutdown.
- Implement or register the frontend software keyboard, Mii selector and image/audio input services where games request them; don't silently treat all applets as successful. For headless tests, return a clear `NEEDS_UI_INTERACTION` event with timeout/fallback policy.

Investigate the actual existing `citra_qt` and SDL-like frontend implementation for callbacks and ownership; `EmuWindow` alone doesn't cover every service.

## Initial running loop and command queue

**Design pseudocode (not drop-in Azahar code):**

```cpp
while (!shutdown_requested) {
    drain_nonblocking_control_messages();  // queues only
    switch (state) {
      case Running:
        result = system.RunLoop(true);
        break;
      case Paused:
        system.ProcessPendingWork();  // including debugger commands
        wait_on_control_condition_with_timeout();
        continue;
      case StepPending:
        result = run_step_for_selected_core_or_stop_with_error();
        state = Paused;
        break;
    }
    publish_run_result_if_needed(result);
    if (fatal(result)) break;
}
```

`SingleStep()` currently executes `RunLoop(false)`, which may interact with **all** CPU cores and scheduling. Do not label that 'step selected ARM11 core by exactly one instruction' until verified; add per-thread/per-core step semantics on experimental branch with tests. Never enter `RunLoop` recursively from a debugger callback. Add a safe-point queue to `ProcessPendingWork`, and use `NotifyPendingWork()` to wake the core on requests. Make pending requests cancellable on shutdown.

## Read/write semantics

A memory read must specify `process_id`, `address_space`, `guest_address`, `length`, `consistency`. Validate range and permissions, avoid blindly copying virtual pointers into host buffers; use Azahar's `MemorySystem` facilities and return a typed fault, unmapped range or permission failure. GPU-backed pages may require a synchronized rasterizer flush to reflect what the CPU would observe; a pure inspector read should not secretly cause guest-visible writes or corrupt renderer caches.

A write must also identify the process/space and note side effects: JIT invalidation if executable code changes, rasterizer cache invalidation for altered graphics resources, debugger-visible mutation provenance and a changed state epoch. Disable writes during uncommitted replay checkpoints.

## Frame emission

- Software path: access the **final top/bottom screen images** after relevant display/layout transforms. Prefer raw native screen buffers over composited window captures; record geometry, pixel format, orientation, eye selection, crop and original guest framebuffer address.
- Vulkan path: use an offscreen target supported by selected backend; wait for submission completion before readback or handoff; never assume `Headless` automatically presents usable images.
- Ring buffer: at least 3 slots, slot generation/sequence, writer marks `WRITING` then `READY` with release/acquire synchronization; consumer owns a lease or copies before the writer reuses it.
- Policy: UI may skip intermediate frames; capture/test mode must either reliably collect requested frames or return explicit dropped-frame counts. Do not silently switch the emulated game into a different clock speed to satisfy the UI.

## Audio and input

- Input: make worker input atomic per emulated sampling boundary; external script has exclusive ownership, no simultaneous live controller events. Map native buttons and touch explicitly; normal gameplay can accept host timestamps but replay uses guest event boundaries.
- Audio: start with existing Azahar audio stream to a per-worker ring; UI/supervisor mixes host playback streams or selects active one. Headless workers can discard playback but preserve emulated DSP execution and track underruns.
- Microphone/camera/system keyboards: provide deterministic fixtures for tests; interactive mode can request user values. Do not pretend missing sensors exist.

## Storage and settings

Azahar uses settings and utility globals, including `Settings::values` and file-path helpers. Before launching a worker, set its user directory **before initialization**, and explicitly override any application-global data/cache paths. Identify every file-writing path (logs, saves, shader cache, texture packs, archive caches, firmware, cheats, screenshots, temporary files). Reject two workers sharing writable roots by default.

## Existing RPC and GDB

Existing Azahar RPC is an implementation reference, not a server to expose unchanged. It binds UDP `45987`, so multiple workers would collide and it may allow unsafe access without consistent synchronization. Initial worker build should disable it and use our authenticated local IPC. Retain GDB functionality through a per-worker configurable endpoint if needed; account for its core safe-point interactions. A direct internal debugger interface is better than layering MCP atop UDP packets.

## Technical risks and explicit tests

| Risk | Test / mitigation |
|---|---|
| Qt-only code hidden in core path | Build `ENABLE_QT=OFF`, link native worker, launch homebrew |
| `Headless` lacks Vulkan output capture | Software first, offscreen Vulkan spike, screenshots compared |
| `ProcessPendingWork` reentrancy/races | Concurrent control storm, pause/resume while sleeping, TSAN where possible |
| `SingleStep` semantics differ from request | Assert per-core PC/instruction behavior with two threads |
| IPC call blocks emulation indefinitely | Deadlines and bounded queues, cancellation tests |
| Audio or applet stalls headless | Synthetic fallback with explicit events; no silent green result |
| Shader cache/shared paths corrupt | Two workers start/stop with separate roots and identical ROM |
| Resource lifetime | Open/close/reload loop, multiple sessions, forced worker kill |
| Performance effect of debug hooks | A/B uninstrumented vs inactive-hook vs active-trace runs |

## Invariants for merge into integration

No new Rust UI dependency, no new default network listener, no changed GPU timing, no permanent trace allocations on the critical path, no change to stock Azahar frontend when worker option is OFF. All changes must have an explicit upstream behavior comparison.
