# 11 — Path from an instrumented Azahar to hardware-accuracy research

## The critical distinction

The bootstrap inherits Azahar's HLE assumptions, approximate scheduling and accelerated rendering. Even if the Rust front end is entirely original, it is **not a new hardware model yet**. Accuracy advances are only credited when an independently designed component or a measured correction reduces divergence from real 3DS behavior. Avoid the trap of equating 'different code' with 'more accurate emulation'.

## Target machine and long-term layers

Original 2011 3DS: ARM11 MPCore (two cores), ARM9, DMP PICA200, audio DSP, VRAM/FCRAM/AXI memory, LCD controllers, DMA and peripherals. Old 3DS is a fixed default. New 3DS CPU core count/clock/L2 and extended memory are an explicit later profile, not an arbitrary speed slider. Some 3DS software uses ARM7 only in legacy DS/GBA modes; full legacy modes are outside phase 1. Hardware references: 3dbrew Memory layout, Hardware, FIRM and Coffeepot CTR index.

### Functional vs temporal vs firmware-system

| Layer | Definition | Independent proof |
|---|---|---|
| Architectural CPU | ARM/Thumb/VFP instruction results, flags, exceptions, MMU behavior | hardware differential instruction tests |
| PICA functional | register commands, vertex/geometry shading, texture/raster output | native framebuffer/reference renderer pixel comparisons |
| Timing | completion times, interrupts, cache/bus effects | repeated physical tests under controlled workloads |
| Firmware/system | real boot, actual ARM9/ARM11 kernels/services/peripherals | boots correctly from user-provided dumped components, passes known OS tests |

Keep accuracy scores separate and per-subsystem. A timing approximation may be useful long before any cycle-accurate simulation is possible.

## Phase A: catalog inherited assumptions

Before rewriting, build a table `assumptions.csv` with: subsystem, Azahar source location, behavior, evidence, affected workloads, confidence, verification test, owner and disposition. Examples from inspected source:

- `Core::Timing::GenerateBaseTicks` can choose random initial ticks, affecting RNG-relevant game behavior. Test scenarios must pin a deterministic base.
- `Core::Timing::ScheduleEvent` uses internal queues and, in a thread-safe mode, enforces a minimum scheduling delay related to slice size. These are HLE policies, not physical SoC properties.
- `Core::System::RunLoop` steps ARM cores in virtual slices, with allowable small desynchronizations; not a cycle-by-cycle multicore processor simulator.
- `GPU::DelayGenerator` estimates copy duration from fixed throughput tables.
- `PicaCore::InitializeRegs` initializes some registers to compatibility defaults that may rely on earlier real firmware state (source comments explicitly acknowledge this).
- HLE kernel/service results can differ from actual NATIVE_FIRM; test individual SVC/IPC semantics, not just whether a game boots.

For each assumption, decide: preserve for fast mode; add observability; replace with a measured device model; or mark unknown.

## Phase B: CPU interpreter and cache model

Develop a **separate Rust reference CPU** rather than porting Dynarmic. It should:

1. Execute correct ARMv6K ARM/Thumb instruction semantics, banked registers, CPSR/SPSR, VFP operations, faults/privilege modes, MMU/TLB, atomic/exclusive monitor semantics.
2. Offer instruction-step state snapshots and an architectural differential executor against real hardware and Azahar interpreter.
3. Model instruction/data caches, write buffers, branch/pipeline effects, interrupt latency and inter-core coherence as separable timing components.
4. Use physical memory requests with explicit delay/visibility results so memory contention isn't a constant cost per load.
5. Keep JIT execution optional; a JIT would still submit memory/synchronization operations into the same model and cannot erase real stalls.

**Dependency order:** first prove execution semantics; then attach calibrated timing; only after that optimize. An interpreter with guessed cycle counts is not cycle-accurate.

## Phase C: independent PICA200 reference model

### Initial component boundaries

- external GPU engines: command submission, memory fills, display/texture transfers, busy/interrupt state;
- command parser and register bank (reuse/reimplement facts from Azahar/3dbrew but separately test mask/group behavior);
- programmable vertex/geometry shader interpreter with actual float24 arithmetic and swizzle/control-flow behavior;
- vertex fetch, primitive assembly, clipping, culling and rasterization;
- texture decoding and addressing, lookup tables, TEV combiners, alpha/depth/stencil/blending;
- display-facing framebuffer/scanout behavior distinct from draw target;
- independent timing pipeline, calibrated from Old 3DS experiments.

