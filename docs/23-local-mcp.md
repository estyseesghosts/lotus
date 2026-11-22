# Local MCP adapter

`lotus-mcp [host-config.json]` serves stdio only (headless). `lotus-desktop
--mcp host-config.json` serves the same tools from the window over the same
sessions; session kinds, cards and the full tool list are in
[30](30-sessions-and-interface.md). Without configuration it can
report status but cannot spawn workers or read files. Host configuration fixes
the executable, root, approved asset handles and control permission:

```json
{
  "worker": "C:/path/to/ctr-worker.exe",
  "storage_root": "C:/path/to/private-runs",
  "assets": {"demo": "C:/path/to/demo.3dsx"},
  "allow_control": true,
  "asset_directories": {"builds": "C:/path/to/build-output"},
  "max_sessions": 4
}
```

The last two fields are optional (defaults: none, 4; limit 16). Optional
`prefer_vulkan` (default `true`) gives every new session whose `profile` names
no `graphics_api` the Vulkan renderer; if the worker refuses Vulkan setup at
load, the supervisor replaces that worker with a software one and reports
`renderer_fallback` (status, telemetry, desktop RENDERER gauge). Set
`"profile": {"graphics_api": 0}` to pin software for reference or determinism
runs, or `prefer_vulkan: false` for the worker default (software). See
[34](34-performance-and-issue-triage.md).

Configuration is trusted local startup input, never a tool parameter. Each
server process owns its sessions and artifact catalog. Client tools cannot
select executables, roots or arbitrary content paths. Controls require
`allow_control`; status/memory/capture operate only on owned sessions.
`max_sessions` workers and the artifact quota bound resources. A headless
MCP client disconnect shuts down its host and sessions. In the desktop, the
window keeps ownership when MCP disconnects. Session handles expire with
the server; create a session on the same primary card to retain SD/NAND data.
CPU execution is not retained across server restarts. Secondary guest data is
discarded. Primary manifests record `lifecycle: closed|crashed` and clear the
PID after verified termination. The reusable `worker.lock` file is an OS-lock
sentinel: its existence never means a worker is alive. Shutdown reports the
number of closed sessions, and each one that did not stop cleanly, on stderr.

