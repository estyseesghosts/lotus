# Build, test, package and release (Windows x64)

Authoritative commands. CI ([windows.yml](../.github/workflows/windows.yml)) runs
the same steps; **verified compiled** there (run 43795955719, 2026-11-16). The
local-checkout variant below differs only in paths. MSVC and macOS are
untested; Linux covers the Rust workspace only
([section 10](#10-linux-rust-workspace)). Rationale and release contents:
[26](26-portable-release.md).

## 0. Just run it

Extract `LOTUS-v0.0.6-windows-x64.zip` from the GitHub release into a writable
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
| rustup | any | `scripts/bootstrap.ps1` installs `1.98.1-x86_64-pc-windows-gnu` and sets it as the directory override; the default MSVC host does not link |
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

`integration` is AZAHAR, `experimental` is CITnLO. Wave 3 pins now differ:
integration carries frontend adapters; experimental adds debugger instrumentation.
Native builds enable software/Vulkan and Cubeb, with Qt/SDL/Libretro/OpenGL,
Azahar's GDB stub and LTO disabled (`-Lto` turns LTO on and is recorded in the
manifest; see [34](34-performance-and-issue-triage.md) before using it). The raw
worker's default profile selects software and Old 3DS; supervisor sessions
(desktop, MCP, CLI) prefer Vulkan with a software fallback ([23](23-local-mcp.md)). Exact flags are recorded in `lotus-build-manifest.json`. First clean build: about 35–45 min on a
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
python scripts/package-windows.py assemble --rust build/rust-artifact --azahar build/azahar-artifact --citnlo build/citnlo-artifact --output build/dist --version 0.0.6
python tests/test_portable.py build/dist/LOTUS-v0.0.6-windows-x64.zip
python scripts/archive-source.py build/dist/LOTUS-v0.0.6-source.zip
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

## 9. Local developer loop (Wave 3 workstation layout)

The owner's workstation keeps sibling checkouts next to `lotus/`:
`../azahar-integration` and `../azahar-experimental` (native development trees,
committed on their branches, then pinned) and `../azahar-stock` (control).
`vendor/azahar` is the integration submodule; pin moves update
`config/pins.toml` and the submodule pointer in one commit. Here the tools live
in `build/toolchains/winlibs/mingw64/bin` (GCC/CMake/Ninja),
`build/windows-sdk/c/bin/10.0.28000.0/x64/fxc.exe` (FXC) and
`build/toolchains/bin` (Arm GNU 15.2 GCC/GDB); a fresh clone uses §2 instead.

```bash
export PATH="$PWD/build/toolchains/winlibs/mingw64/bin:$PATH"
# Fast incremental native rebuild while iterating:
cmake build/azahar-experimental-native          # re-run after native commits: CTR_SOURCE_SHA is set at configure time
cmake --build build/azahar-experimental-native --target ctr-worker --parallel 8
# Before recording evidence or packaging (checks pin, cleanliness; writes lotus-build-manifest.json):
powershell -File scripts/build-azahar.ps1 -Branch experimental -AzaharRoot ../azahar-experimental -BuildDirectory build/azahar-experimental-native -Parallel 8
```

A running `ctr-worker.exe` cannot be relinked on Windows. Settings bindings are
generated: `python tools/generate-settings.py` (see its docstring for apply
classes). Owned fixtures need clang (or `--gcc` devkitARM):
`python tools/build-debug-fixture.py build/fixtures/<name> --fixture <name>`
(debugger, audio, microphone, dsp, peripherals, filesystem, divergence).

Acceptance (outputs go to fresh directories under ignored `runs/`):

| Check | Command |
| --- | --- |
| Native debugger/reverse/filesystem | `python tools/{debugger,reverse,filesystem}_acceptance.py <worker> <fixture.3dsx> <out>` |
| GDB | `python tools/gdb_acceptance.py <arm-none-eabi-gdb> target/debug/ctr-cli.exe <worker> <debugger.3dsx> <out>`; documented client: devkitPro `C:/devkitPro/devkitARM/bin/arm-none-eabi-gdb.exe` (GDB 14.1), CITnLO only |
| Benchmark | `python tools/bench.py --worker NAME=<worker> … --rom <abs 3dsx> --output <fresh>` (see its docstring and [34](34-performance-and-issue-triage.md)) |
| Renderer default | `python tests/test_renderer_default.py <ctr-cli.exe> <worker> <abs 3dsx> <out>` |
| Hidden-start window | `python tests/test_window_visible.py <lotus-desktop.exe> [--mcp]` |
| Focus cycle (#74) | `python tests/test_focus_cycle.py <lotus-desktop.exe> <worker> <content> <out>`; moves real focus for ~30 s |
| Settings | `python tools/settings_acceptance.py <worker> <debugger.3dsx> <gpu.3dsx> <out> [--classes-only]` |
| Overhead / Vulkan | `tools/overhead_acceptance.py`, `tools/vulkan_acceptance.py` (see `--help`) |
| MCP | `python tests/test_mcp_<name>.py <abs lotus-mcp.exe> <abs host.json> <asset handles…> <abs out>` |
| Scenario movies | `python tests/test_scenario_movie.py target/debug/ctr-cli.exe <experimental worker> <3dsx> <out>` |

MCP host configs are JSON (`storage_root`, `allow_control`, `assets`, `worker`;
[23](23-local-mcp.md)); give each run a fresh `storage_root`. Python on
Windows needs absolute paths for the server and config. Results and
retrospective: [31](31-wave-3-progress.md), [33](33-wave-3-retrospective.md).

## 10. Linux (Rust workspace)

Ubuntu 24.04 x86_64, CI [linux.yml](../.github/workflows/linux.yml) (#102).
Desktop, MCP, CLI and mock workers only; native workers and the AppImage are
#103-#109. System packages, one per `-sys` crate in `Cargo.lock`:

| Package | Needed by |
|---|---|
| build-essential, pkg-config, cmake | C builds, pkg-config probes |
| libfontconfig-dev | `yeslogic-fontconfig-sys` (GPUI text) |
| libudev-dev | `libudev-sys` (gilrs controllers) |
| libwayland-dev | `wayland-sys` (GPUI Wayland) |
| libxkbcommon-x11-dev, libx11-xcb-dev | `xkbcommon`, `x11rb` (GPUI X11) |

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
cargo build --locked --release --bin lotus-desktop --bin lotus-mcp --bin ctr-cli
cargo test --locked --release --workspace
python3 tests/test_headless_smoke.py target/release
```

Optional: `apt install mold` and `export RUSTFLAGS="-C link-arg=-fuse-ld=mold"`
links faster; CI does this (#117). `[profile.dev]` keeps line tables only, so
dev targets stay small.

`rust-toolchain.toml` names no host, so the Linux host toolchain is used.
`gpui_platform` gets the `wayland` and `x11` features on Linux; without them
the desktop opens no window and exits 0.

Status: **verified compiled** and headless-tested (unit/integration suite,
CLI and desktop `--help`, both mock workers, MCP initialize) in Docker
ubuntu:24.04, 3 consecutive clean full-suite runs. Windowed desktop on
Wayland/X11/WSLg is untested until #106.

## 11. Linux native workers

`scripts/build-azahar.sh` is `build-azahar.ps1` for Linux (#103): same pin check
against `config/pins.toml`, clean-source check, one build directory per flavor
with a cache-ownership check, and the same CMake options (Qt, SDL, Libretro,
OpenGL, room/web services, GDB stub and LTO off; software renderer, Vulkan and
cubeb on; `CTR_BACKEND_NAME` AZAHAR or CITnLO). One deviation: the stale oracle
include is cleared with `-UCMAKE_PROJECT_citra_INCLUDE`, because CMake 3.28
treats an empty `-DCMAKE_PROJECT_citra_INCLUDE=` as a directory and fails.

```sh
sudo apt install build-essential cmake ninja-build pkg-config git libpulse-dev libasound2-dev
git clone --no-checkout https://github.com/estyseesghosts/azahar.git azahar-integration
git -C azahar-integration checkout --detach <integration_commit from pins.toml>
git -C azahar-integration submodule update --init --recursive --depth 1
scripts/build-azahar.sh integration azahar-integration build/native-integration
build/native-integration/bin/Release/ctr-worker --self-test
python3 tests/test_native_protocol.py build/native-integration/bin/Release/ctr-worker
```

The worker links only glibc (`libc`, `libm`, `libmvec`), `libstdc++` and
`libgcc_s`; cubeb loads PulseAudio/ALSA at run time. Status: **verified
compiled** for both pins in Docker ubuntu:24.04 (GCC 13.3, CMake 3.28), and
`--self-test` and the protocol test pass. `tests/test_windowed_mcp.py` passes
with two AZAHAR sessions of an owned `.3dsx` in `lotus-desktop --mcp` under
WSLg. The frames advance at about 8 fps each with software rendering in a
GPU-less container; that is not a performance result.

## 12. Linux AppImage

`scripts/package-linux.py` assembles `LOTUS-v<version>-linux-x86_64.AppImage`
from the tested Rust binaries and both worker build directories (#108):

```sh
cargo metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu > build/metadata.json
python3 scripts/package-linux.py --rust target/release --metadata build/metadata.json \
  --azahar build/native-integration --citnlo build/native-experimental --version <version> \
  --output build/dist --appimagetool <appimagetool-x86_64.AppImage> --runtime <runtime-x86_64>
```

AppDir layout: `AppRun` (the launcher, [37](37-user-guide.md#linux-appimage)),
`lotus.desktop`, `lotus.png`, `usr/bin/{lotus-desktop,lotus-mcp,ctr-cli}`,
`runtimes/{azahar,citnlo}/ctr-worker` with build manifests, licenses,
`SOURCE.md` and `release-manifest.json` (pins, minimum glibc, host libraries,
file hashes). The tools are pinned by URL and SHA-256 in the script:
appimagetool 1.9.1 and type2-runtime 20251108. The static runtime needs no
libfuse2. Without FUSE, run with `APPIMAGE_EXTRACT_AND_RUN=1` or
`--appimage-extract`. Packaging needs the `file` command.

Nothing is bundled into `usr/lib`. Every library the binaries load belongs to
the host: glibc, `libstdc++`, `libgcc_s`, `libudev`, xcb/xkbcommon, and Wayland,
Vulkan and audio through `dlopen`. These must match the host's drivers and xkb
data. A binary that needs any other library fails packaging. Built on Ubuntu
24.04, so the **minimum is glibc 2.39** (Ubuntu 24.04, Debian 13, Fedora 40 or
newer).

On a `v*` tag, `linux.yml` attaches the AppImage and its `.sha256` to the
release, and `windows.yml` attaches the ZIP. Whichever finishes first creates
the release and the other uploads to it (`scripts/publish-release.sh`).
Existing assets are never replaced.

`tests/test_appimage.py <AppImage>` gates publication (#109). It checks the
name, version and `.sha256`, every file against `release-manifest.json`, both
worker pins and identities, and that every binary resolves its libraries.
Then it runs the image, renamed into a path with spaces, from another
directory, with an isolated `HOME`. It uses `APPIMAGE_EXTRACT_AND_RUN` because
hosted runners have no FUSE. The runs cover `cli --help`, `--configure-only`
for both backends, an MCP initialize, and each worker's `--self-test` and
protocol test. The test fails if anything is written outside `~/.local/lotus`.
GUI qualification stays manual: hosted runners have no GPU.

WSL2 (Windows-only developers): an Ubuntu 24.04 container or distro with the
packages in sections 10 and 11 runs this whole loop. WSLg exposes
`/mnt/wslg/runtime-dir/wayland-0` and `/tmp/.X11-unix/X0`. Mount both, set
`XDG_RUNTIME_DIR=/mnt/wslg/runtime-dir WAYLAND_DISPLAY=wayland-0 DISPLAY=:0`,
and `lotus-desktop` opens a window on the Windows desktop. WSLg has no Vulkan
device in a plain container, so emulation falls back to software and
performance numbers do not carry over to native Linux.
