# Build, test, package and release (Windows x64)

Authoritative commands. CI ([windows.yml](../.github/workflows/windows.yml)) runs
the same steps; **verified compiled** there (run 43795955719, 2026-11-16). The
local-checkout variant below differs only in paths. MSVC, Linux and macOS are
untested. Rationale and release contents: [26](26-portable-release.md).

## 0. Just run it

Extract `LOTUS-v0.0.1-windows-x64.zip` from the GitHub release into a writable
folder (spaces are fine). Run `LOTUS.cmd` (CITnLO) or `LOTUS-AZAHAR.cmd`; pick
your own `.3dsx`. No installer, registry or toolchain. Data stays in `data/`;
keep it when upgrading. Verify the download against `SHA256SUMS.txt`. Binaries
are unsigned, so SmartScreen may warn.

## 1. Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Windows | 11 x64 | Direct3D 11 |
| Git | any recent | submodule support |
| Python | 3.11+ (CI: 3.14) | `tomllib`, `hashlib.file_digest` |
| rustup | any | installs the pin in `rust-toolchain.toml` (`1.98.1-x86_64-pc-windows-gnu`) automatically |
| `gh` | optional | CI dispatch and logs |

GCC, CMake, Ninja and the FXC shader compiler are **not** taken from the system.
The next step downloads hash-verified copies into a private directory.

## 2. Checkout and tools

```powershell
git clone --recurse-submodules https://github.com/estyseesghosts/lotus.git
cd lotus
python scripts/setup-build-tools.py build/tools    # WinLibs GCC 16.2.0 r2 (+CMake/Ninja), SDK 10.0.28000.2705 fxc
$env:PATH = "$PWD\build\tools\winlibs\mingw64\bin;$env:PATH"
$env:GPUI_FXC_PATH = "$PWD\build\tools\sdk\c\bin\10.0.28000.0\x64\fxc.exe"
scripts/bootstrap.ps1      # pinned toolchain, submodules, Cargo fetch, pin check (idempotent)
```

Do this in every new shell: PATH and `GPUI_FXC_PATH` are not persisted. The
submodule fetch is large. Without FXC, GPUI's `build.rs` fails in `--release`.
Pins: `config/pins.toml`; check with `scripts/verify-pins.ps1`.

## 3. Rust (desktop, CLI, MCP)

```powershell
scripts/smoke.ps1                                   # fast: build, tests, fmt, clippy, CLI, pins; no ROM/GPU/display
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --release --bins               # target/release: lotus-desktop, ctr-cli, lotus-mcp
cargo test --locked --release --workspace
```

Always pass `--locked`. Native tests are `#[ignore]` by default; an ignored run
is not a pass (see §5).

Run headless with `lotus-mcp host.json`, windowed with `lotus-desktop host.json`,
or windowed and agent-driven with `lotus-desktop --mcp host.json` (host config:
[23](23-local-mcp.md); sessions: [30](30-sessions-and-interface.md)).

## 4. Native runtimes (AZAHAR and CITnLO)

`build-azahar.ps1` refuses a source whose `HEAD` differs from `config/pins.toml`
or that is dirty, and refuses a CMake cache configured for another source root.
Use **one source checkout and one build directory per branch**; never share a
cache. Mirror CI so the `vendor/azahar` submodule stays untouched:

```powershell
Import-Module ./scripts/Pins.psm1
$pins = Read-Pins config/pins.toml
foreach ($branch in 'integration','experimental') {
    $src = "build/native-source-$branch"; $out = "build/native-$branch"
    git clone --no-checkout $pins['azahar']['fork'] $src
    git -C $src checkout --detach $pins['azahar']["${branch}_commit"]
    git -C $src submodule update --init --recursive --depth 1
    ./scripts/build-azahar.ps1 -Branch $branch -AzaharRoot $src -BuildDirectory $out -Parallel 2
    & "./$out/bin/Release/ctr-worker.exe" --self-test
    python tests/test_native_protocol.py "$out/bin/Release/ctr-worker.exe"
}
```

`integration` is AZAHAR, `experimental` is CITnLO. At the v0.0.1 pins both point
to the same commit, so they differ only by `-DCTR_BACKEND_NAME`. Flags (software
renderer, Qt/SDL/Libretro/Vulkan/OpenGL off, LTO off) are fixed in the script and
recorded in `lotus-build-manifest.json`. First clean build: about 35–45 min on a
GitHub runner. `-Branch stock -Oracle` builds the unmodified-core oracle used for
screen comparison ([20](20-native-baseline.md)).

