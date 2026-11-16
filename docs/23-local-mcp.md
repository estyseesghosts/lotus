# Local MCP adapter

`lotus-mcp [host-config.json]` serves stdio only. Without configuration it can
report status but cannot spawn workers or read files. Host configuration fixes
the executable, root, approved asset handles and control permission:

```json
{
  "worker": "C:/path/to/ctr-worker.exe",
  "storage_root": "C:/path/to/private-runs",
  "assets": {"demo": "C:/path/to/demo.3dsx"},
  "allow_control": true
}
```

Configuration is trusted local startup input, never a tool parameter. Each
server process owns its sessions and artifact catalog. Client tools cannot
select executables, roots or arbitrary content paths. Controls require
`allow_control`; status/memory/capture operate only on owned sessions.
Four workers and 128 screen artifacts (at most 49,152,000 bytes) bound resources.

Tools: status/list, create/load an approved asset, run_until, pause/reset/stop,
input, session status, bounded mapped-memory reads, and capture. Scenario
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
