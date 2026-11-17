"""Shader cache seeding across sessions of one worker binary (#30, `shader_seed: true`).

1. The first Vulkan session starts `cold`, renders, and stops cleanly; its
   shaders/vulkan/pipeline/*.bin is published under <storage>/shader-seed/<worker sha256>/.
2. The next session starts `seeded`; its worker log reports loading the pipeline cache, so the
   copy passed the driver header check (PipelineCache::IsCacheValid).
3. A session whose worker is killed never publishes: the seed is byte-identical afterwards.
Startup times are reported, not asserted (a busy host is no timing evidence).

Usage: test_mcp_shader_seed.py <abs lotus-mcp.exe> <abs worker> <abs content.3dsx> <abs output>
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
from mcp_client import Client

server, worker, rom, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
storage = output / "storage"
config = output / "host.json"
config.write_text(json.dumps({"worker": worker, "storage_root": str(storage), "allow_control": True,
                              "assets": {"rom": rom}, "shader_seed": True}))
client = Client(server, config, output / "transcript.jsonl")
report = {}


def row(session):
    return next(s for s in client.tool("lotus_status", {})["sessions"] if s["id"] == session)


def seed_files():
    root = storage.resolve() / "shader-seed"
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob("*") if p.is_file()} if root.exists() else {}


def session(name):
    created = client.tool("lotus_create_session", {"asset": "rom"})["session"]
    client.tool("lotus_run_until", {"session": created, "frames": 120})
    for _ in range(50):  # gauges refresh at the session's next 100 ms status poll
        telemetry = row(created)["telemetry"]
        if telemetry["renderer"]:
            break
        time.sleep(0.1)
    assert telemetry["renderer"] == "vulkan", telemetry
    report[name] = {"shader_seed": telemetry["shader_seed"], "startup_ms": telemetry["startup_ms"]}
    return created


try:
    first = session("first")
    assert report["first"]["shader_seed"] == "cold", report
    client.tool("lotus_control", {"session": first, "operation": "stop"})
    published = seed_files()
    report["published"] = sorted(published)
    assert any(name.endswith(".bin") and "pipeline" in name for name in published), report

    second = session("second")
    assert report["second"]["shader_seed"] == "seeded", report
    log = (storage.resolve() / "scratch" / second / "log").glob("*")
    text = "".join(p.read_text(errors="replace") for p in log if p.is_file())
    report["second"]["pipeline_cache_loaded"] = "Loading pipeline cache" in text
    assert report["second"]["pipeline_cache_loaded"], "worker did not accept the seeded cache"

    before = seed_files()
    subprocess.run(["taskkill", "/F", "/PID", str(row(second)["telemetry"]["pid"])], check=True,
                   capture_output=True)
    time.sleep(0.5)
    client.send("tools/call", {"name": "lotus_control", "arguments": {"session": second, "operation": "stop"}})
    assert seed_files() == before, "a crashed worker published"
    client.close()
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
except BaseException:
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    client.process.kill()
    raise