A software reference renderer is appropriate even if too slow for real-time games. Retain accelerated Azahar backend for normal use. A GPU capture can be fed into both renderers **only if** it contains the real preconditions and input resource versions; validate equivalence rather than assume capture is self-contained.

### Explicit unknowns

- Primitive Engine and variable-size primitive behavior: 3dbrew explicitly says full functionality is not understood, and specific enable behavior unknown.
- Fine-grained shader throughput, cache replacement, co-issue/overlap among geometry, raster and fragment stages.
- Cache capacities/latencies and exact bus arbitration under mixed CPU/GPU/DMA load.
- Edge cases in hardware texture and framebuffer formats, fixed-point interpolation and subtle coverage rules.

Treat these as research questions with falsifiable experiments, not commitments to implement undocumented details on guesswork.

## Phase D: memory and concurrency

Model physical memory regions with visibility and arbitration:

- FCRAM, VRAM banks and AXI WRAM; CPU cache and MMU mappings; device views may be noncoherent.
- DMA/display transfer engines and PICA reads/writes can overlap CPU activity.
- Hardware device requests should produce timestamped transactions with region/bank, size, service duration and completion/visibility behavior.
- Start with a coarse deterministic scheduler and calibrate under isolated transfers; then mixed workloads where transfer engines compete.

A fixed 'bytes / bandwidth' rule is a baseline only. Compare sequential/random, aligned/misaligned, read/write and mixed GPU traffic. Hardware architecture is not reducible to one shared RAM speed.

## Phase E: real firmware

Eventually replace much of HLE with actual ARM9+ARM11 firmware/kernel execution. 3dbrew FIRM documents ARM9/ARM11 kernel sections and core system modules. A genuine firmware boot needs correct privileged CPU modes, reset/boot state, interrupt controllers, timers, memory maps, crypto/security peripherals and IPC. It also requires legally user-provided firmware/dumped device files; do not include these artifacts. Full boot ROM execution may remain out of reach for many iterations.

Avoid attempting real firmware before the CPU/MMU/IRQ/memory foundations and the diagnostic harness can explain early boot failures. Treat `direct homebrew boot`, `FIRM boot` and `boot-ROM boot` as separate backend features with separate evidence grades.

## Hardware timing model methodology

A timing model should predict **unseen** workload timing. For the GPU:

1. Calibrate command parsing overhead using empty/no-op lists.
2. Isolate vertex shader work with tiny pixel coverage and fixed state.
3. Isolate raster/fragment work with few vertices and controlled screen coverage.
4. Measure texture format/cache behavior by varying addresses, access locality and filter modes.
5. Measure framebuffer/depth/blending pressure and display transfer separately.
6. Run mixed workloads to observe pipeline overlap, contention and saturation.
7. Compare predicted vs measured completion/interrupt times on a held-out set; report median and error distribution, not only average.

'Fast host time × constant = simulated 3DS time' fails because different bottlenecks dominate different workloads. Also, emulate the **causal** completion events, not an isolated per-draw delay that cannot interact with subsequent commands.

## Unknown behavior policy

Every uncertain device behavior must have:

- status `measured / documented / inferred / approximated / unknown`;
- source link or physical experiment ID;
- allowed domain (Old 3DS hardware version, CFW/kernel state, input format);
- counterexample or negative test when available;
- fallbacks (`strict`, `compatible`, or `disabled`), with exact semantics.

Never label compatibility workaround as a physical feature. Allow the debugger to highlight when a workload enters an approximated path.

## Evaluating "more accurate than Azahar"

Produce a public table of cases where (a) physical output/timing is measured; (b) stock Azahar result is measured at pinned SHA; (c) experimental result is measured; (d) the new model reduces error, with error bars; (e) regressions are enumerated. A handful of wins does not imply globally highest accuracy, but it establishes verified improvements.

## Research references

- 3dbrew physical map: https://www.3dbrew.org/wiki/Memory_layout
- 3dbrew GPU: https://www.3dbrew.org/wiki/GPU/Internal_Registers
- 3dbrew Primitive Engine unknowns: https://www.3dbrew.org/wiki/GPU/Primitive_Engine
- 3dbrew FIRM: https://www.3dbrew.org/wiki/FIRM
- ARM11 MPCore: https://developer.arm.com/documentation/ddi0360/latest/
- Mikage Developer Edition: https://github.com/mikage-emu/mikage-dev
- Coffeepot CPU/PICA/benchmarks: https://github.com/estyseesghosts/coffeepot/tree/main/ctr
