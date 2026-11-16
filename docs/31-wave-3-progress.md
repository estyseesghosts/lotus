# 31 — Wave 3 implementation evidence

Implementation checkpoint, 2026-11-16. Native source is committed as integration
`5d32df0a4b68f466df1a232b517927129b045554` and experimental
`f91c94ec5fb495acba171f194894bba6f00f6750`. Earlier development reports used
the original embedded SHA; retained executable SHA256 values distinguish them.
Pinned rebuilds are being verified. Wave 3 remains incomplete.

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
  Forked guest filesystem writes still need acceptance coverage.
- `gpu-acceptance-01`: Old/New, software/Vulkan: actual pre-draw state,
  eight vertex inputs, bounded shader interpreter trace and texture surface.
- `audio-audible-02`: three HLE guest DSP captures per model, 131200 PCM bytes
  each, SHA256 `7c8a00e8f5ed50b0862cc7cc473ca6b2a84a3ff247a92ee39d5a0206c46fda09`.
  Focused Cubeb stream startup/teardown passes; human listening was not tested.
- `wave3-rust-tests-02.log`: protocol, session, debugger, GDB and MCP unit tests
  pass. Generated command parity is enforced by protocol and MCP schema tests.

Open work: complete settings consumer/apply-class audit, LLE DSP and injected
mic guest readback, camera and other peripheral readback, content/movie/matrix
acceptance, continuous-history reverse execution, remaining GDB behavior,
CiTrace record/replay, GPU resource lineage, desktop inspectors and timeline,
scenario integration, pinned regression builds and instrumentation overhead.
No hardware accuracy claim is made. ARM GNU GDB coverage does not yet satisfy
acceptance specifically against the devkitARM-distributed build.
