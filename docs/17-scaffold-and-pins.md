# 17 — Scaffold, pinned dependencies and how to build it

**Status:** scaffolded and verified on the development host. **Date:** 2026-11-15. **Host:** Windows
(x86_64), GNU toolchain.

This document describes the repository skeleton that now exists, the exact revisions everything is
pinned to, how to reproduce the environment, and what is *not* built yet.

## 1. Pinned revisions

All pins live in [`config/pins.toml`](../config/pins.toml) and are checked by
[`scripts/verify-pins.ps1`](../scripts/verify-pins.ps1).

| Dependency | Source | Revision |
| --- | --- | --- |
| Azahar (upstream) | `https://github.com/azahar-emu/azahar.git` | `11eceda5e0e682a1f44e396bf954a1b9137097ca` |
| Azahar fork | `https://github.com/estyseesghosts/azahar.git` | branch `integration` and branch `experimental`, both currently at the upstream commit above |
| GPUI | `https://github.com/zed-industries/zed.git` (`crates/gpui`) | `6b720973027999bae402df23d469026f4cced988` (`gpui` 0.2.2) |
| Rust | rustup toolchain | `1.98.1-x86_64-pc-windows-gnu` |

Notes:

- The Azahar fork exists so that the two long lived branches described in
  [03 — Two-branch Azahar strategy](03-azahar-branches.md) can live in one place. Both branches start
  from the same upstream commit; the `experimental` branch has not diverged yet.
- GPUI is consumed as a git dependency with an exact `rev`. It is **not** the crates.io release: the
  crates.io `gpui` 0.2.2 does not carry the same tree, and the pinned commit is the only revision the
  project builds and tests against.
- The pinned GPUI commit requires Rust 1.98.1, which is the version Zed itself pins at that commit.
  The toolchain is therefore pinned to the same version.

## 2. Repository layout

```
lotus/
├── Cargo.toml            # workspace, shared dependency versions
├── Cargo.lock            # committed: exact versions of every crate
├── rust-toolchain.toml   # pinned toolchain, explicit host triple
├── config/pins.toml      # pinned source revisions
├── crates/
│   ├── protocol/         # versioned worker protocol, FFI structs, capabilities
│   ├── session/          # worker process lifecycle, per session storage isolation
│   ├── trace/            # bounded structured event capture
│   ├── debugger/         # breakpoints, memory requests, register snapshots
│   ├── runner/           # scenario schema, validation, execution limits
│   ├── hardware/         # subsystem, timing domain and evidence vocabulary
│   ├── accuracy/         # frame and state differential comparison
│   ├── mcp/              # MCP server (adapter to the session supervisor)
│   ├── cli/              # `ctr-cli`, headless entry point
│   └── desktop/          # GPUI application shell
├── workers/
│   ├── azahar/           # C++ worker: ABI header, stub entry point, CMake
│   └── native/           # future Rust emulation backend (not started)
├── vendor/azahar/        # pinned Azahar checkout, git submodule
├── tools/hardware-tests/ # devkitPro 3DS ground truth programs (not started)
├── tests/                # smoke, integration, differential, fixtures, scenarios
├── scripts/              # bootstrap, pin verification, native build, smoke
├── specs/                # proposed protocol and scenario shapes
└── docs/                 # this dossier
```

## 3. What is verified right now

| Item | Label |
| --- | --- |
| `cargo build --workspace` | **verified compiled** |
| `cargo test --workspace` (51 tests) | **verified compiled** |
| `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets` | **verified clean** |
| `ctr-cli version`, `ctr-cli pins`, `ctr-cli scenario validate` | **verified compiled** |
| Vendored Azahar checkout with all of its submodules | **verified compiled** (source present, not built) |
| GPUI desktop shell | **verified compiled** and **launched**: the process starts, opens a window and stays running; the window contents were not inspected |
| MCP server | **verified compiled**: `initialize`, `tools/list` and `tools/call` answered over stdio |
| Native worker | **verified compiled**: the entry point is a protocol stub, no Azahar core is linked |
| Azahar core build on this host | **not built**: see section 5 |

