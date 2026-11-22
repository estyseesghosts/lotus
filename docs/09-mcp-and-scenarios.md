# 09 — MCP, programmatic debugging and scenario runner

## Architecture

MCP is an **adapter to the Rust session supervisor**, not a second debugger implementation. Use the Rust MCP SDK (`rmcp`, pin version and check actual API). The same typed operations must be available through: GPUI actions, `ctr-cli`, test scripts and MCP. MCP tools return bounded structured records plus artifact handles; raw multi-megabyte traces/screenshots are resources/files, never giant tool response bodies.

## MCP tool taxonomy

| Group | Suggested tools | Design constraints |
|---|---|---|
| Session | `session_create`, `session_load`, `session_status`, `session_stop`, `session_list` | explicit instance IDs; no accidental 'current' global |
| Control | `run_until`, `pause`, `reset`, `step_instruction`, `step_frame` | guest and wall timeout, stop reason |
| CPU | `cpu_state`, `disassemble`, `breakpoint_set`, `breakpoint_remove` | core/thread/process selectors |
| Memory | `memory_map`, `memory_read`, `memory_write`, `memory_compare` | byte caps, explicit address space, write permission |
| Kernel | `thread_list`, `ipc_trace`, `handle_lookup` | bounded lists, parsed + raw fallbacks |
| GPU | `gpu_capture_frame`, `gpu_capture_draw`, `gpu_registers`, `texture_export` | state-at-draw, resource hashes, renderer sync |
| Trace | `trace_configure`, `trace_start`, `trace_stop`, `artifact_list`, `artifact_open` | budgets, loss reporting, artifacts |
| Tests | `scenario_run`, `scenario_cancel`, `scenario_result`, `comparison_run` | background jobs owned by supervisor, not open loops in tool call |

Prefer fewer higher-value tools and a composable `scenario_run` over thousands of single-frame requests. Do not expose arbitrary `execute_shell`, arbitrary OS memory reads or unrestricted host filesystem access through MCP.

## Resources and metadata

Resources can expose versioned immutable artifacts: screenshots, GPU command lists, memory dumps, trace summaries, reports. Use URI-like handles such as `ctr://session/<id>/artifact/<artifact-id>` if supported by the MCP SDK; access control must be validated on every resource read. Resource description includes content type, size, hash, epoch, lifetime and provenance. `artifact_open` should allow bounded ranges / paging for large traces.

## Example successful agent workflow

**Question:** Why does a title-screen animation get choppy in the original 3DS-targeting program?

1. Agent creates two sessions: baseline build and candidate build, each with private storage and identical emulator model/options.
2. Runs a deterministic input script to a title-screen checkpoint, explicitly disables live input.
3. Captures frame pacing, guest CPU samples, draw/transfer counts, rasterizer-cache events and host profiler metrics; selects 3 anomalous frames for L2 snapshots.
4. Compares draw counts/command order and CPU SVC/thread waits. It may identify CPU-higher work or host-emulation stalls but **must not infer actual Old 3DS GPU milliseconds from host renderer timing**.
5. If hardware performance prediction matters, requests the corresponding Coffeepot/physical-console benchmark or marks this question unresolved.
6. Exports a small Markdown finding, commands/scripts, capture manifest, diff report and confidence labels.

## Scenarios: deterministic, bounded and repeatable

A scenario is declarative data with a **start state**, **input ownership**, **actions/checkpoints**, **stop condition**, **capture policy**, and **assertions**. See `specs/scenario-example.yaml`.

