# 07 — CPU, kernel and memory debugger design

## Philosophy

A debugger should answer **what happened**, **which execution context produced it**, **what state was visible then**, and **how to reproduce it**. Many emulators expose registers and memory but not a consistent history of cause and effect. Build an *explicitly timestamped and scoped* observation model around Azahar's available internals.

## CPU execution inspection

Desktop inspector publication retains the request's SessionTarget (ID, creation
time, guest epoch). Worker reply epoch tags, including the memory-read epoch in
host-derived disassembly, must match before publishing. Target/generation and
current owned summaries are checked again under the desktop publication lock.
Cached registers, rows and surface images expire on a reported target/epoch
transition; imported local traces have no worker target. This metadata adds no
worker request or wire-envelope change. It does not make a multi-request CPU
view an atomic register/memory snapshot, or guard queued mutations at dispatch.

Initial backend: Azahar `ARM_Interface` / Dynarmic; interpreter stepping where appropriate. Source `src/core/core.h` provides `GetCore(core_id)`, `GetNumCores`, `GetRunningCore`; `RunLoop` operates across cores and schedules threads. Avoid treating `SingleStep` as a precise per-thread instruction without testing it.

Capabilities in order:
1. Core register snapshot: r0-r15, CPSR/SPSR where applicable, VFP state, execution mode, PC mapping, selected thread/process.
2. Disassembly for ARM and Thumb from a specified virtual address; show raw bytes and mode, not only mnemonic.
3. Software execute breakpoint with exact address/process/mode; map to JIT backend's invalidation/stop hooks. Watch for breakpoints in shared code and page remapping.
4. Pause reason: explicit breakpoint/exception/watchpoint/manual pause, including prior and next PC semantics.
5. Single stepping: selected core/thread; test precise retired-instruction count, other cores allowed to advance, pending interrupts, ARM/Thumb state and debug-patch behavior.
6. Call-stack unwinding: prefer symbols + CFI/frame metadata if available; otherwise conservative unwind with confidence indicators. Do not fabricate source line mappings.
7. Register, memory and disassembly diff across snapshots.

### Correct breakpoint semantics

A breakpoint needs `backend`, `process_id`, `core_id?`, `virtual_address`, `instruction_set`, `enabled`, `hit_count`, optional condition. Machine-code patches can alter the emulator's guest memory (and JIT cache); prefer native JIT break instrumentation when possible. If using patches, guarantee restoration and cache invalidation, including after reset/restore. Distinguish physical register/IO watchpoints from CPU virtual memory watchpoints.

## Memory browser

Address spaces:
- CPU virtual address scoped to process (including separate core privileges as needed).
- Physical FCRAM, VRAM and other mapped RAM, with region metadata.
- Memory-mapped I/O (device reads may be side-effectful): use safe register snapshots where possible, warn before read.
- GPU resource interpretations (decoded texture/render target) **not interchangeable** with CPU-visible bytes.

Operations: bounded hex/typed read, memory map, aligned search, change-watch, compare against captured memory, named virtual->physical mapping (including current process/permissions), memory-write with explicit side effects, snapshots, diff views.

Use versioned `MemoryRead` result `{space, process, epoch, guest_tick, consistency, bytes, hole_ranges, side_effect_warning}`. Avoid reading GPU-backed pages by simply obtaining a C++ pointer. Azahar has rasterizer flush/invalidation logic; **record whether inspection forced a cache sync** and whether guest execution was paused.

### Watchpoints

Hardware-style watchpoints are not free in an emulator. Begin with page/access instrumentation for selected ranges and precise host JIT hooks when available. Define allowed access types (read/write/execute), address-space mapping, maximum ranges and sampling policy. Unbounded watch ranges can produce orders-of-magnitude slowdown. Report overhead estimate and dropped/approximated events.

## Kernel and service introspection

Azahar HLE kernel provides process/thread/handle/service structures; expose **snapshots** and event hooks rather than raw pointers. A useful kernel panel:

- Process ID, title ID, code sections, memory mappings, active thread list.
- Thread ID, priority, assigned core, state (running/runnable/waiting/terminated), wait reason/object, last switch tick.
- Handle table: handle number -> emulated object kind/identity/rights as exposed by model.
- IPC: sender/receiver, service/command, raw request header/words, parsed parameters, response result, wait duration in guest ticks.
- Wait graph for deadlocks: thread -> event/mutex/session -> owner where known; distinguish known and inferred ownership.
- SVC trap trace (core/thread, number, args, return values) with optional bounded masks for high-frequency calls.

