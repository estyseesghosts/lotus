# Portable Windows release

`LOTUS-v0.0.1-windows-x64.zip` contains the desktop, CLI, MCP server and separate
AZAHAR/CITnLO runtimes. Extract it into a writable folder. `LOTUS.cmd` selects
homebrew using a Windows file dialog and launches CITnLO; `LOTUS-AZAHAR.cmd`
selects AZAHAR. Both recreate absolute host configuration from the installation
location, retaining sessions under `data/<backend>/sessions`. No installer,
registry changes or developer tools are required. Windows 11 x64 / Direct3D 11
is the initial target. The binaries are unsigned.

The packager reads AMD64 PE imports, copies non-system DLLs recursively, and
rejects missing dependencies or MSYS runtime dependencies. Windows system/API-set
DLLs stay supplied by Windows. All payload files have SHA-256 entries in
`release-manifest.json`; the release also supplies ZIP checksums, notices and a
recursive LOTUS/native source ZIP. Guest assets and instance directories are
excluded. Only the three production Rust executables are shipped.

Build from a recursive Git checkout of the release tag. `scripts/setup-build-tools.py`
downloads hash-verified WinLibs GCC 16.2.0 r2 (including CMake/Ninja) and Windows
SDK 10.0.28000.2705 shader tools into a private build directory. Add
`<tools>/winlibs/mingw64/bin` to PATH and set `GPUI_FXC_PATH` to
`<tools>/sdk/c/bin/10.0.28000.0/x64/fxc.exe`. GPUI's pinned
`crates/gpui_windows/build.rs` requires FXC for optimized shader compilation.
The SDK is a build dependency, not part of the runtime ZIP.

The exact build, staging and packaging commands are in
[windows.yml](../.github/workflows/windows.yml). CI runs on pushes to main,
pull requests, tags and manual dispatch: Rust formatting/Clippy/release tests;
independent pinned native builds; native framing/self-tests; package assembly;
relocation to a path containing spaces with a Windows-only PATH; DLL/hash,
launcher, CLI, MCP and native startup checks. No supplied private fixture runs
on GitHub. Tag `v<version>` must match Cargo's workspace version and publishes
only after all build and package jobs pass. Actions and build-tool downloads are
pinned; per-runtime manifests record source/flags/compiler. GitHub's Windows
runner image is maintained externally, so byte-identical rebuilds are not promised.
Manual dispatch with `publish_release=true` instead creates the version tag and
release from the tested workflow SHA after the same checks. It never overwrites
an existing release. Only the release job receives repository write permission.

Local native fixture results and stock comparisons remain in
[25](25-wave-status.md). Visible GPUI/input and actual two-DPI checks are still
deferred at the user's instruction. Audio playback and hardware accuracy are not
claimed. Rollback is extracting an earlier ZIP; preserve `data/` when upgrading.

Corresponding source: the tag's LOTUS tree, pinned fork commits and recursive
submodules, plus Cargo.lock's exact Rust sources. The source ZIP excludes Git
metadata; use `git clone --branch v0.0.1 --recurse-submodules
https://github.com/estyseesghosts/lotus.git` for the Git-based build scripts.
