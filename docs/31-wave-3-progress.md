# 31 — Wave 3 implementation evidence

Implementation checkpoint, 2026-11-16. Native source is pinned as integration
`5a3c702f0fb0291632deecbe32d91aedb235f819` and experimental
`6361ddc38fbe2e1396337064e825dd16bc911bbf`; earlier entries name the pin they
ran against through retained executable SHA256 values. Wave 3 is implemented;
open exceptions are listed under "Completion checkpoint".

Verified emulator checks (local reports in ignored `runs/`):

- `cpu-owned-01`: Old/New 3DS owned ARM fixture: exact main breakpoint,
  temporary run-to cleanup, saved-FP/LR backtrace, conditional write watchpoint
  during instruction stepping, save/load/replay register and memory equality,
  guest stdout, unmapped access, svcBreak and undefined-instruction reports.
- `gdb-owned-05`: actual ARM GNU GDB 16.3.90 through the Rust loopback server,
  Old/New 3DS: source breakpoints, DWARF backtrace, instruction steps, write
  watchpoint, HIO file-I/O stdout and return value, detach/lease handoff.
  ELF symbol and DWARF source-line queries also pass through the shared host.
  This is Arm GNU GDB, not yet the specifically requested devkitARM build.
- `wave3-{integration,experimental}-vulkan-60`: 60-frame Old/New boot/capture,
  shutdown and matching development-branch Vulkan screen hashes.

Reproduce the owned CPU checks:

```powershell
python tools/build-debug-fixture.py build/fixtures/debugger
python tools/debugger_acceptance.py <worker> build/fixtures/debugger/debugger.3dsx <fresh-output>
python tools/gdb_acceptance.py <arm-gdb> target/debug/ctr-cli.exe <worker> build/fixtures/debugger/debugger.3dsx <fresh-output>
```

The fixture builder accepts `--gcc <devkitARM-arm-none-eabi-gcc>`; its default
is Clang targeting ARMv6K. Disassembly requires `llvm-objdump` on the host PATH.
No firmware or third-party content is bundled.

Further **verified emulator** evidence:

- `mcp-debugger-03`: MCP-only Old/New symbols, lease arbitration, unmapped
  access, overwrite watchpoint, svcBreak, undefined instruction, HIO output,
  deadlock graph, model-specific lost update, relaunch and EOF cleanup.
  Fixes issue #1 (worker lifecycle); issue #2 removes the misleading successful
  3DSX title-ID error and is checked against actual worker logs.
- `mcp-workflows-04`: both models pass predicate stops, instruction checkpoint
  reversal, reverse watchpoint continuation and two-child memory isolation.
  Forked guest filesystem writes: see `mcp-fs-fork-01` below.
- `gpu-acceptance-01`: Old/New, software/Vulkan: actual pre-draw state,
  eight vertex inputs, bounded shader interpreter trace and texture surface.
- `audio-audible-02`: three HLE guest DSP captures per model, 131200 PCM bytes
  each, SHA256 `7c8a00e8f5ed50b0862cc7cc473ca6b2a84a3ff247a92ee39d5a0206c46fda09`.
  Focused Cubeb stream startup/teardown passes; human listening was not tested.
- `wave3-rust-tests-02.log`: protocol, session, debugger, GDB and MCP unit tests
  pass. Generated command parity is enforced by protocol and MCP schema tests.

The open-work list that stood here is resolved in "Completion checkpoint" at the
end of this document; what remains open is listed there.


CiTrace **verified emulator**: `citrace-acceptance-08` covers Old/New 3DS on
software/Vulkan, two recorded frames, two byte-identical replays, modified-file
rejection, blocked CPU resume in GPU replay view, then successful checkpoint
restore and forward execution. The recorder is capped at 262144 elements and
48 MiB of unique data, with a 64 MiB reader limit and explicit loss counts.
Recording is expensive and opt-in: two frames measured 1.01/0.74 seconds on
Old software/Vulkan and 1.65/1.30 seconds on New software/Vulkan, versus
0.36/0.03 and 0.32/0.03 seconds without recording in that local run. These
host timings are not guest cycles or a controlled general benchmark.

