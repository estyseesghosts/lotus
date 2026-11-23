# 34. Performance and issue triage (issues #1–#18)

Status: draft. Measurements are from `tools/bench.py` on one Windows machine, t03 homebrew ROM, Old 3DS, 30 warm-up frames. Run directories: `runs/bench-vk-02` (Vulkan, 120 frames x 3), `runs/bench-sw-02` (software, 60 frames x 3), `runs/bench-preview-02` (capture every 2 frames). Workers: `azahar` = integration pin, `citnlo_pin` = CITnLO pin before this work, `citnlo_new` = CITnLO experimental `c48dac6e`. The tables predate the shared-memory previews and the run-length page table; current pins are integration `b4b9f0c2` and experimental `559f3f4d` (`config/pins.toml`; code at `0e98bf18`, README after).

## CITnLO memory scanner (#156)

The scan runs at the worker's debugger safe point. `CheckMemory` in
`src/ctr_worker/debug.h` validates each page or VMA extent before reading through
`MemorySystem::ReadBlock`. Narrowing reads one span per occupied page, including
any cross-page value tail, and checks all pages and VMAs touched by that span.
The original width, little-endian values and candidate order are preserved.

A scan admits at most 1 MiB of guest addresses and 1,048,576 candidates. On
Windows x64, the previous and replacement candidate vectors together reserve at
most 32 MiB, plus a 4 KiB read buffer and the response of at most 256 matches.
The worker checks a cooperative 250 ms wall-time budget before each bulk read
and before publishing the result. An individual read or OS stall can run past
that budget. An error keeps the previous complete result and its paging state.
Lotus uses the `transactional_memory_scan` feature flag to preserve the host
scan generation on failure. Successful `scan` and `scan_narrow` replies add
`scan_stats` with `page_checks`, `bulk_reads`, `bytes_read`, `candidates_checked`,
`candidate_capacity_bytes` and `worker_us`. `scan_results` keeps its existing shape.

Verified emulator on Windows x64, GCC 16.2 Release, LTO off, software renderer.
The baseline was `a3994c1d`. The first matched run recompiled the worker entry
point at `2d0c9925` and linked it against the same pinned native libraries.
Five samples per worker used the owned scanner fixture at its first frame.

| Operation | Baseline median RPC | Updated median RPC | Updated worker median |
| --- | ---: | ---: | ---: |
| 1 MiB zero scan (width 1) | 43.8 ms | 10.8 ms | 5.0 ms |
| Unchanged narrowing of all 1,048,576 candidates | 122.5 ms | 15.2 ms | 8.8 ms |

Each updated operation performed 256 validation checks and 256 reads totaling
1 MiB. Source inspection gives 1,048,576 byte validations for each baseline
operation and 1,048,576 individual reads for baseline narrowing. In a second
five-sample run during a background build, the probe sent an unchanged narrow
and pause together over native stdio. Median time to the pause reply, including
the preceding narrow, fell from 156.7 ms to 9.7 ms (maximum 168.0 and 10.0 ms).
The guest fixture SHA-256 was
`cce5f13aac4622db70d58907c8cf7eb100c1e1429962b5c131e2fc4dd6af3fbf`.
An independent clean build of `2d0c9925` passed the same probe at median RPC
times of 11.5 ms for scan and 14.9 ms for narrowing. The native debugger
acceptance suite passed on both emulated models, including execute breakpoints,
write watchpoints and checkpoint replay.

```powershell
python tools/build-debug-fixture.py target/scan-fixture --fixture scanner
python tests/test_native_scan.py target/debug/lotus-mcp.exe <worker.exe> target/scan-fixture/scanner.3dsx <new-output-directory>
```

Use `--baseline` for an older worker. The probe checks widths 1, 2, 4 and 8,
cross-page and cross-VMA values, sparse and dense matches, all narrowing modes,
sorted paging, MMIO and unmapped-boundary rejection, and cross-page pointer reads.

## Native WAV capture (#161)

Verified emulator on Windows x64 with the software renderer. Integration
`5ecbd982` and experimental `a3994c1d` each ran the owned DSP fixture three
times per emulated console model. All 12 captures contained 32,800 stereo
frames at 32,728 Hz and 16 bits per channel. Each had PCM SHA-256
`7c8a00e8f5ed50b0862cc7cc473ca6b2a84a3ff247a92ee39d5a0206c46fda09`
and raw worker FNV-1a value `4857884725183162789`. The probe reads the WAV
back and checks the reported PCM byte count against the sample count.

```powershell
python tools/build-debug-fixture.py target/audio-161-fixture --fixture audio
python tools/audio_acceptance.py <worker.exe> target/audio-161-fixture/audio.3dsx <new-output-directory>
```

The report records the exact worker source alongside the wall times for the
60-frame run and capture-stop request. Those times include IPC and do not
measure the DSP producer lock or isolate filesystem stalls.

The async writer was built locally at integration `fb7845d` and experimental
`1db4ff6`. On each worker, two simultaneous sessions ran the fixture on both
emulated console models with software and with Vulkan. Every sealed WAV matched
the baseline PCM hash, even when finalized by orderly process shutdown. Killing
a worker during replacement preserved the previous sealed WAV and left a
`.part` file. The muted Cubeb focus toggle also preserved the raw hash.

`tests/worker_wav_capture.cpp` stalls the writer while pushing into its queue.
It checks bounded stop, overflow and first-loss truncation, the 64 MiB cap,
WAV byte order, invalid output and rename failure, and recovery. The emulator
lifecycle check takes a fresh output directory for each worker and renderer.

```powershell
python tests/test_worker_wav_capture.py <worker.exe> target/audio-161-fixture/audio.3dsx <new-output-directory> vulkan
python tests/bench_worker_audio_record.py <native-source> <native-build> <c++.exe> <pre-change-sha> <updated-sha> <new-output-directory>
```

The second command inserts timing probes into temporary copies of the actual
`WorkerSink::Record` and PCM write call sites. Each mode uses 1,000 calls of
160 stereo frames, with at least one millisecond between starts and a competing
`Drain` thread. Slow mode requests a 12 ms sleep immediately before each PCM
write. Pass `--workers 2` for two independent processes per mode. The JSON
records average, p95, p99 and maximum for Record, producer
lock wait and hold, and PCM writes. Times below are in microseconds.

| Worker | Capture | Record avg / p95 / p99 / max before → after | Lock hold p99 before → after |
| --- | --- | --- | --- |
| AZAHAR | off | 1.08 / 1.1 / 9.1 / 28.9 → 1.05 / 1.1 / 7.3 / 14.5 | 8.9 → 7.1 |
| AZAHAR | on | 3.94 / 13.1 / 46.8 / 228.2 → 1.30 / 1.9 / 10.9 / 25.1 | 45.6 → 9.9 |
| AZAHAR | slow | 15,653 / 16,041 / 17,491 / 31,144 → 1.14 / 1.2 / 6.4 / 19.2 | 17,479 → 5.0 |
| CITnLO | off | 1.10 / 1.0 / 9.8 / 16.7 → 1.01 / 1.0 / 8.1 / 14.7 | 9.4 → 8.0 |
| CITnLO | on | 3.50 / 11.8 / 39.0 / 160.3 → 1.47 / 3.4 / 12.9 / 21.1 | 38.8 → 12.6 |
| CITnLO | slow | 15,679 / 16,041 / 16,398 / 31,060 → 1.16 / 1.2 / 7.3 / 22.3 | 16,389 → 5.8 |

In slow mode, producer lock wait p99 was 0.6 → 0.1 µs on both workers.
PCM write p99 was 17.48 → 23.75 ms on AZAHAR and 16.39 → 22.31 ms on
CITnLO. The writer uses larger batches, and its per-write latency includes
the injected sleep. Both retained the exact first 66,496 of 160,000 PCM
frames and reported the remaining 93,504 as dropped. Raw DSP totals and
hashes covered all 160,000 frames in every case. With capture off, the
average cost of the full diagnostic path was about 1 µs per 160 stereo
frames. The change keeps the existing ring and hash calculation.

