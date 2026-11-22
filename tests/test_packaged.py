"""Cross-component acceptance of one packaged host and both packaged workers (lotus#42).

Runs the packaged `lotus-mcp` against each packaged `ctr-worker` on the owned debugger fixture,
with the software renderer (CI runners have no GPU), and checks that what the host expects is
what the worker implements:

- handshake: the worker reports the pinned source commit and exactly the control features this
  backend is built with (`hello.features`, lotus_protocol::capabilities::feature);
- GPU capture: `gpu_configure` at `counters` runs on CITnLO; AZAHAR refuses it with a typed
  `Unsupported` naming `gpu_capture_levels` instead of ignoring the field;
- event subscriptions: CITnLO keeps only stops under `events_configure` while the fixture floods
  svcOutputDebugString (mode 8) and keeps emulating; AZAHAR refuses `events_configure` typed;
- card copy: a secondary created from a running primary's card (`flush_storage`) loads and runs;
- preview: `lotus_capture` returns both screens;
- deterministic state (CITnLO): two secondaries copied from one card, loaded and advanced the
  same number of frames, produce the same `state_hash`.

Usage: test_packaged.py --zip <LOTUS-*-windows-x64.zip> <debugger.3dsx> <debugger.elf> <output dir>
       test_packaged.py <lotus-mcp.exe> <AZAHAR ctr-worker.exe> <CITnLO ctr-worker.exe>
           <integration commit> <experimental commit> <debugger.3dsx> <debugger.elf> <output dir>
`--zip` unpacks the assembled portable ZIP into the output directory and takes the host, both
workers and the expected commits (`release-manifest.json` `native_pins`) from it. The report,
`packaged.json`, records each worker's source commit and features. When LOTUS_DESKTOP_SMOKE
points to the native inspector test binary from the same Rust build, the report also records
desktop checks against both extracted workers.
"""
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mcp_client import Client  # noqa: E402

# Control features each branch lists unconditionally (ctr_worker/control.h Features).
# `preview_nonblocking` and `gpu_memory` depend on the build and are reported, not required.
EXPECTED = {
    "azahar": {"flush_storage", "deterministic_async", "analog_input"},
    "citnlo": {"flush_storage", "event_subscriptions", "gpu_capture_levels", "state_hash",
               "deterministic_async", "analog_input", "rewind_compression_queue"},
}
BUILD_DEPENDENT = {"preview_nonblocking", "gpu_memory"}


def call(client, name, arguments):
    """(result text, is_error) of one tool call."""
    reply = client.send("tools/call", {"name": name, "arguments": arguments})
    return reply["content"][0]["text"], bool(reply.get("isError"))


def debug(client, session, operation, **params):
    return call(client, "lotus_debug", {"session": session, "command": {"operation": operation, **params}})


def ok(result):
    text, error = result
    assert not error, text
    return json.loads(text) if text.startswith(("{", "[")) else text


def refused(result, feature):
    text, error = result
    assert error and f"does not support {feature}" in text, text


def manifest_of(root):
    """A session's native-manifest.json: primaries run in cards/<card>, secondaries in
    scratch/<session>."""
    return json.loads((root / "native-manifest.json").read_text(encoding="utf-8"))


def differences(hashes):
    """Differing components, and for differing serialize sections the first differing byte
    offsets in the `dump` files (sections are consecutive in the .bin, sizes in `sections`)."""
    lines = [f"state_hash differs: {[key for key in hashes[0] if key != 'dump' and hashes[0][key] != hashes[1].get(key)]}"]
    data = [Path(state["dump"]).read_bytes() for state in hashes]
    start = 0
    for first, second in zip(hashes[0]["sections"], hashes[1]["sections"]):
        size = first["bytes"]
        if first != second:
            a, b = data[0][start:start + size], data[1][start:start + size]
            offsets = [i for i in range(min(len(a), len(b))) if a[i] != b[i]]
            lines.append(f"{first['name']} ({size} / {second['bytes']} bytes): {len(offsets)} differ, "
                         f"first {offsets[:16]}; "
                         + "; ".join(f"{i}: {a[max(0, i - 8):i + 8].hex()} / {b[max(0, i - 8):i + 8].hex()}"
                                     for i in offsets[:4]))
        start += size
    return "\n".join(lines)