Replay requires the paired session checkpoint because CiTrace v1 omits initial
LUT and geometry state. It currently replays the latest recording on that card;
standalone import of arbitrary legacy traces is not implemented. GPU replay
holds CPU execution; restore a named state or reset before executing CPU code.
The native commits also bind states/movies to ROM SHA256, preventing collisions
between different homebrew ROMs whose title IDs are all zero.

`wave3-clippy-05.log`: workspace/all-targets Clippy passes with warnings denied.
`wave3-workspace-tests-01.log`: workspace tests pass; native tests marked ignored
remain unverified by that command. Pinned native and MCP checks are separate.


`mcp-replay-02` (**verified emulator**, development executable SHA in retained
manifests): Old/New movie recording plus three identical replays at frames
20/40/60, rejected live playback input, 2x captures, and two identical fresh-root
matrix profiles with three repeats each. Matrix checkpoints compare screen hashes
and CPU registers. It currently drives frame checkpoints without scenario input
scripts; scenario/movie integration and deliberately divergent profiles remain open.


Pinned builds for both native branches pass (`wave3-pinned-*-build-02.log`).
`state-identity-01` checks both models: reject a state from a different owned
3DSX ROM without changing registers, restore the original ROM/state, reject a
modified archive without changing registers. `mcp-debugger-04` repeats the
complete owned CPU/kernel/lease/lifecycle suite against the updated pin.


GPU lineage (`gpu-lineage-06`, **verified emulator**): all four Old/New ×
software/Vulkan cases freeze actual vertex buffer spans, compare shader-trace
outputs against actual vertex outputs, preserve unchanged texture versions and
change both content hash/version after a CPU overwrite. Resource snapshots also
include validated virtual aliases. Version tracking is bounded to 128 entries
with an eviction counter; draw captures stay bounded to eight draws and 4 MiB
of resource bytes per draw.

Desktop controller/model coverage is recorded in [24](24-desktop.md). New
inspector visuals remain unverified because computer-use access was rejected.
`mcp-replay-03` also passes read-only movie save/advance/restore at identical
full timeline lengths, on both models, followed by the repeated matrix checks.


## Completion checkpoint (2026-11-16, second session)

All local reports are in ignored `runs/`; tools and tests named here are
committed. Old and New 3DS unless stated. **Verified emulator** throughout; no
hardware accuracy claim.

- **Reverse continue** (`w3c-reverse-02`, `tools/reverse_acceptance.py`):
  continuous `run_until` with rewind; write-watchpoint values 2, 3, 4, then
  `reverse_continue` lands before the access (old value in memory,
  `access_completed: false`), forward re-hits it, and two further reversals
  step back through 3 and 2. Execute breakpoints land on the previous hit
  (counter one lower). Mechanism: stops are counted per rewind checkpoint;
  the checkpoint holding the previous hit is replayed with instruction
  stepping; a watchpoint is replayed a second time to its access instruction.
  A diverging replay restores the present state and reports failure. Changing
  breakpoints forgets hit counts; with none set the request is refused.
  `mcp-workflows-05`, `w3c-dbg-regress-01` and `w3c-gdb-02` still pass.
