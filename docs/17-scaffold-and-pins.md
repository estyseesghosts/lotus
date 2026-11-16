# Build and dependency pins

LOTUS uses native process workers: **AZAHAR** (`integration`) and **CITnLO**
(`experimental`). The future replacement backend is **LOTUS**. The two Azahar
branches currently share integration code; no accuracy experiments exist yet.

Immutable revisions live in [config/pins.toml](../config/pins.toml); Cargo.lock
pins Rust dependencies and vendor/azahar pins native source. Upstream control:
`11eceda5e0e682a1f44e396bf954a1b9137097ca`. GPUI/Zed:
`6b720973027999bae402df23d469026f4cced988`. Rust:
`1.98.1-x86_64-pc-windows-gnu`.

## Build

This Windows host uses GCC/MinGW-w64 16.2, CMake 4.4.3 and Ninja. MSVC and other
host platforms are untested. GPUI links against Windows D3D import libraries.
Its existing `.rsrc merge failure: multiple non-default manifests` warning remains
visible; visual acceptance is deferred at the user's request.

```powershell
scripts/bootstrap.ps1
scripts/smoke.ps1
scripts/build-azahar.ps1 -Branch integration
scripts/build-azahar.ps1 -Branch experimental
scripts/build-azahar.ps1 -Branch stock -Oracle -BuildDirectory build/azahar-stock
```

Native builds require sibling worktrees `azahar-stock`, `azahar-integration`,
`azahar-experimental`, or an explicit `-AzaharRoot`. Each must be clean at its
manifest pin, with recursive submodules initialized. Existing worktrees were
created from the supplied parent azahar clone; see [20](20-native-baseline.md).
The script rejects moving/dirty inputs and a cache belonging to another tree.
Each flavor has its own cache and `lotus-build-manifest.json` with SHA/flags/host.
It builds native `ctr-worker` directly. `workers/azahar` is a retired scaffold
stub and is not the runtime target. Local native commits must be published or
transferred with their Git objects before a different machine can fetch the pins.

The initial native matrix is Release, software renderer, Qt/SDL/Libretro off,
host audio devices off, GDB compiled but disabled, LTO off. See script for every
flag. There are no runtime dependency downloads and no bundled Nintendo assets.

## Run

Create an ignored local host configuration as shown in [23](23-local-mcp.md).
The desktop and MCP consume the same configuration and native session API:

```powershell
cargo run -p lotus-desktop --bin lotus-desktop -- build/local-host.json
cargo run -p lotus-mcp --bin lotus-mcp -- build/local-host.json
cargo run -p lotus-cli --bin ctr-cli -- scenario run specs/native-smoke.yaml build/azahar-integration-native/bin/Release/ctr-worker.exe build/runs 3
```

Both applications default to an empty host without configuration. Explicit
approved asset handles avoid arbitrary-path MCP access. The desktop can create
four sessions, select/dock screens, run/pause/reset/stop and route keyboard/touch.
No real audio device is opened; the native DSP sink drains a bounded muted ring.

## Verification and source map

`cargo test --workspace`, rustfmt and Clippy cover the Rust workspace. Asset
tests are explicitly opt-in using `LOTUS_NATIVE_WORKER` and `LOTUS_FIXTURE`:

```powershell
scripts/test-native.ps1 -Worker build/azahar-integration-native/bin/Release/ctr-worker.exe -Fixture ../t03.3dsx
```

Current checks and acceptance gaps: [25](25-wave-status.md). Interfaces and
primary source paths: [18](18-control-pipes.md), [21](21-native-control.md),
[22](22-scenario-runner.md), [23](23-local-mcp.md), [24](24-desktop.md).
The original FFI structs remain scaffold types; the implemented IPC is bounded
JSON control plus file-ring BGRA frames. Neither implies hardware accuracy.
