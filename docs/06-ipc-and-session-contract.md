# 06 — IPC, session protocol, safe points and lifecycle

## Purpose

The IPC contract must survive Azahar upgrades, debug feature additions, replacement of the C++ worker and future non-Azahar execution engines. Treat IPC as a *product boundary*, not an easy way to expose Azahar classes.

## Transport

- Windows: private named pipe by default. Linux/macOS: Unix-domain socket. Authenticate via OS filesystem/pipe permissions and a supervisor-created random capability token in handshake; token is a defense-in-depth measure, not a substitute for OS access rules.
- Frame format: length-delimited binary Protocol Buffers (proposal), explicit max message size, request ID, session ID, epoch, protocol major/minor, payload, response status; async events have monotonically increasing per-worker sequence.
- Reject oversized frames before allocating memory; no unbounded recursion/arrays or untrusted length fields. Define a maximum for every request, including reads, captures and event batches.
- No host-facing public TCP/UDP listener in normal mode. If remote control is added later, require explicit opt-in, TLS, strong authentication and per-session authorization.

## Proposed envelope

```text
Request { version, request_id, session_id, expected_epoch,
          deadline_ms, method, body }
Reply   { version, request_id, session_id, actual_epoch,
          status, body, error_detail }
Event   { version, session_id, epoch, seq, guest_tick?,
          host_ns?, kind, body }
```

Schema fields are **proposals**, not verified Azahar wire structures. Maintain machine-readable schema in `specs/` and generate Rust/C++ code with reproducible tools. C ABI and public RPC structs use fixed-width types; no cross-language C++ STL objects.

### Critical statuses

`OK`, `INVALID_ARGUMENT`, `UNSUPPORTED`, `NOT_READY`, `NOT_PAUSED`, `BUSY`, `TIMEOUT`, `STALE_EPOCH`, `UNMAPPED`, `PERMISSION_DENIED`, `RESOURCE_LIMIT`, `INCONSISTENT_SNAPSHOT`, `WORKER_EXITED`, `INTERNAL`. Do not answer `OK` because a request was queued; distinguish `ACCEPTED` (pending) from committed completion.

## Handshake example

```
client -> worker: HELLO(protocol=1.0, session_uuid, nonce,
                        requested_features=[cpu_debug,gpu_capture,frame_ring])
worker -> client: CAPABILITIES(protocol=1.0, worker_sha,
                        azahar_sha, model=old3ds,
                        backends=[software,vulkan], max_read=..., ...)
client -> worker: CONFIGURE(instance_root, output_buffers, audio_policy, deterministic_seed)
worker -> client: READY(epoch=1)
```

Every session advertises **actual** capabilities. Never promise features based solely on the branch name. Handshake failure cannot leave a live orphan worker or a partially writable save root.

## Epoch and sequencing

Increment state epoch after load, reset, restore/rewind or any operation invalidating address-space/resource identity. A request referencing an older epoch is rejected, except immutable historical-capture reads that specify an artifact ID. GPU draw/resource references include `(epoch, frame_seq, draw_seq)`; a plain register index is not sufficient context to reconstruct past state.

All mutating requests (memory writes, breaks, input injection, clock changes) are serialized through the emulation thread. Query commands may use a published immutable snapshot or schedule a safe-point read.

## Safe-point queue

Existing Azahar's `System::ProcessPendingWork()` runs GDB and scripting operations on the emulation thread and is invoked between slices/while waiting in the frame limiter. Add the worker's request queue here or a similarly clearly defined place; follow the same wakeup strategy via `NotifyPendingWork()` when new work arrives. Source: https://github.com/azahar-emu/azahar/blob/master/src/core/core.cpp