- **Settings apply classes** (T24, `w3c-settings-03`, both branches): 174
  bindings — 19 `live`, 67 `renderer`, 44 `boot`, 29 (28 on integration)
  `cfg-savedata`, 15 `inert`; each cites its consumer lines. Non-live classes
  are refused on a running session with the class named; `battery_*`,
  `steps_per_hour` and `audio_muted` apply live.
  Correction (#50): `use_vsync` was bound twice, once per arm of
  `#ifdef ANDROID` in `settings.h`. The generator now dedupes by name: 173
  bindings, 66 `renderer`.
- **GPU setting observables** (T37, same run, software and Vulkan): guest ticks
  at successive PICA draws shift with `simulate_3ds_gpu_timings` (second draw
  134,639,954 → 134,682,037) and `delay_game_render_thread_us` (first draw
  134,601,484 → ~136.1–136.2 M; the delay replies through `RunAsync`, so its
  landing point varies between runs). JIT and interpreter shader engines both
  match the reference shader trace. `shaders_accurate_mul` is consumed only by
  hardware shaders (`gpu.cpp:448`); no output difference is claimed.
  Observation: on Vulkan, early draws were once 96 ticks earlier in one of four
  identical runs before reconverging; software was exact. Vulkan checks
  therefore require shifts above measured baseline jitter.
- **Content confinement** (T31, `w3c-fs-01`, owned `fixtures/filesystem`): a
  guest SDMC write lands in `<root>/sdmc`; six `..`-climbing paths return
  `0xE0E046BE` (invalid path) and no file appears above the session root.
- **Forked filesystem isolation** (`mcp-fs-fork-01`): parent and two forks write
  different bytes to the same SDMC path; each root holds its own file and the
  pre-fork file is inherited by both forks.
- **Offscreen Vulkan** (T45, `w3c-vulkan-01`, integration): four workers in
  parallel, 60 frames each, both screens captured (hashes equal per model).
  `physical_device=99` and a loader with a missing ICD both return typed
  `unsupported` replies within 0.4 s, the worker keeps answering, exits 0, and
  a following software session works.
- **Audio focus** (T46, `mcp-focus-01`): focus moved A → B → A across sessions
  is exclusive at every step (now visible as `audio_focused` in status); the
  raw DSP captures of both switched sessions equal an unfocused reference
  (SHA256 `7c8a00e8…`). Host-device glitches are not measured.
- **JIT-sensitive matrix** (T41/T18, `mcp-matrix-div-01`, owned
  `fixtures/divergence`): JIT vs interpreter differ repeatably at the first
  checkpoint in guest ticks over a fixed loop (60,361 vs 90,368), NaN sign
  (`0x7fc00000` vs `0xffc00000`) and denormal flushing (0 vs `0x116c2`); two
  JIT profiles agree. The matrix labels these observations; which backend
  matches hardware is untested.
- **Desktop inspectors** (T17, `w3c-desktop-01`, checked by screenshot):
  CPU/disassembly, threads, breakpoints, memory, paged settings with classes,
  IPC (bounded, loss counter shown), logs, GPU draw, audio, snapshots and
  timeline panels render live CITnLO data. Two-DPI interaction remains
  unverified.
- **Instrumentation overhead** (T32, `w3c-overhead-02`, Old 3DS software,
  `tools/overhead_acceptance.py`): host-time medians of three fresh workers
  over 60 frames, relative to off (12.7 s): SVC/IPC trace 1.09×, PICA event
  capture 1.25×, rewind checkpoints every frame 2.47×. Guest ticks and both
  screen hashes are identical to the off run in every configuration. An armed
  execute breakpoint forces instruction stepping: one frame did not finish
  within the 20 s host deadline (≥ 82× the 0.24 s one-frame reference); this
  is a lower bound, not a measurement. Host time on this machine only.
- **Scenario movies and determinism profile** (T29, `w3c-movie-04`,
  `tests/test_scenario_movie.py`, user GPU homebrew): per model, three
  scenario recordings and three read-only playbacks with no scenario input
  produce identical screen checkpoints at frames 35/45/60; playback consumes
  all 60 recorded inputs. That content's screens do not change for D-pad, A or
  touch within 60 frames (an idle run is identical), so this shows replay
  determinism, not input sensitivity, which remains unshown for movies.
  Profile hashes differ per fresh root because CFG console
  IDs are generated per root; playback applies the recorded one.

Pinned regression (`w3p-*`, both pins above, 2026-11-16): reverse, owned CPU
debugger, GDB (Arm GNU GDB 16.3), settings classes on both branches (draw
observables on experimental), content confinement, scenario movies, and MCP
workflows/debugger/forked filesystem/audio focus/divergent matrix pass;
`scripts/verify-pins.ps1` passes.

Open exceptions: the devkitARM-specific GDB build was not run (its download was
not approved; Arm GNU GDB passes the same session); actual two-DPI interaction
and host audio-device glitches are unmeasured; Vulkan tick jitter is recorded
above; no hardware accuracy claim is made.

Update (issue #12, doc 34): devkitPro GDB 14.1 now passes the same session, including
Thumb checks, on CITnLO for Clang and devkitARM-GCC fixtures, Old and New 3DS.
