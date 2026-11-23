# Bounded native scenarios

**Verified emulator**, Windows GNU Release AZAHAR, source
`05773bbb5380bc5094fcd8eacf189fdc5d6360bb`; build flags in document 20.

```powershell
cargo run -p lotus-cli --bin ctr-cli -- scenario run specs/native-smoke.yaml build/azahar-integration-native/bin/Release/ctr-worker.exe build/scenario-runs 3
```

The external fixture is not distributed. `specs/native-smoke.yaml` resolves its
path relative to the YAML; set its optional SHA-256 to require exact identity.
Execution supports Old 3DS/software, HLE DSP/discard, ticks 0, empty private
storage, exclusive scenario input, frame captures and frame/no-crash assertions.
Unsupported profiles, snapshots and capture types fail before launch. Version 2
assertions are described below.

Each run creates a fresh root. Inputs apply before the frame following their
checkpoint; `press` releases one frame later. Touch uses native coordinates.
Runs split at input/capture boundaries and advance at most 60 frames per request.
Native requests have a 20-host-second deadline; each scenario also has its wall
deadline. Limits: 100,000 frames, one hour, 4,096 input entries, 64 captures.
Final native screens are always captured, bounding retained pixels below 45 MB
per run. These limits describe host resources, not hardware cycles.

`report.json` records source SHA, worker/content SHA-256, frame checkpoints,
native BGRA8 hashes and host elapsed time. Failures retain `failure.json` and
worker logs. Shutdown is attempted on all paths; the process job is the fallback.
Repeat mode accepts 1–10 runs and rejects differing pixel hashes, retaining
individual reports for investigation.

Three local runs of the supplied fixture with A at frame 30 and touch at frames
40–42 matched at frame 60. Fixture SHA-256:
`aad3c425c95bb30df6eabea08bf14f0b115c3f3836361659079c06bb8560b491`.
Top BGRA8: `e5540f2eae5d948e8108203caa8c9a2816dec14ea0bd864ca6e3a560a244c48a`;
bottom: `da0e70765f3a92d7c692cdf785bd4890282e4693cf05ddd7578355d1987e5d5e`.
Final-pin rerun on 2026-11-15 matched all three reports; host elapsed times were
9,436 / 12,151 / 9,944 ms, including local contention from acceptance tests.
These are host observations, not a throughput benchmark or guest timing result.
The fixture submitted no audio in these runs; audio-ring unit checks do not
prove audio playback. No hardware verification or timing improvement is claimed.

## Version 2 assertions (lotus#94)

**Verified emulator**, CITnLO `experimental` worker (SHA-256
`a8f9f49d4ca93ca1b54c331b6e49f2f92e3724d484fbac8b17f51a99da0d3766`) and the AZAHAR
integration worker, 2026-11-18, via `tests/test_scenario_assertions.py`.

`version: 2` adds these assertion types; `specs/scenario-example.yaml` uses each
one against the owned debugger fixture. Version 1 files keep their old meaning.

| Type | Fields | Observation | Workers |
|---|---|---|---|
| `memory` | `at_frame`, `address`, `width` 1/2/4, `equals` or `minimum`/`maximum` | `read_memory` at the frame checkpoint, little-endian | both |
| `register` | `at_frame`, `core`, `register` 0..16 (16 = cpsr), bounds | `registers` at the checkpoint | both |
| `frame_hash` | `at_frame`, `screen`, `sha256`, optional `region` `{x, y, width, height}` | raw BGRA capture, or the region's rows (each `width * 4` bytes, top to bottom); needs a frame capture there | both |
| `event_seen` / `no_event` / `event_count` | `event`, `text_contains`, `count_at_least` or bounds | worker event history from load to `max_frames` | CITnLO |
| `reached` / `not_reached` | `address` (bit 0 = Thumb), `count_at_least` | execute-breakpoint hit counter | CITnLO |

Event kinds are `guest_log`, `hio_stdout`, `native_log`, `stopped`,
`svc_enter`, `svc_exit` and `ipc` (the last three enable `trace`). `pica` is
rejected because scenarios do not configure GPU capture, so the absence of
`pica` events would mean nothing. Before load the runner sets the worker's
event history to the needed categories and drains `events` at every checkpoint.
Any `lost_total`, `pending_lost_total` or `history_gap` in the window, or a
history that does not drain in nine 128-event pages, makes every event
assertion `inconclusive`. Overflow is never reported as absence.

Coverage sets an execute breakpoint with `hit_after = u64::MAX`. The worker
increments `hits` before it compares against `hit_after`, so the breakpoint
counts without stopping or logging. The source is the execute path in
`azahar-experimental/src/ctr_worker/debug.h`. In the fixture run, `tick`
counted 4,716,968 hits by frame 60. That is the fixture's `counter` (4,716,969)
minus its initial value of 1.

