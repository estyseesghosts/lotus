# 29 — Wave 3 plan: comprehensive agentic debugger

Status: **implementation in progress**, 2026-11-16. Development evidence and
remaining acceptance work are in [31](31-wave-3-progress.md). Tickets and their Deliver/Check/Avoid terms
are in [15](15-implementation-tickets.md); this document holds the goal,
decisions, Azahar parity matrix and milestone order. No item here is
complete; source facts below are **source inspected** at the native pin in
`config/pins.toml`.

## Goal

LOTUS + CITnLO must meet or exceed every debugging, configuration and
inspection capability of Azahar, exposed uniformly through the worker
protocol, CLI, MCP and desktop, so that an agent can find and explain guest
bugs without the Qt frontend. "Exceed" means capabilities Azahar lacks:
rewind, session forking, predicate stops, symbol-aware debugging, matrix
runs, and a single arbitrated control model across GDB, MCP and desktop.

## Decisions (owner, 2026-11-15)

| ADR | Decision | Consequence |
| --- | --- | --- |
| ADR-008 | **GDB server is written in Rust** (`crates/gdb`) on top of the Lotus debug API; Azahar's stub is not used | CITnLO workers build with `ENABLE_GDBSTUB=OFF`; HIO is rerouted to the worker debug channel. Multi-session, non-stop and reverse execution become possible |
| ADR-009 | **Offscreen Vulkan worker from the start of Wave 3**; software renderer stays as default-safe fallback and oracle | New headless presentation path; `graphics_api`, `resolution_factor`, `physical_device` and hardware-renderer GPU debugging become real |
| ADR-006 (unchanged) | **Old 3DS is the default profile**; New 3DS is a fully supported, explicitly labeled profile | Every test that touches CPU, scheduling or memory layout runs on both profiles |
| ADR-010 | **Settings schema and transport live in `integration`**; debugger and accuracy toggles that change behavior are exposed but not altered there | Both AZAHAR and CITnLO accept profiles; default profile reproduces today's behavior byte for byte (proven by T24 check) |
| ADR-011 | **Audio is in Wave 3**: audible output, capture, HLE/LLE DSP and mic injection | Supersedes the muted-only profile in [25](25-wave-status.md) |

ADR-010 rationale: a settings channel is a frontend adapter (allowed in
`integration` by [03](03-azahar-branches.md)); keeping it there means the
AZAHAR backend can serve as a control in matrix runs (T41) with identical
profiles. Any setting whose *default* would differ from stock still needs its
own ADR.

## Source facts that constrain the design

| Fact | Source | Effect |
| --- | --- | --- |
| Azahar GDB stub binds `INADDR_ANY` | `src/core/gdbstub/gdbstub.cpp:2125` | Violates local-only rule; another reason for ADR-008. Build-flag test must prove the stub is absent |
| HIO request enters via `svcBreak`-family path into `GDBStub::SetHioRequest` | `src/core/hle/kernel/svc.cpp:1178`, `gdbstub/hio.h` (152-byte `PackedGdbHioRequest`, libctru layout) | CITnLO patch forwards the request address to the debug channel; Rust server implements the `F` reply |
| Worker forces software renderer, Old 3DS, stub off | `src/ctr_worker/frontend.h:61-64` | Defaults move into the T24 default profile instead of hard-coded writes |
| `is_new_3ds` read at boot (memory mode, cores) | `src/core/core.cpp:401,461` | Apply class `boot` |
| `cpu_clock_percentage` applied live | `System::ApplySettings`, `core.cpp:876` | Apply class `live` |
| Vulkan instance accepts `WindowSystemType::Headless`, but `PresentWindow` always creates a surface | `vk_instance.cpp:136`, `vk_present_window.cpp:104` | T45 adds an offscreen presenter with readback into the frame ring |
| LLE DSP selected from `audio_emulation` at system init | `core.cpp:661-662` | Apply class `boot`; firmware is a user asset in the session root, never bundled |
| CFG values (username, birthday, language, country, sound mode, play coins) live in CFG save data, not `Settings::values` | `configure_system.ui`, `hle/service/cfg` | Apply class `cfg-savedata`; written through the CFG module before boot |