With two simultaneous processes per branch in slow mode, Record p99 ranged
from 30.53 to 30.57 ms before and 4.5 to 6.2 µs after. Both WAVs in each
run contained the reported contiguous prefix and all raw sample totals and
hashes still matched. This process-level stress probe uses synthetic DSP
samples. The lifecycle probe separately exercises simultaneous emulator
sessions with the owned guest fixture.

The following medians are milliseconds for six unthrottled 60-frame RPCs
per configuration using `tools/audio_acceptance.py --unthrottled`. Use
`--capture-off` for the off row and `--renderer vulkan` for the Vulkan run.

| Worker | Renderer | Before off / on | After off / on |
| --- | --- | --- | --- |
| AZAHAR | Software | 170.05 / 169.05 | 168.29 / 169.92 |
| AZAHAR | Vulkan | 146.89 / 146.14 | 160.95 / 161.19 |
| CITnLO | Software | 170.55 / 167.89 | 166.83 / 168.13 |
| CITnLO | Vulkan | 146.68 / 147.91 | 147.20 / 147.35 |

The before binaries were cached builds of the pinned sources. AZAHAR Vulkan
was about 14 ms slower in the updated build in both modes. These are
whole-run timings and include IPC. Per-frame hitch and Cubeb underrun
counts, actual disk exhaustion, and desktop record/playback still need
verification.

## GPU memory status baseline (#159)

`tools/gpu_memory_acceptance.py` used the Windows workers at `fb7845d`
(AZAHAR) and `1db4ff6` (CITnLO), before changing GPU memory queries. Each
worker booted the owned DSP fixture, issued 160 status RPCs while paused
and 160 while the guest ran, then measured an unthrottled 60-frame run.
The tool retained every RPC time and confirmed that only Vulkan workers
reported GPU memory. The table pools each phase's RPCs across workers
and takes the median of the 60-frame runs.

```powershell
python tools/gpu_memory_acceptance.py <worker.exe> target/audio-161-fixture/audio.3dsx <new-output-directory>
```

| Renderer | Workers | AZAHAR p99 paused / running (µs) | CITnLO p99 paused / running (µs) | Median 60-frame AZAHAR / CITnLO (ms) |
| --- | ---: | ---: | ---: | ---: |
| Software | 1 | 172 / 1,136 | 185 / 1,254 | 92.85 / 94.52 |
| Software | 2 | 227 / 1,149 | 228 / 1,212 | 103.04 / 97.09 |
| Software | 4 | 754 / 1,628 | 839 / 1,378 | 98.33 / 107.09 |
| Software | 8 | 1,563 / 2,101 | 1,736 / 2,209 | 138.58 / 137.96 |
| Vulkan | 1 | 200 / 3,034 | 235 / 3,013 | 69.92 / 70.69 |
| Vulkan | 2 | 234 / 2,143 | 236 / 2,981 | 75.23 / 75.27 |
| Vulkan | 4 | 811 / 2,516 | 1,000 / 2,693 | 80.93 / 83.38 |
| Vulkan | 8 | 2,232 / 3,412 | 2,148 / 2,937 | 124.81 / 120.72 |

These observations include IPC, scheduling and reply construction. The
software rows show the cost also grows without a live Vulkan allocator.
Inner timing at the GPU accounting mutex, VMA and Vulkan calls is needed
to attribute the measured cost.

## Measured (verified emulator)

| renderer | config | citnlo_pin s | citnlo_new s | ratio vs off (new) |
| --- | --- | --- | --- | --- |
| Vulkan | off | 2.035 | 2.048 | — |
| Vulkan | 1 execute breakpoint | >= 20 (deadline) | 2.045 | 0.999 |
| Vulkan | 64 execute breakpoints | >= 20 (deadline) | 2.076 | 1.014 |
| Vulkan | PICA events | 5.130 | 4.980 | 2.43 |
| Vulkan | rewind every frame | 38.263 | 37.804 | 18.5 |
| Vulkan | rewind every 2 / 5 / 10 frames | n/a | 19.198 / 9.201 / 5.776 | 9.4 / 4.5 / 2.8 |
| Vulkan | SVC/IPC trace | 2.032 | 2.035 | 0.99 |
| software | off | 9.520 | 9.486 | — |
| software | PICA events | 13.486 | 13.376 | 1.41 |
| software | rewind every frame / every 5 | 30.022 / n/a | 30.028 / 13.431 | 3.17 / 1.42 |

AZAHAR off: 2.040 s Vulkan, 9.424 s software. Vulkan is about 4.6x faster than software for the same guest work, which is the basis for #5.

Observations:

