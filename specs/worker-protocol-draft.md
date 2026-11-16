# Proposed worker protocol v1 (draft, NOT IMPLEMENTED)

The protocol is versioned independently from Azahar branch SHA. This is a human-readable contract to drive a future machine-readable `.proto` definition after the worker spike.

## Identity and envelope

```
Envelope:
  version_major: u16
  version_minor: u16
  request_id: u64
  session_uuid: 128-bit
  session_epoch: u64
  type: request | reply | event
  payload: tagged binary message
  max_length: enforced before decoding
```

All cross-process integers are fixed width. Strings are UTF-8. Any guest addresses are typed and qualified by address-space + process/core ID, never host pointers. Unknown optional fields may be ignored only with compatible versioning; unknown required semantics trigger explicit failure. Replies echo request ID and record the state epoch at which they executed.

## Method skeletons

```
CreateSession(Config) -> (SessionId, Capabilities)
Load(SessionId, ContentHandle) -> (Epoch, ProgramIdentity, LoadResult)
RunUntil(SessionId, Predicate, WallDeadline) -> (StopReason, StateSummary)
Pause(SessionId, Mode) -> (StopReason, SnapshotId)
StepInstruction(SessionId, CpuSelector) -> (StopReason, InstructionCount, SnapshotId)
ReadMemory(SessionId, Space, ProcessId, Address, Length, Consistency) -> (Bytes|Holes, SnapshotId)
WriteMemory(SessionId, Space, ProcessId, Address, Bytes, ExpectedEpoch) -> (NewEpochOrRevision, SideEffects)
CaptureDraw(SessionId, FrameId, DrawId, Options) -> (ArtifactId, MissingFields)
ConfigureTrace(SessionId, Filter, Levels, ByteBudget) -> (TraceConfigId)
ExportTrace(SessionId, TraceId) -> (ArtifactId, Size, SHA256)
Stop(SessionId, Deadline) -> (ExitCode, FinalStatus)
```

### Run predicates

`frame_count`, `cpu_pc`, `svc`, `ipc_service_cmd`, `gpu_command`, `gpu_draw_id`, `guest_tick_limit`, `exception`, `wall_timeout`. Predicates must be validated and termination must be bounded.

### Snapshot visibility

`cpu_safe_point`, `gpu_consistent`, `full_checkpoint`, `telemetry`. A GPU-consistent read may block on host graphics processing. A full checkpoint is refused if unsupported async work is pending. A `telemetry` read may be stale; its age is included.

### Capabilities example (illustrative)

```json
{
  "protocol": "1.0",
  "worker_sha": "<commit>",
  "model": "old3ds",
  "render_backends": ["software", "vulkan"],
  "features": {"cpu_debug": true, "gpu_draw_snapshot": false, "firmware_boot": false},
  "max_memory_read_bytes": 1048576,
  "max_trace_bytes": 536870912
}
```

Example caps are **policy examples, not measured/runtime support**. Feature advertisement must be generated from actual build and runtime capability checks.

## Stream artifacts

Raw screen output uses shared memory with `(generation, slot, seq, format, stride, width, height, screen, eye, epoch)`. Events use ordered worker sequence. Trace artifacts store a versioned index plus compressed chunks; chunk type IDs are distinct from serialized internal C++ structs. Export direct capture bytes only with actual user permission.

## Error semantics

Never conflate submitted/accepted/finished. A long request returns a job ID and `PENDING`, followed by completion or timeout. If state mutation may have occurred but an IPC connection dropped, client must query mutation/request ID history; do not retry blindly.

## Security

All endpoints local-only by default. Apply OS pipe permissions and per-session capabilities. No host shell commands or unbounded file writes. Persist audit events for debugger memory patches and external input injection.
