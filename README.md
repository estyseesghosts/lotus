# Independent 3DS emulator — engineering dossier

**Status:** design package, not an implementation. **Research checked:** 2026-11-15. **Primary target:** original 2011 3DS; New 3DS later. **Host:** Windows first, macOS/Linux next. **UI:** Rust + Zed GPUI. **Initial emulation:** modified **Azahar native core**, **not Libretro**. **Long-term aim:** measurable hardware accuracy beyond Azahar/Citra.

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
17. [AGENTS.md](AGENTS.md): mandatory rules for implementer/agents.

Additional artifacts: [`specs/`](specs/) contains *proposed* protocol and scenario shapes; [`templates/`](templates/) contains review/experiment record formats. Examples are not compiled or tested. Protocol names and proposed code are not claims about existing Azahar APIs.

## Architectural decision in one paragraph

The Rust application **owns the product**: GPUI, sessions, CLI, MCP, tests, artifacts and long-lived debugger interfaces. Each emulator instance is a **separate OS process** running a deliberately modified Azahar C++ core through a **native worker frontend**, preserving existing CPU/kernel/graphics/audio functionality. Azahar is tracked in a **single fork with two enduring branches**: `integration` (upstream-minimal native worker hooks) and `experimental` (deeper instrumentation, accuracy changes). The worker and UI use versioned IPC. Hardware accuracy is pursued using separately validated components and physical-3DS experiments, never by declaring host frame time to be hardware time.

## Guarantees this dossier does not make

- No binary has been built or run for this dossier.
- The current upstream `master` was inspected by source reads; **record a full Azahar commit hash before opening the first implementation PR**. Do not assume code paths stay stable.
- GPUI is pre-1.0: pin a commit and compile the smallest UI spike before adopting details of its image API.
- A `Headless` window enum is **not** proof that GPU readback, audio and applet integration work end to end without added code.
- Citra/Azahar accuracy is not equivalent to hardware timing accuracy.
- GPL/subcomponent licenses need review before distributing a combined application; no Nintendo firmware or keys are included.

## Non-negotiable project tests

A test counts only when it is reproducible with fixed input, a known source/build hash, isolated storage and a bounded stop condition. Keep stock Azahar as an independent behavior baseline, and a physical Old 3DS as the authority for hardware claims. A trace must label **guest behavior**, **host performance**, and **estimated hardware timing** separately.

## Project naming

Working name: **CTR emulator** (placeholder; rename the repository/project before publication). All examples use `ctr`/`ctr-worker` as placeholders. Do not build a protocol dependency on the name.
