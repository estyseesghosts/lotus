# 01 — Product contract: what this project is, and is not

## Thesis

Build an **independently designed developer-first 3DS emulator platform**. The initial machine underneath it is Azahar, selectively modified. This is a conscious halfway state: useful immediately for testing and agentic debugging, but with a migration path to models that are more faithful to the original hardware. Avoid building a new skin for Citra; build a system that can interrogate, compare, reproduce and eventually replace the emulation machinery.

## Why stock Azahar is insufficient for this goal

Azahar optimizes broad commercial-game compatibility, interactive play, conventional frontend UX, and generally performant host rendering. It already contains a GDB stub, profiler, debugging facilities and RPC. Our gap is **integration and repeatable programmatic observability**, not the absence of all debugging functions. For developer automation we need immutable trace artifacts, safe-point-consistent reads, stable scripted actions, per-draw GPU state with provenance, deterministic multi-instance execution and an external API designed from day one. Stock Azahar's process-wide globals and assumptions about frontend/runtime state make simultaneous instances inside a single process unsafe without substantial changes; use separate worker processes initially. An Azahar-GPU timing correction is not a detailed PICA200 scheduling simulator.

## Users and primary tasks

- Homebrew/port developer: launch a specific `.3dsx`, reset to a known checkpoint, stop at an SVC or GPU event, capture exact state and reproduce a rendering regression.
- Emulator engineer: run the same instruction/memory/GPU test on stock Azahar, `integration`, `experimental`, and physical Old 3DS; classify divergences with evidence.
- Agent/automation: use a bounded API to perform multi-step debugging, capture artifacts, produce a falsifiable bug report, and avoid console/UI babysitting.
- General player: run games when supported. This use is valuable but not the first acceptance priority.

## Must-have by first usable release

- Native Azahar-based worker, without Qt/SDL/Libretro as frontend dependencies.
- Rust GUI using GPUI for top/bottom screens; controller/touch input; logs; pause/resume/reset; attach a debugger.
- Headless CLI performing same operations as GUI.
- At least 2 isolated workers running concurrently (target acceptance: 4); no shared mutable NAND/save writes by default.
- Consistent memory/register reads, breakpoint handling and export of bounded, structured captures.
- First-class local MCP tools with the same semantics as the CLI/UI.
- Game/test/hardware identity, source revision, configuration, executable hash and tool versions captured for every run.

## Explicit non-goals for bootstrap

- No full HLE rewrite, real ARM9 boot or whole-SoC cycle accuracy.
- No reimplementation of rendering with wgpu merely because the desktop uses Rust.
- No custom JIT during first bring-up.
- No attempt to make a single Azahar `Core::System` safely host several consoles in one address space.
- No network service open to the LAN; no implicit ROM/key/firmware discovery or export.
- No promise of synchronized multiplayer/link emulation. Two isolated instances are not a local-wireless implementation.
- No one-pass automatic decompiler or unlimited instruction tracing in routine gameplay.

## Success criteria / metrics

| Metric | Bootstrap target | Longer-term target |
|---|---|---|
| Program launch | Known homebrew `.3dsx` reaches both screens + input | Broad compatibility at least matching tracked Azahar subset |
| Instance isolation | Four instances, separate storage and ports, repeatable shutdown | Efficient cloning/replay with validated snapshot semantics |
| Debugger integrity | Snapshot reads consistent at an explicit safe point | Event-causal replay and hardware-visible model inspection |
| GPU observability | Command-list capture + state-at-draw + output hashes | Pixel reference renderer, calibrated timing estimates |
| Reproducibility | Fixed sequence reproduces outcome and metadata | Cross-implementation differential regression corpus |
| Accuracy evidence | Known mismatches cataloged, no false timing claims | Demonstrated reductions in mismatches to physical 3DS |
| Overhead | Sampling/light tracing benchmarked separately | Low-overhead continuous counters, optional heavyweight captures |

Every performance threshold is recorded against a **specified host and test workload**; do not invent a universal overhead target before benchmarking.

## Three accuracy dimensions

1. **Functional**: architecture-visible values, images, service outcomes and memory behavior match observed hardware.
2. **Temporal**: guest device completion, interrupt and contention behavior match hardware within measured error bounds.
3. **System**: actual system firmware/kernel, ARM9 interactions and peripheral state match the machine.

Ship independent evidence scores. A visually correct draw is not proof of precise GPU timing; Citra speed is not Old 3DS speed.

## Decision log (initial)

- ADR-001: Rust owns product, UI, IPC/MCP, sessions, trace formats; C++ Azahar code supplies initial emulation.
- ADR-002: Two persistent Azahar branches in **one fork**; branch-forward workflow rather than two drifting forks.
- ADR-003: Process per instance; immutable base storage with private writable overlay.
- ADR-004: Start with CPU-backed screen transfer before GPU interop.
- ADR-005: Observability at explicit safe points, configurable tracing and artifact-based comparisons.
- ADR-006: Original 3DS first; New 3DS support is explicitly labeled.
- ADR-007: Native custom worker, **not Libretro**.

## Outward-facing differentiation

It is *not* yet "the most accurate 3DS emulator" merely because it has an independent UI. The legitimate bootstrap claim is **more instrumentable, automatable and reproducible**. Only call it "more accurate" for a specific, independently verified domain where real hardware supports that claim.
