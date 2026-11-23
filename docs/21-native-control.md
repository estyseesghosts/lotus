# Native sessions, frames and containment

LOTUS is the application. The upstream-tracking `integration` branch supplies
the **AZAHAR** backend. The `experimental` branch supplies **CITnLO**, based on
integration. The eventual replacement backend is **LOTUS**. These names do not
change the existing wire protocol or executable names.

The native frontend uses software rendering, Old 3DS, fixed initial ticks 0,
fixed time 2000-01-01 UTC, HLE services/DSP and a muted audio sink. It never opens
Qt/SDL windows. Unsupported keyboard/Mii callbacks pause with an explicit
`unsupported_applet` reason. This does not implement arbitrary firmware applets.

## Implemented protocol

The length-prefixed private pipe transport from document 18 now accepts `params`
and returns `data`. Version remains 1.0; these are additive fields. The native
backend identifier is `azahar_native_software`. Only one mutable Rust owner sends
commands. IDs increase and are never retried after a transport failure.

| Method | Parameters | Completion |
| --- | --- | --- |
| hello/status | none | state, source SHA, presented frames, input/audio statistics |
| load | path, local `.3dsx` | loaded and paused, epoch incremented, or typed error |
| run | frames 1–600 | started; automatically pauses at budget or 20 host seconds |
| run_until | frames 1–600 | paused after advancing, or explicit error/deadline |
| pause | none | outer RunLoop returned; no nested teardown |
| reset | none | same content reloaded, paused, epoch incremented |
| capture | none | two native screen leases copied from latest software output |
| preview | none | two leases into the worker's shared-memory ring (CITnLO and AZAHAR on Windows); `unsupported` elsewhere |
| flush_storage | none | paused worker with no guest FS call pending on a host thread: every stdio stream flushed; otherwise `not_ready` with `reason` `running` or `async_fs_pending` (both branches; #28) |
| input | owner live/scenario, buttons, x/y, touch | complete input state installed |
| registers | core 0–3, also checked against active core count | 16 registers/CPSR at safe point |
| read_memory | address u32, length 1–4096 | mapped current-process bytes at safe point |
| stop | none | core shut down, then process exit required by supervisor |

Requests are at most 64 KiB. At most 16 are in flight (#160, `transport.h`
`RequestWindow`, both branches): a request holds a credit from the moment the
reader takes it off stdin until the writer thread has written its reply. With
all 16 held, the reader stops reading, so the host's further writes block in
the OS pipe. Nothing is rejected, dropped or reordered, and the emulation thread
never waits on the reader. Every request gets exactly one reply, so no credit
is held forever. This also bounds the reliable reply queue when a client stops
reading. Before #160 the 17th queued request ended the reader and the worker
exited 0 with nothing on stderr. Stop closes the window before interrupting the
reader, so a reader waiting for a credit exits.
`status.transport.requests` reports `limit`, `in_flight`, `peak`, `completed`,
`backpressure_waits`, `backpressure_wait_us_total` and `backpressure_wait_us_max`.

A frame length outside 1–65536, a truncated frame or a payload that is not
JSON ends the worker with exit code 1 and the stderr line
`worker_control_error: <reason>`. A reply write to a closed pipe or an
exception from the control loop produces the same line. End of stream between
frames writes `worker_control_closed: request stream closed` and exits 0.
Lotus classifies `worker_control_error` as failure cause `control_error` with
the line as its marker (`failure.rs`). Lotus itself never relies on
backpressure: every Supervisor exchange holds the session's `native` lock, so a
worker sees at most one Lotus request at a time.

Only the reader thread fills the request queue; all emulated state stays on
the emulation thread. The existing
`NotifyPendingWork` event wakes the frame limiter. The optional frontend callback
in `ProcessPendingWork` services status/register/memory inspection there.
Load/reset/stop and capture run outside nested RunLoop calls. No selected-core
single-step precision is claimed.

Replies and notifications go through one writer thread (`transport.h`
`ControlWriter`, both branches, #26); the emulation thread only queues them.
Replies and `stopped` notifications keep their order and are never dropped.
Other notifications are best effort: at most 1024 queued, a newer one evicts the
oldest, and each later notification carries `notifications_dropped`. A host that
stops reading therefore no longer stops emulation (`tools/event_flood.py`
slow reader). `status.transport` reports queue depth (`queued`, and
`best_effort_queued` against `best_effort_limit`), `peak` (whole queue, so up to
the limit plus queued replies and stops), and written, dropped and oversize
(above 64 KiB) counts.

CITnLO groups events into categories `stopped`, `log` (`guest_log`,
`hio_stdout`, `logpoint`), `native_log`, `svc`, `ipc` and `pica`. `hello`
takes `notifications` (bool) and optional `subscriptions` (categories to push;
without it every category is pushed, as before) and `history` (categories kept
in the 1024-event ring read by `events`/`logs`; default all). The debug
operation `events_configure` changes `notify` and/or `history` later. A
category that is neither pushed, kept, nor needed by the active `run_until`
predicate is not built: no JSON, no ring entry, no pipe write. `stopped` is
always kept. SVC, IPC and PICA events still need `trace` or `gpu_configure`.
`status.events` lists the sets, the latest sequence and ring/pending loss, and
`status.stop.sequence` is the sequence of the current stop, so a consumer that
missed the notification recovers it from `status`. Lotus subscribes to
`stopped` only; its GDB server is the only consumer of pushed events. The sets
last for the worker process (load and reset keep them); closing the pipe ends
the worker as before.

Determinism (#29). Both workers' `DefaultProfile` (`src/ctr_worker/debug.h`)
fixes the boot tick count (`init_ticks` 0) and the guest clock (`FixedTime`,
2000-01-01). CITnLO's also turns on `deterministic_async_operations`: HLE
async sections (`hle_ipc.h` `RunAsync`, `RunOnThreadWorker`) run on the
emulation thread and wake the guest after their emulated delay, instead of on a
host thread whose finish time sets the wake tick. AZAHAR keeps the stock value
(off) under ADR-010; proposed ADR-012 in [34](34-performance-and-issue-triage.md)
#29. `settings_set deterministic_async_operations` before `load` changes it
per session on either worker (a boot setting, `settings_bindings.h`). The console identity is per root: CFG writes a random MAC (also in the
shared page) and console unique ID into `sysdata`/`nand` on the first load, so
two fresh roots differ in those bytes. Copying one root's `sysdata` and `nand`
into another before load gives both the same console (`tools/bench.py` and
`tools/divergence.py --same-identity` do this).

CITnLO debug operation `state_hash` (paused only; `chunk_kib` power of two
4–65536, default 1024; `sections` default true; optional `dump` name) returns
`guest_ticks`, per core `pc`, `thread`, `ticks`, `idle_ticks` and a `context`
hash, XXH3 hashes of FCRAM, VRAM, DSP RAM and N3DS extra RAM in `chunk_kib`
chunks, `async_pending`, and, when no async HLE work is pending, the size and
hash of each top-level `System::serialize` section (`system`, `timing`,
`cpu0`–`cpu3`, `services`, `archives`, `dsp`, `memory`, `kernel`, `gpu`,
`movie`, `cheats`) plus `serialized_hash`. `dump` also writes the serialized
bytes and section offsets to `<root>/state-dumps/<dump>.bin` and `.json`.
`tools/divergence.py` steps several workers in lockstep, compares their hashes
and prints the first differing bytes with context.

## Ownership and budgets

Frames use three reusable file slots per screen under the private root. Each
has a 32-byte little-endian header: `LTRF`, version u32, epoch u64, sequence u64,
width u32, height u32; then native BGRA8 pixels in top-left row order. Metadata
also includes byte length, stride, slot and orientation. Rust validates all
fields, bounds allocation, checks the header and returns owned pixels before
another capture. There is no wait for UI acknowledgement. Old slot sequences,
load/reset epochs and stopped/crashed sessions invalidate leases. Total pixel slot
storage is bounded to 2,073,600 bytes plus six headers.

Live previews skip the files (issue #6). On Windows `hello` reports
`preview: "shared_memory_v1"` and the worker owns a pagefile-backed section
`Local\lotus-ctr-<pid>-<nonce>` of three slots, each holding both screens with
the same 32-byte header (`src/ctr_worker/frames.h` `PreviewRing`). `preview`
writes one slot and returns its name, size, one lease per screen with a byte
offset, and `frame`/`current_frame`/`current_epoch`. The host maps the section
read-only (`crates/session/src/preview_ring.rs`), validates the lease against
native geometry and the mapping size, checks the header and copies the pixels
before its next request to that worker. The session command lock serializes
both, so a slot is never read while being written. A lease from an older epoch
is refused. A running Vulkan worker never waits for the GPU (#25,
`RendererBase::PreviewScreens`): it polls completion (`MasterSemaphore::Refresh`,
`Scheduler::IsFree`), publishes the newest frame whose top and bottom readbacks
have both finished and that it has not published yet, and submits the current
frame into a free staging slot (three per screen), or submits nothing while all
are in flight (`skipped_busy`). Both screens of one request share one GPU tick,
so a published pair is one frame. With nothing newer the reply repeats the
last slot with `new_frame: false`; early in a new epoch it has no frames. The
host keeps showing its last image in both cases (`Supervisor::preview` returns
nothing). While the host is playing continuously it sends
`preview {"nonblocking": true}` so the worker also avoids waiting between
bounded runs. Without this option a paused worker may wait and shows the
current frame. Workers predating the option still accept the request and use
the worker's running or paused state. Each
reply carries `stats` (requests, reused, readbacks, skipped_busy, gpu_waits,
gpu_wait_ns, copied_bytes), surfaced as `telemetry.preview` with the shown
frame's age. `tools/preview_acceptance.py` asserts that `gpu_waits` does not
move during 120 back-to-back running previews on both workers (`runs/preview-25-02`,
`runs/preview-25-int-01`, verified emulator; lag at most 1 frame there).
Software previews are never behind. `capture` is unchanged and stays
synchronous and exact, for tests and artifacts. Paused previews
match `capture` byte for byte on both workers (`tools/preview_acceptance.py`,
verified emulator). Disk writes stay below 4 KiB per preview, which is the pipe
reply, and the capture files are untouched.

Input factories expose one complete state to HID. Scenario ownership excludes
live input until reload/reset. Sending all-zero state releases keys/touch after
device disconnect. The worker defaults to muted audio. The DSP supplies a
16,384-stereo-frame diagnostic ring, drops the oldest frames on overflow, and
reports sample/drop counts and an FNV-1a checksum. Status drains the ring.
`audio_focus` controls optional Cubeb playback. `audio_capture` records raw
stereo PCM at 32,728 Hz before host volume and time stretching.

Workers with the #161 async recorder report `state`, queued and peak PCM
bytes, `queue_capacity_pcm_bytes`, `dropped_blocks`, `dropped_frames`,
`queue_overflow`, and `limit_reached`. A dedicated writer drains at most
256 KiB of queued PCM in chunks up to 16 KiB. At the first queue overflow
or 64 MiB file limit the recorder keeps a contiguous WAV prefix and counts
all subsequent frames as dropped. Stop returns `complete` or `truncated`
with the WAV artifact after the header is finalized. If the writer is
still busy after two seconds, stop returns `finalizing`; a later stop can
retrieve the result. On filesystem failure the response reports `failed`
with an error. A forced termination may leave `audio/capture.wav.part`.
The previous sealed WAV remains available until the new writer publishes
its replacement.

On workers advertising `gpu_memory_refresh` (#159), a loaded Vulkan renderer
samples VMA heap budgets, the Vulkan budget extension and supported Windows
adapter-wide WDDM usage every 500 ms. Load and reset publish a fresh sample before
replying. Status reports `sample_sequence`, monotonic `sample_age_ms`,
`sample_ttl_ms` (1000), and `sample_state` (`fresh`, `stale`, or `unknown`).
Heap block bytes, high-water marks, and the allocation failure counter are
read live. Budget, usage, and VMA suballocation bytes belong to the sample.
`gpu_memory_refresh` returns `data.gpu_memory` after an explicit sample at a
control boundary. Software and unloaded workers return null.

Renderer teardown clears the published sample. Terminal GPU failure reports
invalidate it immediately and use a nonblocking read of the previous sample
with the live failure count. If the cache is busy or has no sample, the
failure report carries `sample_state: unknown`. The sampler stops during
worker exit.

On Windows, the supervisor starts the child suspended, assigns a kill-on-close
job, then resumes its initial thread. Descendants inherit containment. Each
root has an OS file lock held through process lifetime. Crashes retain stderr
and native logs, invalidate leases, terminate the job and release the lock.
Non-Windows builds currently retain direct-child termination only; descendant
containment there is unimplemented and untested.

## Reproducible checks

`cargo test -p lotus-session` includes five real mock process tests, including
descendant termination and same-root exclusion. Native tests are opt-in, never
silently counted as passing without assets:

```powershell
$env:LOTUS_NATIVE_WORKER = (Resolve-Path build/azahar-integration-native/bin/Release/ctr-worker.exe).Path
$env:LOTUS_FIXTURE = (Resolve-Path ../t03.3dsx).Path
cargo test -p lotus-session --test native_process -- --ignored --nocapture
```

**Verified emulator**, Windows GNU Release at native pin `05773bbb5`: all three
native tests passed (20.36 host seconds) with the supplied fixture:
lifecycle/reset/reload/relaunch, ring overwrite and epoch invalidation, scripted
input exclusion, safe reads while running, bounded invalid reads, four simultaneous
workers, forced sibling failure, fifth-worker rejection and scoped artifacts
retained after stop. Guest SD-write isolation is recorded in [20](20-native-baseline.md);
save-game correctness for every title remains untested.
No firmware was needed by this fixture; the supplied DSP dump remains external.
Hardware verification: none. Other platforms/accelerated renderers: untested.

Request window (#160), **verified emulator**, Windows GNU Release, software
renderer, workers built from AZAHAR `a78f7f3` and CITnLO `638c9f4`.
`python tests/test_control_backpressure.py <out> ../t03.3dsx <workers...>`
drives a worker directly. Every case passed on both:
- 40 statuses queued behind a load were answered in order, with peak 16 and
  backpressure waits.
- 200 statuses sent while the client did not read replies left the writer
  queue at peak 15 and the worker alive.
- A mixed burst of stale epochs, input, nonblocking preview, status, run and
  pause got one reply each.
- Stop behind a blocked reader with the reply pipe closed exited 1 in under
  0.1 s, with `worker_control_error: control pipe disconnected`.
- The same stop with reading resumed exited 0.
- Malformed frames exited 1 with their reasons, and EOF exited 0.

With `--baseline`, the earlier workers (`fb7845d`, `1db4ff6`) answered 16 of 40
statuses and exited 0 with an empty stderr. The opt-in
`supervisor_traffic_stays_inside_the_worker_request_window` test sends 240
concurrent status, register, preview and desktop input calls through
`Supervisor`. On both workers: 122 exchanges completed, `peak` 2,
`backpressure_waits` 0, no failure and a clean stop. Low-load latency on the
performance machine and Linux remain unmeasured.

Source map: native `src/ctr_worker/{frontend,frames,input,transport,control}.h`,
`src/core/frontend/control_callback.h`, `src/core/core.cpp`,
`src/audio_core/{worker_sink.h,sink_details.*,CMakeLists.txt}`; Rust
`session::{controlled_worker,native_session,process_job}`. Immutable native pins
and build flags are in `config/pins.toml` and generated build manifests.
Rollback: disable `ENABLE_CTR_WORKER`, use immutable stock worktree; no accuracy
model changes were made.
