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
| load | path, local `.3dsx` | loaded and paused, or typed error |
| run | frames 1–600 | started; automatically pauses at budget or 20 host seconds |
| run_until | frames 1–600 | paused after advancing, or explicit error/deadline |
| pause | none | outer RunLoop returned; no nested teardown |
| reset | none | same content reloaded, paused, epoch incremented |
| capture | none | two native screen leases copied from latest software output |
| input | owner live/scenario, buttons, x/y, touch | complete input state installed |
| registers | core 0–3, also checked against active core count | 16 registers/CPSR at safe point |
| read_memory | address u32, length 1–4096 | mapped current-process bytes at safe point |
| stop | none | core shut down, then process exit required by supervisor |

Queue capacity is 16 messages of at most 64 KiB. Only the reader thread touches
the queue; all emulated state stays on the emulation thread. The existing
`NotifyPendingWork` event wakes the frame limiter. The optional frontend callback
in `ProcessPendingWork` services status/register/memory inspection there.
Load/reset/stop and capture run outside nested RunLoop calls. No selected-core
single-step precision is claimed.

## Ownership and budgets

Frames use three reusable file slots per screen under the private root. Each
has a 32-byte little-endian header: `LTRF`, version u32, epoch u64, sequence u64,
width u32, height u32; then native BGRA8 pixels in top-left row order. Metadata
also includes byte length, stride, slot and orientation. Rust validates all
fields, bounds allocation, checks the header and returns owned pixels before
another capture. There is no wait for UI acknowledgement. Old slot sequences,
reset epochs and stopped/crashed sessions invalidate leases. Total pixel slot
storage is bounded to 2,073,600 bytes plus six headers.

Input factories expose one complete state to HID. Scenario ownership excludes
live input until reload/reset. Sending all-zero state releases keys/touch after
device disconnect. This version has no host audio device: the DSP submits to a
16,384-stereo-sample muted ring, drops oldest samples on overflow, and reports
sample/drop counts and FNV-1a checksum. Status drains the ring. The checksum is
an emulator diagnostic, not an audio quality/hardware claim.

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

**Verified emulator**, Windows GNU Release: both native tests passed (17.21 host
seconds total) with the supplied fixture: lifecycle/reset/relaunch, ring overwrite
and reset invalidation, scripted input exclusion, safe reads while running,
bounded invalid reads, four simultaneous native workers, forced sibling failure.
This proves private runtime files, not save-game correctness for every title.
No firmware was needed by this fixture; the supplied DSP dump remains external.
Hardware verification: none. Other platforms/accelerated renderers: untested.

Source map: native `src/ctr_worker/{frontend,frames,input,transport,control}.h`,
`src/core/frontend/control_callback.h`, `src/core/core.cpp`,
`src/audio_core/{worker_sink.h,sink_details.*,CMakeLists.txt}`; Rust
`session::{controlled_worker,native_session,process_job}`. Immutable native pins
and build flags are in `config/pins.toml` and generated build manifests.
Rollback: disable `ENABLE_CTR_WORKER`, use immutable stock worktree; no accuracy
model changes were made.