The Rust workspace builds against the pinned GPUI commit and links with the GNU toolchain. The
protocol ABI is defined once, in Rust, and mirrored in `workers/azahar/include/lotus_protocol.h`; both
sides assert the same struct sizes (`FrameHeader` 48 bytes, `WorkerHello` 64 bytes, `EventHeader` 40
bytes), so the ABI cannot drift silently.

## 4. Reproducing the environment

```powershell
# Toolchain, vendored sources, Cargo dependencies, pin verification.
scripts\bootstrap.ps1

# Fast checks.
scripts\smoke.ps1
```

`bootstrap.ps1` is idempotent. It installs the pinned toolchain with rustup, updates the Azahar
submodule and its own submodules, runs `cargo fetch` and then `verify-pins.ps1`.

`verify-pins.ps1` fails when the vendored checkout, the recorded submodule pointer, the GPUI entry in
`Cargo.lock` and the active toolchain drift away from `config/pins.toml`.

## 5. Toolchain situation on this host

This machine has **no MSVC installation**: no Visual Studio, no Windows SDK and no `link.exe` from
MSVC. `rustup` therefore resolves `channel = "1.98.1"` to the MSVC host by default, which cannot link
anything here.

The project therefore pins the toolchain with an explicit host triple,
`1.98.1-x86_64-pc-windows-gnu`, and links with the MinGW-w64 toolchain that is installed. The
import libraries GPUI needs on Windows (`d3d11`, `dxgi`, `dwrite`, `d3dcompiler`) are present in that
toolchain, so the GPUI shell links and runs.

Consequences to keep in mind:

- The MSVC toolchains that rustup lists are installed but cannot link. Do not "fix" a build failure by
  switching to them without installing the MSVC build tools.
- A future switch to MSVC means changing `channel` in `rust-toolchain.toml`, reinstalling the
  toolchain and re-running `cargo build` from a clean `target` directory.
- Linking `lotus-desktop` prints one linker warning: `.rsrc merge failure: multiple non-default
  manifests`. It comes from GPUI's own `windows-manifest` feature on Windows and does not affect the
  binary. It is left visible rather than silenced.

## 6. What is deliberately not built yet

- **The Azahar core.** Building Azahar on Windows needs its own toolchain (MSYS2/MinGW-w64 or MSVC),
  CMake, Ninja and the Vulkan SDK for the accelerated backend. `scripts/build-azahar.ps1` configures
  the worker against `vendor/azahar` with one build directory per branch, but the native core itself
  has not been compiled on this host.
- **The worker frontend.** `workers/azahar/src/worker_main.cpp` prints a handshake and exits. It
  deliberately contains no emulation code, no GPUI, no MCP and no agent concepts.
- **The GPUI application.** `crates/desktop` is a window shell with an instance list and two screen
  placeholders. The screens are not yet fed by a worker.

## 7. Rules that follow from this scaffold

1. A pin is moved in `config/pins.toml`, in `Cargo.lock` and in the submodule pointer **in the same
   commit**, with a note about why.
2. The `integration` and `experimental` Azahar branches never share a CMake build directory.
3. The worker stays free of GPUI, MCP and agent concepts.
4. Every number in a report declares its time domain: host milliseconds, virtual guest ticks or
   estimated hardware cycles.
5. Anything that is not compiled, run on hardware or inspected in source is labelled **proposed**.

## 8. Next tickets

| Ticket | Where |
| --- | --- |
| Build the Azahar core on this host and record the exact CMake flags | `docs/13-build-test-release.md` |
| Replace the worker stub with a minimal `EmuWindow` frontend | `docs/04-azahar-native-worker.md`, `docs/16-azahar-patch-blueprint.md` |
| Implement the shared memory frame ring and the event stream | `docs/06-ipc-and-session-contract.md` |
| Feed the GPUI screens from the frame ring | `docs/05-gpui-desktop.md` |
| First CLI scenario run against a real worker | `docs/15-implementation-tickets.md` |
