"""Replay a Windows investigation bundle with the normal native scenario runner."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

from mcp_client import Client
from test_investigation_evidence import investigation


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run(server, cli, worker, fixture, output):
    output.mkdir()
    worker_hash = digest(worker)
    content_hash = digest(fixture)
    scenario = {
        "version": 2, "name": "Owned fixture replay",
        "content": {"kind": "homebrew_3dsx", "path_handle": "C:/private-fixtures/owned.3dsx",
                    "sha256": content_hash},
        "profile": {"model": "old3ds", "renderer": "software", "worker_flavor": "experimental",
                    "worker_sha256": worker_hash},
        "storage": {"mode": "isolated_copy"}, "input": {"owner": "scenario"},
        "stop": {"max_frames": 3, "wall_timeout_seconds": 15},
        "assertions": [{"type": "frame_count", "minimum": 3}],
    }
    approved = output / "approved.json"
    approved.write_text(json.dumps(scenario), encoding="utf-8")
    unpinned = output / "unpinned.json"
    unpinned.write_text(json.dumps(dict(scenario, profile={
        key: value for key, value in scenario["profile"].items() if key != "worker_sha256"
    })), encoding="utf-8")
    config = output / "host.json"
    config.write_text(json.dumps({"worker": str(worker), "storage_root": str(output / "storage"),
                                  "allow_control": True, "prefer_vulkan": False,
                                  "scenarios": {"approved": str(approved), "unpinned": str(unpinned)}}),
                      encoding="utf-8")
    client = Client(server, config, output / "source.jsonl", stderr_path=output / "source.stderr")
    try:
        record = investigation(client, "create", details={"title": "Native replay"})
        request = {"id": record["id"], "expected_revision": record["revision"]}
        for handle, reason in ((str(approved), "unknown scenario handle"),
                               ("unpinned", "pinned worker executable SHA-256")):
            error = investigation(client, "export", **request, replay_scenario=handle, expected_error=True)
            assert reason in error, error
        bundle = investigation(client, "export", **request, replay_scenario="approved")["bundle"]
    finally:
        client.close()

    # Confirm a second client can use the archive independently of its original scenario file.
    approved.unlink()
    client = Client(server, config, output / "reader.jsonl", stderr_path=output / "reader.stderr")
    try:
        exported = investigation(client, "bundle", bundle=bundle)
        assert "private-fixtures" not in json.dumps(exported)
        replay = exported["replay"]
        assert replay["confidence"] == "unverified"
        assert [{key: value for key, value in row.items() if value is not None}
                for row in replay["scenario"]["assertions"]] == scenario["assertions"]
    finally:
        client.close()

    scenario_file = output / replay["scenario_filename"]
    scenario_file.write_text(json.dumps(replay["scenario"]), encoding="utf-8")
    local_fixture = output / replay["scenario"]["content"]["path_handle"]
    shutil.copyfile(fixture, local_fixture)

    def execute(binary, run_root):
        command = replay["argv"].copy()
        command[0] = str(cli)
        command[command.index("<worker.exe>")] = str(binary)
        command[command.index("<new-output-directory>")] = run_root
        return subprocess.run(command, cwd=output, capture_output=True, text=True, timeout=30)

    wrong_worker = execute(server, "wrong-worker")
    assert wrong_worker.returncode and "worker executable SHA-256 mismatch" in wrong_worker.stderr
    assert not (output / "wrong-worker").exists()
    passed = execute(worker, "replay-results")
    assert passed.returncode == 0, passed.stderr
    report, = json.loads(passed.stdout)
    assert report["worker_executable_sha256"] == worker_hash
    assert report["content_sha256"] == content_hash and report["frames"] == 3
    with local_fixture.open("ab") as content:
        content.write(b"modified")
    wrong_asset = execute(worker, "wrong-asset")
    assert wrong_asset.returncode and "fixture SHA-256 mismatch" in wrong_asset.stderr
    assert not (output / "wrong-asset").exists()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "worker", "fixture", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    run(*(path.resolve() for path in vars(args).values()))
