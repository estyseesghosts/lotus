"""Archive a bounded CiTrace event slice from a native CITnLO session."""

import argparse
import hashlib
import json
from pathlib import Path
import struct

from mcp_client import Client
from test_investigation_evidence import investigation


def run(server, worker, fixture, output):
    output.mkdir()
    config = output / "host.json"
    storage = output / "storage"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(storage), "allow_control": True,
        "prefer_vulkan": False, "assets": {"owned": str(fixture)},
    }), encoding="utf-8")
    client = Client(server, config, output / "source.jsonl", stderr_path=output / "source.stderr")
    try:
        session = client.tool("lotus_create_session", {"kind": "secondary", "asset": "owned"})["session"]

        def debug(operation):
            return client.tool("lotus_debug", {"session": session, "command": {"operation": operation}})

        client.tool("lotus_run_until", {"session": session, "frames": 1})
        debug("citrace_start")
        client.tool("lotus_run_until", {"session": session, "frames": 3})
        captured = debug("citrace_stop")
        artifact, = captured["artifacts"]
        trace = {key: artifact[key] for key in ("uri", "sha256")}
        missing = {"uri": "lotus://artifact/" + "1" * 32, "sha256": "0" * 64}
        record = investigation(client, "create", details={"title": "CiTrace slice", "traces": [trace, missing]})
        request = {"id": record["id"], "expected_revision": record["revision"]}
        selected = {"uri": trace["uri"], "offset": 1, "limit": 2}
        archive = investigation(client, "export", **request, trace_slices=[selected, {
            "uri": missing["uri"], "offset": 0, "limit": 1,
        }])
        unsliced = investigation(client, "export", **request)
        assert not investigation(client, "bundle", bundle=unsliced["bundle"])["trace_slices"]
        for selection in (dict(selected, offset=captured["elements"]), dict(selected, limit=129),
                          dict(selected, uri="lotus://artifact/" + "2" * 32)):
            investigation(client, "export", expected_error=True, **request, trace_slices=[selection])

        path = storage / "artifacts" / trace["uri"].split("/")[-1]
        raw = path.read_bytes()
        assert hashlib.sha256(raw).hexdigest() == trace["sha256"]
        start, count = struct.unpack_from("<II", raw, 92)
        assert count == captured["elements"] and count >= 3

        def event_at(index):
            kind, first, second, third = struct.unpack_from("<4I", raw, start + index * 20)
            return {
                0xE1: {"kind": "frame_marker"},
                0xE2: {"kind": "memory_load", "bytes": second, "physical_address": third},
                0xE3: {"kind": "register_write", "physical_address": first, "value": second},
            }[kind]

        events = [event_at(1), event_at(2)]
        tail = investigation(client, "export", **request, trace_slices=[dict(selected, offset=count - 1)])
        tail_slice, = investigation(client, "bundle", bundle=tail["bundle"])["trace_slices"]
        assert tail_slice["events"] == [event_at(count - 1)]
        investigation(client, "export", expected_error=True, **request, trace_slices=[selected, selected])
        with path.open("ab") as corrupt:
            corrupt.write(b"corrupt")
        damaged = investigation(client, "export", **request, trace_slices=[selected])
        assert investigation(client, "bundle", bundle=damaged["bundle"])["trace_slices"][0]["status"] == "hash_mismatch"
    finally:
        client.close()

    client = Client(server, config, output / "reader.jsonl", stderr_path=output / "reader.stderr")
    try:
        bundle = investigation(client, "bundle", bundle=archive["bundle"])
        measured, unavailable = bundle["trace_slices"]
        assert measured == {**trace, "offset": 1, "total_events": count, "format": "citrace_v1",
                            "confidence": "measured", "status": "included", "events": events}
        assert unavailable == {**missing, "offset": 0, "confidence": "unknown", "status": "missing"}
        assert archive["bytes"] < 4096
        assert not bundle["traces"][0]["capture_included"]
        assert investigation(client, "summary", bundle=archive["bundle"])["manifest"]["trace_slices"] == [measured, unavailable]
    finally:
        client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "worker", "fixture", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    run(args.server.resolve(), args.worker.resolve(), args.fixture.resolve(), args.output.resolve())
