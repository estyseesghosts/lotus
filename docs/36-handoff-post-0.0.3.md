# Handoff: post-v0.0.3 issue work

State on 2026-11-17. Commits are by terezi; nothing is pushed.

## Committed

| Issue | Lotus | CITnLO (experimental) | AZAHAR (integration) |
| --- | --- | --- | --- |
| #24 GPU capture levels | `4ba73cb` | `459d3662` | — |
| #19 supervision under back-to-back commands | `84c6326` | — | — |
| #28 card copy waits for its primary | `995e988` | — | — |
| #25 non-blocking Vulkan preview | `9f39e16` | `58e00ed1` | `ed51b869` |
| #22 stdin read blocks instead of 1 ms sleep | — | `3537ccb7` | `7af07040` |
| #22 paused loop returns when a request arrives | — | `f5abd961` | `24a33631` |
| #23 host policy (budgets, memory admission, focus priority) | `548dbc6` | — | — |
| #22 `tools/bench_parallel.py`, CI smoke, `watch_write_1` | `61f625e` | — | — |
| #26/#33 writer thread, event categories, stops-only hello | `386e31d` | `0cef6413` | `9016f8ab` |
| #29 `state_hash`, divergence.py, deterministic async (CITnLO default), field init | `b41caaf` | `e7b5fbbf3` | `794b84ca3` |

docs/34 "Issues after v0.0.3" has the per-issue results. Busy-host numbers there are not
acceptance evidence. Quiet-machine commands are in docs/34 "Quiet-machine timing runs".

## Waiting on the owner

- Proposed ADR-012 (docs/34 #29): make `deterministic_async_operations` AZAHAR's default too.

## Not started

- #32 has since landed on the local CITnLO experimental branch. See the
  [measurements and remaining work](34-performance-and-issue-triage.md#32-async-rewind-compression).
- Proposals #34–#41: not read this session beyond overlaps.

Done before this session (2026-11-16, see `git log v0.0.3..c9ca88f`): #19/#20 per-session supervision
and input mailboxes (`6b5e664`), #31 concurrent shutdown (`f220106`), #21 desktop loop (`5439367`),
#27 artifact quota (`c32701c`), #28 coordinated card copy (`2ca239f`), #30 hash cache and shader seed
(`df18aa8`; `shader_seed` stays off until `bench_parallel --shader-seed` runs on a quiet host).
- Release: repin `config/pins.toml` + `vendor/azahar`, rebuild, update docs/35 and SHA256SUMS.

## Found, not filed

- A Vulkan worker aborts when the GPU runs out of memory: `vk_texture_runtime.cpp:224` calls
  `UNREACHABLE()` after any `vmaCreateImage` failure. Four t03 Vulkan workers next to a
  12.6 GB process on the 16 GB RX 9060 XT hit `VK_ERROR_OUT_OF_DEVICE_MEMORY` (-2) after 305 s
  (`runs/div29-both-vk-oom/r0/w0/log/azahar_log.txt`). Not measured: per-worker device memory.
- Lotus reports a dead worker as "worker closed stdout". The reason (here the `LOG_CRITICAL`
  line) is only in `<root>/log/azahar_log.txt`; nothing in `crates/` reads that file. Its tail
  should go into the session's failure record.

## Lessons

- Windows timer resolution (15.6 ms) was behind two latency bugs. Any `sleep_for(1ms)` or
  deadline wait on a request path costs a whole tick.
- Measure through the real supervisor. `bench_parallel` found supervision starvation and both
  tick waits, which `bench.py` hid. A probe rate that is not fixed distorts throughput.
- Identical boots repeat only with deterministic async HLE (CITnLO default since #29) and the
  same `sysdata`/`nand`. AZAHAR and host-async sessions still diverge; fidelity checks against
  them must retry boots until a pair matches, or compare within one boot.
- Tool input un-escapes backslashes. Write `C:\abs\t03` paths with byte codes or the Write
  tool, never in an inline Python string (this produced the BEL/TAB path in docs/34).
- Don't edit sources while a build of them runs. Long bash heredocs with Python fail; use
  scratchpad scripts with `ed.py`.
- Keep `lotus/AGENTS.md` (the user's own modification) out of every commit.
- `cmd > log; echo exit $?` in a background task reports success whatever `cmd` did. Let the
  command's own exit code end the task (flood-03 hid a traceback this way).
- Smoke-run a tool after editing it. `identity = identity_template(...)` in bench.py `main`
  shadowed the `identity()` function and broke every bench run; a 20-frame run caught it.
- Check `settings_bindings.h` before toggling a setting after load: "boot" settings are refused
  once loaded (`deterministic_async_operations` is one), and bench.py records the refusal as
  `skipped`. bench.py also runs at the worker's 100 % frame limit unless `--frame-limit 0`.