Rules:
1. The IPC receiving thread **does not read mutable `Core::System`, GPU or kernel objects directly**.
2. It validates framing, enqueue bounded command, returns pending/request ID if long-running.
3. The emulation thread reaches a safe point, validates epoch and required run state, performs operation, emits final completion.
4. Screenshot/GPU readback may finish on a renderer worker; the control operation remains pending until the requested consistency contract is met.
5. Safe-point operations have a budget; a massive memory read runs in bounded chunks or deliberately stops emulation. Long tasks should never leave a half-applied guest mutation.
6. A pause request transitions through `PAUSING`, then completes only once requested CPU state is quiescent. Avoid claiming GPU is quiescent unless a separate barrier completed.

## Stepping contract

Avoid ambiguous `step`. Define:

- `step_cpu_instruction(core_id, thread_id?)`: exactly one retired guest instruction on the selected core/thread or return `UNSUPPORTED` if backend cannot honor it. Test against Azahar's `SingleStep()` semantics; it currently delegates to `RunLoop(false)` and is not proven to target one selected core.
- `step_to_event(event_filter)`: stop at next selected callback or guest event; may execute arbitrary instructions.
- `step_frame(boundary=top_vblank_start|bottom_vblank_start|presented_frame)`: explicit boundary, record skipped/duplicate presentation.
- `run_until(breakpoint|guest_tick|frame_count|event|timeout)`: bounded by both guest goal and host timeout, with stop reason and final state.
- `pause(mode=cpu_only|capture_consistent)`: stronger mode may wait for renderer synchronization.

## Shared-memory image ring

Descriptor per ring: magic, schema, session/epoch, slot_count, width, height, stride, pixel format, screen/eye identity, handle/lifetime token. Slot metadata: atomic state, generation, sequence, guest frame seq, capture host timestamp, bytes valid, hash. Writer must never overwrite a leased slot; use bounded drop/backpressure policy. Ring reallocation increments generation, requires consumer acknowledgement and does not reuse an old OS handle while clients still reference it. Stale access becomes a typed error.

Nominal 3DS native screens: top 400x240, bottom 320x240; RGB(A) output must identify stereoscopic eye. Hardware often stores rotated framebuffers; define whether published pixels are normalized to display orientation. Do not rely on shape alone.

## Control / observation surface

| Method group | Commands | Minimum info returned |
|---|---|---|
| Session | load, run, pause, reset, stop, status | state, epoch, stop reason, config hash |
| CPU | regs, disasm, step, breakpoints | core, thread, mode, PC, validity |
| Memory | map, read, write, search, watch | process, space, range, epoch, errors |
| Kernel | process/thread list, IPC trace, wait graph | IDs, scheduling/wait states, event seq |
| GPU | external/internal regs, command lists, draw, textures, frame | draw/frame identity, capture consistency |
| DSP | state, audio ring/underrun metrics | sample time, emulation status, backend |
| Trace | arm, start, stop, flush, export | filters, counts, dropped, artifact hash |
| Input | inject, set script ownership, touch | accepted boundary, effective sample tick |

## Cancellation and idempotency

- `get_*` reads are idempotent with snapshot/epoch identities; live reads may differ across calls.
- `load`, `reset`, `write_memory` and `run_until` are **not** automatically idempotent. Replayed request IDs return recorded result within a bounded retention window, not reapply mutation.
- Cancellation interrupts waiting/pending operations and reports whether effect was already committed; cannot rewind a half-frame by fiat.
- `stop` attempts graceful shutdown, then terminates worker at deadline; report unflushed output and preserve crash logs.

## Artifacts

Export a manifest and content-addressed files, not opaque log text. `manifest.json` includes worker/branch hashes, GPUI/protocol version, game hash (not ROM bytes), model, graphics backend, selected test, input seed, time domains, capture feature flags, timestamps, dropped-event counters, storage snapshot ID and host environment. Large binary files have SHA-256 checksums and explicit schema IDs.

## Security and privacy

MCP is not allowed to supply arbitrary absolute worker paths. Supervisor allocates roots under its own managed workspace; reads/writes are by object handles or normalized virtual-relative paths. Memory writes/patches and firmware access are privileged tool categories that require explicit session capabilities. Retain a local audit trail of mutating commands. Avoid collecting screenshots or saves unless requested by user/test scenario; provide retention/deletion controls.
