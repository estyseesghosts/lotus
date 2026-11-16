# Native branch worktrees and patch inventory

Upstream base: `11eceda5e0e682a1f44e396bf954a1b9137097ca`.
Local worktrees in the 3DS parent:

| Source | Branch | Build cache | Purpose |
| --- | --- | --- | --- |
| azahar-stock | detached at upstream base | build/azahar-stock | unmodified core control |
| azahar-integration | integration | build/azahar-integration-native | minimal native frontend |
| azahar-experimental | experimental | build/azahar-experimental-native | future instrumentation |

All three were created from the same base; no experimental model changes exist.
The stock checkout stays detached and clean. Never use a branch name as a
reproducible build input: the build script checks HEAD against config/pins.toml,
rejects dirty sources and refuses caches configured for another source root.

## Matrix

Windows GNU Release software-only is the initial tested configuration. Qt, SDL,
Libretro, web service, scripting RPC, room servers, hardware graphics APIs and
hardware audio sinks are off; software renderer is on; LTO and warnings-as-errors
are off. GDB is compiled by upstream default but disabled in driver settings.
The exact configuration is in build manifests and scripts/build-azahar.ps1.
Linux/macOS, MSVC and accelerated renderers are untested. No hardware timing
claim follows from the software baseline.

Stock core: **verified compiled**. A standalone, bounded oracle driver is linked
against the stock core via CMAKE_PROJECT_citra_INCLUDE; the source tree remains
unchanged. Driver fallback applets are explicitly upstream compatibility mode,
not a new implementation. This is not verification of the upstream Qt frontend.
**Verified emulator**: the stock driver loaded the locally supplied `t03.3dsx`
and presented 60 frames with load/run result 0 on 2026-11-15. Firmware was not
needed in this HLE configuration. The first driver attempt exposed a missing
frontend initialization of `Settings::values.lle_modules`; initializing every
entry in `Service::service_module_map` to HLE resolved it without changing core.
Differential image acceptance is recorded separately.

## Integration patches

| Ticket | Files | Rationale | Acceptance | Removal |
| --- | --- | --- | --- | --- |
| T04 | root CMakeLists, src/CMakeLists, src/ctr_worker | opt-in process frontend | native link/probe, OFF remains stock | remove if upstream supplies equivalent target |

T04 commit: `1f46572ad84256002d211f472351df8b39353da3` on both branches.
Native target **verified compiled**, version/probe executed. The stock oracle
presented 60 frames; top SHA-256
`b2f4decc3b22adfd5307a03cee2f85c0a7230449217352f795cd739274ae76bc`,
bottom `2ced806750a345801fd8615af6a9dd7770f935bd41b638c7cc1a1bf051d42101`.
These are emulator output bytes, not hardware witnesses. Stock uses upstream
compatibility applets. Source-layout metadata: 240 columns by 400/320 rows,
transposed by upstream into contiguous native display rows (400/320 by 240).

Current shared native pin: `05773bbb5380bc5094fcd8eacf189fdc5d6360bb`.
Additional patches: T05 frontend `e089d9825`, T06 native frames `616b21889`,
T07 input/muted audio `f7e593c8a`, T08 safe-point control `31a7c1188`,
backend naming `020136d27`, load-epoch invalidation `05773bbb5`.
All belong to integration; no accuracy changes exist.
AZAHAR and CITnLO are independently compiled and emulator-tested.
See [21](21-native-control.md) for interfaces, tests and limitations.

## Final differential and storage checks

**Verified emulator**, final pin above, Release flags from the matrix. The fork
also builds with `ENABLE_CTR_WORKER=OFF`: its oracle driver booted 60 frames and
matched stock exactly. Using the same ordinary absolute `t03.3dsx` path in both
drivers produced RGBA SHA-256:

- Top: `5c4a4f1128dbdbf3c5879fbc119cc1de706b3a8d414f6c3d822d05ae96853134`
- Bottom: `c1e3d414bf7f28fe565a47b763ad41c77f5bbb5f7f960e459b5e23dc36b32743`

The native worker's `--boot` output matches these bytes after BGRA/RGBA channel
conversion. The earlier relative-path stock run above gave different pixels;
stock itself reproduces this launch-path sensitivity. Its cause is unresolved.
Use identical absolute fixture paths for differential runs; do not interpret
path-dependent fixture output as an established accuracy regression.

A second external fixture, `coffeepot/ctr/benchmarks/drawbench/drawbench.3dsx`,
SHA-256 `e881bb724620c5adab64d5947932022c86755ab05dd57513eb00795e199e13ef`,
booted 60 frames in stock and two simultaneous native workers. Both workers
matched stock RGBA hashes:

- Top: `f4bf5eedd807f45307d7eabbc8ab8558ac4d3c1e34fcc684dbd0e9ce58301b87`
- Bottom: `d0600b9d5c84922f0e79720b009428f4ca56185221d04aa714c6a297d80296cc`

Each guest wrote its own 713-byte `sdmc/coffeepot/s001/drawbench.csv` under its
private root. This verifies simultaneous guest SD writes, not all title-save
formats or any hardware timing conclusion. Primary path ownership is in
`src/ctr_worker/frontend.h`, `src/common/file_util.cpp` and
`src/core/hle/service/fs/archive.cpp` at the native pin.

Reproduce with identical absolute fixture paths and fresh absolute output roots:
`lotus-stock-oracle.exe <fixture> <stock-root>`,
`ctr-worker.exe --boot <fixture> <native-root>`, then
`python tests/compare_stock_native.py <stock-root> <native-root>`.
Raw captures and guest CSVs remain local in ignored build directories.
