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

Native commits and their immutable product pins are recorded only after their
build/test gates pass. No experimental changes should be merged wholesale into
integration. Do not upload guest assets or local run logs.
