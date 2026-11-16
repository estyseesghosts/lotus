# Glossary

Single source of truth for project terms. Other documents link here instead of
redefining them. Numbered documents (`01`–`30`) keep their numbers as stable
identifiers regardless of directory; prose references like "document 18" remain
valid.

## Names

**LOTUS**
The product: a developer-first 3DS emulator platform for homebrew debugging
with agents — Rust supervisor, GPUI desktop, CLI, MCP adapter and scenario
runner, driving isolated native worker processes. LOTUS is also the name
reserved for the future in-house emulation backend that replaces the
Azahar-derived workers. Canonical: [01](01-product-contract.md),
[02](02-system-architecture.md), [21](21-native-control.md).

**AZAHAR**
Backend name for a worker built from the `integration` branch of the Azahar
fork: upstream-minimal native hooks, no accuracy experiments. Native backend
identifier `azahar_native_software`. Canonical: [03](03-azahar-branches.md),
[21](21-native-control.md).

**CITnLO** (CITra n LOtus)
Backend name for a worker built from the `experimental` branch: tracing,
debugger controls and accuracy experiments live here. Both branches currently
share one pin, so AZAHAR and CITnLO builds are identical today; no accuracy
experiments exist yet. Canonical: [03](03-azahar-branches.md),
[17](17-scaffold-and-pins.md), [20](20-native-baseline.md).

## Branches and sources

**integration** — upstream-tracking branch carrying only the minimal native
worker/front-end hooks, adapters and proven fixes. Regular upstream merges;
no guest-timing changes without evidence. [03](03-azahar-branches.md)

**experimental** — derived from `integration`; structured tracing, debugger
controls, snapshot and timing experiments. Never merged wholesale into
`integration`. [03](03-azahar-branches.md)

**stock** — detached, unmodified upstream checkout used as an independent
control/oracle, not a maintained branch. [03](03-azahar-branches.md),
[20](20-native-baseline.md)

**Pin** — immutable commit SHA recorded in `config/pins.toml`; builds refuse
moving or dirty inputs. [17](17-scaffold-and-pins.md)

## Runtime concepts

**Profile** — the settings a session is created with (console model, CPU
backend, renderer, DSP mode, system/CFG values, determinism options); its
hash is recorded in every manifest and report. Old 3DS is the default.
[29](29-wave-3-plan.md), T24 in [15](15-implementation-tickets.md)

**Apply class** — when a setting may change: `boot`, `restart`, `live`,
`renderer` or `cfg-savedata`. Each class is justified by the source line
that consumes the setting. [29](29-wave-3-plan.md)

**Debug lease** — exclusive right to control execution (pause, step,
continue) of one session, arbitrated between desktop, MCP and GDB clients;
the debugger counterpart of the input lease. T25 in
[15](15-implementation-tickets.md)

**RSP server** — the Rust GDB remote serial protocol server (`crates/gdb`),
loopback-only, one opt-in port per session. Replaces Azahar's stub
(ADR-008). T26 in [15](15-implementation-tickets.md)

**Primary session** — a session that mounts a card: guest SD/NAND reads and
writes are real and persist. One per card at a time.
[30](30-sessions-and-interface.md)

**Secondary session** — a disposable session in a private scratch root, empty
or a point-in-time copy of one card; its guest data is deleted when it ends.
Shown nested under that card. [30](30-sessions-and-interface.md)

**Card** — the persistent guest storage of a primary session,
`<storage>/cards/<name>/` holding Azahar's `sdmc/`, `nand/` and `sysdata/`.
Not a *profile* (settings). [30](30-sessions-and-interface.md)

**Pump** — the shared supervisor loop that refreshes gauges and re-issues
bounded runs for sessions that are playing, skipping busy sessions.
[30](30-sessions-and-interface.md)

**Headless / windowed** — the two ways to drive LOTUS: `lotus-mcp`, or
`lotus-desktop` (optionally `--mcp`) with the same tools over the same
sessions. [30](30-sessions-and-interface.md)

**Worker** (`ctr-worker`) — one OS process per emulated console; a headless
native C++ frontend over Azahar's `Core::System`, no Qt/SDL/Libretro.
[04](04-azahar-native-worker.md)

**Supervisor / session** — Rust process manager owning worker lifetimes,
private storage roots, IPC endpoints and quotas. [02](02-system-architecture.md),
[10](10-multi-instance-and-storage.md)

**Safe point** — the emulation-thread drain point (`ProcessPendingWork` plus
`NotifyPendingWork`) where queued control/debug commands execute; no thread
reads mutable emulated state elsewhere. [06](06-ipc-and-session-contract.md),
[16](16-azahar-patch-blueprint.md)

**Epoch** — monotonically increasing state-generation counter; references from
an older epoch are rejected. Incremented on load/reset/restore.
[06](06-ipc-and-session-contract.md)

**Frame ring / LTRF slots** — three reusable per-screen file slots carrying a
32-byte header and native BGRA8 pixels; leases invalidated by reset/crash.
[21](21-native-control.md)

**Scenario** — declarative bounded run: start state, input ownership,
checkpoints, stop condition, capture policy, assertions. [09](09-mcp-and-scenarios.md),
[22](22-scenario-runner.md)

## Planning vocabulary

**Wave / ticket** — dependency-ordered implementation plan; tickets `T00`–`T23`,
scoped `INT` (integration), `EXP` (experimental), `RUST` (LOTUS).
[15](15-implementation-tickets.md)

**Milestone M0–M6** — release exit gates, from reproducible sources to the
first hardware-verified accuracy gain. [13](13-build-test-release.md)

**Accuracy phases A–E** — research path from cataloging inherited assumptions
to real firmware execution. [11](11-accuracy-research.md)

## Evidence and time

**Evidence labels** — `verified compiled`, `verified emulator`,
`verified hardware`, `source inspected`, `proposed`. Required on every claim;
no unverifiable check marks. [14](14-sources-and-evidence.md), AGENTS.md

**Time domains** — guest ticks (emulator), host monotonic ns, and measured
hardware times are never interchangeable. [02](02-system-architecture.md),
[12](12-hardware-validation.md)

**Old 3DS** — default target machine. New 3DS is a later, explicitly labeled
profile. [01](01-product-contract.md), [11](11-accuracy-research.md)
