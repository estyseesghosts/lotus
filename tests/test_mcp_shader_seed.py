"""Shader cache seeding across sessions of one worker binary (#30, `shader_seed: true`).

1. The first Vulkan session starts `cold`, renders, and stops cleanly; its
   shaders/vulkan/pipeline/*.bin is published under <storage>/shader-seed/<worker sha256>/.
2. The next session starts `seeded`; its worker log reports loading the pipeline cache, so the
   copy passed the driver header check (PipelineCache::IsCacheValid).
3. The seeded sessions produce the same guest tick and screen hashes as the cold session.
4. Two seeded workers can crash without changing the published seed.
Startup times are reported, not asserted (a busy host is no timing evidence).

Usage: test_mcp_shader_seed.py <abs lotus-mcp.exe> <abs worker> <abs content.3dsx> <abs output>
"""
import base64
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
                              "assets": {"rom": rom}, "shader_seed": True,
                              "profile": {"graphics_api": 2, "frame_limit": 0, "init_clock": 1,
                                          "deterministic_async_operations": True}}))
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
    payload = client.send("tools/call", {"name": "lotus_capture_image", "arguments": {"session": created}})
    assert not payload.get("isError"), payload
    report[name]["screen_sha256"] = [hashlib.sha256(base64.b64decode(item["data"])).hexdigest()
                                    for item in payload["content"] if item["type"] == "image"]
    assert len(report[name]["screen_sha256"]) == 2, payload
    report[name]["guest_ticks"] = client.tool("lotus_debug", {
        "session": created, "command": {"operation": "registers", "core": 0}})["guest_ticks"]
    return created


try:
    first = session("first")
    assert report["first"]["shader_seed"] == "cold", report
    client.tool("lotus_control", {"session": first, "operation": "stop"})
    published = seed_files()
    report["published"] = sorted(published)
    assert any(name.endswith(".bin") and "pipeline" in name for name in published), report

    seeded = {}
    for name in ("second", "third"):
        sid = session(name)
        seeded[name] = sid
        assert report[name]["shader_seed"] == "seeded", report
        log = (storage.resolve() / "scratch" / sid / "log").glob("*")
        text = "".join(p.read_text(errors="replace") for p in log if p.is_file())
        report[name]["pipeline_cache_loaded"] = "Loading pipeline cache" in text
        assert report[name]["pipeline_cache_loaded"], "worker did not accept the seeded cache"
        for field in ("guest_ticks", "screen_sha256"):
            assert report[name][field] == report["first"][field], report

    before = seed_files()
    for sid in seeded.values():
        subprocess.run(["taskkill", "/F", "/PID", str(row(sid)["telemetry"]["pid"])], check=True,
                       capture_output=True)
    for sid in seeded.values():
        client.send("tools/call", {"name": "lotus_control", "arguments": {"session": sid, "operation": "stop"}})
    client.close()
    assert seed_files() == before, "a crashed worker published"
    report["seed_unchanged_after_two_crashes"] = True
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
except BaseException:
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    if client.process.poll() is None:
        client.process.kill()
        client.process.wait()
    raise