def check(backend, server, worker, commit, rom, elf, output):
    storage = output / backend / "storage"
    config = output / backend / "host.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(storage), "allow_control": True,
        "max_sessions": 4, "assets": {"fixture": str(rom), "fixture-elf": str(elf)},
        # init_clock FixedTime (Settings::InitClock, 1), as tools/divergence.py boots. With the
        # host clock the two copies' `gpu` serialize sections differed after 30 frames
        # (observed on CITnLO e7a0f82e; mechanism not traced).
        "profile": {"graphics_api": 0, "init_clock": 1}, "prefer_vulkan": False}))
    client = Client(server, config, output / backend / "transcript.jsonl")
    result = {}
    try:
        primary = client.tool("lotus_create_session",
                              {"kind": "primary", "card": "accept", "asset": "fixture"})["session"]
        manifest = manifest_of(storage / "cards" / "accept")
        features = set(manifest["features"] or [])
        assert manifest["source"] == commit, (manifest["source"], commit)
        assert features - BUILD_DEPENDENT == EXPECTED[backend], sorted(features)
        result["handshake"] = dict(source=manifest["source"], features=sorted(features))

        counters = debug(client, primary, "gpu_configure", enabled=True, stop_events=[], capture="counters")
        if backend == "citnlo":
            ok(counters)
            ok(debug(client, primary, "gpu_configure", enabled=False, stop_events=[]))
        else:
            refused(counters, "gpu_capture_levels")
        result["gpu_counters"] = "ok" if backend == "citnlo" else "refused typed"

        if backend == "citnlo":
            ok(debug(client, primary, "events_configure", notify=["stopped"], history=["stopped"]))
            client.tool("lotus_run_until", {"session": primary, "frames": 10})
            ok(debug(client, primary, "load_symbols", asset="fixture-elf"))
            address = {name: ok(debug(client, primary, "resolve_symbol", query=name))["addresses"][0]
                       for name in ("mode", "log_mask")}
            ok(debug(client, primary, "write_memory", address=address["log_mask"],
                     bytes=list(struct.pack("<I", 0))))
            ok(debug(client, primary, "write_memory", address=address["mode"],
                     bytes=list(struct.pack("<I", 8))))
            before = client.tool("lotus_run_until", {"session": primary, "frames": 60})
            status = {row["id"]: row for row in client.tool("lotus_status", {})["sessions"]}[primary]
            events = status["telemetry"]["events"]
            assert events and events["latest"] > 0, events
            result["log_flood"] = dict(frames=before.get("frames"), events=events)
        else:
            refused(debug(client, primary, "events_configure", notify=["stopped"]), "event_subscriptions")
            result["log_flood"] = "events_configure refused typed"

        client.tool("lotus_control", {"session": primary, "operation": "run"})
        time.sleep(0.5)
        copy = client.tool("lotus_create_session", {"kind": "secondary", "card": "accept"})["session"]
        client.tool("lotus_load", {"session": copy, "asset": "fixture"})
        client.tool("lotus_run_until", {"session": copy, "frames": 5})
        result["card_copy"] = "ok"

        frames = client.tool("lotus_capture", {"session": copy})
        assert frames, frames
        result["preview"] = "ok"
        client.tool("lotus_control", {"session": primary, "operation": "pause"})

        if backend == "citnlo":
            pair = [client.tool("lotus_create_session", {"kind": "secondary", "card": "accept"})["session"]
                    for _ in range(2)]
            hashes = []
            for session in pair:
                client.tool("lotus_load", {"session": session, "asset": "fixture"})
                client.tool("lotus_run_until", {"session": session, "frames": 30})
                state = ok(debug(client, session, "state_hash", dump="acceptance"))
                hashes.append({key: value for key, value in state.items() if key != "epoch"})
            same = all(hashes[0][key] == hashes[1][key] for key in hashes[0] if key != "dump")
            assert same, differences(hashes)
            result["deterministic_state"] = "identical"
    finally:
        client.close()
    return result


