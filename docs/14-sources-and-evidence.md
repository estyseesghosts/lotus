# 14 — Sources, code map and evidence status

**Inspection date:** 2026-11-15. Sources below are directly inspected default-branch GitHub files or externally documented references. These are *not* immutable snapshots. Before implementing, pin a full Azahar/GPUI/Coffeepot commit SHA and replace unpinned URLs in implementation-facing evidence with commit-specific links. No worker code has been compiled as part of this dossier.

## Azahar — primary implementation

| Source | Verified fact / what to inspect | Status |
|---|---|---|
| [root CMake](https://github.com/azahar-emu/azahar/blob/master/CMakeLists.txt) | `ENABLE_QT`, `ENABLE_LIBRETRO`, `ENABLE_SOFTWARE_RENDERER`, `ENABLE_VULKAN`, `ENABLE_SCRIPTING`, `ENABLE_SCRIPTING_SYNC`, `ENABLE_GDBSTUB` and dependency options | Source inspected |
| [src/CMakeLists](https://github.com/azahar-emu/azahar/blob/master/src/CMakeLists.txt) | core/video/audio/Qt/Libretro subdirs, Qt-tied CLI conditionals; add separate worker target | Source inspected |
| [core CMake](https://github.com/azahar-emu/azahar/blob/master/src/core/CMakeLists.txt) | `citra_core` static target and substantial frontend/HLE code | Source inspected |
| [core.cpp](https://github.com/azahar-emu/azahar/blob/master/src/core/core.cpp) | `System::s_instance`, `RunLoop`, `Load`, `SingleStep`, `ProcessPendingWork`, `NotifyPendingWork`, savestate async checks | Source inspected |
| [core.h](https://github.com/azahar-emu/azahar/blob/master/src/core/core.h) | `Load`, `RunLoop`, `SingleStep`, `GetCore`, `Kernel`, `Memory`, `GPU`, `CoreTiming` public methods | Source inspected |
| [EmuWindow](https://github.com/azahar-emu/azahar/blob/master/src/core/frontend/emu_window.h) | headless window type, null surface, frontend callbacks and touch mapping entrypoints | Source inspected |
| [GPU](https://github.com/azahar-emu/azahar/blob/master/src/video_core/gpu.cpp) | `GPU::Execute`, GSP command handling, DMA delay tables, PICA submission, VBlank event | Source inspected |
| [PicaCore](https://github.com/azahar-emu/azahar/blob/master/src/video_core/pica/pica_core.cpp) | register bank, shader engine, command flags, trigger-draw logic | Source inspected |
| [video core build](https://github.com/azahar-emu/azahar/blob/master/src/video_core/CMakeLists.txt) | native software/GL/Vulkan render paths, shader interpreter/JIT, rasterizer cache | Source inspected |
| [core_timing.cpp](https://github.com/azahar-emu/azahar/blob/master/src/core/core_timing.cpp) | event scheduler, tick seeds, clock scale, thread-safe scheduling delay | Source inspected |
| [RPC server](https://github.com/azahar-emu/azahar/blob/master/src/core/rpc/rpc_server.cpp) | read/write/screenshot code, sync vs async option and process selection | Source inspected |
| [UDP RPC](https://github.com/azahar-emu/azahar/blob/master/src/core/rpc/udp_server.cpp) | UDP socket binds port **45987**, unsuitable unmodified for parallel workers | Source inspected |
| [Vulkan platform](https://github.com/azahar-emu/azahar/blob/master/src/video_core/renderer_vulkan/vk_platform.cpp) | `WindowSystemType::Headless` recognized; not proof of usable screen readback | Source inspected |
| [license](https://github.com/azahar-emu/azahar/blob/master/LICENSE.txt) | top-level GPLv3 text; per-file notice varies | Source inspected |
| [Azahar releases](https://github.com/azahar-emu/azahar/releases) | packaging/toolchains and behavior are changing | Public changelog |

**Important code interpretation:** Azahar native `Core::System` is sufficient to bootstrap a worker, but the proposed `ctr-worker` target, Rust control socket, shared-memory rings and fully synchronized GPU capture do **not** yet exist upstream. Every mention is a design proposal.

## GPUI / Rust

- [GPUI README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md): active pre-1.0, `gpui_platform::application()`, `App::open_window`, native platform requirements, entities/views/elements. **Source inspected**.
- [GPUI image source](https://github.com/zed-industries/zed/blob/main/crates/gpui/src/elements/img.rs): `ImageSource::{Render,Image,Custom}` entries and `RenderImage` references; exact construction API must be revalidated. **Source inspected**.
- [GPUI repository](https://github.com/zed-industries/zed/tree/main/crates/gpui): source and examples; GPUI versioning changes. **Source anchor**.
- [Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk): tool/resource semantics; pin SDK and verify release API before implementing. **Source anchor**, not inspected line by line in this dossier.

## Citra, Mikage and independent context

- [Azahar project overview](https://github.com/azahar-emu/azahar): actively maintained 3DS emulator derived from Citra. **Public project overview**.
- [Citra documentation archive](https://citra.azahar-emu.org/wiki/home/): historical emulator engineering material. **Reference**, not a code contract.
- [Mikage Developer Edition](https://github.com/mikage-emu/mikage-dev): independent design/reference for CPU, HLE and Vulkan renderer work. **Reference**; do not assume it provides complete hardware timing accuracy.
- [Original Citra historical fork](https://github.com/neobrain/citra): historical starting point, not maintained source. **Reference**.

## Coffeepot — local hardware research and engineering handbook

- [CTR index](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/index.md): 3DS hardware, software, benchmark and development references. **Source inspected**.
- [CTR GPU](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/hardware/gpu.md): shader pipeline, register map, Primitive Engine uncertainty, VRAM/display handling. **Source inspected**.
- [CTR debugging](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/debugging.md): GDB/Rosalina, Azahar-script lessons, bounded instrumentation, crash and GPU-hang debugging. **Source inspected**.
- [CTR hardware tests](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/testing-on-hardware.md): testing tiers, benchmarks, scripts and measurement caveats. **Source inspected**.
- [CTR porting](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/porting.md): how to validate native software under Old 3DS constraints. **Source inspected**.
- [Source versions/verification](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/source-versions.md): pinned toolchain revisions and which examples actually ran on real console vs merely compiled. **Source inspected**.
- [CTR benchmarks](https://github.com/estyseesghosts/coffeepot/tree/main/ctr/benchmarks): expand/verify exact harness source at pinned commit before porting tests. **Source anchor**.

## 3dbrew and hardware

- [Memory layout](https://www.3dbrew.org/wiki/Memory_layout): physical memory layout, ARM11/ARM9/VRAM/FCRAM. **Public documentation**.
- [GPU external registers](https://www.3dbrew.org/wiki/GPU/External_Registers): GX/transfer/display control and profiling. **Public reverse-engineering reference**.
- [GPU internal registers](https://www.3dbrew.org/wiki/GPU/Internal_Registers): PICA register meanings. **Public reverse-engineering reference**.
- [GPU Primitive Engine](https://www.3dbrew.org/wiki/GPU/Primitive_Engine): explicitly states full function/enable mechanics remain unknown. **Public unknown**.
- [FIRM](https://www.3dbrew.org/wiki/FIRM): ARM9/ARM11 firmware kernel sections and system components. **Public documentation**.
- [ARM11 MPCore TRM](https://developer.arm.com/documentation/ddi0360/latest): CPU microarchitecture, caches and MMU, subject to 3DS configuration. **Vendor reference**.

## Research confidence conventions

- **Source-inspected:** exact existing code/public docs were read; correctness of our future integration is not yet established.
- **Known behavior:** verified by tests on pinned current emulator or 3DS hardware; attach raw proof and revision.
- **Proposed:** new architecture/API or example syntax not present upstream; compile/test required.
- **Inferred:** supported but not directly measured; list alternative explanation.
- **Unknown:** insufficient evidence; require a concrete discriminating experiment.

If a source is corrected or later contradicts older Coffeepot text, preserve both references and revise this dossier with the specific resolved conflict, rather than silently copying the more convenient interpretation.