`shader_seed` (default false) seeds new worker roots' shader caches from the
last clean stop of the same worker binary ([30](30-sessions-and-interface.md#startup-cost-30)).

Artifacts (#27, `crates/session/src/artifacts.rs`): a host-wide quota,
`"artifacts": {"max_count": 512, "max_bytes": 536870912, "lease_seconds": 60}`
by default, replaces the earlier lifetime cap of 128, which needed a restart
once full. Each live session is guaranteed `quota / max_sessions` (its floor);
space beyond it is borrowed. When room is needed the store evicts, least
recently used first: unpinned artifacts of ended sessions and of sessions above
their floor once their lease expired, then the requester's own unleased ones,
and, for a requester still below its floor, leased ones of sessions above
theirs. A new artifact is leased for `lease_seconds`. `lotus_pin_artifact`
keeps one until released (a session may pin up to its floor);
`lotus_release_artifacts` deletes at once. Reading an evicted or released URI
fails with `NotFound` and the reason ("artifact expired: released"); an unknown
URI stays `PermissionDenied`. Files are written as `<id>.tmp` and renamed only
after the whole batch is written, eviction and file I/O happen outside the
catalog lock, and files left by an earlier host are deleted when the store
opens (URIs expire with the host). `lotus_status.artifacts` reports totals,
floors, eviction and release counts and per-session usage; the desktop sidebar
footer shows count, MiB and pinned. Unit tests cover 300 capture/release
cycles in an 8-entry store with a byte-identical pinned file, three sessions
where one fills the store with leased artifacts and the others still get their
floors, the pin limit, a failure in the second file of a batch, and orphan
removal (**verified compiled**).

To observe two owned sessions in one window, create and run each through
`lotus-desktop --mcp`, then call `lotus_workspace` with
`{"operation":{"op":"tile","first":"<A>","second":"<B>","axis":"auto"}}`.
Use the returned revision for `expected_revision` on changes. Check
`lotus_status.workspace.resolved` for the resolved orientation and
`frontend_metrics.ui.rendered_workspace` for the revision and per-tile
preview and gauge counters that GPUI rendered. Sample twice to confirm both
tiles advance. `untile` with `keep` returns to one visible session and both
workers remain available to MCP. See [workspace actions and shortcuts](30-sessions-and-interface.md#workspace-67).

Worker features (#42). A native worker lists the optional control features it
implements in `hello`/`status` `features` (`ctr_worker/control.h` `Features`;
names in `lotus_protocol::capabilities::feature`): `flush_storage`,
`event_subscriptions`, `gpu_capture_levels`, `state_hash`,
`preview_nonblocking`, `deterministic_async`, `gpu_memory`, `analog_input`
(#60). The list is in the
session's `native-manifest.json`. A request that needs a missing feature fails
with `Unsupported` ("worker AZAHAR does not support state_hash; it advertises
[...]") before it reaches the worker: `state_hash`, `events_configure`,
`gpu_configure` with `capture`, `settings_set deterministic_async_operations`,
`flush_storage` for card copies, and fast `input` with a stick off centre. Older workers ignored unknown fields such
as `capture` and `subscriptions`, which is why the host checks. A worker
without the field advertises nothing.

Policy (#23, `crates/session/src/policy.rs`), `"policy": {...}` in the host
config, defaults shown:
`{"workload": "interactive", "rewind_sessions": null, "rewind_memory_mib": null, "gpu_capture_sessions": null,
"snapshot_slots": null, "snapshot_queue_limit": 16, "snapshot_wait_ms": 10000, "memory_reserve_mib": 1024,
"gpu_admission": true, "gpu_reserve_mib": 0, "focus_priority": false}`.
Interactive defaults allow two rewind sessions, two GPU captures, and two
snapshot operations. Batch leaves these budgets unlimited unless set explicitly.

- `rewind_sessions`: sessions recording rewind checkpoints at once. Enabling a
  third is refused with `ResourceBusy` naming the holders; disabling or ending a
  holder frees its place. docs/34 measured 142 MiB (Old 3DS) and 274 MiB (New 3DS)
  serialized per checkpoint. The experimental CITnLO rewind worker accepts
  `compression_queue` (0..4, default 1) in `rewind_configure`. A positive depth
  bounds the number of serialized checkpoints waiting or being compressed.
  Depth 0 uses one serialized checkpoint at a time on the emulation thread.
  An explicit depth requires the worker's `rewind_compression_queue` feature.
  The stored history has its own `budget_mib` limit. See [#32](34-performance-and-issue-triage.md#32-async-rewind-compression).
- `rewind_memory_mib` caps the estimated memory reserved for recording.
  Lotus reserves `budget_mib` for stored history plus 320 MiB per serialized
  buffer. Synchronous compression and the default queue each reserve one buffer.
  Pending reconfiguration reserves the greater of its requested cost and the
  active cost until the worker replies. `lotus_status.policy.rewind` reports
  the limit and per-session reservations. The default is 1536 MiB in
  interactive mode and unlimited in batch mode. Set 0 to remove the limit.
  The estimate uses the measured New 3DS checkpoint size with headroom.
  Actual process memory also includes guest memory, renderer allocations,
  allocator capacity and zstd scratch.
- `gpu_capture_sessions`: sessions with `gpu_configure` above `counters`.
  A worker exit releases both rewind and GPU-capture slots. The failed
  session and its report stay available. Reset and content load also release
  the slots after the worker clears its recordings.
- `snapshot_slots`: one-shot full-state work host-wide (`save_state`,
  `load_state`, `fork`, `matrix`, copying a card for a secondary). Up to
  `snapshot_queue_limit` extra requests can wait for at most
  `snapshot_wait_ms`. Overflow fails immediately with `ResourceBusy`.
  A queue limit of 0 refuses any request that needs to wait.
- `memory_reserve_mib`: a new session is refused while available physical
  memory is below the reserve plus the largest current worker working set
  (1 GiB with none running; a t03 Vulkan worker measured 962 MiB). Device memory
  is the next item.
- `gpu_admission`, `gpu_reserve_mib` (#46): a new Vulkan session needs
  device-local headroom of at least the largest running session's device-local
  high-water mark plus `gpu_reserve_mib`. The host takes the smallest headroom
  estimate from the running Vulkan sessions. On the largest device-local heap
  it uses `budget - usage` from `VK_EXT_memory_budget` when present. On Windows,
  a supported single-node discrete Vulkan adapter also reports WDDM dedicated
  usage across processes, matched by adapter LUID. The host subtracts that
  usage from heap size and uses the lower estimate.
  `headroom_source` identifies the estimate in use. If neither source is
  available, admission has no measured headroom and VMA's estimate is displayed
  as telemetry. Measuring starts with the first Vulkan session. Under pressure a
  session that only prefers Vulkan (`prefer_vulkan`, no `graphics_api` in the
  profile) is created on software before load, with the reason in
  `telemetry.renderer_fallback`; a profile with `graphics_api: 2` is refused
  with the numbers. Running sessions are never switched.
- `focus_priority`: the focused session's worker (desktop selection,
  `lotus_focus_session`) runs in the above-normal priority class. Off until
  `tools/bench_parallel.py --focus-priority` shows a gain on a quiet host.

`lotus_status.policy` lists limits, holders, slots in use and queued, refusals
memory (available, reserve, estimate, pressure) and `gpu_memory`
(`budget_source`, `headroom_source`, `headroom_mib`, `estimate_mib`, `reserve_mib`,
`allocation_failures`, `pressure`); a session's
`telemetry.policy_note` holds its last refusal or wait. The desktop previews only
the selected session (`desktop_controller.rs` `preview_loop`), so hidden sessions
cost no readbacks under either workload. `tests/test_mcp_policy.py` checks the
budgets, batch mode and memory admission against a CITnLO worker.

Tools: status/list (sessions, cards), create (primary/secondary), load an
approved asset, run/run_until, pause/reset/stop, input, session status with
gauges, registers, bounded mapped-memory reads, capture and focus. Scenario
validation accepts only a handle registered by the embedding host; the standalone
server currently registers none. `lotus_capture_image` returns top and bottom
screens as lossless PNG image blocks, each preceded by its label and dimensions.
It accepts integer `scale` from 1 to 4, default 1. PNG encoding only runs for
this explicit tool call and creates no catalog entries. Two captures can
prepare image replies concurrently on a server. `lotus_capture`
returns raw BGRA as `lotus://artifact/...` links, including for clients without image support.
`resources/list` and `resources/read` expose only catalogued captures; read checks
size/hash. MCP uses stdio. The GDB tool below opens a separate loopback
debugger listener on request.

### CPU inspector

Choose a core in the CPU inspector. Follow its PC or enter an aligned guest
address and choose ARM or Thumb. Use Previous and Next to browse code in
windows of up to 64 bytes within each guest page. Click a register to edit it
while the guest is paused. The host checks the guest epoch, pause state and
debugger lease before dispatching a register edit or execution control. Run to
Address uses the selected core's current instruction mode.

Load an approved ELF in the command palette to see symbol labels and backtrace
provenance. Lotus uses the explicitly selected ELF's absolute addresses and
shows its SHA-256. A guest epoch change expires the selection. Verified
ELF-to-guest relocation and source navigation are tracked in #76 and #77.

### Memory inspector

Open Memory from the CPU or Breakpoints panel, enter a guest virtual address, or
choose a mapped region. Reads use the worker's current process. The map
enumerates process IDs for context. Each page contains at most 256 bytes and
ends at a guest page boundary. Select a byte to interpret four bytes as
little-endian and big-endian values, copy data, or open its watchpoint.

To change bytes, pause the guest and enter up to 16 space-separated hex bytes.
Review the pending diff and commit. Lotus compares the original bytes and
verifies readback under the same session lock. A session or epoch change
discards the draft. A read-only host or a debugger lease can still inspect
memory but cannot commit edits through this panel.

### Breakpoint configuration

CITnLO's `breakpoints` reply lists IDs, locations, kinds and hit counts. Lotus adds
`host_configuration` to entries whose creation or configuration this host
observed in the current guest epoch. It contains the acknowledged `process`,
`hit_after`, `condition` and `log` values. An absent field means the settings
are unavailable. The native request schema is unchanged.

In the desktop CPU inspector, click a disassembled instruction to open
Breakpoints at that address. Select a listed breakpoint to configure or remove
it. Changes require host control and respect the debugger lease. A guest epoch
change clears pending edits. CITnLO uses instruction stepping while a memory
watchpoint is armed.

### Guest tools in the command palette

Use the Guest system commands to list or edit cheats, browse read-only NAND or
SDMC directory listings, and select approved content handles. Directory rows
open the next listing in the same panel. The empty path `""` selects the root.
CIA installation, HOME Menu boot, system regeneration, content transformation
and cheat removal use a confirmation sheet tied to the current guest.

### GDB listener (#152)

On a loaded CITnLO session, open **Debug → GDB** to start a listener, copy its
address, see whether a client holds the mutation lease, or stop it. Leave the
port blank or enter `0` to select a free port on `127.0.0.1`. AZAHAR is
unsupported.

`lotus_gdb` accepts `session` and `operation` (`start`, `status`, `stop`).
`start` also accepts an optional `port` and reports the assigned address.
Start and Stop require host control permission. Status also works on a
read-only host and reports the listener state, backend, address, and lease.
The desktop and embedded MCP share the same listener. Either can stop it,
and a second Start reports a conflict. An attached GDB client holds exclusive
guest mutation control until detach, disconnect, or Stop. Read debugger
operations remain available.

Desktop listeners survive an embedded MCP disconnect. Worker exit, session
stop, or an external guest reset or replacement retires the listener. A rewind
issued by the attached GDB client updates the listener to the new guest epoch.

### GPU capture (#147)

The CITnLO GPU inspector provides Counters, Events, Draws, Full and Disabled
presets and a **Configure** form. Draws mode accepts an inclusive 64-bit draw
ID range, a 1–4096 snapshot budget, and optional resource copies. Events,
Draws and Full can pause at selected PICA events. Full takes snapshots with
resources on every draw. Counters and Disabled use the batched command path.
Opening the inspector reads status and existing draw headers.

The desktop and `lotus_debug` use the same `gpu_configure` command. The
inspector shows the last settings acknowledged by the worker alongside the
live `gpu_draws` path, counts and losses. Capture budgets and mutation leases
apply to requests from either frontend.

**Verified emulator**: the opt-in `native_parity` test compares MCP screen bytes
with the shared native API at frame 60, and checks read-only permission, asset,
session and resource restrictions. Windows GNU AZAHAR at native pin
`05773bbb5380bc5094fcd8eacf189fdc5d6360bb`; the same acceptance passed on CITnLO.
`tests/test_mcp_native.py` additionally exercises actual stdio initialization,
tool calls, resource listing/reading and shutdown. No hardware verification.

Source: `mcp::{server,tool_arguments}`, `session::{supervisor,native_session}`.
MCP framing/types follow pinned `rmcp 3.10.0` source in Cargo.lock. Windows path
normalization converts Rust verbatim canonical paths for MinGW std::filesystem;
native manifest/logs remain in each private root. Rollback: launch without host
configuration or set `allow_control` false.
