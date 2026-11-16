# 13 — Build system, CI, milestones, release and risk management

## Build philosophy

Two technologies, two lockfiles and independent test pipelines:

- Rust workspace: locked Rust toolchain, Cargo dependencies, pinned GPUI Git revision; builds desktop, CLI, session manager, trace/MCP libraries.
- Azahar native worker: CMake/C++ with upstream submodules, pinned Azahar branch SHA, compiler, CMake options and dependency versions; builds worker executable and C++ tests.

No requirement to compile GPUI and Azahar into one binary. IPC compatibility is tested with separate build outputs. Do not let the Rust build dynamically fetch Azahar's moving `master` during CI.

## Local development on Windows first

Suggested prerequisites: Git, CMake, Ninja, C++ compiler/toolchain matching Azahar's supported build paths, Rust/Cargo, Python for small scripts where useful, Windows SDK/Vulkan tools for GPU backend. Use official Azahar build instructions for actual compiler versions; don't assume Microsoft/MSYS2/MXE settings from a past release remain valid. The source currently uses CMake >=3.25 and has platform-specific conditionals. On macOS/Linux, follow GPUI's platform prerequisites and Azahar's native build requirements separately.

Suggested commands after implementation (not executable in untouched upstream):

```powershell
cargo build --locked -p ctr-cli -p ctr-desktop -p ctr-supervisor
cmake -S vendor/azahar -B build/native-integration -G Ninja -DENABLE_QT=OFF -DENABLE_LIBRETRO=OFF -DENABLE_CTR_WORKER=ON -DENABLE_SOFTWARE_RENDERER=ON
cmake --build build/native-integration --target ctr-worker --config RelWithDebInfo
cargo test --locked --workspace
ctr-cli doctor --worker build/native-integration/src/ctr_worker/ctr-worker.exe
```

**Do not cargo-build or invoke the worker from a post-build script that downloads copyrighted Nintendo files.** User firmware and game assets are external; fixtures can use self-authored homebrew programs.

## Continuous integration jobs

| Job | Trigger | Gate |
|---|---|---|
| `rust-check` | every Rust PR | format, clippy, unit tests, no semver drift |
| `gpui-compile` | GPUI/desktop PR | Windows build and smoke; Linux/macOS where runners available |
| `azahar-integration-build` | integration PR/upstream sync | worker+stock Azahar compile, C++ tests, minimal native run |
| `azahar-experimental-build` | experimental PR | same worker protocol, instrumentation tests |
| `protocol-contract` | schema or worker PR | version negotiation, invalid inputs, CLI/MCP parity |
| `worker-smoke` | worker or session PR | .3dsx launch, input, audio, two screens, stop/restart |
| `multi-instance` | storage/supervisor PR | 4 sessions and fs isolation under fault injection |
| `golden-corpus` | core/GPU PR | fixed binaries, GPU outputs and stable checkpoints |
| `performance-regression` | selected nightly | baseline/diagnostic builds, median and p95 host cost, A/B |
| `upstream-watch` | scheduled | latest upstream SHA, diff hotspots and license changes |
| `hardware-data` | manual physical run | validates fixture metadata, units, hashes and statistics |

GitHub CI cannot validate real 3DS hardware unless a controlled physical runner is built and explicitly maintained. Don't label emulator-only CI as hardware verification.

## Release profiles

- `fast-play`: Azahar-compatible settings, accelerated renderer, low overhead, minimal tracing.
- `debug`: breakpoint state, safe-point RPC, L1 event rings and artifacts, reasonable performance.
- `capture`: selective L2/L3 GPU/CPU events, may pause guest or incur major slowdown.
- `research`: experimental models with explicit version/config and measured uncertainty, not enabled by default without tests.

A release manifest must record product version, worker source revision/branch, protocol, GPUI revision, compiler, dependencies, defaults, known unsupported functions, and license notices. Experimental builds must be visually distinguishable from integrated baseline in GUI and trace metadata.

## Exit gates, not arbitrary calendar deadlines

### Milestone M0 — reproducible sources

- Pinned Azahar revision, GPL/license inventory, stock build works, two worktrees configured.
- Minimal native worker target compiles with Qt/Libretro disabled where supported.
- No feature claim yet.

### M1 — core boots headless

- Program load and basic execution; emitted status and complete shutdown.
- Software top/bottom frames and event ordering validated against stock Azahar in one known homebrew fixture.
- Failing input/unsupported applet emits typed error, not hang.

### M2 — Rust supervisor and CLI

- Native control IPC handshake and session state; pause/resume/reset/runs; isolated storage.
- At least 2 workers; acceptance target 4 concurrent independent instances.
- Repeatable scenario with fixed stop condition and output hash.

### M3 — GPUI usability

- Both screens, touch mapping, controller input, live status and logs.
- Dynamic frame content under synthetic and real worker; UI survives worker crash.
- No required Qt windows or Libretro dependencies.

### M4 — developer debugging

- CPU registers and disassembly, safe-point memory reads, breakpoints; kernel thread/process snapshots; consistent error semantics.
- MCP and CLI command equivalence; agent reproduces a known crash through scenario runner.

### M5 — GPU captures

- PICA command list, draw-at-state, texture versions, frame ID, output raw image and diff.
- Instrumentation A/B test and missing/dropped event reporting.
- Three representative rendering regressions diagnosed with captures.

### M6 — first hardware verified accuracy gain

- Physical old-3DS fixture corpus exists; at least one meaningful independent behavior correction with before/after vs stock Azahar; held-out evidence.
- No broad claim of perfect accuracy; known failures cataloged.

## Risk register

| Risk | Probability/impact | Response |
|---|---|---|
| Azahar core relies on hidden frontend hooks | High/high | compile minimal worker early; write interface inventory before UI |
| GPUI APIs change | High/medium | pin commit and isolate custom Elements |
| Vulkan headless readback complicated | High/medium | software path first; benchmark readback |
| Upstream and experimental branches drift | High/high | integration-first merge policy and nightly watch |
| Snapshot semantics unreliable | Medium/high | scripted launch as fallback; validate async barriers |
| Guest debugger reads race with core | High/high | queue to `ProcessPendingWork`, epoch/safe-point checks |
| Trace destroys performance | High/medium | bounded levels and A/B measurements |
| Multiple instances share writable files/ports | High/high | per-worker root, explicit locks, collision tests |
| Hardware research open-ended | High/high | falsifiable short experiments with ROI prioritization |
| GPL/third-party compatibility | Medium/high | per-file license audit, release source/notice compliance |
| User game content/security | Medium/high | local-only, scoped paths, user-provided files |
| Timing model overfits | High/high | held-out benchmarks; confidence bounds, no magic constants |

## Licensing and distribution

The checked Azahar `LICENSE.txt` contains GPLv3, but individual files contain copyright notices such as GPLv2-or-later and GPLv3-or-later. **Verify all file-level licenses, linked dependencies, GPUI licensing, combined-work and source distribution obligations** when packaging. Different processes/IPC do not automatically settle legal characterization. Preserve copyright attribution, provide corresponding source and build information as required; do not bundle Nintendo firmware, games, console-unique data or keys. Consult competent legal guidance if commercial or restrictive distribution is planned.

## Publication criteria

Before claiming a public release: install/uninstall cleanly; no external network listener; user-supplied files never leave machine by default; license notices and source provenance included; debugger features documented with limitations; crash reports scrub absolute user paths and private data by default; reproducibility run manifests generated. Publish an accuracy claims table with direct evidence rather than marketing adjectives.