`captures[].when` takes either `frame` or `event` (+ `text_contains`). An
event capture is taken at the end of the frame in which the event is recorded.
While one is pending the run advances one frame per request. A trigger that
never fires adds a `capture_trigger` result of `fail` (`inconclusive` if
events were lost).

Each assertion ends `pass`, `fail` or `inconclusive`, with expected, actual
and provenance. A run passes only when all assertions pass. `report.json` adds
`schema_version`, `assertions`, `passed`, `first_failure` and `event_window`.
`report.txt` gives one line per assertion. A run that does not pass also
writes `failure.json` and exits non-zero. A host deadline or a worker error
writes only `failure.json`.

Event, coverage and event-capture checks on `worker_flavor: integration` are
rejected before launch. If the flavor is declared wrongly, AZAHAR refuses
`events_configure` at runtime (`does not support event_subscriptions`) and
no report is written. `ctr-cli scenario validate` and MCP
`lotus_validate_scenario` both call `validate_execution`.

Acceptance (`python tests/test_scenario_assertions.py ctr-cli.exe <experimental
worker> <integration worker> <out>`, 28 s). The script builds the fixture and
resolves symbols with `llvm-nm`. The passing scenario runs twice and repeat mode
matches the hashes. These cases fail with the expected assertion named:

- a changed frame hash
- a changed memory expectation (actual `0xcafebabe`)
- `reached deadlock_main` (0 hits)
- an event trigger that never fires

A 2-second host deadline and the AZAHAR runtime refusal produce no report.
`native-smoke.yaml` still passes on AZAHAR.

Not implemented: guest markers (#83) and fault receipts (#92/#93) are rejected
by name until those exist. Predicate and breakpoint-stop capture triggers are
not offered: they stop mid-frame and would break exact frame checkpoints.
Timing assertions are also not offered. Draw counts need GPU capture
configuration that scenarios do not expose.

## Recorded tests (lotus#95)

**Verified emulator**, AZAHAR `7b45415d` and CITnLO `64883d29`, Windows
WinLibs UCRT GNU Release, 2026-11-22, `crates/runner/tests/native_recording.rs`.

A recording turns a live session into a version 2 scenario. MCP
`lotus_record_test` and `ctr-cli record-test <host-config> <session> [json]`
take `{"op": ...}` with these operations:

- `start` reloads the session's content, so frame 0 is a fresh boot as in a
  scenario run.
- `mark` pauses, records both screen hashes, up to 8 region hashes and up to
  16 memory words, then resumes a playing session.
- `stop`, `status` and `discard` end, read or drop the recording.
- `export` writes `recorded-tests/<name>/scenario.yaml` and `manifest.json`
  under the storage root.

Start requires the `input_boundary` worker feature
([21](21-native-control.md)). The worker applies live input only at a frame
boundary and returns the frame in `input_frame`. The export schedules the
same state at that frame, which the runner installs while paused before the
next frame. A pause also completes at a frame boundary, so a mark's hashes
and memory are taken at the same point the runner checks them.

Each accepted input is kept with its source (desktop, MCP, host) and owner.
Export turns consecutive states into button and touch transitions. Analog
sticks and button bits without a scenario name are not replayed. Export
options edit without re-recording:

- `stop_frame` trims the end.
- `omit_marks` drops marks.
- `omit_screen_hashes` keeps only a mark's region and memory assertions, for
  screens with a clock or animation outside the checked regions.
- `assertions` adds any version 2 assertion, such as `reached` or
  `event_seen`.

The 3DSX is named by file name and SHA-256 only and is never copied; place it
next to `scenario.yaml`. The manifest lists every condition the runner cannot
reproduce, instead of claiming determinism:

- analog input
- unnamed buttons
- a non-software renderer (screen hashes are then left out)
- determinism settings off (#29)
- guest files beyond the 11 nand/sysdata files both workers create at boot
- non-default settings
- debugger changes made while recording

A guest restart invalidates the recording.

`fixtures/input` folds every HID pad and touch sample into a checksum, so an
input applied on a different HID update fails the mark's memory assertion. The
acceptance test sends 60 random button and touch states over about 10 s at
irregular wall times, with two marks. The second mark is checked by a
top-half region and memory only. The exported scenario passed the runner
(15 assertions) on both workers. Before the worker change, one input that
followed a pause and resume landed mid-frame and the checksum differed.

Source: `session::test_recording`, `session::supervisor::recording`,
`runner::recording`, worker `ctr_worker/control.h` `FrameBoundary`.

Source: `runner::execution`, `runner::assertions`, `session::native_session`; upstream frame layout
comes from `video_core/renderer_software/renderer_software.cpp` at the stock pin.
Rollback: stop using `scenario run`; validation and stock oracle remain available.
