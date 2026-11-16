# 10 — Multiple independent emulator instances and storage

## Why separate OS processes are mandatory at bootstrap

Azahar's `Core::System` is implemented as a static singleton (`System::s_instance`) and other process-wide settings/file utilities exist. Running two 'systems' in one address space would require a broad refactor of global state, GDB/RPC ports, file paths, audio mixers and renderer lifetimes. Do not take that dependency before the rest of the product works. Multiple worker processes are isolated by address space, can be killed independently, and are easy for a Rust supervisor to manage.

## Session root layout

```text
workspace/
  profiles/                         reusable read-only configurations
  base/                             user-supplied immutable NAND/SD/firmware base refs
  instances/
    <uuid>/
      config/                        instance settings and input mapping
      fs-overlay/                    writable NAND/SD/save changes
      shader-cache/                 per-instance unless verified safe shared cache
      captures/                      immutable trace/frame artifacts
      logs/                          worker stdout/stderr and structured log
      ipc/                           Unix socket or named-pipe metadata
      snapshots/                     backend-versioned save states
      manifest.json                  versions, paths (redacted in exports), lifecycle
  results/                           run reports, corpus
```

Base files are referenced by a manifest/reader abstraction, not copied indiscriminately into each run. However, **read-only overlays are an implementation project**, not a guaranteed feature of Azahar's file abstractions. First implement separate full working directories and verify isolation; introduce copy-on-write only after proving every write path is intercepted. For reliability, a copied/private NAND/SD root is preferable to an incomplete COW implementation.

## Resource ownership

- Each worker gets unique session UUID, IPC endpoint, logging destination, GDB port (if enabled), per-instance emulator config and saves.
- Host input devices are mapped to explicit worker(s); duplicate broadcast input is opt-in and labeled.
- Audio: route each worker's output independently; allow global mixer and per-worker mute/volume, but no shared guest DSP state.
- GPU: separate Vulkan devices/queues/surfaces where driver behavior requires. Shared hardware means performance contention; four workers do *not* behave as four independent physical consoles in real time.
- Files: no two workers write same physical path unless a proven safe shared cache with atomic/immutable file semantics.
- Hardware console/network devices: exclusive resource leases; UDS/local wireless is a separate emulation problem.

## Scheduling and host interference

Expose policies:

- `interactive`: foreground display/audio at host refresh, others continue normally but may skip UI frames.
- `benchmark`: throttle background activity or serialize independent timing measurements to eliminate noisy neighbors; record host load.
- `batch`: headless unthrottled execution where supported, host CPU/GPU ceilings and disk cap.
- `accuracy`: more expensive reference model if available, explicitly reduced emulation speed.

**Never infer hardware cycle timing from host execution wall time**, especially under multi-instance contention. Separate the workload's emulated guest timestamps from host scheduler stalls.

## Instance lifecycle

States: `CREATED`, `STARTING`, `READY`, `LOADING`, `RUNNING`, `PAUSING`, `PAUSED`, `CAPTURING`, `STOPPING`, `STOPPED`, `FAILED`, `ORPHANED`.

For each:
1. Supervisor creates root with permissions and lock, validates user assets and quotas.
2. Spawns child with only necessary arguments/environment and private endpoint capability.
3. Handshake verifies protocol, worker SHA and available backend features.
4. Worker loads content; reaches running/paused state; reports checkpoint.
5. On stop, `RequestShutdown` is processed, files flushed, renderer/audio stopped, child exits and resources reaped.
6. If timeout/crash, terminate process tree, mark captures partial, preserve logs; remove stale IPC and lock **only after verifying worker PID/liveness**.

## Clone and snapshots

Initial cloning means **start new worker with same immutable ROM, profile, initial save data and input script**. It does not require cloning RAM/CPU state.

Later `clone_checkpoint` may use Azahar save states, but must capture/verify:
- Azahar worker commit and serialization schema version.
- No pending unsupported async operations; Azahar's `RunLoop` already delays/fails saves when async kernel operations are pending.
- Shader/cache/graphics resource state on restore, DSP and input state, epoch increment and rebind of debugging IDs.
- Correct restoration of guest timer seeds and per-instance user data.
- Whether saves are compatible between integration and experimental branches; default to **incompatible until tested**.

For test automation, prefer a repeatable scripted boot to a known checkpoint if save-state correctness is uncertain.

## Stress/acceptance tests

- Four workers boot test homebrew from a common immutable executable, each with isolated writable root; each finishes exact fixed-frame scenario.
- Simultaneous saves yield four independent results; verify no shared writes or cache corruption via filesystem auditing.
- Kill one worker mid-save; other three keep running; damaged overlay is detected/recovered or surfaced.
- Start two GDB-enabled workers; ports never collide. Existing Azahar UDP RPC is disabled to avoid fixed-port collision.
- Deliberately exhaust memory/disk quota in one instance; others remain operable, supervisor returns bounded error.
- Restart same instance UUID after crash with new epoch; stale renderer shared-memory clients cannot access old slot handles.
- Run same test in four processes and compare stable output hashes; document nondeterministic causes.

## Later optimization

Only after profiling consider sharing read-only ROM mappings, shader compilation cache and immutable assets across processes. Shared writable GPU caches or HLE state are high risk. OS-level memory deduplication and copy-on-write file cloning may already provide sufficient benefit without dangerous in-process embedding.
