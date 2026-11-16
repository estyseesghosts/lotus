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

The last two fields are optional (defaults: none, 4; limit 16).

Configuration is trusted local startup input, never a tool parameter. Each
server process owns its sessions and artifact catalog. Client tools cannot
select executables, roots or arbitrary content paths. Controls require
`allow_control`; status/memory/capture operate only on owned sessions.
`max_sessions` workers and 128 screen artifacts (at most 49,152,000 bytes)
bound resources. On client disconnect every session is stopped. Session handles expire with
the server; create a session on the same primary card to retain SD/NAND data.
CPU execution is not retained across server restarts. Secondary guest data is
discarded. Primary manifests record `lifecycle: closed|crashed` and clear the
PID after verified termination. The reusable `worker.lock` file is an OS-lock
sentinel: its existence never means a worker is alive. Shutdown reports the
number of closed sessions on stderr.

Tools: status/list (sessions, cards), create (primary/secondary), load an
approved asset, run/run_until, pause/reset/stop, input, session status with
gauges, registers, bounded mapped-memory reads, capture and focus. Scenario
validation accepts only a handle registered by the embedding host; the standalone
server currently registers none. Captures return `lotus://artifact/...` links.
`resources/list` and `resources/read` expose only catalogued captures; read checks
size/hash. No shell, arbitrary filesystem, network listener or Azahar UDP RPC.

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