## 5. Native acceptance (needs your own homebrew)

```powershell
scripts/test-native.ps1 -Worker build/native-integration/bin/Release/ctr-worker.exe -Fixture C:\abs\path\t03.3dsx
python tests/test_mcp_native.py <lotus-mcp.exe> <host-config> <asset-handle>   # see 21/23
python tests/test_windowed_mcp.py <lotus-desktop.exe> <host-config> <asset-handle> [hold-seconds]   # see 30
```

Use absolute fixture paths. When comparing against stock, use the *same* absolute
path in both; launch-path changes the pixels ([20](20-native-baseline.md)). No
ROMs, firmware or captures are committed or uploaded.

## 6. Portable ZIP

```powershell
python scripts/package-windows.py rust --binary-dir target/release --metadata build/metadata.json --output build/rust-artifact --dll-dir build/tools/winlibs/mingw64/bin
python scripts/package-windows.py runtime --build build/native-integration --output build/azahar-artifact --dll-dir build/tools/winlibs/mingw64/bin
python scripts/package-windows.py runtime --build build/native-experimental --output build/citnlo-artifact --dll-dir build/tools/winlibs/mingw64/bin
python scripts/package-windows.py assemble --rust build/rust-artifact --azahar build/azahar-artifact --citnlo build/citnlo-artifact --output build/dist --version 0.0.1
python tests/test_portable.py build/dist/LOTUS-v0.0.1-windows-x64.zip
python scripts/archive-source.py build/dist/LOTUS-v0.0.1-source.zip
```

`build/metadata.json` is `cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-gnu`.
`assemble` fails on unresolved non-system DLLs or MSYS runtime imports.
`test_portable.py` relocates the ZIP to a path with spaces, with a Windows-only
PATH, and checks hashes, DLL closure, launchers, CLI, MCP and native startup.
`--version` must equal the workspace version in `Cargo.toml`.

## 7. CI and release

Workflow `Windows portable`: jobs `rust`, `native (integration|experimental)`
(parallel, 90 min cap), then `package`, then `release`.

| Trigger | Effect |
| --- | --- |
| push to `main`, PR | build and test; no release |
| tag `v*` | full run, then release from the tag; tag must equal `v<Cargo version>` |
| manual dispatch, `publish_release=true` | full run, then creates tag and release `v<Cargo version>` at the tested SHA |

```powershell
gh workflow run windows.yml --ref main -f publish_release=true
gh run watch <id> --exit-status
gh release view v<version> --json assets
```

A release is never overwritten; to redo one, bump the version or delete it by
hand first. Only the `release` job has `contents: write`. A new push to a ref
cancels its in-progress run (concurrency group); tag runs are not cancelled.

**Packaging-only recovery** (no recompile, about 10 min): add
`-f reuse_build_run=<run-id>` to the dispatch. `scripts/validate-build-reuse.py`
requires that run to be a completed `windows.yml` run on `main` of this repo, with
`rust` and both native jobs green, an ancestor of the current commit, and no
changes outside `tests/test_portable.py`, the workflow, `scripts/package-windows.py`,
`scripts/archive-source.py`, `scripts/validate-build-reuse.py`, `SHA256SUMS.txt`,
`packaging/` and `docs/`. Anything else means rebuild. Artifacts expire after 7
days, so a reuse run must be recent.

## 8. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `GPUI_FXC_PATH`/shader error in `--release` | set it (§2) |
| `Source pin mismatch` / `Build inputs must be committed and clean` | checkout the pinned commit; do not edit the source tree |
| `Cache belongs to a different source tree` | use a separate `-BuildDirectory` per source |
| Missing/odd DLL at package time | build with the WinLibs GCC on PATH, not MSYS2 |
| Worker fails to start on a `\\?\` path | already normalized by the session adapter; pass ordinary absolute paths |
| Tests "pass" with nothing run | native tests are opt-in; use `scripts/test-native.ps1` |
| Release job skipped after green `package` | fixed in `7f9dc3e`; `release` needs `always()` when build jobs are skipped |
| Linker warning about duplicate manifest/resource | GPUI's, known and harmless on GNU |
