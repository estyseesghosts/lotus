# 08 — PICA200 debugger, draw capture and structured tracing

## Goal

Make GPU behavior explainable at command/draw/texture granularity. The debugger must answer: **which work was submitted, when was it accepted, which exact state and memory did it consume, which output did it produce, and what synchronization was involved?** This is essential for native graphics ports and for future timing research. It is more useful than merely exposing a register panel.

## Source-level insertion points in Azahar

| Source | Hook intention | Branch |
|---|---|---|
| `src/video_core/gpu.cpp`, `GPU::Execute` | GSP command type, source/target, submit ID and guest tick; DMA, fill, display transfer, completion events | `experimental` unless tiny optional adapter needed |
| `src/video_core/gpu.cpp`, `SubmitCmdList`, `VBlankCallback` | command-buffer lifetime and frame boundary; events associated with GPU completion | `experimental` |
| `src/video_core/pica/pica_core.cpp`, `ProcessCmdList` | decode command stream and register effects; special draw trigger | `experimental` |
| `src/video_core/pica/pica_core.cpp`, trigger draw handlers | snapshot state **at the draw**, not frame end | `experimental` |
| `src/video_core/pica/regs_*.h` | typed register decode schema and display labels | UI/tooling with limited C++ export |
| `src/video_core/shader/` | shader bytecode, swizzle, uniforms, CPU JIT/interpreter state | `experimental` |
| `src/video_core/renderer_software/` | reference output, stages, pixel-level debugging | `experimental` |
| `src/video_core/renderer_vulkan/` | host pipeline states, texture/cache lineage, readback timing | `experimental` |
| `src/video_core/rasterizer_cache/` | guest VRAM/FCRAM -> host surface mapping, dirty/flush/invalidate events | `experimental` |

Don't place huge allocation/copy operations in the critical draw path. For full capture, preallocate bounded buffers and/or freeze a capture workload with explicit barriers.

## Capture identity

Every event or draw has stable, non-reusable identity `(instance_id, epoch, frame_id, command_list_id, draw_id)`. Link to `guest_tick`, `worker_event_seq`, originating `GSP` command, GPU command address, and selected guest process where known. An event can be associated with **submission** vs **completion** vs **presentation**; those are separate moments.

### A draw snapshot contains

- Raw command-list bytes and parsed command sequence (preserve unknown writes and malformed values).
- Full relevant PICA register snapshot or delta against a known base state with base hash/version.
- Shader code and operands/swizzles; vertex attribute layouts, index buffer, primitive topology.
- Uniforms, lighting/LUT/fog/TEV/procedural texture and shader state as used at the draw.
- Texture descriptors, format, address, size and **captured contents or immutable content hash**.
- Framebuffer/depth/stencil descriptors, prior contents when required, and clear/transfer lineage.
- Rasterizer backend and its host pipeline/binding state, with caveat that host state is implementation data rather than physical PICA200 state.
- Output image/hash and optional pixel-diff reports.
- Any missing elements explicitly marked `unavailable`, `stale` or `not_captured`.

**State-at-draw rule:** bytes consumed by a draw can change later. Capture them at or before the draw boundary or make an immutable copy. A later VRAM read is not a historical reconstruction. For hash identity, choose input bytes and stable state only; do not include `VCOUNT`, wall clocks or other time-dependent fields in a purported image signature unless they truly affect draw output.

## Command trace architecture

The command list can be large and register writes frequent. Use levels:

- **L0 metrics:** aggregated number of submits/draws/transfers/interrupts and bytes; minimal continuous overhead.
- **L1 structural:** ring of command-list IDs, draw IDs, source/target addresses, shader keys and timestamps; bounded, dropped count explicit.
- **L2 snapshot:** for selected draw/frame, full relevant register state + referenced resource versions; synchronization permitted.
- **L3 microtrace:** vertex/shader instruction trace, clipping/interpolation/pixel tests, per-stage internal software renderer data. Only targeted operations.

Selecting `L3` must report that timing is **severely perturbed**; do not use its host costs as calibration data. Capture APIs require a filter and budget (specific frame, command ID, draw ID, guest tick range, event predicate), plus maximum bytes and stop condition.

## Resource lineage (the main differentiator)

Track emulated resource versions, not just host texture handles. For each VRAM/FCRAM range accessed by PICA:

```text
Guest range + generation
   ├─ CPU write event(s)
   ├─ cache flush/invalidate
   ├─ GPU command fetch
   ├─ texture decode or host upload
   ├─ draw references this version
   ├─ render target write / transfer
   └─ later CPU or display read
```

This is **observed emulation lineage**, not a claim to have reproduced the physical memory bus. Use event-indexed version IDs and compact interval tracking. Each record includes whether a read used guest RAM, a cached host surface, or synthesized texture, and whether a flush/readback was necessary. Heavy per-byte provenance is an opt-in diagnostic.

