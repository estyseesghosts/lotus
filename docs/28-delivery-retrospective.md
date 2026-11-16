# Delivery retrospective: Waves 0–2 and v0.0.1

Session record, 2026-11-15/16. Source: the agent session transcript
([chatlog01](chatlog01.md), reviewed by outline and by every failure/fix passage,
not line by line). Evidence for results: [25](25-wave-status.md),
[20](20-native-baseline.md), [26](26-portable-release.md). Build steps:
[27](27-build-and-run.md). No hardware accuracy claim is made.

## What was done

| Phase | Outcome |
| --- | --- |
| Native baseline (T01/T04) | Three worktrees at upstream `11eceda5e`; stock core built; bounded oracle driver booted `t03.3dsx` for 60 frames; default-off `ctr-worker` target |
| Native worker (T05–T09) | Boot, BGRA frame ring with leases, input, muted audio ring, safe-point control, Windows job containment and root locks |
| Rust (T02, T03, T10–T13) | Private-pipe protocol, supervisor, GPUI desktop, CLI scenarios, four-worker isolation, scoped local MCP |
| Naming | LOTUS (app), AZAHAR (`integration`), CITnLO (`experimental`) applied across UI/docs/CLI |
| Release | Portable ZIP + source ZIP + checksums; CI on GitHub; **v0.0.1 published** (run 43801752833) |

## What went well

- Every native result is tied to an exact commit: `build-azahar.ps1` rejects a
  source that differs from `config/pins.toml`, is dirty, or has a foreign cache.
- A stock-core oracle gave an independent reference: native screens matched stock
  byte for byte (after BGRA/RGBA), including two simultaneous guests.
- Crash containment (job objects, root locks, epoch/lease invalidation) was tested
  by killing workers, not assumed.
- Three CLI runs from fresh roots gave identical hashes.
- CI built from clean checkouts and the packaged ZIP was tested relocated, with a
  Windows-only PATH.
- Recovery without recompiling (`reuse_build_run`) turned a 45-minute rebuild into
  about 9 minutes.

## What went wrong and how it was resolved

| Problem | Cause | Resolution |
| --- | --- | --- |
| Duplicate session IDs/paths in parallel tests | ID generator reused the same Windows clock tick | OS-random IDs plus a process-local counter |
| Stock build failed on nested submodules | One submodule pass was not enough | Second recursive pass; CI uses `--init --recursive` |
| Oracle died with `unordered_map::at` before load | Frontend never initialized `Settings::values.lle_modules` | Set every `service_module_map` entry to HLE in the driver; core unchanged |
| Oracle/worker link errors | Missing Windows `crypt32` | Link `crypt32` on `WIN32` |
| Workers failed to start from MCP | MinGW cannot open `\\?\` verbatim paths | Session adapter converts to ordinary Win32 paths |
| Stock vs native screens differed | Fixture output depends on the launch path string (relative vs absolute); **root cause unresolved** | Use identical absolute paths; documented in [20](20-native-baseline.md); not treated as an accuracy regression |
| Stale frames after a failed/second load | Leases survived content loads | Load-epoch invalidation on every load ([21](21-native-control.md)) |
| Visible GPUI rendering and two-DPI interaction unverified | Computer Use was not approved for the apps; the user said to continue | Left **unverified** and labeled so everywhere; only geometry and controller tests exist |
| Release build failed to compile shaders | GPUI needs `fxc.exe` | `setup-build-tools.py` fetches a hash-verified SDK package; `GPUI_FXC_PATH` |
| Source archiver hung | Windows pipe stall on `git archive` padding | Drain output before waiting (`9b3123c`) |
| ZIP creation failed on old timestamps | `zipfile` strict timestamps | `strict_timestamps=False` |
| Package test failed in CI after all builds passed | Compared `RUNNER~1` vs `runneradmin` path strings | `samefile`/`resolve` comparison (`d2a09b0`) |
| Rebuilding to fix a test would cost ~45 min | Packaging bug sat behind the build jobs | Added `reuse_build_run` plus `validate-build-reuse.py` (trusted main run, successful jobs, ancestor commit, path allowlist) |
| Reuse run published nothing | `release` skipped transitively because `rust`/`native` were skipped | `always() && needs.package.result == 'success'` (`7f9dc3e`) |

## Lessons

- Test the real CI path early; two of the last three defects were Windows
  runner-only.
- A skipped job skips every dependent unless `always()` is used. Re-test
  conditions with the skip path, not just the green path.
- Keep native tests visibly separate from ignored tests so a skip is never read as
  a pass.
- Record deferred checks next to the claim, not in a separate list.

## Still open

Visible GPUI rendering and input, actual two-DPI behavior, audible audio, title
save formats beyond the drawbench CSV, accelerated renderers, other platforms,
and the launch-path pixel sensitivity. Update: Wave 3 (debugger) is implemented,
with visible rendering/input checked by screenshot; see
[33](33-wave-3-retrospective.md). Wave 4 (accuracy) is not started.
