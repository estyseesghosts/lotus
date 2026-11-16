# 33 — Wave 3 retrospective and handoff

Session record, 2026-11-16: two agent sessions that completed Wave 3 after
the earlier implementation sessions (their results open [31](31-wave-3-progress.md)). Evidence lives in
[31](31-wave-3-progress.md); plan and milestones in [29](29-wave-3-plan.md);
build steps in [27](27-build-and-run.md). No hardware accuracy claim is made.

## Where the project stands

LOTUS is a Rust platform (GPUI desktop, `ctr-cli`, `lotus-mcp`, scenario
runner, session supervisor) driving one `ctr-worker` process per emulated
3DS. Workers are built from two branches of an Azahar fork: `integration`
(AZAHAR, minimal hooks) and `experimental` (CITnLO, debugger and tracing).
Wave 3 (comprehensive agentic debugger) is implemented and pinned:
integration `5a3c702f`, experimental `6361ddc3` (`config/pins.toml`). Wave 4
(accuracy research, [15](15-implementation-tickets.md)) has not started.

## What these sessions did

| Area | Result (**verified emulator** unless stated) |
| --- | --- |
| Reverse continue | Continuous reverse to the previous breakpoint/watchpoint hit (`tools/reverse_acceptance.py`) |
| Settings | 174 bindings classified live/renderer/boot/cfg-savedata/inert from consumer source lines (`tools/generate-settings.py`); non-live refused while running |
| GPU settings | GPU-timing and render-delay settings shift guest ticks at PICA draws; JIT and interpreter shaders match the reference trace |
| Filesystem | Guest SDMC writes confined to the session root; forks keep separate files (owned `fixtures/filesystem`) |
| Vulkan | Four offscreen workers in parallel; bad/missing device gives typed `unsupported` |
| Audio focus | Exclusive focus across sessions; guest audio unchanged by focus |
| Divergent matrix | JIT vs interpreter differ in ticks, NaN sign, denormals (owned `fixtures/divergence`) |
| Overhead A/B | Trace 1.09×, PICA events 1.25×, rewind 2.47×, armed breakpoint ≥ 82× (host time); guest identical |
| Movies | Scenario `movie` block and `profile.determinism`; 3 recordings + 3 playbacks identical per model |
| Desktop | Every inspector panel renders live data (**checked by screenshot**) |
| Pins | Both branches pinned; full regression and `verify-pins.ps1` pass ([31](31-wave-3-progress.md)) |
| Packaging | Portable ZIP rebuilt and tested with Wave 3 binaries (see [26](26-portable-release.md)) |

## What went well

- Small owned fixtures (filesystem, divergence, debugger) gave exact,
  repeatable checks without user assets.
- Classifying settings from the source lines that consume them made the apply
  classes auditable instead of hand-maintained.
- Rebuilding at the exact pins before the final regression exposed stale
  embedded source SHAs before they reached reports.
- Every acceptance tool is committed and rerunnable (`tools/*_acceptance.py`,
  `tests/test_mcp_*.py`, `tests/test_scenario_movie.py`).

## What went poorly and how it was fixed

| Problem | Cause | Fix |
| --- | --- | --- |
| Reverse continue with no breakpoint reported a misleading divergence | Stale per-checkpoint hit counts | `ForgetHits()` on breakpoint changes; empty set refused |
| Movie playback rejected as "profile or pin mismatch" | CFG console ID is random per fresh root | `movie_play` applies the recorded `cfg-savedata` values, with rollback |
| Movie test could not show input sensitivity | The user's GPU homebrew ignores D-pad/A/touch for 60 frames (checked by direct probe; Start ends it) | Reported, not asserted; T29 requires identical runs only. An unverified doc claim was removed |
| Armed-breakpoint timing hit the 20 s worker deadline | Breakpoints force instruction stepping | Timed one frame; recorded as a lower bound |
| Vulkan draw ticks once drifted 96 ticks | Host-variable timing on Vulkan | Checks require shifts above measured baseline jitter; recorded as a finding |
| Settings tool failed on integration | PICA draw stops exist only on experimental | `--classes-only` for AZAHAR workers |
| Divergence fixture failed to build | Empty `.data` for objcopy; VFP asm needs softfp | Initialized data; softfp/vfpv2 for that fixture only |
| `ctr-worker` embedded a stale source SHA | `CTR_SOURCE_SHA` is set at CMake configure time | Reconfigure (or use `build-azahar.ps1`) after native commits |
| Build manifests named old pins after direct `cmake --build` | Only `build-azahar.ps1` writes `lotus-build-manifest.json` | Final builds always through `build-azahar.ps1` |
| MCP tests failed to start the server | Windows `Popen` with relative paths | Pass absolute Windows paths (`cygpath -w`) |
| Bash heredocs with nested quotes failed | Shell quoting | Patch scripts written to the scratchpad |

## Lessons

- Verify that content reacts to an input before using it as an input test.
- Treat any host-deadline result as a bound, never a measurement.
- Rebuild through the pinned script before recording release evidence.
- State reported-but-unasserted observations as such in the test docstring.

## Open items for the next agent

1. **devkitARM GDB** (milestone 3.1 exit check): install devkitPro, then run
   `tools/gdb_acceptance.py` with its `arm-none-eabi-gdb`. Arm GNU GDB 16.3
   already passes the same session.
2. **Movie input sensitivity**: record a movie on owned content that draws
   differently per button, then assert `input_changes_output`.
3. Actual two-DPI desktop interaction, host audio-device glitches, other
   platforms: unverified.
4. Publishing a Wave 3 release needs a version bump in `Cargo.toml` (tags must
   equal `v<version>`; v0.0.1 is already published) and the owner's go-ahead.
5. Wave 4: [15](15-implementation-tickets.md), [11](11-accuracy-research.md),
   [12](12-hardware-validation.md).

Start by reading `AGENTS.md`, [index](index.md), this document and [27](27-build-and-run.md)
("Local developer loop" covers the sibling worktrees and acceptance tools).