Requirements:
- Every scenario must have a terminal condition and host timeout; `run forever` is invalid for automation.
- `seed`, virtual-clock policy, initial save/NAND snapshot, game hash, model, graphics backend, worker SHA and build flags are recorded.
- Inputs are sampled at explicit guest boundaries or frame IDs. If replay requires exact instruction tick timing, only claim it when supported; frame-anchored input is a weaker level.
- Detect desync at checkpoints using targeted stable state (PC/thread, memory ranges, framebuffer hash, GPU state). Hashes must exclude volatile fields unless relevant.
- For gameplay checks, show script coverage and which branch actually executed; a passing fixture for an unused fast path isn't evidence that optimization helped the game.
- Assertions can be `equal`, `within_tolerance`, `monotonic`, `event_seen`, `no_event`, `frame_count`, `resource_limit`, `stop_reason`, etc., with explicit confidence of signal. Implemented subset: document 22, "Version 2 assertions".

## Agent ergonomics

A single `run_until` can return:

```json
{
  "session_id":"<uuid>", "epoch":4, "stop_reason":"breakpoint",
  "guest_tick":123456789, "process_id":12, "thread_id":7,
  "artifacts":["ctr://session/.../trace/..."]
}
```

Never silently fall back from `step_cpu_instruction` to `step_frame`. Return `UNSUPPORTED` with `available_alternatives` when semantics cannot be met. Make tool schemas self-describing and prefer typed enums over stringly typed free-form commands.

## Concurrency

- One **lease owner** for state-mutating control of a session at any time (UI or agent). Other clients may read immutable snapshots. Lease has timeout and revocation on disconnect.
- Queue operations with priorities and explicit budgeting; `stop`/cancel remains available even when trace export is active.
- A scenario runner owns its input channel; manual keyboard changes are suspended or require explicit takeover with scenario failure marker.
- An agent should be able to spawn multiple sessions but supervisor enforces total CPU, RAM, GPU, disk and concurrent-run quotas.

## Failure modes

- Agent asks for memory past mapped range -> partial read with typed holes or explicit error.
- Agent uses stale epoch -> `STALE_EPOCH`, not accidental reads from another program.
- GPU readback not supported -> `UNSUPPORTED`, no made-up capture.
- Scenario times out -> partial artifact pack with `INCOMPLETE` and last stable checkpoint, not green result.
- Worker crashes -> supervisor preserves per-run metadata and stdout/stderr while invalidating shared-memory handles.
- Tool uses nonsensical clock time -> error with valid time domains.

## Security

Local MCP stdio is a good initial transport. Refuse broad remote-listen defaults. Require per-session grants for writes, launch, attaching to user-provided firmware and reading captures. Validate every path under a managed run root and never let model-generated commands pass directly to shell execution. Allow explicit user approval for destructive file/restore operations.

On Windows, `lotus-desktop --mcp host.json` bridges stdio to the desktop host's
named pipe. A logon-SID access list and remote-client rejection restrict it to
the current interactive Windows logon. A storage-root lock admits one shared
desktop, and the endpoint identity includes the requested host configuration
so a bridge cannot inherit different asset or control permissions. Each
connection has a separate MCP message stream over the same supervisor.
`lotus_status.frontend_pid` identifies that process. `lotus-mcp` keeps its
isolated stdio lifecycle.

## API versioning and tests

Include tool compatibility matrix (`MCP tool version -> protocol -> worker capability`). Generate tool schemas from shared Rust types. Contract tests must run the *same* command through CLI and MCP and compare normalized result. Add fuzz tests for invalid addresses, malformed tool payloads, huge lists and expired artifact handles.

### Repeated scenario assessment

Run `ctr-cli scenario assess <scenario> <worker> <output> [repeats]` to
retain two to ten independent attempts, with three by default. The CLI
writes the result to `<output>/<id>/assessment.json` and prints it as JSON.
Each completed attempt has a relative path to its full report.

- `PASS` or `FAIL` means every attempt completed with the same verdict and
  the same frame, screen, and assertion observations.
- `FLAKY` means completed attempts disagreed in verdict or observations.
- `INCOMPLETE` means at least one attempt lacked evidence, such as an
  unavailable fixture, an inconclusive assertion, or a worker deadline.

