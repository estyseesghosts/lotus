# 03 — Two enduring Azahar branches, one fork

## Decision

Use **one GitHub fork** of `azahar-emu/azahar` with two tracked branches:

- `integration`: *upstream-minimal*. Minimal native C++ worker target, frontend adapter, IPC control hooks, frame/audio/input adapters, build changes, and required fixes. It tracks upstream closely.
- `experimental`: *our engineering branch*. Derived from the current `integration` tip, carries debugger instrumentation, capture machinery and accuracy experiments. It follows integration after upgrades, not upstream directly.

This satisfies the desire for two forks/branches without managing two unrelated fork histories. A separate project repo (`ctr-emulator`) is the Rust workspace and supervisor; it pins one of these branches by **immutable commit SHA**, never by a moving branch name in CI/release builds. Each branch can be built as its own worker executable (`ctr-worker-integrated`, `ctr-worker-experimental`) and controlled through the same protocol.

## Why two branches, not two upstream forks

Two long-lived divergent forks amplify maintenance burden and let 'minimal' and 'experimental' unknowingly differ by upstream drift. A single shared history provides a clean monotonic chain:

```text
Azahar upstream/master: A ── B ── C ── D
                        │             │
integration:             a1 ── a2 ──── merge(D) ── i3
                              │                │
experimental:                  e1 ── e2 ── rebase/merge integration ── e3
```

The diagram is conceptual. In practice, choose **regular merge commits** from upstream into integration (auditably preserving upstream history), then **merge integration into experimental**. Avoid continuous rebase of published experimental commits; it's harmful to reproducibility and CI evidence. Feature branches may be rebased before merge.

## Branch invariants

| Test | `integration` | `experimental` |
|---|---|---|
| Builds upstream core with native frontend | Required | Required |
| Broad stock Azahar behavior unchanged | Required, verified by differential tests | Expected except labeled experimental features |
| Changed guest timing behavior | Not allowed without narrow upstream fix + evidence | Allowed behind explicit option and measurements |
| Per-draw tracing | Optional low-cost hook, dormant | Full bounded capture machinery |
| New PICA200 timing model | No | Yes, opt-in |
| GPU rasterizer rewrites | No | Yes, independent correctness tests |
| IPC stable with Rust product | Required | Required |
| Upstream merges | Regular, low-conflict | Receives through integration |
| Unrelated Azahar features | Keep upstream | Keep upstream unless required |

### Allowed patches in integration

1. Add `ENABLE_CTR_WORKER` / worker build target without changing normal frontend defaults.
2. Native `EmuWindow` adapter, no GUI dependency.
3. Queue commands into `Core::System::ProcessPendingWork` on emulation thread, with minimal state-read APIs.
4. Frame output via snapshot/capture adapter with bounded ring, optional no-audio backend, input routing and temporary applet bridge.
5. Resolve config/state directories from an explicit per-worker root.
6. Per-worker independent IPC endpoint, proper termination, clear error returns.
7. Small targeted bugfixes found while integrating, with reproducer and best-effort upstream contribution.

### Not allowed in integration

- Broad rewrites of PICA core state, shader interpreter, renderer cache or CoreTiming semantics.
- Replacing HLE kernel/dynarmic, changing frame scheduling because one game is slow.
- General-purpose high-volume GPU event logging, novel time travel/replay machinery.
- Any MCP/GPUI code linked into Azahar.

A minimal callback hook may exist in `integration` to support experimental subscribers. It must default to no-op, be measured and require a real integration use case. Avoid blank interfaces added on speculation.

## Concrete Git workflow

The following names are **suggested**, not existing branches. Pin base to full SHA after checking upstream. `--recurse-submodules` matters because Azahar uses submodules.

```bash
# One fork on GitHub, replace YOURORG with owner of the fork
git clone --recurse-submodules git@github.com:YOURORG/azahar.git
cd azahar
git remote add upstream https://github.com/azahar-emu/azahar.git
git fetch --all --tags

git switch -c integration upstream/master
git push -u origin integration

git switch -c experimental integration
git push -u origin experimental

# Keep two dedicated on-disk worktrees. Never share CMake build dirs.
git worktree add ../azahar-integration integration
git worktree add ../azahar-experimental experimental
```

After `integration` is checked out in an additional worktree, the original checkout must be moved to some other branch or made detached first; `git worktree add` refuses to check out the same branch simultaneously. The above is a **sequence sketch**, not a guaranteed one-shot script. A robust sequence is:

```bash
# Start on experimental in the main checkout.
git switch experimental
# integration is not currently checked out, so this succeeds:
git worktree add ../azahar-integration integration
# Optional: use the main checkout as experimental; no second worktree required.
```

### Upstream sync procedure

```bash
cd ../azahar-integration
git fetch upstream
git switch integration
git merge --no-ff upstream/master
# Configure/build integration and run smoke + differential suite.
git push origin integration

cd ../azahar
git switch experimental
git merge --no-ff integration
# Resolve conflicts. Build and run deeper tests.
git push origin experimental
```

Every sync PR captures:
- From/to upstream SHA; list of upstream changes touching patched files.
- Build result: Windows first; Linux/macOS where available.
- Tests on load, input, audio, frames, HLE calls, save/load, clean exit, two isolated workers.
- Frame/state baseline changes and whether expected.
- One new integration SHA and one new experimental SHA for Rust lock manifest.

## Version manifest

The Rust product pins the selected C++ worker via a checked-in file, e.g.:

```toml
[azahar]
upstream_commit = "<40-hex-upstream-sha>"
integration_commit = "<40-hex-integration-sha>"
experimental_commit = "<40-hex-experimental-sha>"

[worker_protocol]
major = 1
minor = 0
```

Placeholders above must not ship in a release manifest. `git submodule` or a `CMake FetchContent`-style build can check out the pinned revision, but using a vendored Git submodule under `vendor/azahar` pinned to one branch at a time is simpler. For testing both worker flavors in one CI run, use **two checkout dirs** at the pinned SHAs, separate native build trees and distinct artifact names.

## CI drift detection and branch protection

- Protect `integration`: PR reviews, CI, no force-push, mandatory upstream-diff report.
- Protect `experimental`: PR reviews/CI, no force-push. Accuracy changes must have fixture evidence and rollback switches.
- Nightly job fetches upstream and reports new commits, API/compiler changes, conflicts in patched files, license changes and GPUI dependency changes. **Do not auto-merge upstream without tests.**
- Keep a maintained `patch-inventory.md`: PR, changed files, purpose, branch, upstreamability, affected test, owner, removal plan.
- Label an integration patch `upstreamable` when generally useful to Azahar and uncontroversial; upstreaming it can lower fork-maintenance cost.

## Conflict triage

When Azahar upstream modifies a patched file:
1. Build exact upstream revision standalone first.
2. Compare upstream edits against local patch by behavior and tests; don't reflexively preserve local hunk.
3. Reapply the smallest necessary adapter change.
4. Run instrumentation-on/off tests; a compatible compile doesn't mean semantically safe.
5. Create a new stable baseline before merging into `experimental`.

## Hard rule about branch claims

`integration` is not 'unmodified Azahar' and should not be used as the sole accuracy oracle. Track **stock upstream at the same base SHA** as an independent control. Maintain a third **unmodified checkout** for testing, *not a third maintained branch*.
