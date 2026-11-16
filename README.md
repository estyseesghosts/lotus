# Independent 3DS emulator — engineering dossier and workspace

**Status:** scaffolded workspace plus design dossier. No emulator yet. **Research checked:** 2026-11-15. **Primary target:** original 2011 3DS; New 3DS later. **Host:** Windows first, macOS/Linux next. **UI:** Rust + Zed GPUI. **Initial emulation:** modified **Azahar native core**, **not Libretro**. **Long-term aim:** measurable hardware accuracy beyond Azahar/Citra.

## Start here

1. [01 — Product contract](docs/01-product-contract.md): identity, goals, non-goals, metrics.
2. [02 — Overall architecture](docs/02-system-architecture.md): Rust/GPUI + native C++ worker and runtime boundaries.
3. [03 — Two-branch Azahar strategy](docs/03-azahar-branches.md): upstream tracking, integration vs experimental, commands and merge policy.
4. [04 — Native worker implementation](docs/04-azahar-native-worker.md): file-by-file intervention points, build and bring-up.
5. [05 — GPUI desktop](docs/05-gpui-desktop.md): native UI, input and video, debugging views.
6. [06 — Protocol and lifecycle](docs/06-ipc-and-session-contract.md): transport, safe points, artifacts, failure handling.
7. [07 — CPU/kernel/memory debugging](docs/07-debugger-core.md): precise debugger semantics, breaking, stepping and symbols.
8. [08 — GPU inspection](docs/08-gpu-and-trace.md): draw capture, shader state, resource lineage, synchronized observation.
9. [09 — MCP and agent automation](docs/09-mcp-and-scenarios.md): small safe tool API, reproducible scenarios, example workflow.
10. [10 — Multi-instance operations](docs/10-multi-instance-and-storage.md): worker isolation, cloning, inputs, storage and limits.
11. [11 — Accuracy roadmap](docs/11-accuracy-research.md): independent CPU/GPU/memory models and experimental method.
12. [12 — Hardware validation](docs/12-hardware-validation.md): test harness, benchmarks, ground truth and statistics.
13. [13 — Build, CI, acceptance](docs/13-build-test-release.md): milestones, matrices, exit gates and risk register.
14. [14 — Sources and evidence](docs/14-sources-and-evidence.md): annotated direct code links and assumptions.
15. [15 — Implementer task sequence](docs/15-implementation-tickets.md): dependency-ordered PRs with acceptance criteria.
16. [16 — Azahar patch blueprint](docs/16-azahar-patch-blueprint.md): source files, native hooks, branch-specific patch sequence.
17. [17 — Scaffold, pins and build instructions](docs/17-scaffold-and-pins.md): what exists, what is pinned, how to build it.
18. [AGENTS.md](AGENTS.md): mandatory rules for implementer/agents.

Additional artifacts: [`specs/`](specs/) contains *proposed* protocol and scenario shapes; [`templates/`](templates/) contains review/experiment record formats. Protocol names and proposed code are not claims about existing Azahar APIs.

## What exists today

The repository is scaffolded and builds. There is no emulator in it yet.

| Part | State |
| --- | --- |
| Rust workspace (`crates/`) | **verified compiled**: protocol, session, trace, debugger, runner, hardware, accuracy, MCP, CLI and the GPUI shell; 51 tests pass; `fmt` and `clippy` clean |
| GPUI desktop shell | **verified compiled** and launched: opens a window; screens are placeholders |
| MCP server | **verified compiled**: serves `initialize`, `tools/list` and `tools/call` over stdio |
| `ctr-cli` | **verified compiled**: `version`, `pins`, `scenario validate` |
| Native worker (`workers/azahar`) | **verified compiled**: a protocol stub that prints its handshake; no Azahar core linked |
| Vendored Azahar | **source present** at the pinned commit with all of its submodules; the core itself is **not built** on this host |
| `workers/native`, `tools/hardware-tests` | **not started** |

Everything above is pinned; see [`config/pins.toml`](config/pins.toml) and [17 — Scaffold and pins](docs/17-scaffold-and-pins.md).

## Getting started

```powershell
# Toolchain, vendored sources, dependencies, pin verification.
scripts\bootstrap.ps1

# Fast checks: build, tests, CLI, pins.
scripts\smoke.ps1

# The pinned revisions, from the command line.
cargo run --quiet --bin ctr-cli -- pins
```

## Repository layout

```
lotus/
├── crates/          # Rust: protocol, session, trace, debugger, runner, hardware,
│                    #        accuracy, mcp, cli, desktop
├── workers/azahar/  # C++ worker: ABI header, entry point, CMake
├── workers/native/  # future Rust emulation backend (not started)
├── vendor/azahar/   # pinned Azahar checkout (git submodule)
├── tools/           # devkitPro 3DS ground truth programs (not started)
├── tests/           # smoke, integration, differential, fixtures, scenarios
├── scripts/         # bootstrap, pin verification, native build, smoke
├── config/pins.toml # pinned source revisions
└── docs/            # the dossier
```

## Architectural decision in one paragraph

The Rust application **owns the product**: GPUI, sessions, CLI, MCP, tests, artifacts and long-lived debugger interfaces. Each emulator instance is a **separate OS process** running a deliberately modified Azahar C++ core through a **native worker frontend**, preserving existing CPU/kernel/graphics/audio functionality. Azahar is tracked in a **single fork with two enduring branches**: `integration` (upstream-minimal native worker hooks) and `experimental` (deeper instrumentation, accuracy changes). The worker and UI use versioned IPC. Hardware accuracy is pursued using separately validated components and physical-3DS experiments, never by declaring host frame time to be hardware time.

## Guarantees this dossier does not make

- No emulator has been built. The native worker is a protocol stub; the Rust side is a shell.
- The Azahar core has **not** been compiled on this host. The pinned commit is recorded in `config/pins.toml` and checked by `scripts/verify-pins.ps1`; do not assume code paths stay stable.
- GPUI is pre-1.0: the commit is pinned and the smallest UI shell is compiled before any detail of its image API is adopted.
- A `Headless` window enum is **not** proof that GPU readback, audio and applet integration work end to end without added code.
- Citra/Azahar accuracy is not equivalent to hardware timing accuracy.
- GPL/subcomponent licenses need review before distributing a combined application; no Nintendo firmware or keys are included.

## Non-negotiable project tests

A test counts only when it is reproducible with fixed input, a known source/build hash, isolated storage and a bounded stop condition. Keep stock Azahar as an independent behavior baseline, and a physical Old 3DS as the authority for hardware claims. A trace must label **guest behavior**, **host performance**, and **estimated hardware timing** separately.

## Project naming

Working name: **CTR emulator** (placeholder; rename the repository/project before publication). All examples use `ctr`/`ctr-worker` as placeholders. Do not build a protocol dependency on the name.