Service marshalling happens in C++ at the emulator's real HLE request boundaries. Derived service parser should be schema-driven and have fallback raw dumps when unrecognized; never discard unknown commands to make logs look tidy.

## Exception reporting

Capture program counter, instruction bytes, CPU registers, process/core/thread IDs, guest tick, exception type/vector, memory fault address if known, surrounding mapped memory, current kernel wait state, recent IPC and GPU events. Attach ELF build ID and DWARF/symbol file identity. A 3DS exception is not always a host crash; label guest and host failures separately.

## Symbols and mappings

For homebrew, load ELF/map file metadata from the user's build output. Cache symbol indexes keyed by binary hash and load mapping; support ASLR/mapping and relocated CRO modules when observable. For proprietary games, require user-provided symbols or show raw addresses; never use guessed source names as authoritative. Prefer `arm-none-eabi-addr2line`/Rust DWARF parsing on host for source mapping, without sending binaries to remote services.

### Session-bound ELF identity (lotus#76)

`load_symbols` binds an approved ELF to one session only after the guest proves it is the
loaded build. The [binding](../crates/session/src/debug_image.rs) reads every read-only
allocated section (`SHF_ALLOC` without `SHF_WRITE`) back from guest memory at its link
address, in 4 KiB worker reads, and refuses the ELF at the first differing byte. 3DSX code
loads at `PROCESS_IMAGE_VADDR` with relocations resolved against that base
(`vendor/azahar/src/core/loader/3dsx.cpp`, `Load3DSXFile`), so the load bias is always zero
and no bias is guessed. Writable sections are not compared because the guest changes them.
The binding names the single process whose `memory_map` regions cover every compared section;
none or several is a refusal. A failed `load_symbols` keeps the previous binding.

Each binding records the ELF asset and SHA-256, GNU build ID when present, the content file's
size and SHA-256 (hashed after each load or reset, up to 64 MiB), process, epoch and a
per-session generation. `symbolize`, `resolve_symbol`, `disassemble`, `backtrace` and
`crash_report` revalidate it under the native lock: a new epoch (load, reset, state load,
replay, reverse step) or a process that no longer maps the sections drops it, and later
requests report why. When source breakpoints exist, the host binds the same asset again
after the epoch change (below). Replies carry the evidence as `image`. Addresses outside the ELF's
allocated sections return no symbol or line; only executable sections return lines.

