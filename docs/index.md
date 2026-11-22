# Lotus — documentation index

Start here. **Using LOTUS?** Read the [37 — user guide](37-user-guide.md):
install, the window, controllers, settings, MCP, CLI and troubleshooting.
**New agent?** Read `AGENTS.md`, this page,
[33 — Wave 3 retrospective and handoff](33-wave-3-retrospective.md) (current
state, open items), [34 — performance and issue triage](34-performance-and-issue-triage.md)
(post-Wave 3 fixes and measurements), [35 — release notes](35-release-notes.md) (what each release changed) and [27 — build and run](27-build-and-run.md) (build,
acceptance, portable ZIP). Terms are defined once in the [glossary](glossary.md); documents
link to it instead of redefining them. Each topic below points at exactly one
canonical document — when writing or updating docs, link rather than duplicate.

## What Lotus is

A **developer-first 3DS emulator platform** for writing and debugging homebrew
with agents. Today it is an instrumentation, debugging and MCP wrapper around
modified Azahar: a Rust product (GPUI desktop, CLI, MCP, scenario runner,
session supervisor) that launches isolated native worker processes, each an
emulated Old 3DS. The eventual goal is to replace the Azahar-derived workers
with a more hardware-accurate in-house emulator. Contract, users and success
metrics: [01 — product contract](01-product-contract.md).

## What Lotus is not

Not "the most accurate 3DS emulator", not a Citra reskin, not a new hardware
model yet. Bootstrap non-goals (no full HLE rewrite, no custom JIT, no
in-process multi-instance, no LAN listeners, no synchronized multiplayer):
[01, non-goals](01-product-contract.md). Why an independent UI does not make it
accurate: [11, the critical distinction](11-accuracy-research.md). Current
build makes no accuracy claim: [20](20-native-baseline.md).

## Lotus and Azahar

Azahar supplies the initial machine; Lotus selectively modifies it through
**one fork with two branches** — `integration` (minimal native worker hooks)
and `experimental` (tracing, debugger, accuracy experiments) — plus a detached
`stock` checkout as a control. Branch policy and invariants:
[03](03-azahar-branches.md). Planned source edits:
[16 — patch blueprint](16-azahar-patch-blueprint.md). Currently applied
patches and baselines: [20](20-native-baseline.md).

## What is CITnLO

CITnLO (CITra n LOtus) is the **backend name for the `experimental` branch**:
debugger controls, tracing, rewind/reverse execution, GPU capture and movies
(Wave 3). AZAHAR is the `integration` backend name, with minimal hooks and
frontend adapters. The branches have separate pins; neither carries accuracy
experiments yet. See
[glossary → CITnLO](glossary.md), [03](03-azahar-branches.md),
[20](20-native-baseline.md).

## How it works internally

- **Process graph and ownership boundaries** — desktop/CLI/MCP → Rust session
  supervisor → per-instance workers; shared-memory frames, artifact store:
  [02](02-system-architecture.md).
- **Native Azahar worker** — headless `EmuWindow`, emulation thread, control
  queue: [04](04-azahar-native-worker.md); what's actually merged:
  [21](21-native-control.md).
- **IPC / session contract** — length-prefixed pipes, epochs, safe points:
  [06](06-ipc-and-session-contract.md), [18](18-control-pipes.md).
- **GPUI desktop** — screens, input, sessions: [05](05-gpui-desktop.md),
  [24](24-desktop.md).
