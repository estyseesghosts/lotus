# 12 — Physical 3DS validation, microbenchmarks and evidence

## Hardware test harness is a first-class subproject

Create a tiny native 3DS executable test runner built with devkitPro, designed to emit repeatable state and timing reports, with test identifiers, firmware/CFW/model, toolchain revision and raw samples. Use Coffeepot's existing benchmark programs and documentation rather than inventing a fresh suite without reviewing it.

Starting points:
- https://github.com/estyseesghosts/coffeepot/tree/main/ctr/benchmarks
- https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/testing-on-hardware.md
- https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/cpu-optimization.md
- https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/PICA200/pica200-command-pipeline.md

Coffeepot's source-version page already distinguishes *compiled*, *run on emulator*, and *run on hardware*, and records pinned devkitPro/libctru/citro3d/Luma3DS revisions. Preserve that standard and put code/tests and raw data together.

## Test record structure

```text
experiment_id
hypothesis, expected_observable, null_result, confounders
binary_sha256, source_commit, compiler_flags, tool_versions
console_model/region, firmware_version, Luma3DS_version, mode
system_state: clocks, screens/3D, wifi, battery/charging, services
input fixture, RNG seed, memory addresses/alignment, warmup
raw timestamps/counters, units, calibration/overhead
repeat_count, median/p95/p99/spread, error count
raw frame/memory/GPU hashes, sample files
result interpretation, limitations, next discriminating test
```

Don't invent fields when the hardware cannot reveal them; use `unavailable` and explain. A successful no-error log is still not proof of exact rendering or timing.

## Measurement calibration

- Measure clock timer read cost with empty loops and back-to-back reads. Control compiler optimization, unroll and dead-code elimination (emit observable checksum).
- Use warmup/training iterations separately from measurement iterations; keep raw results.
- Record counter frequency and overflow/wrap behavior; use sufficiently wide accumulators.
- Randomize test order to reduce thermal/cache/time drift; report if the device's clocks or cache states cannot be fixed.
- Repeat on separate hardware sessions, not merely a million measurements in one uninterrupted run.
- For GPU completion, know precisely which IRQ/fence/busy flag constitutes completion. CPU submission time does not equal GPU completion time.
- For framebuffer correctness, record native buffer bytes where possible, pixel format/orientation and when the capture was made. Screenshots taken later may miss transient state.

## Minimum initial test families

### CPU architecture and memory

1. ARM/Thumb conditional execution, flag behavior, unusual shift operands, exclusive access, exceptions, privilege transitions, VFP arithmetic.
2. Instruction vs data cache working sets, strides, dirty-line writeback, invalidate, instruction cache self-modification, page-table/TLB changes.
3. Read/write latency by physical region and cacheable mapping, concurrent ARM11 core and DMA pressure where software permits.
4. Branch/control-flow tests with same instruction count but different predictability; test VFP dependencies/throughput.

### GPU functional behavior

1. Empty command lists, register masks/groups, immediate-mode vertices, indexed draw, primitive topology and restart.
2. Shader operations (including float24 special values, swizzles, branches and loop control), shader output masks.
3. Geometry/Primitive Engine cases, clipping limits, culling, interpolation, two-eye/rotation effects.
4. Textures: swizzle/layout/format/ETC1, filtering, wrap, mip, procedurals, palettes if relevant.
5. TEV, fragment lighting, LUT, fog, alpha/depth/stencil/blend operations; partial prior-buffer retention.
6. Display transfers, fills, tiled/linear conversion, alignment and busy/IRQ behavior.
7. CPU writes + cache flush before draw, GPU write then CPU read, CPU/GPU concurrent access.

### GPU timing isolation

- Constant draw state; sweep vertex count × vertex shader operations at low coverage.
- Constant geometry; sweep covered pixels × overdraw at fixed material/shader.
- Sweep textures in VRAM vs FCRAM, formats, filter, locality and mip usage.
- Busy flags/interrupt latency of `PSC0/PSC1`, PPF transfer and P3D command engine.
- Mixed workloads designed to falsify additive cost assumptions: vertex-heavy plus pixel-heavy, transfer overlapping draw, repeated state changes.
- Hardware hotspot counter experiments when registers are accessible; interpretation remains tentative until correlated with independent measurements.

### System behavior

- VBlank, framebuffer swap and screen state; sleep/resume/HOME lifecycle.
- HLE SVC vs real kernel scheduling/IPC/timeouts.
- DSP sample timing and synchronization; DMA, underruns.
- ARM9 storage/crypto behavior for firmware experiments in later phase.

## Per-experiment method

Write each test as an empirical question:

**Hypothesis:** moving identical vertex data from FCRAM to VRAM changes GPU completion latency by a measurable amount for workload X.

**Controls:** identical vertices, commands, shader, raster coverage, render target, cache maintenance, warmup and measured start/stop boundaries. Verify both paths render pixel-identical outputs.

**Independent variable:** data location only.

**Recorded values:** command submission ticks, GPU completion/IRQ ticks, hotspot register readings if available, bytes/addresses and framebuffer hash.

**Falsification:** distributions statistically overlap at the suite's observed noise level, or unexpected state changes explain the result. Conclude only for tested region/size, not universal memory hierarchy.

## Differential execution

For each fixture:

```text
hardware binary + initial data
        ├─ physical Old 3DS -> raw measured result
        ├─ Azahar upstream pinned -> emulated result
        ├─ integration branch -> emulated result
        └─ experimental branch -> emulated result
                                -> comparison report
```

The hardware binary must make guest-observable checksums, not assume the host emulator can inspect inaccessible hardware internal registers. Normalize known capture differences, preserve raw samples and report mismatches by observable.

## Test prioritization

Score candidate experiments by **uncertainty × likely impact × experimental observability**. Start with boundary behaviors for which Citra/Azahar source has explicit `TODO`, magic constants or compatibility defaults; prioritize commands executed by realistic homebrew. Unknown Primitive Engine behavior is interesting but shouldn't block basic software rendering. Track hardware evidence coverage of each accuracy-model assumption.

## Practical harness tooling

- Use devkitPro/libctru to construct and load homebrew; when researching register semantics, minimize high-level library code where safe and documented.
- Use `3dslink -s` for live logs, but avoid printing inside timed critical sections; buffer results and send after test.
- Use Luma3DS Rosalina GDB for process/virtual/physical mapping, registers and SVC traps when applicable; recognize debug attachment perturbs timing.
- Record failures with checksums and raw logs on SD, then retrieve from console.
- Keep test programs small enough to audit, with build scripts and pinned deps. No kernel patch that would invalidate a measurement without recording it.

## Statistical reporting and falsifiability

- Preserve all raw measurements and invalid samples with reason.
- Report central tendency, spread, threshold definition, timer precision and confidence interval when possible.
- Distinguish random measurement noise from deterministic emulator mismatch.
- Hold out some workloads from calibration. A fitted model only tested against its training set is not validated.
- When a timing model predicts observed hardware within 5%, that is a **test-specific finding**; document worst-case errors and scope.
- Experiments that do not detect an effect still count as useful results when instrumented and reproducible.

## Acceptance for first empirical accuracy gain

A pull request may claim improvement only with `before/after` test data on the **same stock Azahar source base** and independent physical 3DS samples, plus at least one held-out test and documented regressions. For purely architectural refactors or observability features, state no new accuracy claim.