Every other setting's apply class is determined in T24 by reading where it is
consumed; none is assumed.

## Parity matrix

Azahar column names its frontend action, setting or interface. Each row must
be reachable from worker protocol, CLI and MCP (T44 enforces this);
desktop coverage follows in T17.

### Execution and CPU

| Azahar | LOTUS | Ticket |
| --- | --- | --- |
| Pause / Resume / Stop / Restart | Existing, plus typed stop reasons and debug lease | T25 |
| Debug Step, Advance Frame | Instruction step, step over/out, run-to, frame advance; per-core semantics stated | T14c |
| GDB `Z0/Z1` breakpoints | Execute breakpoints with conditions, hit counts, logpoints | T14a |
| GDB `Z2/Z3/Z4` watchpoints | Read/write/access watchpoints with cost estimate and cap | T14b |
| GDB `g/G/p/P/m/M`, RPC Read/WriteMemory | Register and memory read/write, JIT invalidation | T14d |
| Registers widget | Snapshot including CPSR/SPSR, VFP, mode, thread | T14d |
| (none) disassembly | ARM/Thumb disassembly with raw bytes and symbols | T14d, T14h |
| GDB stub (whole) incl. HIO, `qXfer:threads`, `osdata`, `vCont`, attach next process | Rust RSP server, loopback only, per-session port | T26 |
| `use_cpu_jit`, `use_fastinterp` | Profile settings (JIT / fast interpreter / interpreter) | T24 |
| `cpu_clock_percentage` | Live setting, labeled as virtual-clock scaling | T24 |
| `is_new_3ds` | Boot profile; Old 3DS default | T24 |
| `enable_exception_handler` | Crash/exception reports with backtrace | T14g |

### Kernel, services and system

| Azahar | LOTUS | Ticket |
| --- | --- | --- |
| Wait tree | Processes, threads, wait objects, handle tables, memory map, deadlock graph | T14e |
| Debug Unschedule All / Schedule All | Per-thread suspend/resume plus all-threads | T14e |
| IPC recorder | SVC and IPC trace with schema-decoded commands, raw fallback | T14f |
| LLE service modules dialog, `lle_modules`, `lle_applets`, `enable_required_online_lle_modules`, `delay_start_for_lle_modules` | Boot profile settings | T24 |
| System config: region, username, birthday, language, sound, country, init clock/time/offset, init ticks type/override, play coins, steps/hour, battery source/level/charging, console ID, MAC, system setup | Profile (`boot` / `cfg-savedata`) settings, regenerate actions | T24, T31 |
| `deterministic_async_operations`, `async_fs_operations` | Determinism profile | T29 |
| `toggle_unique_data_console_type`, real console unique data (OTP, SecureInfo, LFCS, movable) | Session-scoped user assets, never bundled | T31 |
| Plugin loader, region-free patch | Profile settings | T24, T31 |
| Install CIA, Setup System Files, Boot HOME Menu, NAND/SDMC folders | Content tools confined to session roots | T31 |
| Cheats (Gateway) | List/add/enable cheats | T31 |
| Load/Remove Amiibo | NFC injection | T30 |
| Compress/Decompress ROM, ROM bundles | Content tools | T31 |
| Log filter, regex filter, instant log, console | Per-session structured log stream, `svcOutputDebugString`, HIO stdout | T27 |
| Profiler (microprofile), `record_frame_times`, RPC GetPerfStats | Perf stats, frame times, scopes; host and guest time separate | T32 |

### State, replay and input

| Azahar | LOTUS | Ticket |
| --- | --- | --- |
| Quick Save/Load, slots, RPC Save/LoadState | Named savestates keyed by pin + profile hash | T28 |
| Record/Play/Close Movie, read-only mode | Movies integrated with scenarios; determinism check | T29 |
| RPC SetInput (Home, Power, touch, circle pad) | All buttons incl. Home/Power, C-stick/ZL/ZR, circle pad, 3D and volume sliders, gyro/accelerometer, IR | T30 |
| Camera config, mic input, `simulate_headphones_plugged` | Image/WAV injection, headphone state | T30, T46 |
| (none) | Rewind, reverse step/continue (GDB `bs`/`bc`) | T38 |
| (none) | Fork new sessions from a savestate | T39 |

