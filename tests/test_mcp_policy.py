"""Host workload policy through MCP against a CITnLO worker (#23).

1. interactive (default): two sessions may record rewind checkpoints; a third is refused with
   the two holders named, shown in lotus_status.policy and in its telemetry.policy_note, and
   admitted once a holder disables rewind. GPU `counters` capture never counts; `events` does.
2. batch: three rewind sessions, no limit.
3. memory admission: a reserve larger than the host's memory refuses a new session with the
   numbers; reserve 0 disables the check.

Usage: test_mcp_policy.py <abs lotus-mcp.exe> <abs CITnLO worker> <abs content.3dsx> <abs output>
"""
import json
from pathlib import Path
import sys
from mcp_client import Client

server, worker, rom, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
report = {}


def host(name, policy):
    root = output / name
    root.mkdir()
    config = root / "host.json"
    config.write_text(json.dumps({"worker": worker, "storage_root": str(root / "storage"),
                                  "allow_control": True, "max_sessions": 4, "assets": {"rom": rom},
                                  "profile": {"graphics_api": 0}, "policy": policy}))
    return Client(server, config, root / "transcript.jsonl")


def create(client):
    return client.tool("lotus_create_session", {"asset": "rom"})["session"]


def debug(client, session, operation, expected_error=False, **params):
    result = client.send("tools/call", {"name": "lotus_debug", "arguments": {
        "session": session, "command": {"operation": operation, **params}}})
    assert bool(result.get("isError")) == expected_error, result
    return result["content"][0]["text"]


def rewind(client, session, enabled, expected_error=False):
    return debug(client, session, "rewind_configure", expected_error, enabled=enabled, capacity=8, budget_mib=32)


def status(client):
    return client.tool("lotus_status", {})


clients = []
try:
    client = host("interactive", {})
    clients.append(client)
    a, b, c = (create(client) for _ in range(3))
    for session in (a, b, c):
        client.tool("lotus_run_until", {"session": session, "frames": 5})
    rewind(client, a, True)
    rewind(client, b, True)
    refused = rewind(client, c, True, expected_error=True)
    assert "rewind budget" in refused and a in refused and b in refused, refused
    policy = status(client)["policy"]
    assert sorted(policy["rewind"]["sessions"]) == sorted([a, b]) and policy["rewind"]["limit"] == 2, policy
    note = next(s for s in status(client)["sessions"] if s["id"] == c)["telemetry"]["policy_note"]
    assert "rewind budget" in note, note
    rewind(client, a, False)
    rewind(client, c, True)
    debug(client, a, "gpu_configure", enabled=True, stop_events=[], capture="events")
    debug(client, b, "gpu_configure", enabled=True, stop_events=[], capture="events")
    debug(client, c, "gpu_configure", enabled=True, stop_events=[], capture="counters")
    gpu_refused = debug(client, c, "gpu_configure", True, enabled=True, stop_events=[], capture="events")
    assert "GPU capture budget" in gpu_refused, gpu_refused
    report["interactive"] = {"refused": refused, "gpu_refused": gpu_refused,
                             "policy": status(client)["policy"]}
    client.close()
    clients.pop()

    client = host("batch", {"workload": "batch"})
    clients.append(client)
    sessions = [create(client) for _ in range(3)]
    for session in sessions:
        client.tool("lotus_run_until", {"session": session, "frames": 5})
        rewind(client, session, True)
    policy = status(client)["policy"]
    assert policy["rewind"]["limit"] is None and len(policy["rewind"]["sessions"]) == 3, policy
    report["batch"] = policy
    client.close()
    clients.pop()

    client = host("memory", {"memory_reserve_mib": 1 << 30})
    clients.append(client)
    result = client.send("tools/call", {"name": "lotus_create_session", "arguments": {"asset": "rom"}})
    assert result.get("isError") and "host memory" in result["content"][0]["text"], result
    report["memory"] = {"refused": result["content"][0]["text"], "status": status(client)["policy"]["memory"]}
    assert report["memory"]["status"]["pressure"] is True, report
    client.close()
    clients.pop()

    client = host("no-reserve", {"memory_reserve_mib": 0})
    clients.append(client)
    create(client)
    client.close()
    clients.pop()
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
except BaseException:
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    for client in clients:
        client.process.kill()
    raise
