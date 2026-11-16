"""T32 instrumentation on/off overhead A/B (host time) with guest-identity checks.

Each configuration boots a fresh worker on the software renderer (Old 3DS),
warms up 30 frames, then times 60 frames in 10-frame calls. Host seconds are
wall-clock on this machine: not guest time, not a controlled benchmark.
Guest identity: guest ticks and both screen hashes after the timed window
must equal the uninstrumented run; instrumentation must not change the guest.
An armed breakpoint single-steps every instruction, so it is timed over one
frame against a one-frame reference; if the worker's 20 s host deadline ends
that frame first, the result is recorded as a lower bound.
"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import time
from native_debug_probe import Peer

CONFIGS = {
    "off": [],
    "off_one_frame": [],
    "svc_ipc_trace": [("trace", dict(enabled=True))],
    "pica_events": [("gpu_configure", dict(enabled=True, stop_events=[]))],
    "rewind_frames": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64))],
    "armed_breakpoint": [("breakpoint_set", dict(address=0x0F000000, kind="execute", length=4))],
}


def measure(worker, rom, root, operations, frames=60):
    peer = Peer(worker, root)
    try:
        peer.call("hello")
        peer.call("load", {"path": str(rom)})
        peer.call("run_until", {"frames": 30})
        for operation, params in operations:
            peer.debug(operation, **params)
        start = time.perf_counter()
        for _ in range(max(1, frames // 10)):
            peer.call("run_until", {"frames": min(10, frames)}, expected=None)
            if peer.status != "ok":
                return dict(host_seconds_lower_bound=time.perf_counter() - start, status=peer.status)
        seconds = time.perf_counter() - start
        ticks = peer.debug("registers", core=0)["guest_ticks"]
        capture = peer.call("capture")
        screens = {lease["screen"]: hashlib.sha256((root / lease["path"]).read_bytes()[32:]).hexdigest()
                   for lease in capture["frames"]}
        return dict(host_seconds=seconds, guest_ticks=ticks, screens=screens)
    finally:
        peer.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    out = args.output.resolve()
    report = {}
    for name, operations in CONFIGS.items():
        frames = 1 if name in ("off_one_frame", "armed_breakpoint") else 60
        repeats = 1 if name == "armed_breakpoint" else args.repeats
        runs = [measure(args.worker.resolve(), args.rom.resolve(), out / f"{name}-{i}", operations, frames)
                for i in range(repeats)]
        report[name] = dict(frames=frames, runs=runs)
        timed = [r["host_seconds"] for r in runs if "host_seconds" in r]
        report[name]["median_host_seconds"] = statistics.median(timed) if timed else None
    for name, entry in report.items():
        base = report["off_one_frame" if entry["frames"] == 1 else "off"]
        reference = (base["runs"][0]["guest_ticks"], base["runs"][0]["screens"])
        if entry["median_host_seconds"] is None:
            entry["host_ratio_vs_off_lower_bound"] = round(
                entry["runs"][0]["host_seconds_lower_bound"] / base["median_host_seconds"], 1)
            entry["guest_identical"] = None  # frame not reached; nothing to compare
        else:
            entry["host_ratio_vs_off"] = round(entry["median_host_seconds"] / base["median_host_seconds"], 2)
            entry["guest_identical"] = all((r["guest_ticks"], r["screens"]) == reference for r in entry["runs"])
        print(json.dumps({"config": name, "frames": entry["frames"],
                          "median_host_seconds": entry["median_host_seconds"] and round(entry["median_host_seconds"], 3),
                          "ratio": entry.get("host_ratio_vs_off", entry.get("host_ratio_vs_off_lower_bound")),
                          "lower_bound": entry["median_host_seconds"] is None,
                          "guest_identical": entry["guest_identical"]}), flush=True)
    (out / "acceptance.json").write_text(json.dumps(report, indent=2))
    failed = [name for name, entry in report.items() if entry["guest_identical"] is False]
    assert not failed, failed
