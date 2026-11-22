"""Windows evidence acceptance using an owned mock worker and independent MCP clients."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import time

from mcp_client import Client


def investigation(client, operation, *, expected_error=False, **fields):
    return client.tool(
        "lotus_investigation", {"operation": {"op": operation, **fields}}, expected_error
    )


def run(server, worker, fixture, output):
    output.mkdir(parents=True, exist_ok=False)
    storage = output / "storage"
    config = output / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(storage),
        "allow_control": True, "prefer_vulkan": False, "assets": {"owned": str(fixture)},
        "artifacts": {"max_bytes": 1024 * 1024},
    }), encoding="utf-8")

    original = Client(server, config, output / "original.jsonl")
    try:
        session = original.tool("lotus_create_session", {"kind": "secondary", "asset": "owned"})["session"]

        def session_row():
            return next(row for row in original.tool("lotus_list_sessions", {}) if row["id"] == session)

        row = session_row()
        screens = original.tool("lotus_capture", {"session": session})["content"]
        refs = [{"uri": screen["uri"], "sha256": screen["description"].split()[-1]} for screen in screens]
        missing_uri = "lotus://artifact/" + "1" * 32
        refs.append({"uri": missing_uri, "sha256": "0" * 64})
        record = investigation(original, "create", details={
            "title": "Owned worker failure",
            "purpose": r"C:\private\do-not-export.txt",
            "sessions": [{"session": session, "created_unix_ms": row["created_unix_ms"],
                          "epoch": row["telemetry"]["epoch"]}],
            "artifacts": refs,
            "traces": [{"uri": "lotus://artifact/" + "2" * 32, "sha256": "0" * 64}],
        })
        subprocess.run(["taskkill", "/F", "/PID", str(row["telemetry"]["pid"])],
                       check=True, capture_output=True, timeout=8)
        deadline = time.monotonic() + 10
        while not (failure := session_row()["telemetry"]["failure"]):
            assert time.monotonic() < deadline, "worker exit was never published"
            time.sleep(0.05)
        capture = {"id": record["id"], "expected_revision": record["revision"],
                   "screenshots": [refs[0]["uri"], missing_uri]}
        exported = investigation(original, "export", **capture)
        assert exported["bytes"] < 1024 * 1024
        assert investigation(original, "export", **capture) == exported
        bundle = exported["bundle"]
        rejected = refs[1]["uri"]
        artifact = storage / "artifacts" / (rejected.split("/")[-1] + ".bgra")
        with artifact.open("ab") as file:
            file.write(b"unexpected trailing bytes")
        corrupt = investigation(original, "export", **dict(capture, screenshots=[rejected]))
        observed = investigation(original, "bundle", bundle=corrupt["uri"])
        assert not observed["images"]
        assert observed["screenshot_observations"] == [
            {"uri": rejected, "confidence": "unknown", "status": "hash_mismatch"},
        ]
    finally:
        original.close()

    tightened = json.loads(config.read_text(encoding="utf-8"))
    tightened["artifacts"]["max_bytes"] = 64
    config.write_text(json.dumps(tightened), encoding="utf-8")
    consumer = Client(server, config, output / "consumer.jsonl")
    try:
        assert consumer.tool("lotus_list_sessions", {}) == []
        result = consumer.send("tools/call", {"name": "lotus_investigation", "arguments": {
            "operation": {"op": "bundle", "bundle": bundle},
        }})
        assert not result.get("isError"), result
        manifest_block, png_block = result["content"]
        manifest = json.loads(manifest_block["text"])
        assert hashlib.sha256(manifest_block["text"].encode()).hexdigest() == bundle
        assert manifest["notes"] is None and "do-not-export" not in manifest_block["text"]
        assert manifest["sessions"][0]["observation"]["failure"] == {
            key: failure[key] for key in ("cause", "exit_code", "renderer")
        }
        assert manifest["sessions"][0]["observation"]["identity"]["worker_sha256"] == (
            hashlib.sha256(worker.read_bytes()).hexdigest()
        )
        assert manifest["sessions"][0]["observation"]["identity"]["content_sha256"] == (
            hashlib.sha256(fixture.read_bytes()).hexdigest()
        )
        assert manifest["traces"][0]["status"] == "missing"
        assert manifest["screenshot_observations"] == [
            {"uri": missing_uri, "confidence": "unknown", "status": "missing"},
        ]
        image, = manifest["images"]
        png = base64.b64decode(png_block["data"], validate=True)
        assert png_block["mimeType"] == "image/png" and png.startswith(b"\x89PNG\r\n\x1a\n")
        assert hashlib.sha256(png).hexdigest() == image["png_sha256"]
        assert image["native_sha256"] == refs[0]["sha256"]
        summary = investigation(consumer, "summary", bundle=bundle)
        assert image["data"] not in summary["markdown"]
        assert "PICA draw history" in summary["markdown"]
        refused = investigation(consumer, "export", expected_error=True,
                                id=record["id"], expected_revision=record["revision"])
        assert "evidence bundle exceeds its byte limit" in refused
        path = storage / "evidence" / f"{bundle}.json"
        path.write_bytes(path.read_bytes() + b" ")
        refused = investigation(consumer, "bundle", bundle=bundle, expected_error=True)
        assert "hash mismatch" in refused
    finally:
        consumer.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ("server", "worker", "fixture", "output"):
        parser.add_argument(argument, type=Path)
    args = parser.parse_args()
    run(args.server.resolve(), args.worker.resolve(), args.fixture.resolve(), args.output.resolve())