## GPU timeline / causal graph

Show separate lanes for:

- CPU/GSP request (`DMA`, `SubmitCmdList`, `MemoryFill`, `DisplayTransfer`).
- PICA command-processing start/end.
- Draw trigger and shader work.
- Rasterizer host task and resource uploads/downloads.
- Emulated GPU-completion/interrupt events.
- Display/VBlank and presented frame.

A timeline edge can mean **known callback/provenance**, **same command ID**, or **heuristic correlation**. Label the edge; do not depict inferred causality as verified causality. Host GPU completion and emulated guest GPU-completion interrupts are separate nodes.

## Visual analysis tools

- Register diff before/after each draw, grouped by logical PICA stages, with raw bit values and decoding from verified register maps.
- Command-list browser with per-word offsets and hoverable register interpretations; mark unknown/reserved registers.
- Shader disassembly with branch visualization, executed paths when using interpreter, uniforms and float24 differences.
- Vertex inspector with indexed vertex data, pre/post transform coordinates and primitive assembly outcome; geometry shader mode if supported.
- Texture viewer: tiled/swizzled/ETC1 and indexed formats as appropriate, mips, channel interpretation, address and version.
- Framebuffer layer explorer: color, depth, stencil, alpha; before/after selected draw and before/after transfer.
- Pixel history: for one output coordinate, list candidate draws/tests and final winner, **available in software reference path only initially**. Avoid claiming precise per-pixel history from an accelerated host rasterizer without a dedicated capture method.
- Shader/texture similarity and region hashes to identify missed effects while preserving false-negative caveats.

## Difference engine

Two modes:
1. **Visual:** compare same orientation/resolution/color space, account for prior framebuffer contents where retained pixels exist; report absolute/per-channel difference, max, mismatched count and diff image. Distinguish exact-match and tolerance-based comparisons. Don't compare GPUI scaled/composited screenshots when raw output is available.
2. **Structural:** align command-list/draw sequence by stable content keys and event relationships, not just list index; find earliest differing draw state/texture/version. Maintain ambiguity when commands are reordered.

When comparing Azahar integrated vs experimental, first show **guest state differences**, then **renderer differences**, then **host overhead**. For hardware, use a console capture/measurement with known capture constraints; a photograph is not pixel-exact framebuffer reference.

## Replay and re-execution limits

A saved draw snapshot might not be independently replayable if it relies on prior framebuffer state, stale host cache or cross-command writes. Explicitly record preconditions and the required resource snapshot. Initial deliverable is *inspectable capture*, not guaranteed replay. Advance toward replay only after capturing all referenced memory/state and demonstrating equivalence on exact-backend test fixtures.

## Timing accuracy hooks

Existing Azahar's `gpu.cpp` contains `DelayGenerator::CalculateDelayNanoseconds` and speed constants for DMA/texture copies. These are *model assumptions*. Expose their chosen values and input parameters in experimental trace. Compare GPU completion delays against physical hardware and report a residual/error model rather than replacing them with one game-specific table.

The PICA200 also has external busy/profiling registers described by 3dbrew and Coffeepot. Their meaning and calibration require physical tests. A future accuracy backend should model **command queue, vertex stage, rasterization/texture, framebuffer, transfers, memory arbitration and interrupt times** separately; measured pipeline overlap matters.

## Test suite

1. Known simple triangle (fixed shader/textures) and expected framebuffer.
2. Primitive/culling/clipping boundary tests, 16/24-bit float quirks.
3. CPU texture overwrite between successive draws: captures must show correct historical versions.
4. Render-to-texture then CPU read: observer respects cache synchronization and doesn't rewrite memory unexpectedly.
5. VBlank/framebuffer swap and repeated top/bottom/eye frames: capture identity must not collapse distinct screens.
6. Stress 10k draws while L1 tracing; verify bounded memory and dropped counts, compare output to tracing disabled.
7. L2 capture stops at expected draw and exports all resource references; replay not claimed until verified.
8. Real 3DS geometry, GPU transfer and hotspot experiments compared with emulator trace (separate accuracy suite).

## References

- Azahar PICA implementation: https://github.com/azahar-emu/azahar/tree/master/src/video_core/pica
- Azahar GPU commands: https://github.com/azahar-emu/azahar/blob/master/src/video_core/gpu.cpp
- Azahar renderer/cache: https://github.com/azahar-emu/azahar/tree/master/src/video_core
- Coffeepot command pipeline: https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/PICA200/pica200-command-pipeline.md
- Coffeepot GPU registers: https://github.com/estyseesghosts/coffeepot/blob/main/ctr/hardware/gpu-internal-registers.md
- Coffeepot performance: https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/performance.md
- 3dbrew: https://www.3dbrew.org/wiki/GPU/Internal_Registers and https://www.3dbrew.org/wiki/GPU/External_Registers