Only `PASS` returns exit status zero. Host timing and filesystem paths
are excluded from the comparison. Worker and guest content hashes are
part of the comparison, so a file change during assessment cannot
produce a stable verdict.

A suite manifest lists scenario files and their repeat count.

```yaml
version: 1
repeats: 2
scenarios:
  - basic-draw.yaml
  - input-regression.yaml
```

Run `ctr-cli suite run <manifest> <worker> <output> <source-revision>`.
The revision is a full Git object ID supplied by the caller. Each attempt
records measured worker and guest content hashes in its report. Scenario
files must resolve within the manifest directory. A suite can list up to
16 files and reserve up to 1,800 seconds of scenario execution deadlines.
A missing or invalid scenario is recorded as `INCOMPLETE`.

`ctr-cli suite show <output> <suite-id>` reads a completed suite from
disk. Each case points to its repeated assessment and full attempt
reports. The suite returns nonzero if any case is incomplete, flaky, or
failed, in that order of precedence.

To browse suites in the Windows desktop or through MCP, use the host's
`<storage_root>/regressions` directory as the suite output. Open
**Actions → Debug → Regression History** (Ctrl+Shift+R), or call
`lotus_regression_history` with an `operation` of `{"op":"list"}`,
`{"op":"detail","id":"…"}`, or
`{"op":"compare","baseline":"…","candidate":"…"}`. The CLI equivalents
are `suite list <output>`, `suite detail <output> <id>`, and
`suite compare <output> <baseline-id> <candidate-id>`.

History lists the 32 most recent completed suites. Details include the
first failed assertion with its expected and actual values. Comparisons
align cases by manifest-relative scenario path and compare screen hashes and assertion values
only when both runs have repeatable observations for the same scenario
definition. All three interfaces use the same [history reader](../crates/runner/src/history.rs).

### Guarded first-bad search on Windows

Run `ctr-cli suite bisect <host-config> <plan.json>`. The plan identifies
the inclusive first-parent range and assigns a host-approved build profile
to each revision. Give each profile its own clean, linked Git worktree
at that revision. Set `allow_control` in the local host configuration to
approve execution of those profiles. Lotus reads Git history and worktree
status without changing the checkout or Git's bisect state.

```json
{
  "version": 1,
  "git": "C:/Program Files/Git/cmd/git.exe",
  "repository": "C:/dev/homebrew",
  "suite": "suite.yaml",
  "good": "<full good commit ID>",
  "bad": "<full bad commit ID>",
  "profiles": {
    "<full good commit ID>": "regression-good",
    "<full bad commit ID>": "regression-bad"
  }
}
```

The suite path is relative to the plan. Every scenario runs against the
profile's freshly built and hashed 3DSX. Omit scenario content hashes and
movies from bisect suites. The approved profile must specify one or two
Make jobs or set `CMAKE_BUILD_PARALLEL_LEVEL` to 1 or 2, and set a build
timeout of at most 90 seconds. Its 3DSX output must be absent before
the build. Keep build outputs ignored by Git.

One search runs at a time per history directory. Limits are eight
revisions, 64 emulation attempts, 900 seconds wall time, 45 seconds of
aggregate user-mode CPU time per Windows build job, 64 MiB of monitored
files per worktree, and 8 MiB per collected 3DSX. Retained suite evidence
is checked against the host byte quota and preflighted at up to 64 MiB
per search. Build profiles are trusted local programs. Workspace
monitoring checks the worktree and collected output, and is not a
volume quota for files an approved build writes elsewhere.

The command verifies the good and bad endpoints using the suite's
repeat count, searches the range, and verifies the prefix through the
first failure. An unresolved prefix, including failed builds, missing
outputs, timeouts, or flaky runs, yields no first-bad ID. The JSON
report and suite IDs are retained in
`<storage_root>/regressions/bisect-<id>.json` whether the search
identifies a revision or ends unresolved.