Not covered: relocated CRO modules stay raw addresses, sessions forked with `fork` start
unbound, and an external GDB client symbolizes with whatever ELF it loads itself. ELF
suggestion from a build manifest belongs to the build workflow (lotus#79).

### Source breakpoints and line stepping (lotus#77)

The [line table](../crates/debugger/src/lines.rs) keeps every DWARF 4/5 row with its
column, discriminator, `is_stmt` and `prologue_end` flags, and DWARF 5 MD5 file checksums.
Paths are joined exactly as addr2line 0.27.1 `path_push` joins them, so line-table and
`addr2line` paths compare equal. A stripped ELF refuses source requests instead of guessing.

`source_breakpoint_set` keeps `file:line` on the host and arms a worker execute breakpoint at
each site of that line in the bound ELF: each statement start, plus the entry of each
subroutine inlined from the line (`DW_AT_call_line`; the entry is `DW_AT_entry_pc`, else the
lowest range address, DWARF 5 2.18 and 3.3.8.3). GCC may emit no row for the call line
before the inlined body. One site is kept per function and inline scope, as gdb's
`linespec.c` keeps one location per block, so a line that clang resumes after an inlined call
stops once. Execute breakpoints stay on the JIT path: the worker single-steps only for
non-execute breakpoints or PC and memory predicates (`debug.h`, `NeedsInstructionStep`).

After an epoch change the host removes the old build's worker breakpoints, re-reads the ELF
asset, repeats the identity check above and re-arms at the new addresses. If the ELF no
longer matches the guest, every source breakpoint stays pending with the reason, and no old
address stays armed. The debug reply that changed the epoch reports the outcome as
`source_breakpoints`.

`source_step` (`into`, `over`, `out`) runs within `max_instructions`:

- A stop at the entry of inlined subroutines is shown at their call site, and `into` from it
  enters the inlined frame without executing, as gdb's `skip_inline_frames` and
  `step_into_inline_frame` do.
- `into` stops after the callee's prologue: the first `prologue_end` row, else the next
  statement of another line, as gdb's `skip_prologue_using_sal` does. Callees without line
  information run to their return, matching gdb's `step`. A GNU ld ARM/Thumb veneer of up
  to 8 instructions that leaves SP and LR unchanged is followed to its target.
- Calls are run to their return address and accepted only when SP equals the SP at the
  call, since `BL`/`BLX` do not change SP (ARM ARM A4.1.10, A4.1.12). Recursion therefore
  does not stop early.
- `out` of a concrete function runs to the return address that the `.debug_frame` row gives
  (DWARF 5 6.4) once SP equals the CFA. Unspecified return-address rules are treated as
  unchanged, as in gdb's `dwarf2-frame.c`. Without CFI it falls back to the worker's
  backtrace `step_out` and reports `unwind: worker_backtrace`. `out` of an inlined frame
  steps until the inline depth drops.

Replies carry the raw PC, ARM/Thumb state, file, line, column, discriminator, row range,
function, inline depth, thread, core, epoch and `certainty` (`statement_start`,
`inline_call_site`, `inside_statement` or `no_line_information`). A breakpoint hit, thread
switch or exhausted budget ends the step at the actual PC with that reason.

`source_excerpt` reads only below a configured source root. It compares the file with the
line table's DWARF 5 MD5 when present, and reports `not_recorded` when the producer gave
none (devkitARM GCC). The CPU inspector shows the excerpt around the disassembled address,
toggles a source breakpoint when a line is clicked, and adds Step line, Over line and Out
line with a 10,000-instruction budget. These behaviours are verified natively on CITnLO with
clang and GCC builds of [the C++ fixture](../fixtures/source/fixture.cpp), checked against
`llvm-addr2line -i` and `llvm-objdump --line-numbers`.

## Trace design

Suggested event kinds:

```text
cpu.exception, cpu.breakpoint, cpu.watchpoint, cpu.sample
kernel.thread_switch, kernel.thread_wait, kernel.thread_wake
svc.enter, svc.exit, ipc.send, ipc.reply
memory.map, memory.unmap, memory.write_sample
service.request, service.response
```

High-rate instruction-level events are **not** enabled by default. Sampling should show both guest instruction counts (if available) and host profiler time, separately. Frame-specific context may link CPU events to GPU submits via event sequence, not by guessing from timestamps alone.

## Differential analysis workflows

**Crash regression:** run same homebrew build and script in stock Azahar, integration and experimental; compare loaded title, process layout, PC history samples, SVC returns; stop at first state divergence. If trace mode alters divergence, collect instrumentation A/B before concluding.

**Scheduling bug:** create two guest threads, shared event and timer, inject fixed input; record runnable/waiting transitions and tick deadlines. Compare kernel wait semantics with hardware GDB logs or microbenchmark results; treat Azahar's HLE scheduler as a reference, not final truth.

**Memory bug:** watch write to a selected range, decode nearby disassembly, capture the guest virtual mapping and physical backing, note rasterizer cache state, and compare source-hash/previous buffer contents. For framebuffer mismatches, don't compare only the latest RAM image; capture the image at the relevant draw/transfer.

## Robustness tests

- Read unmapped, partially mapped and protected spans without worker crash.
- Set a breakpoint, reset/load a different executable, verify invalidation and no stale location.
- Pause while blocked on SVC and while GPU readback is pending.
- Multi-core test proves whether step advances only selected core or permits other cores to progress; tool response states precisely which semantics it supports.
- Concurrent UI and MCP clients reading one snapshot return same bytes and epoch.
- Large watchpoint with tracing deliberately hits resource cap; worker remains alive and reports loss.

## Useful source map

- `src/core/core.cpp` and `.h`: run loop, core selection and pending requests.
- `src/core/arm/dynarmic/arm_dynarmic.cpp`: JIT callback, SVC entry, memory backend.
- `src/core/hle/kernel/svc.cpp`: system call handlers.
- `src/core/hle/kernel/thread.cpp`, `process.cpp`, `kernel.cpp`: scheduler/object state.
- `src/core/memory.cpp`: emulated reads/writes and rasterizer cache interactions.
- `src/core/gdbstub/`: existing GDB integration and lessons.
- `src/core/rpc/rpc_server.cpp`: current scripting behavior and safe-point limitations.

Inspect exact function names and data layout at your pinned SHA before introducing hooks. Links in [14](14-sources-and-evidence.md).