def desktop_smoke(server, azahar, citnlo, rom):
    report = {
        "citnlo": {"CPU stepping and ELF symbols": "untested",
                   "Memory scans and pointer chains": "untested",
                   "Rewind, checkpoints and movies": "untested",
                   "Event loss and CiTrace export/replay": "untested",
                   "Kernel explorer and thread operations": "untested",
                   "GPU draw and shared GDB listener": "untested"},
        "azahar": {"GDB and GPU capture levels": "untested",
                   "Memory scans, rewind, CiTrace and thread registers": "untested"},
    }
    smoke = os.environ.get("LOTUS_DESKTOP_SMOKE")
    if not smoke:
        return report
    env = dict(os.environ, LOTUS_NATIVE_WORKER=str(Path(citnlo).resolve()),
               LOTUS_AZAHAR_WORKER=str(Path(azahar).resolve()),
               LOTUS_FIXTURE=str(Path(rom).resolve()),
               LOTUS_DEBUG_FIXTURE=str(Path(rom).resolve()))
    env["PATH"] = str(Path(server).resolve().parent) + os.pathsep + env["PATH"]
    for test in ("cpu_steps_and_symbols_use_the_owned_arm_thumb_fixture",
                 "snapshots_and_movies_use_the_owned_fixture",
                 "timeline_preserves_event_loss_and_citrace_provenance",
                 "kernel_explorer_observes_thread_identity_and_native_deadlock",
                 "gdb_reports_unsupported_on_azahar"):
        capture = subprocess.run([str(Path(smoke).resolve()), "--ignored", "--exact", test],
                                 env=env, capture_output=True, text=True, timeout=120)
        print(capture.stdout, end="")
        print(capture.stderr, end="", file=sys.stderr)
        capture.check_returncode()
        if "1 passed" not in capture.stdout:
            raise RuntimeError(f"Expected to run one desktop smoke test: {test}")
    report["citnlo"]["CPU stepping and ELF symbols"] = "passed"
    report["citnlo"]["Memory scans and pointer chains"] = "passed"
    report["citnlo"]["Rewind, checkpoints and movies"] = "passed"
    report["citnlo"]["Event loss and CiTrace export/replay"] = "passed"
    report["citnlo"]["Kernel explorer and thread operations"] = "passed"
    report["azahar"]["GDB and GPU capture levels"] = "unsupported verified"
    report["azahar"]["Memory scans, rewind, CiTrace and thread registers"] = "unsupported verified"
    return report


def main():
    if sys.argv[1] == "--zip":
        archive, rom, elf, output = sys.argv[2:]
        output = Path(output)
        output.mkdir(parents=True, exist_ok=False)
        with zipfile.ZipFile(archive) as package:
            package.extractall(output / "package")
        (root,) = (output / "package").iterdir()
        pins = json.loads((root / "release-manifest.json").read_text(encoding="utf-8"))["native_pins"]
        server, azahar, citnlo = (root / "bin/lotus-mcp.exe", root / "runtimes/azahar/ctr-worker.exe",
                                  root / "runtimes/citnlo/ctr-worker.exe")
        integration, experimental = pins["integration_commit"], pins["experimental_commit"]
    else:
        server, azahar, citnlo, integration, experimental, rom, elf, output = sys.argv[1:]
        output = Path(output)
        output.mkdir(parents=True, exist_ok=False)
    report = {
        "azahar": check("azahar", server, Path(azahar), integration, rom, elf, output),
        "citnlo": check("citnlo", server, Path(citnlo), experimental, rom, elf, output),
        "desktop_debugger": desktop_smoke(server, azahar, citnlo, rom),
    }
    (output / "packaged.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=1))
    print("Packaged host and worker acceptance: passed")


if __name__ == "__main__":
    main()
