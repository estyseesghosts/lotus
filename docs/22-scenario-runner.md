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
Unsupported profiles, snapshots, capture types and trace assertions fail before
launch. `scenario-example.yaml` remains a proposed Wave 3 trace example.

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

Source: `runner::execution`, `session::native_session`; upstream frame layout
comes from `video_core/renderer_software/renderer_software.cpp` at the stock pin.
Rollback: stop using `scenario run`; validation and stock oracle remain available.