### GPU and video

| Azahar | LOTUS | Ticket |
| --- | --- | --- |
| `graphics_api`, `physical_device`, `use_gles`, `resolution_factor`, texture filter/sampling, anti-aliasing | Profile settings; real on offscreen Vulkan | T45, T24 |
| `renderer_debug` (validation layers) | Vulkan validation output into the log stream | T45, T27 |
| PICA breakpoints (7 events in `debug_utils.h`) | Stop on each event, with draw/command identity | T33 |
| Command list viewer, `pica_debugging`, `dump_command_buffers` | Command stream capture and viewer | T34, T15 |
| Vertex shader debugger | Per-vertex inputs/outputs and instruction trace | T34 |
| Surface viewer, `dump_textures`, custom/preload textures | Surface viewer for any address/format; texture dump | T35, T16 |
| Graphics tracing (CiTrace) | Record and replay | T36 |
| `simulate_3ds_gpu_timings`, `delay_game_render_thread_us`, `shaders_accurate_mul`, `use_shader_jit`, `use_hw_shader`, shader caches, async shader compilation/presentation, skip duplicate frames, frames-ahead limit | Profile settings labeled with time domain; no accuracy claim | T37, T24 |
| Capture Screenshot, RPC Take/ReadScreenshot | Native capture (exists) plus upscaled capture | T45 |
| Dump Video (FFmpeg) | Frame + audio dump | T46 |
| Layouts, stereo 3D, post-processing/anaglyph shaders, background colour, cardboard | Reachable through generic settings; display-only, no tooling | T24 |

### Audio

| Azahar | LOTUS | Ticket |
| --- | --- | --- |
| `audio_emulation` HLE / LLE / LLE multithreaded | Boot profile; LLE requires user-supplied `dspfirm.cdc` in the session root | T46 |
| `output_type`, `output_device`, `volume`, mute | Audible output for the focused session only; others muted by default | T46 |
| `enable_audio_stretching`, `enable_realtime_audio`, `bypass_audio_effects` | Profile settings | T46 |
| `input_type`, `input_device` | Mic from host device or injected WAV | T46 |
| (none) | WAV capture of DSP output; HLE DSP voice/source state inspector | T46, T47 |

### Out of scope

Network rooms/multiplayer, Artic Base, web service and telemetry, Discord:
non-goals in [01](01-product-contract.md). Their settings remain reachable
through T24 with no tooling. Qt-only UI preferences (themes, hotkeys, game
list) have no equivalent.

## Milestones

Order is dependency order; each milestone ends with a recorded status in
[25](25-wave-status.md).

| Milestone | Tickets | Exit check |
| --- | --- | --- |
| **3.0 Foundations** | T24 settings/profiles, T25 stop model/event stream/debug lease, T45 offscreen Vulkan, T46 audio, T26 RSP skeleton against the mock worker | Default profile reproduces Wave 2 hashes; Old and New 3DS fixture boots on software and Vulkan; audible + captured audio from a fixture; GDB client connects to mock and reads registers |
| **3.1 CPU, kernel, system** | T14a–h, T26 full, T27–T32 | devkitARM `arm-none-eabi-gdb` session (break, bt, watch, HIO printf) passes on both profiles; deterministic three-run hashes with movie + determinism profile |
| **3.2 GPU** | T33–T37, T15, T16 | PICA event stops, command capture and shader trace match on software and Vulkan for the GPU fixture |
| **3.3 Beyond Azahar** | T38–T43, T47, T18 | Agent test suite (T43) solved through MCP alone |
| **3.4 Surfaces** | T17, T44 | CI fails if any protocol method lacks CLI/MCP surface; parity matrix generated |

## Cross-cutting rules for Wave 3

- Every report records backend, pin, profile hash and renderer.
- Every debug result states whether execution was paused and whether a
  rasterizer cache flush was forced ([07](07-debugger-core.md)).
- Time domains (host ms, guest ticks, inferred cycles) are never mixed.
- New 3DS results are labeled; Old 3DS is the default in every example.
- Instrumentation is bounded, has loss counters and an on/off overhead A/B.
- User assets (DSP firmware, console unique data, system files) stay in the
  session root and are never bundled or uploaded.