- Execute breakpoints were the largest regression (#8). With the JIT block-split they cost nothing measurable.
- Rewind checkpoints are far more expensive under Vulkan (18.5x) than software (3.2x). Inference, not verified: the snapshot path reads back renderer state each frame. The interval option scales roughly linearly. Every-frame Vulkan rewind is still the slowest mode.
- PICA events: the per-command path is only taken while the debug context is installed. With PICA tools disabled the fast path is restored (`fast_path_commands`); the counter is not yet asserted by a test.
- Capture every 2 frames under Vulkan drops to 35.9 fps (1.669 s / 60 frames).

Guest repeatability (verified emulator, cause not found): `guest_ticks` is identical in every trial, but the final screens fall into 2–4 distinct images across repeats of the same worker and config. The images differ by a one-to-two pixel scroll and a moved sprite, so the guest state really differs. Capture is not the cause. The software path reads `RendererSoftware::Screen` synchronously (`src/ctr_worker/frames.h`). The divergence predates this work: the unmodified AZAHAR worker shows it under Vulkan (`runs/bench-clock-01`), and the pre-change CITnLO worker shows it under software (1 of 8 runs, `runs/bench-clock-02`). The default `init_clock=SystemTime` seeds guest date/time from the host clock (`src/core/hle/kernel/shared_page.cpp:40-58`), so `tools/bench.py` now pins `init_clock=FixedTime`. That did not remove the divergence, so some other host-timing-dependent input reaches the guest. Until it is found, a `False` in the bench's `guest identical` column is not evidence that a debugger feature changed guest behaviour. The ratios are unaffected. `runs/bench-clock-*` ran while another process was saturating the host, so their timings are not used above.

## Issue status

- #1 resolved on main (shutdown count, `lifecycle: closed`, scratch cleanup). `tests/test_mcp_native.py` passes against the CITnLO worker (`runs/i1-mcp-01`). Afterwards `host/scratch` is empty, only the two capture artifacts remain, and no `ctr-worker` or `lotus-mcp` process is left (verified emulator).
- #2 fixed at current pins; AZAHAR log shows "3DSX homebrew has no NCCH title id; continuing with id 0".
- #3 fixed: desktop calls `ShowWindow(SW_SHOWNORMAL)` once when started hidden. `tests/test_window_visible.py`.
- #4 resolved: the PICA debug context is installed only while GPU tools are enabled. `tools/pica_path_acceptance.py` asserts it per renderer on CITnLO (`runs/pica-path-01`, verified emulator). `fast_path_commands` advances while passive (22,454 then 35,876), freezes while capture is on (8 draws recorded), and resumes after capture is turned off (48,626). Guest ticks match a run that never enabled capture (672,177,568). Screens are only reported because of the divergence described above.
- #5 implemented: supervisor prefers Vulkan (`prefer_vulkan`, default true) and replaces the worker with a software one if Vulkan load is unsupported, recording `renderer_fallback`. `tests/test_renderer_default.py` passes on both backends.
- #6 previews use a shared-memory ring and asynchronous Vulkan readback in both workers ([21](21-native-control.md#ownership-and-budgets)). Paused previews skip work when (epoch, frames) is unchanged. `tools/preview_acceptance.py` passes on CITnLO and AZAHAR (`runs/preview-shm-01`, `runs/preview-shm-azahar-01`, verified emulator). Paused previews equal `capture`. Running previews have matching epochs and monotonic frames, lagging 0 frames under software and at most 3 (one request) under Vulkan. Capture files are untouched and pipe writes are about 660 bytes per preview. After a reset the epoch is new and the preview equals `capture`. `crates/session/tests/native_process.rs` `shared_previews_match_capture_per_worker_and_epoch` covers the Rust side. See [visible desktop playback](#6-visible-desktop-playback) for the matched quiet-host benchmark and the follow-up fix to pauses between run batches.
- #7 the worker loop used to build the JSON input snapshot on every `RunLoop` iteration, even with rewind off. A standalone harness reproduced the call shape (`SnapshotInput` in `src/ctr_worker/input.h`) with a counting `operator new`, built with the same nlohmann json and winlibs GCC -O2. It measured 75 heap allocations per call (verified compiled, not counted inside the emulator). `RunLoop` runs at least 4 times per frame (`MAX_SLICE_LENGTH = BASE_CLOCK_RATE_ARM11 / 234`, `core_timing.h:184`), so that is at least 300 allocations per frame. The snapshot is now built only when `WantsFrameCheckpoint` is true (`src/ctr_worker/debug.h:386`). That gives 0 allocations with rewind off and one snapshot per stored checkpoint with rewind on.
- #8 execute breakpoints stay in the JIT (`PreCodeReadHook` block split). Watchpoints and pc/memory predicates still single-step: Dynarmic's memory-abort path re-executes the instruction after writeback (`load_store.cpp` 61-62, `a32_emit_x64_memory.cpp` 253-255), so it cannot report a write watchpoint correctly.
- #9 checkpoint interval (`interval_frames`, 1..60). Input changes force a checkpoint. Testing intervals above 1 exposed a reverse-continue bug that predates this work. When the breakpoint set changed after the newest checkpoint, hits were counted against that older checkpoint. The replay then counted executions from before the change as hits and landed on the wrong one. Interval 20 landed about 430,000 counter values early (`runs/perf-rev-interval-01`). With interval 1 the same bug needed a mid-frame breakpoint change. Fix: `ForgetHits` now forces a checkpoint and leaves hits uncounted until one is stored. History entries also keep the resume-from-breakpoint skip state, because `RestoreHistory` used to clear it, so a checkpoint taken at a breakpoint PC no longer replays an extra hit. `tools/reverse_acceptance.py --interval-frames N` has an `armed_mid_window` check. It passes at N = 1, 5 and 20 on Old and New 3DS (`runs/perf-rev-int{1,5,20}-02`, verified emulator). `debugger_acceptance` and `gdb_acceptance` pass on the same worker (`runs/perf-dbg-02`, `runs/perf-gdb-02`).
  Checkpoint cost. `rewind_status` now reports serialize, compress and restore time and serialized and stored bytes (`checkpoint_*_ns`, `restore_ns`). Timing `System::serialize` per section (temporary instrumentation, not committed) put 621 of about 840 ms per checkpoint in memory and 218 ms in timing, CPU cores, services and archives; kernel, GPU and DSP were under 3 ms together. Both large parts are page tables: each `PageTable` stores 2^20 `MemoryRef`s, each a Boost-tracked `shared_ptr` plus offset, and 2^20 attributes (`src/core/memory.h`). FCRAM itself is one binary object. Changes: the savestate is serialized into a reused buffer and compressed from it without the extra copies (`SaveStateBuffer`, `CompressDataZSTDAfter`), and `PageTable` is now class version 1, which stores runs of pages that share an attribute and a backing object at consecutive offsets. Loading validates every run and still accepts version 0. `tools/savestate_format_check.py` saves with the previous worker, loads in the new one and compares registers, fixture memory and screens at load and after 20 more frames, then round-trips a new-format state, on Old and New 3DS (`runs/savestate-fmt-01`, verified emulator). On the new worker `reverse_acceptance` passes at intervals 1, 5 and 20 (`runs/rev-rle-01`, `runs/rev-rle-int{5,20}-01`) `debugger_acceptance` passes (`runs/rle-dbg-01`), and `gdb_acceptance` passes on the Clang and devkitARM Thumb fixtures (`runs/rle-gdb-debugger-thumb{,-dkp}-01`).
  Back-to-back, same host at about 50 % load from another process, interval 1 (`runs/rev-ab-old-02`, `runs/rev-ab-new-02`), per checkpoint, old -> new worker:

  | console | serialize ms | zstd ms | serialized MiB | restore ms | stored KiB |
  | --- | --- | --- | --- | --- | --- |
  | Old 3DS | 719 -> 94 | 72 -> 50 | 182.5 -> 142.4 | 867 -> 409 | 29.5 -> 24.8 |
  | New 3DS | 779 -> 144 | 119 -> 99 | 314.6 -> 274.4 | 1020 -> 579 | 33.6 -> 28.9 |

  The zstd level is selectable (`compression_level`, -7..19, default 3). It matters less than the format: on the previous worker level 1 against 3 was 50 against 66 ms per Old 3DS checkpoint for 30.6 against 29.5 KiB, and level 19 cost 473 ms (`runs/rev-level{3,1}-01`, `runs/rev-level19-int5-01`). `tools/bench.py` keeps a level-1 `rewind_*_fast` config. What remains per checkpoint is mostly copying and compressing FCRAM and VRAM. Delta checkpoints were not attempted: Dynarmic reads and writes guest memory directly through `PageTable::pointers.raw` (`src/core/memory.h`), so dirty-page tracking would need write protection or JIT changes. These timings come from a busy host; the quiet-machine A/B is in the commands below.
- #10 `tools/bench.py` records host and native build metadata, worker hashes, and measured-window CPU and I/O counters. It rotates worker order each repeat, checks the loaded settings, and suppresses cross-worker ratios when the recorded profiles differ or a trial ends early. Use `--setting deterministic_async_operations=true` to match the native backends' filesystem scheduling mode, which differs by default. `--shader-cache` controls the initial Vulkan disk cache. The separate `tools/bench_frontends.py` compares headless MCP with the visible desktop, unthrottled by default. A pinned Windows Vulkan smoke test matched guest ticks and screens with shared settings, and skipped GPU counters on AZAHAR after the worker explicitly refused the operation. The host was compiling during this run. The stock comparison and consolidated acceptance run are pending.
- #11 `scripts/lto-ab.ps1` builds one clean pin in two fresh directories with only `ENABLE_LTO` changed. It checks the Ninja compile rules for the worker and the libraries under Azahar's `src/CMakeLists.txt`, and the worker's executable link rule. It records coverage by target including external dependencies, configure-and-build wall time, executable link time, executable size, SHA-256 and build flags. The runtime benchmark disables the frame limiter and requires complete trials with matching settings, guest ticks and screen hashes. Use `-RequireIdle` for performance qualification. Production builds keep IPO disabled pending that evidence.
- #12 devkitPro GDB 14.1 passes `gdb_acceptance` on CITnLO (Old/New 3DS, Clang and devkitARM GCC fixtures, Thumb checks). GDB server refuses to start on AZAHAR.
- #13–#17 (desktop layout): resizable, snap-collapsible panes with saved widths, an always-visible inspector rail with vector icons, grouped inspector navigation with per-view commands, and a toolbar and gauges that adapt to the centre width. Design and evidence in [30](30-sessions-and-interface.md#window).
- #18 opt-in Discord Rich Presence, verified against a running Discord client. Policy, transport and limits in [30](30-sessions-and-interface.md#discord-rich-presence).

## Issues after v0.0.3 (#19–#32)

- #24 GPU capture levels (CITnLO `459d3662`). `gpu_configure` takes `capture`: `counters` (debug context stays detached, so the batched PICA path runs; new PicaCore `command_lists` and `draw_calls` counters), `events` (per-command events, draw metadata only), `draws` (register and shader snapshots of draws in `draw_range`, at most `max_draws`, resource bytes only with `resources: true`), `full` (every draw with resources; the default, so older clients keep their behaviour). A snapshot keeps raw copies; JSON, hex and the file are built only when `gpu_draw` asks for that draw, so no level writes a file per draw. Synchronous cost per snapshotted draw is the register/shader copy plus, with resources, `FlushRegion` and the copy of each texture/index/vertex range (4 MiB budget per draw, 8 draws retained). `tools/gpu_capture_modes.py` passes on software and Vulkan (`runs/gpu-modes-24-03`, verified emulator): counters keep `command_path: batched` and write no `gpu/` files; events write none either; `max_draws: 2` takes 2 snapshots and counts 778 skipped; draw 1 under `draws` + `draw_range [1,1]` + resources is byte-identical to `full`, and `full` is byte-identical to the v0.0.3 worker (103,380 bytes). `gpu_acceptance` and `pica_path_acceptance` pass unchanged (`runs/gpu-24-01`, `runs/pica-path-24-01`). Busy-host Vulkan times for 30 frames, not acceptance evidence: off 524 ms, counters 508, events 901, draws 807, full 799. `events` costs as much as `full`: the remaining per-event cost is building, buffering and notifying one JSON object per PICA event (`Record` in `src/ctr_worker/debug.h`), which is #26. Identical boots of t03 do not always reach draw 1 in the same guest state, on Vulkan and occasionally on software (`runs/gpu-repeat-24-01`, `runs/gpu-modes-24-02`), so the fidelity check repeats boots until a pair matches; that divergence is #29. Desktop GPU view: Count draws, Capture 8 draws, Full capture, Disable capture. Bench configs `gpu_counters`, `gpu_events`, `gpu_draws_8`; `pica_events` is `full`.

- #19 supervision under load (`84c6326`). Each session already had its own supervision task (v0.0.3); the poll skipped a session whose lock was held, so `bench_parallel` probing one session back to back recorded 74 busy polls and no poll in 8 s while the run sat at its 600-frame budget. After 4 skipped polls the poll now queues on the session's FIFO lock, and while a run should reach its budget within one period the next poll is due 1 ms after it (at least 5 ms). Test `back_to_back_commands_do_not_starve_supervision` fails with the old poll (verified compiled).
- #22 `tools/bench_parallel.py`: N sessions through the real `lotus-mcp` (or the desktop), any bench.py config, fixed-rate probes (registers, input, optionally capture), `--idle K` paused sessions, `--stall` (one suspended worker), seeded or cold shader cache, `--workload`, `--focus-priority`, `--reference` (bench.py single-worker row). Writes `report.json` (raw rounds, SHA-256 of every binary and ROM, build manifests, host and GPU driver), `summary.md` and `aggregate.svg`. CI runs a 2-session mock smoke and a stall smoke. GPU memory per session and the policy's headroom are recorded since #46; GPU wait time is not measured. The harness found three latency bugs, fixed in both workers (verified emulator):
  - stdin was polled with a 1 ms sleep, which costs a whole 15.6 ms timer tick: `status` 15.4 ms → 0.1 ms (CITnLO `3537ccb7`, AZAHAR `7af07040`);
  - the paused loop waited out its 10 ms deadline after a request arrived: back-to-back `run_until` of 1 frame, unthrottled Vulkan, 27.4 ms → 2.2 ms per call (CITnLO `f5abd961`, AZAHAR `24a33631`). bench.py's reference went from 333 fps to 1772, matching the 1-session supervisor row (1788);
  - supervision starvation (#19 above).

  Busy host, t03 Vulkan unthrottled, not acceptance evidence (`runs/bp-real-04`): aggregate 1788 / 3058 / 5075 fps at N = 1 / 2 / 4 (per session 1788 / 1529 / 1274), command p50 0.5–0.6 ms, no late polls, about 961 MiB working set per worker. Two paused sessions next to two running ones used 0.002–0.008 CPU s/s and 938 MiB each (`runs/bp-idle-01`). One armed write watchpoint (`watch_write_1`) puts CITnLO into instruction stepping: 0.4 fps per session.
- #23 host policy (`crates/session/src/policy.rs`, docs/23): `interactive` (default) allows 2 rewind sessions, 2 GPU-capture sessions above `counters` and 2 concurrent full-state operations; `batch` has no budgets. New sessions need available RAM above `memory_reserve_mib` plus the largest worker working set. Optional above-normal priority for the focused session's worker. `tests/test_mcp_policy.py` passes against CITnLO (`runs/policy-01`, verified emulator).
- #46 Vulkan allocation failure. The pinned fork called `UNREACHABLE()` when `vmaCreateImage` failed (`vk_texture_runtime.cpp`), which ended the 4-worker run in docs/36. CITnLO now turns allocation failures and device loss into a typed failure (`vk_gpu_error.h`, `vk_gpu_status.h`): the texture runtime, swapchain/present, stream buffers, pipeline builds, scheduler and master semaphore report it instead of aborting. The worker writes one `worker_gpu_error: {json}` stderr line (cause, `VkResult`, object, frame, heap budgets) and exits with code 4, and Lotus classifies it (`failure.rs`, `telemetry.failure.gpu`). The worker exits instead of unwinding because a draw that allocates is called from guest code: gdb on an injected failure showed `Draw` ← GSP ← SVC ← a dynarmic JIT frame, and the C++ exception died there (exit 0x20474343, MinGW's unhandled-exception code); JIT frames carry no SEH unwind data. Injection: `CTR_WORKER_VK_FAIL_ALLOC=N` fails the Nth Vulkan image/buffer allocation, `=file` reads N from `<root>/vk-fail-alloc`. Swept N = 1–14 on t03: every case reported `vulkan_out_of_memory` with exit 4. Before the failure handler existed, N = 1–2 (inside the rasterizer cache constructor) ended in 0xc0000409 rather than an exception; the cause was not established. Four sessions with one failing mid-scene: the other three kept advancing and capturing. AZAHAR carries the same change (`4f441bcdb`) and passes both checks: first allocation fails at load as `vulkan_out_of_memory`, and mid-scene session A fails on `present frame 400x240` with its report while three siblings advance 194 → 317 frames and capture (verified emulator, both workers). Status reports `gpu_memory` (VMA heap budgets, `VK_EXT_memory_budget` or labelled `vma_estimate`, live and high-water device-memory bytes, failed allocations) and the host admits new Vulkan sessions on that headroom (docs/23). See the [packaged sweep and controlled external load](#46-packaged-vulkan-worker-scaling).
- #25 non-blocking Vulkan preview (CITnLO `58e00ed1`, AZAHAR `ed51b869`, Lotus `9f39e16`). A running preview never waits for the GPU: three staging slots per screen, the newest finished one is copied, a request that finds none ready repeats the last slot (`new_frame: false`). Only the selected session is previewed. `preview_acceptance` asserts `gpu_waits` does not move over 120 running previews (both workers, `runs/preview-wait-*-01`); max lag 2 frames.
- #28 card copies (`995e988`): a secondary created together with its card's primary waits up to 30 s for the primary to commit, instead of failing with "still being mounted".
- #26 event delivery, with proposal #33 (CITnLO `0cef6413`, AZAHAR `9016f8ab`). Workers wrote each reply and notification on the thread that produced it, so emulation stopped when the host stopped reading, and every PICA, SVC and log event was built as JSON and written whether or not anyone used it. Now a `ControlWriter` thread (`transport.h`, both branches) writes every frame. Replies and `stopped` keep their order and are never dropped. Other notifications are best effort: at most 1024 queued, the oldest dropped first, counted in `notifications_dropped` and `status.transport`. CITnLO groups events into the categories `stopped`, `log`, `native_log`, `svc`, `ipc` and `pica`. hello `subscriptions` and `events_configure` (`notify`, `history`; accepted before load) choose which categories are pushed and which are kept for `events`/`logs`, and producers skip any category nobody wants. `stop_reason.sequence` ties `status.stop` to its notification. Lotus subscribes to `stopped` only, because the GDB server is the only consumer of pushed events. The protocol refuses a `notify` set without `stopped`. Fixture mode 8 floods svcOutputDebugString at a `log_mask` rate; `tools/event_flood.py` drives it.
  Busy host (load 0.53), not acceptance evidence (`runs/flood-05`, verified emulator):
  - Slow reader, 3 s unread: the new worker emulated 179 frames, private bytes went from 544 to 546 MiB, and 45,857 events were dropped and counted. The stop arrived and `status.stop.sequence` named it. v0.0.3 emulated 1 frame.
  - `log_mask 0xff` (31k–48k events/s per worker), per-session fps at N = 1 / 4 / 8: v0.0.3 117 / 57 / 30 (0.26 CPU s/s at N = 8, blocked on the pipe); new worker pushing every category 119 / 94 / 71; `stops` (the Lotus default) 184 / 168 / 136; `quiet` (no history) 205 / 177 / 165. Status p50 was 0.3 ms under `stops`, against 6.6–10 ms for v0.0.3 (which still has the #22 tick wait).
  - At N = 1 the new worker pushing every category is no faster than v0.0.3. There the cost is building the events, not the pipe, which is why unsubscribed categories are not built at all.
  Regressions pass on the new workers: `debugger_acceptance` (`runs/r26-dbg-01`), `gdb_acceptance` continue/stop through `ctr-cli` (`runs/r26-gdb-01`), `preview_acceptance` on both workers (`runs/r26-preview-{exp,int}-01`, no running GPU waits), and the `native_process` ignored tests on both workers.

- #29 first divergence. CITnLO debug operation `state_hash` (docs/21) hashes guest ticks, per-core context, physical memory in chunks and each top-level `System::serialize` section; `tools/divergence.py` steps several workers in lockstep, compares the hashes and prints the first differing bytes with context. Identical t03 boots diverged for three independent reasons, each found by the first differing bytes (verified emulator):
  - Host-timed async HLE. With `deterministic_async_operations` off (upstream default), `hle_ipc.h` `RunAsync`/`RunOnThreadWorker` run the service's async section on a host thread, which calls `WakeAfterDelay` when it finishes, so the guest wake tick follows host timing. On t03 Vulkan the runs differed at frame 2 in cpu0, services and gpu and ended on 2 distinct screens out of 4 (`runs/div29-noasync-vk`); `bench.py` trials with host async ended on 2 (software) and 4 (Vulkan) distinct screens out of 5, against 1 with deterministic async (`runs/bench-async29-t03`). With deterministic async the section runs on the emulation thread and the guest sleeps the emulated delay.
  - Console identity is per root. CFG writes a random MAC (`GenerateRandomMAC`, also in the guest-readable shared page) and console unique ID (`GenerateConsoleUniqueId`) into `sysdata`/`nand` on first load (`runs/div29-noident-vk`: cpu0 shared page and CFG bytes differ; t03's screens did not). This is a boundary, not a bug: identical behaviour needs the same `sysdata`/`nand`. `bench.py` and `divergence.py --same-identity` copy one template root's.
  - Two uninitialized members that only show in savestates: `Kernel::Mutex::priority`/`lock_count` (`runs/div-04`, services+9269) and `IR::ExtraHID::hid_period` (`runs/div-05`, after `IR:ReceiveEvent`). Both are now value-initialized on both branches.
  With all three addressed, 4 runs stay identical through 600 frames on software (`runs/div29-both-sw`), Vulkan (2 repeats, `runs/div29-both-vk`) and Vulkan with 4 runs stepping concurrently next to 4 unthrottled workers (`runs/div29-both-vk-contention`).
  Owned regression: the filesystem fixture's `spin` mode starts a lowest-priority counting thread next to `churn` writes. With `--host-async` the runs differ at frame 2 in guest ticks, cpu0, timing, memory and kernel (`runs/div29-spin-host`, exit 3); with deterministic async they are identical through 120 frames (`runs/div29-spin-deterministic`), and with the worker default under contention through 300 frames, 2 repeats (`runs/div29-spin-default-contention`). Without `spin`, churn alone stays identical with host async (`runs/div29-churn-host`): its only thread blocks in the IPC, so the core idles while the host write finishes.
  Cost, unthrottled, 5 trials each, CITnLO (dev host, not acceptance timing; `runs/bench-async29-{churn,t03}`): churn software 0.177 s against 0.178 s for 300 frames, Vulkan 0.063 against 0.068 s (spreads 0.060–0.073 and 0.059–0.091 overlap); t03 software 50.8 against 51.9 s (spreads overlap), Vulkan 0.150 against 0.152 s. The guest result differs, as intended: in the same 360 frames the churn guest completed 1473 writes with host async and 1479 with deterministic async, because host async adds the host's completion time to the emulated delay.
  AZAHAR built with both value-initialization fixes (`794b84ca3`), t03 Vulkan, 5 trials (`runs/bench-async29-integration-794b`; before: `runs/bench-async29-integration`, `b4b9f0c`): deterministic async ends on 1 screen out of 5, as before; host async on 3 (before 4). Guest ticks were identical within each config both times, so the host-async spread is in what the guest drew, consistent with the async wake timing above.
  Decision: CITnLO's `DefaultProfile` turns `deterministic_async_operations` on; `settings_set` before `load` turns it off for a session (it is a boot setting). AZAHAR keeps the upstream default under ADR-010. **Proposed ADR-012** (owner decision): make it the default for both workers, so AZAHAR control runs in matrix comparisons are repeatable too; the cost above is the evidence. Not covered: games whose FS or AM operations take long on the host (large reads, CIA installs), where deterministic async stalls the emulation thread for the host duration instead of overlapping it.

### #22 Parallel worker throughput under matched settings

On Windows 11 with a Ryzen 5 3600, 40 GiB RAM and Radeon RX 9060 XT
(32.0.32029.2010), the release MCP host ran t03 on CITnLO `a3994c1` and
AZAHAR `5ecbd98`. Both workers were WinLibs UCRT GNU Release builds with
Vulkan and software enabled and Qt, SDL2, OpenGL, Libretro and LTO disabled.
Settings were Old 3DS, deterministic async HLE, fixed guest clock, no speed
limiter and fresh shader storage. Each count had one six-second window after
three seconds of warm-up. The harness measured 2.4% host CPU load before the
sweep and 7.0% after (`runs/bp-matched-goal-01/report.json`, verified emulator).

| Running workers | CITnLO aggregate fps | AZAHAR aggregate fps | Slowest CITnLO / AZAHAR worker fps |
| --- | ---: | ---: | ---: |
| 1 | 1834 | 1865 | 1834 / 1865 |
| 2 | 3458 | 3565 | 1714 / 1765 |
| 4 | 5204 | 5393 | 1299 / 1328 |
| 8 | 7790 | 6777 | 953 / 835 |
| 16 | 9703 | 8932 | 382 / 481 |

The direct `bench.py` reference gave 1946 and 1812 fps at one worker. It
advances a fixed 300 guest frames without the supervisor's free-running
playback and probes. At four workers, the highest register RPC p95 across
workers was 1.1 ms on CITnLO and 0.9 ms on AZAHAR. At 16 workers it was 17.3
and 7.6 ms. The slowest workers delivered substantially less throughput at
16 even though total throughput increased. All ten trials shut down cleanly,
in at most 1.55 s. These single-window results describe scaling on this host.

`bench_parallel.py` retains raw command, input, optional capture and status
RPC times. It validates the actual worker identity and requested profile
against the live session manifest, and reports an invalid trial if a
running session stops advancing, leaves playback, changes epoch or reports
failed probes. It checks that the worker accepted the capture mode, rewind
options and event categories before timing them. The direct reference gets
the same fixed clock, HLE mode and speed limit at boot. Windows process I/O
includes IPC and device traffic. Poll and RPC latency are host measurements.
Per-frame p95 and renderer-wide GPU wait time are unavailable from this
interface.

The Windows two-worker CI smoke exercises the screenshot resource-link
response. The separate stall smoke suspends one of its two mock workers.
Native checks also passed with t03 and the owned debugger fixture running
side by side on New 3DS software (`runs/bp-software-mixed-goal-01`) and
with four Vulkan CITnLO workers capturing screenshots while one ran rewind
and three ran SVC/IPC tracing (`runs/bp-contention-goal-02`). The latter
completed all 20 probe rounds, recorded 50 checkpoints and reported a
register RPC p95 of 83 ms on the rewind worker and 0.94–1.25 ms on its
siblings. Every worker shut down cleanly (verified emulator).

### #24 PICA event history versus draw snapshots

The same Windows host ran CITnLO `a3994c1` with t03, Old 3DS, Vulkan,
deterministic async HLE and no limiter. Each cell is aggregate guest fps in
one four-second window after one second of warm-up
(`runs/bp-graphics-goal-03/report.json`, verified emulator).

| Capture configuration | 1 worker | 2 workers | 4 workers |
| --- | ---: | ---: | ---: |
| Off | 1821 | 3436 | 5111 |
| Batched counters | 1851 | 3590 | 5532 |
| Draw metadata, PICA history enabled | 90 | 197 | 389 |
| Draw metadata, PICA history disabled | 1441 | 2739 | 4691 |
| Eight snapshots per worker, PICA history enabled | 100 | 193 | 383 |
| Full snapshots, PICA history enabled | 127 | 172 | 391 |
| Full snapshots, PICA history disabled | 1008 | 2052 | 3333 |

With four workers, metadata-only capture with PICA history evicted 4.21
million events from the bounded event history and produced zero snapshots.
Disabling PICA history kept the per-command path and draw metadata active
and recorded zero event-history evictions. The eight-snapshot mode took 32
snapshots total, then counted 47,148 budget skips. The sweep retrieved no
draw artifacts. The `gpu_events_no_pica_history` and
`gpu_full_no_pica_history` modes leave the other history categories enabled.
The throughput change points to event construction and retention as the
first place to reduce steady-state overhead (#33). The no-history
comparison still shows a cost for full snapshots (#24).

`--first-config pica_events --gpu-metrics` ran one full-capture worker with
three passive workers (`runs/bp-graphics-mixed-goal-02/report.json`,
verified emulator). The capture worker advanced 95 fps and the passive
workers advanced 1,354–1,403 fps with register RPC p95 at or below 0.7 ms.
The report keeps command latency, CPU time, I/O
bytes, and losses per worker. Draw-export serialization, flush wait time
and bytes copied per draw still need separate measurements.

### #4 Matched PICA command path

`tools/pica_path_acceptance.py` with `--integration <AZAHAR worker>` compares
capture off, on and off again in CITnLO with matching windows in two fresh
processes (CITnLO capture always off and AZAHAR). The Windows t03 run used
CITnLO `a3994c1` and AZAHAR `5ecbd98`, WinLibs UCRT GNU Release, deterministic
async HLE, a fixed clock and no speed limiter.

On both renderers, `fast_path_commands` rose from 25,148 to 38,522 while
passive, stayed at 38,522 while capture recorded eight draws and rose to
51,272 after disabling capture. The three processes ended at 672,177,568
guest ticks and both screen SHA-256 hashes matched within each renderer.

| Renderer | AZAHAR, capture window | CITnLO, always passive, same window | CITnLO, capture enabled |
| --- | ---: | ---: | ---: |
| Software | 5191 ms | 5192 ms | 5763 ms |
| Vulkan | 14.4 ms | 15.2 ms | 293.2 ms |

Times cover one 30-frame window per mode, including three control round
trips (`runs/r04-matched/report.json`, verified emulator). They describe
this fixture and run.

### #6 Visible desktop playback

The 0.1.0 Windows release frontends and CITnLO `c3422b0a` ran the same Old
3DS t03 fixture unthrottled with a fixed guest clock. On Windows 11, a Ryzen
5 3600 and a Radeon RX 9060 XT (driver 32.0.32029.2010), each combination ran
three 20-second trials after five seconds of warm-up. Headless and visible
desktop alternated order. CPU load was 2% before and 3% after
(`runs/r06-frontends-quiet-01/report.json`, verified emulator).

| Renderer | Headless guest fps | Visible desktop guest fps | Mean ms per guest frame, headless / desktop | Desktop fresh previews/s |
| --- | ---: | ---: | ---: | ---: |
| Vulkan | 1819.2 | 1731.1 | 0.550 / 0.578 | 25.1 |
| Software | 5.9 | 5.6 | 169.5 / 178.6 | 4.5 |

These are trial medians. Frame time is wall time divided by guest frames
advanced during each trial. The Vulkan desktop recorded 21, 17 and 20 GPU
waits, one for each request that required a blocking readback in those
windows. The host continually issues 600-frame runs and the worker can
service a preview while paused at a batch boundary. Lotus now asks for
nonblocking previews when playback is set to continue, including across that
boundary. A manual pause still requests an exact preview.

`tools/preview_acceptance.py --require-nonblocking` reproduced the added
Vulkan wait at a bounded run stop on the packaged worker
(`runs/r06-native-before.log`). On CITnLO `a3994c1` and AZAHAR `5ecbd98`
the wait count stayed unchanged at that stop and during 120 running
previews. The next exact preview matched the capture bytes. Capture files
were untouched during live playback, lag peaked at one guest frame on
CITnLO and two on AZAHAR, and neither worker returned a preview from a
previous epoch (`runs/r06-native-citnlo`, `runs/r06-native-azahar`, verified
emulator). Both native builds used the WinLibs UCRT GNU Release configuration
with Vulkan and software enabled and Qt, SDL2, OpenGL, Libretro and LTO disabled.

With the rebuilt release frontends and CITnLO `a3994c1`, the same three
Vulkan trials recorded zero desktop GPU waits and a median of 25.4 fresh
previews per second. Guest throughput was 1849.5 fps headless (0.541 ms per
guest frame) and 1749.6 fps on the visible desktop (0.572 ms per guest frame).
CPU load was 6% before and 3% after. This measures the desktop cost on the
candidate build. The package and candidate use different host and worker
revisions, so their throughput difference cannot isolate the cost of the wait
change (`runs/r06-frontends-candidate/report.json`, verified emulator).

### #25 Concurrent Vulkan previews

`tools/bench_parallel.py` measures per-worker preview requests, reused frames,
readbacks, GPU waits and copied bytes. The desktop trial now waits for a
visible window, closes it before closing stdio and checks the process exit
code. Closing only stdio had hit the tool's 20-second kill deadline.

Windows 11, Ryzen 5 3600, Radeon RX 9060 XT (32.0.32029.2010), CITnLO
`a3994c1` (WinLibs UCRT GNU Release, Vulkan and software enabled), t03, Old
3DS, unthrottled. Each row is one 12-second sample after a three-second
warm-up. Host CPU load was 1% before and 3% after.

| Workers | Aggregate guest fps, headless / desktop | Highest register RPC p95 across workers, headless / desktop | Fresh previews/s | Live preview GPU waits |
| --- | ---: | ---: | ---: | ---: |
| 1 | 1830 / 1648 | 0.8 / 1.2 ms | 24.6 | 0 |
| 2 | 3479 / 3244 | 0.8 / 1.2 ms | 25.1 | 0 |
| 4 | 5461 / 4972 | 1.1 / 1.4 ms | 25.4 | 0 |

The headless and unselected workers received zero preview requests. All
six frontends exited cleanly in 0.15–0.52 seconds. The desktop trials
reused 21, 14 and 10 previous frames and reported zero busy-slot skips
(`runs/r25-preview-sweep-clean/report.json`, verified emulator). A forced
GPU delay and a saturated staging ring still need qualification.

### #32 Async rewind compression

CITnLO `0ce7886` serializes at the existing emulation-thread safe point and
passes the immutable result to one zstd thread per rewind worker. A reverse
operation can restore a serialized checkpoint directly while compression is
pending. `rewind_configure.compression_queue` defaults to 1, accepts 0..4, and
uses the synchronous path at 0. A full queue waits before allocating another
serialized buffer. `676e395` drains old work on reset and reconfigure so
switching to synchronous compression cannot allocate beside the old queue.
See `src/ctr_worker/checkpoint.h` and `src/ctr_worker/debug.h` in the CITnLO fork.

Verified emulator on Windows 11, Ryzen 5 3600 and Radeon RX 9060 XT
(32.0.32029.2010). The sweep used unthrottled Old 3DS t03, Vulkan, a
three-second warm-up, one eight-second window per row, and the batch policy
through `lotus-mcp`. Worker `0ce7886` was built with WinLibs UCRT GNU in
Release, Vulkan and software enabled, Qt, SDL2, OpenGL, Libretro and LTO
disabled. Host CPU load was 1.7% before and 4.7% after the sweep.
See `runs/r32-sweep-02/report.json` for per-worker checkpoint timings,
CPU time, probe latency, memory and binary hashes.

| Workers | Off, aggregate fps | Sync, aggregate fps | Queue 1, aggregate fps | Queue 2, aggregate fps |
| --- | ---: | ---: | ---: | ---: |
| 1 | 1845 | 37.9 | 43.1 | 72.1 |
| 2 | 3342 | 64.7 | 65.9 | 106.0 |
| 4 | 5588 | 80.7 | 76.7 | 81.3 |
| 8 | 7607 | 107.8 | 95.5 | 125.7 |

At four workers, mean emulation-thread checkpoint cost was 146, 142 and
129 ms for sync, queue 1 and queue 2. Mean serialization cost rose from
72 ms on sync to 109 ms on queue 2, and aggregate worker CPU rose from
31.7 to 56.2 CPU seconds over the window. An earlier 20-second four-worker
run measured 81, 77 and 72 fps (`runs/bp-rewind32-01`), so the results do
not establish a four-worker speedup. The sweep saw only capacity evictions
and no probe errors or compression failures.

At four workers, queue 1 used 571 MiB of serialized payload at the sum of
the per-worker peaks. Queue 2 used 1141 MiB. With the configured 64 MiB
history allowance per worker, those payload sizes give about 827 and
1397 MiB of queued and retained data respectively for this title.
These figures exclude allocator capacity, zstd scratch, guest memory
and renderer allocations. The four-worker sums of process peak private
commit were 4.92 GiB with rewind off, 7.52 GiB sync, 8.14 GiB queue 1,
and 9.53 GiB queue 2. Individual lifetime peaks can occur at different
times. Serialized sizes vary by guest and 3DS model. The interactive
host policy defaults to two rewind sessions and a 1536 MiB admission
estimate for queued checkpoints and stored history. See [host policy](23-local-mcp.md).
Separate allocator, page-walk and GPU-wait timings remain open.

The Windows MCP test on CITnLO `a3994c1` admitted four workers at 384 MiB
each, refused a queue expansion that would have raised the estimate to
1856 MiB, and accepted it at 1472 MiB after a worker disabled rewind
(`runs/r32-rewind-admission-01`, verified emulator).

The fixed `bench.py` restore probe takes one forward instruction step after
the timed frame window to create a core-specific checkpoint. For t03 Vulkan
at interval 5, median host time for 40 frames was 702, 637 and 441 ms at queue
depths 0, 1 and 2. The separately timed reverse step took 425, 384 and 382 ms.
Both screen hashes matched across all six trials at guest tick 313,686,688
before the instruction probe (`runs/r32-pinned-restore-cost-02`, verified
emulator). These short single-worker trials used the pinned Windows
CITnLO build and the same host as the sweep above.

`tools/reverse_acceptance.py` passed intervals 1, 5 and 20 at depths 0, 1
and 2 on both models, including watchpoints, breakpoint changes mid-window
and replay after input changes (`runs/r32-final-int*-q*`, worker `676e395`).
Reconfiguring from depth 2 to depth 0 with work pending cleared both the
pending and peak byte counts. The same test left 142 MiB pending on
`0ce7886` (`runs/r32-reset-before`). On the earlier `0ce7886` matrix
(`runs/rev32-int*-q*-01`),
mean restore time across intervals was 193–199 ms for Old 3DS and
256–274 ms for New 3DS, with up to three direct serialized restores per run.

`tools/rewind_failure_acceptance.py` terminated one of four Vulkan workers.
Lotus reported the failure and each sibling advanced 15 frames and took two
more checkpoints after Lotus observed the exit. All three answered register
requests and returned both screen artifacts (`runs/r32-failure-final-02`).
With a four-session rewind limit, the same test on CITnLO `9e6d051`
verified that each survivor took another five checkpoints and returned both
screen artifacts. A replacement started recording in the released slot
while the failed session and its report remained visible
(`runs/r32-crashed-budget-live`, verified emulator). The host process test
`crashed_worker_releases_tool_budgets_before_unregistering` reproduced the
stale rewind and GPU-capture slots before the fix
(`runs/r32-crashed-budget-before.log`, verified compiled).

Debugger and save-state round trips passed on Old and New 3DS
(`runs/r32-debugger-676e395`, `runs/r32-format-roundtrip-676e395`).
The prior format comparison used matching source IDs and compared registers,
fixture memory and screen hashes against the pinned worker
(`runs/savestate-fmt-32-01`). Loading a checkpoint from `c3422b0a` with
`676e395` was rejected by the source pin check before deserialization
(`runs/r32-format-final`). The 0.1.0 CITnLO package uses `c3422b0a`.
Queue depth configuration requires the newer experimental worker.
The updated host refused an explicit depth on `c3422b0a` and ran it on
the worker that advertised support (`runs/r32-capability-live`). The reverse
matrix, debugger and save-state round trips passed again on `9e6d051`
(`runs/r32-citnlo-9e6d051`).

### #46 Packaged Vulkan worker scaling

Verified emulator on Windows 11 with the Ryzen 5 3600 and Radeon RX 9060 XT
(driver 32.0.32029.2010). The Lotus 0.1.0 package ran unthrottled Old 3DS t03
through the batch MCP host with rewind off, a five-second warm-up and one
20-second sample per row. The package's worker build manifests record
CITnLO `c3422b0a` and AZAHAR `b04d8a6`. Both are WinLibs GNU Release
builds with Vulkan and software enabled, Qt, SDL2, OpenGL, Libretro and LTO
disabled. Host CPU load was 3.6% before and 7.7% after the sweep.

| Workers | CITnLO aggregate fps | AZAHAR aggregate fps |
| --- | ---: | ---: |
| 1 | 1817 | 1837 |
| 2 | 3484 | 3435 |
| 4 | 5475 | 5603 |
| 8 | 7721 | 7332 |
| 16 | 9194 | 9098 |

All ten trials used Vulkan and had zero command, input or allocation
failures. Each worker reached 32 MiB of device-local VMA blocks. The
packaged workers reported 15,301 MiB of budget headroom at every count.
See `runs/r46-packaged-sweep-02/report.json` for binary hashes and per-session
throughput, latency and memory.

The same Radeon driver kept reporting 15,301 MiB of Vulkan headroom while an
independent D3D12 process held 2 GiB of dedicated allocations. Querying
`VK_EXT_memory_budget` directly gave the same value as the packaged worker's
VMA cache (`runs/r46-pressure-live-01`, `runs/r46-pressure-live-02`).
CITnLO `9e6d051` also samples WDDM's `GPU Adapter Memory` dedicated usage on
the Vulkan adapter's LUID, at most twice a second per worker. Lotus uses the
smaller of that estimate and Vulkan's. The counter is optional. See
[host admission](23-local-mcp.md).

With four existing Vulkan sessions, the 2 GiB load reduced estimated headroom
from 15,074 to 13,000 MiB, below the test reserve of 14,700 MiB. Lotus refused a
new explicit Vulkan session and started a Vulkan-preferred session on software
before guest load in a separate run. The four existing sessions accepted input,
advanced frames and returned both screen artifacts in both runs. New Vulkan
sessions were admitted again after the load was released
(`runs/r46-pressure-live-03`, repeated with the committed harness in
`runs/r46-pressure-citnlo`, verified emulator).

On the same worker, allocation fault injection produced typed out-of-memory
reports at first allocation and at frame 64 while creating a present image.
Three other Vulkan sessions kept advancing and capturing
(`runs/r46-injection-citnlo`). A host regression test reproduced stale
admission pressure from the exited worker's last sample. Admission now uses
active workers only, and session status retains the failure report.

AZAHAR `7148e13`, built separately from `b04d8a6` with the package's
flags, passed both admission cases on the same machine. Its reported headroom
went from 15,078 to 13,004 MiB under load and cleared pressure after release.
First-allocation and mid-scene injection produced typed failures. The three
siblings advanced and returned both screens
(`runs/r46-pressure-azahar`, `runs/r46-injection-azahar`).
Both candidate workers completed 120 running Vulkan previews with zero
GPU waits and a maximum lag of one guest frame
(`runs/r46-preview-citnlo`, `runs/r46-preview-azahar`, verified emulator).

The regression uses the Windows SDK/WinLibs and the same t03 fixture.
Choose a reserve between the test adapter's idle and loaded headroom.

```powershell
g++ -std=c++20 -O2 -static tools/wddm_pressure.cpp -o runs/wddm-pressure.exe -ld3d12 -ldxgi -ldxguid
python tools/vulkan_pressure_acceptance.py --server <lotus-mcp.exe> --worker <ctr-worker.exe> `
  --allocator runs/wddm-pressure.exe --rom C:\abs\t03.3dsx --reserve-mib 14700 --output runs/r46-pressure
```

## Quiet-machine timing runs

Use a Release build ([27](27-build-and-run.md) §4) on an otherwise idle host; `--require-idle` refuses to start above 15 % CPU load. ROM paths must be absolute. `<w>` is `build/native-<branch>/bin/Release/ctr-worker.exe`, or `runtimes/<backend>/ctr-worker.exe` in an unpacked portable ZIP.

```powershell
# Suite (#10): stock software, both native workers, and headless vs visible desktop
./scripts/build-azahar.ps1 -Branch stock -Oracle -AzaharRoot C:/abs/pinned-stock -BuildDirectory build/native-stock
python tools/bench_suite.py --stock-oracle build/native-stock/lotus-stock-oracle.exe `
  --integration <integration w> --citnlo <experimental w> --mcp target/release/lotus-mcp.exe `
  --desktop target/release/lotus-desktop.exe --rom C:\abs\t03.3dsx --output runs/suite-quiet-01 --require-idle
# Backends, PICA events and rewind (#4, #9, #10)
python tools/bench.py --worker azahar=<integration w> --worker citnlo=<experimental w> --rom C:\abs\t03.3dsx `
  --output runs/bench-quiet-01 --renderer vulkan --renderer software --config off --config pica_events `
  --config rewind_frames --config rewind_interval_5 --config rewind_interval_1_fast --config rewind_interval_5_fast --require-idle
# Rewind cost before and after the run-length page table (#9): v0.0.1 against v0.0.3 CITnLO
python tools/bench.py --worker v001=<v0.0.1 citnlo w> --worker v003=<v0.0.3 citnlo w> --rom C:\abs\t03.3dsx `
  --output runs/bench-rewind-ab-01 --renderer vulkan --renderer software --config off --config rewind_frames --require-idle
python tools/reverse_acceptance.py <v0.0.3 citnlo w> C:\abs\debugger.3dsx runs/rev-v003-01
# GPU capture levels (#24); add --worker v003=<v0.0.3 citnlo w> for the before figure
python tools/bench.py --worker citnlo=<experimental w> --rom C:\abs\t03.3dsx --output runs/bench-gpu-01 `
  --renderer vulkan --renderer software --config off --config gpu_counters --config gpu_events `
  --config gpu_draws_8 --config pica_events --require-idle
# Parallel sessions (#22): CITnLO and AZAHAR, 1-16 sessions, with the bench.py reference
python tools/bench_parallel.py --server target/release/lotus-mcp.exe --worker citnlo=<experimental w> `
  --worker azahar=<integration w> --rom t03=C:\abs\t03.3dsx --output runs/bp-quiet-01 --workers 1 --workers 2 `
  --workers 4 --workers 8 --workers 16 --reference --require-idle
# Interference: debug configs, paused sessions, one stalled worker, screenshots, focus priority
python tools/bench_parallel.py --server target/release/lotus-mcp.exe --worker citnlo=<experimental w> `
  --rom t03=C:\abs\t03.3dsx --output runs/bp-quiet-02 --workers 4 --config off --config pica_events `
  --config gpu_draws_8 --config rewind_interval_5 --config bp_execute_8 --config watch_write_1 --require-idle
python tools/bench_parallel.py ... --workers 4 --idle 4 --output runs/bp-quiet-03 --require-idle
python tools/bench_parallel.py ... --workers 4 --stall --capture --output runs/bp-quiet-04 --require-idle
python tools/bench_parallel.py ... --workers 4 --focus-priority --output runs/bp-quiet-05 --require-idle
# Event delivery under a guest_log flood (#26): slow reader and 1/4/8 sessions, against v0.0.3
python tools/event_flood.py --worker citnlo=<experimental w> --worker v003=<v0.0.3 citnlo w> `
  --rom C:\abs\debugger.3dsx --output runs/flood-quiet-01 --require-idle
# Deterministic async HLE cost (#29): t03 and the churn fixture, both modes, unthrottled
python tools/bench.py --worker citnlo=<experimental w> --rom C:\abs\t03.3dsx --output runs/bench-async-quiet-01 `
  --renderer vulkan --renderer software --config host_async --config deterministic_async --frame-limit 0 --require-idle
python tools/bench.py --worker citnlo=<experimental w> --rom C:\abs\filesystem.3dsx --output runs/bench-async-quiet-02 `
  --renderer vulkan --renderer software --config host_async --config deterministic_async --frame-limit 0 `
  --poke churn=1 --read generation_done --require-idle
# Headless against visible desktop, unthrottled (#6, #10)
python tools/bench_frontends.py --mcp target/release/lotus-mcp.exe --desktop target/release/lotus-desktop.exe `
  --worker <experimental w> --rom C:\abs\t03.3dsx --output runs/frontends-quiet-01 --require-idle
# LTO (#11), about two clean native builds
./scripts/lto-ab.ps1 -Branch experimental -AzaharRoot C:/abs/pinned-experimental -Rom C:/abs/t03.3dsx -Output runs/lto-ab-01 -RequireIdle
```

`bench_suite.py` combines cold native software and Vulkan trials, warm Vulkan trials, and paired
software and Vulkan frontend trials in one `report.json` and `summary.md`. It uses unthrottled
playback and deterministic HLE async scheduling in all phases. The stock oracle runs the unmodified
software core directly with a null audio sink. Its report lists observed profile differences and
compares ticks and pixels against the worker runs. The desktop phase reports preview readback waits
and wait time separately from frame throughput.

The Windows t03 smoke comparison used 20 warmup frames and 19 measured frames. Stock Azahar
matched the integration and experimental workers' guest ticks and both software framebuffer
hashes. The reported core settings matched apart from the audio sink. This smoke run establishes
functional parity on that fixture. The longer idle-host run and allocation counts are pending.

On WinLibs GCC 16.2.0 the original CITnLO Release worker linked in 4.396 s (87,111,364 bytes).
The IPO variant reached the final link, with IPO in the worker and Azahar libraries but outside
bundled Crypto++ and Dynarmic. GNU ld rejected Crypto++ link-once symbols. Enabling IPO for
Crypto++ removed those errors, but two Boost exception thunks still failed to link. A single
GCC LTO partition failed with an internal compiler error while reading `LTO_section_decls`.
These builds yielded no runtime comparison. The A/B script now preserves the failed phase's
log and report, and production builds retain `ENABLE_LTO=OFF`.

`reverse_acceptance` writes `checkpoint_serialize_ns`, `checkpoint_compress_ns`, `checkpoint_serialized_bytes`, `checkpoint_stored_bytes` and `restore_ns` with `checkpoints` and `restores` counts in the `rewind_status` objects of `acceptance.json`; divide for per-checkpoint figures. v0.0.1 workers do not report these counters, so the release comparison uses `bench.py` wall time. Build the debugger fixture with `python tools/build-debug-fixture.py C:\abs\debugger.3dsx`.
