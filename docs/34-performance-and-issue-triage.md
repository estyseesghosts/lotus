# 34. Performance and issue triage (issues #1–#18)

Status: draft. Measurements are from `tools/bench.py` on one Windows machine, t03 homebrew ROM, Old 3DS, 30 warm-up frames. Run directories: `runs/bench-vk-02` (Vulkan, 120 frames x 3), `runs/bench-sw-02` (software, 60 frames x 3), `runs/bench-preview-02` (capture every 2 frames). Workers: `azahar` = integration pin, `citnlo_pin` = CITnLO pin before this work, `citnlo_new` = CITnLO with the changes below, now pinned as experimental `c48dac6e` (`config/pins.toml`). The integration pin is unchanged.

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
- #4 implemented: PICA debug context installed only while GPU tools are enabled.
- #5 implemented: supervisor prefers Vulkan (`prefer_vulkan`, default true) and replaces the worker with a software one if Vulkan load is unsupported, recording `renderer_fallback`. `tests/test_renderer_default.py` passes on both backends.
- #6 partial: paused previews with unchanged (epoch, frames) skip capture. Shared-memory frame transfer and async Vulkan readback not done.
- #7 the worker loop used to build the JSON input snapshot on every `RunLoop` iteration, even with rewind off. A standalone harness reproduced the call shape (`SnapshotInput` in `src/ctr_worker/input.h`) with a counting `operator new`, built with the same nlohmann json and winlibs GCC -O2. It measured 75 heap allocations per call (verified compiled, not counted inside the emulator). `RunLoop` runs at least 4 times per frame (`MAX_SLICE_LENGTH = BASE_CLOCK_RATE_ARM11 / 234`, `core_timing.h:184`), so that is at least 300 allocations per frame. The snapshot is now built only when `WantsFrameCheckpoint` is true (`src/ctr_worker/debug.h:386`). That gives 0 allocations with rewind off and one snapshot per stored checkpoint with rewind on.
- #8 execute breakpoints stay in the JIT (`PreCodeReadHook` block split). Watchpoints and pc/memory predicates still single-step: Dynarmic's memory-abort path re-executes the instruction after writeback (`load_store.cpp` 61-62, `a32_emit_x64_memory.cpp` 253-255), so it cannot report a write watchpoint correctly.
- #9 checkpoint interval (`interval_frames`, 1..60). Input changes force a checkpoint. Testing intervals above 1 exposed a reverse-continue bug that predates this work. When the breakpoint set changed after the newest checkpoint, hits were counted against that older checkpoint. The replay then counted executions from before the change as hits and landed on the wrong one. Interval 20 landed about 430,000 counter values early (`runs/perf-rev-interval-01`). With interval 1 the same bug needed a mid-frame breakpoint change. Fix: `ForgetHits` now forces a checkpoint and leaves hits uncounted until one is stored. History entries also keep the resume-from-breakpoint skip state, because `RestoreHistory` used to clear it, so a checkpoint taken at a breakpoint PC no longer replays an extra hit. `tools/reverse_acceptance.py --interval-frames N` has an `armed_mid_window` check. It passes at N = 1, 5 and 20 on Old and New 3DS (`runs/perf-rev-int{1,5,20}-02`, verified emulator). `debugger_acceptance` and `gdb_acceptance` pass on the same worker (`runs/perf-dbg-02`, `runs/perf-gdb-02`).
- #10 `tools/bench.py` added.
- #11 `-Lto` switch in `scripts/build-azahar.ps1`. The LTO A/B was not run: an unrelated process held the host at about 80% CPU for the rest of the session, so timings taken then are not comparable.
- #12 devkitPro GDB 14.1 passes `gdb_acceptance` on CITnLO (Old/New 3DS, Clang and devkitARM GCC fixtures, Thumb checks). GDB server refuses to start on AZAHAR.
- #13–#17 (desktop layout): resizable, snap-collapsible panes with saved widths, an always-visible inspector rail with vector icons, grouped inspector navigation with per-view commands, and a toolbar and gauges that adapt to the centre width. Design and evidence in [30](30-sessions-and-interface.md#window).
- #18 opt-in Discord Rich Presence, verified against a running Discord client. Policy, transport and limits in [30](30-sessions-and-interface.md#discord-rich-presence).