- **Driving LOTUS** — headless or windowed, one shared supervisor and MCP
  surface; primary sessions on persistent cards, disposable secondaries,
  sidebar and gauges: [30](30-sessions-and-interface.md). For settings scopes,
  compatibility and schema coverage, see [the Settings registry](30-sessions-and-interface.md#settings-registry-50).
- **Multi-instance and storage** — one process per console, private roots:
  [10](10-multi-instance-and-storage.md).
- **Debugging** — CPU/kernel/memory: [07](07-debugger-core.md); GPU capture
  and tracing: [08](08-gpu-and-trace.md); MCP and scenarios:
  [09](09-mcp-and-scenarios.md), [22](22-scenario-runner.md),
  [23](23-local-mcp.md).
- Workspace layout: `crates/` (protocol, session, trace, debugger, runner,
  hardware, accuracy, mcp, cli, desktop), `workers/` (azahar, native, oracle),
  `vendor/azahar` (pinned source).

## Future plans

- **Milestones M0–M6** — exit gates from reproducible sources to the first
  hardware-verified accuracy gain: [13](13-build-test-release.md).
- **Tickets T00–T47 in four waves** — dependency-ordered plan with status:
  [15](15-implementation-tickets.md); delivered so far:
  [25 — wave status](25-wave-status.md).
- **Wave 3 — comprehensive agentic debugger** — decisions, Azahar parity
  matrix and milestones 3.0–3.4: [29](29-wave-3-plan.md); implemented,
  evidence in [31](31-wave-3-progress.md), handoff in
  [33](33-wave-3-retrospective.md).
- **Accuracy research path** — phases A–E, from cataloging Azahar's inherited
  assumptions to an independent PICA200 model and real firmware:
  [11](11-accuracy-research.md).
- **Hardware validation** — physical Old 3DS microbenchmarks and evidence
  standards: [12](12-hardware-validation.md).
- Remaining acceptance gaps (two-DPI interaction, audio playback, other
  platforms): [25](25-wave-status.md); visible rendering and input were checked
  by screenshot in [24](24-desktop.md).

## Building on Windows

Primary toolchain: Rust 1.98.1 `x86_64-pc-windows-gnu` (GCC/MinGW-w64),
CMake + Ninja. MSVC and other platforms are untested.

Step-by-step instructions (tools, Rust, both native runtimes, portable ZIP, CI,
release and recovery): **[27 — build and run](27-build-and-run.md)**. Pins live
in `config/pins.toml`; each native source gets its own checkout, build directory
and manifest. Background: [17](17-scaffold-and-pins.md), [13](13-build-test-release.md).
Opt-in native tests: [21, reproduction](21-native-control.md).

## Document map

| Group | Documents |
| --- | --- |
| Start | [index](index.md) · [glossary](glossary.md) · [37](37-user-guide.md) user guide |
| Product | [01](01-product-contract.md) contract · [15](15-implementation-tickets.md) tickets · [29](29-wave-3-plan.md) Wave 3 plan |
| Architecture | [02](02-system-architecture.md) system · [04](04-azahar-native-worker.md) worker · [05](05-gpui-desktop.md) GPUI · [06](06-ipc-and-session-contract.md) IPC · [10](10-multi-instance-and-storage.md) instances |
| Backend | [03](03-azahar-branches.md) branches · [16](16-azahar-patch-blueprint.md) patches · [20](20-native-baseline.md) baseline |
| Debugging | [07](07-debugger-core.md) CPU/kernel · [08](08-gpu-and-trace.md) GPU · [09](09-mcp-and-scenarios.md) MCP |
| Research | [11](11-accuracy-research.md) accuracy · [12](12-hardware-validation.md) hardware · [14](14-sources-and-evidence.md) sources |
| Build & process | [13](13-build-test-release.md) build/CI · [17](17-scaffold-and-pins.md) pins |
| Implementation records | [18](18-control-pipes.md) pipes · [19](19-screen-spike.md) screen spike · [21](21-native-control.md) native control · [22](22-scenario-runner.md) scenarios · [23](23-local-mcp.md) MCP · [24](24-desktop.md) desktop · [25](25-wave-status.md) wave status · [26](26-portable-release.md) portable release · [27](27-build-and-run.md) build and run · [28](28-delivery-retrospective.md) retrospective · [30](30-sessions-and-interface.md) sessions and interface · [31](31-wave-3-progress.md) Wave 3 evidence · [32](32-debugger-command-parity.md) debugger command parity · [33](33-wave-3-retrospective.md) Wave 3 retrospective · [34](34-performance-and-issue-triage.md) performance and issue triage · [35](35-release-notes.md) release notes |
| Desktop debugger | [39](39-desktop-debugger-coverage.md) GUI and MCP coverage · [40](40-desktop-debugger-acceptance.md) acceptance evidence |
| UI staging | [38](38-silvestris-adoption.md) opt-in adapter and rollout gates |

## Documentation rules

- Numbers `01`–`38` are stable identifiers; never renumber, never duplicate
  another document's content — link to it or to the glossary.
- New documents get the next number and a row in the map above, plus any new
  terms in the glossary.
- Keep claims labeled with the evidence vocabulary in the glossary; AGENTS.md
  governs everything not covered here.
