"""Debugger event delivery under a guest_log flood (lotus#26). Windows.

Uses the debugger fixture's mode 8 (fixtures/debugger/fixture.c): one svcOutputDebugString per
`log_mask + 1` guest loop iterations. Event counts come from the worker's `status.events.latest`
(the event sequence) and `status.transport` (writer counters); a worker without them (v0.0.3)
is reported with nulls.

slow_reader, per worker: hello with every category pushed (`notifications: true`, no
`subscriptions`, the v0.0.3 Lotus hello), flood at the highest rate, `run` 600 frames at the
default speed limit, then stop reading stdout for `--pause` seconds. Recorded: frames emulated
during the pause (a worker that writes on the emulation thread stops when the pipe fills), the
worker's private bytes before and after, and, after reading resumes, whether `pause` produces
its reliable `stopped` notification and whether `status.stop.sequence` names it. Asserted for
workers that report `transport`: emulation advanced during the pause, the best-effort queue
stayed within its bound, the writer dropped and counted events, and the stop arrived.

matrix: N workers (`--workers`, default 1 4 8) run back-to-back `run_until` of `--frames` with
the speed limit off, for each subscription mode and flood rate, reading notifications as they
arrive. Modes: `all` (every category pushed: v0.0.3 Lotus), `stops` (`subscriptions:
["stopped"]`, the Lotus default from lotus#26; history keeps everything), `quiet` (stops only,
`events_configure` history []; skipped for workers without `events_configure`, such as v0.0.3).
Recorded per worker: fps, events per second, notifications
received, status round trip (p50/p95) measured between batches, CPU seconds.

    python tools/event_flood.py --worker citnlo=<ctr-worker.exe> [--worker v003=<v0.0.3 worker>] \
        --rom <abs debugger.3dsx> --output runs/<fresh> [--workers 1 --workers 4 --workers 8] \
        [--rate 0xffff --rate 0xfff --rate 0xff] [--frames 120] [--batches 5] [--pause 3] [--require-idle]

Build the fixture with `python tools/build-debug-fixture.py <dir>`; the ELF next to the 3DSX
gives the symbol addresses (llvm-nm).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import struct
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_debug_probe import Peer  # noqa: E402
from debugger_acceptance import symbols  # noqa: E402
from bench_parallel import Process  # noqa: E402
import bench  # noqa: E402

MODES = {
    "all": ({"notifications": True}, None),
    "stops": ({"notifications": True, "subscriptions": ["stopped"]}, None),
    "quiet": ({"notifications": True, "subscriptions": ["stopped"]}, []),
}


def start(worker, rom, root, syms, hello, history, mask, frame_limit):
    peer = Peer(worker, root)
    peer.call("hello", hello)
    if history is not None:
        peer.debug("events_configure", history=history)
    peer.debug("settings_set", name="frame_limit", value=frame_limit)
    peer.call("load", {"path": str(rom)})
    peer.call("run_until", {"frames": 10})
    peer.debug("write_memory", address=syms["log_mask"], bytes=list(struct.pack("<I", mask)))
    peer.debug("write_memory", address=syms["mode"], bytes=list(struct.pack("<I", 8)))
    return peer


def configures_events(worker, root):
    """Whether the worker accepts `events_configure` (v0.0.3 does not; it needs no content)."""
    peer = Peer(worker, root)
    try:
        peer.call("hello", MODES["stops"][0])
        peer.call("debug", dict(operation="events_configure", history=["stopped"]), expected=None)
        return peer.status == "ok"
    finally:
        peer.close()


def events(status):
    return (status.get("events") or {}).get("latest")


def slow_reader(worker, rom, root, syms, mask, pause):
    peer = start(worker, rom, root, syms, MODES["all"][0], None, mask, 100)
    process = Process(peer.process.pid)
    try:
        before = peer.call("status")
        memory_before = process.usage()["private_bytes"]
        peer.call("run", {"frames": 600})
        # The reader thread blocks once its 32-entry queue is full, so the pipe fills behind it.
        time.sleep(pause)
        memory_paused = process.usage()["private_bytes"]
        peer.notifications.clear()
        after = peer.call("status")
        frames_during = after["frames"] - before["frames"]
        peer.call("pause")
        deadline = time.monotonic() + 10
        stop = None
        while stop is None and time.monotonic() < deadline:
            status = peer.call("status")
            stop = next((n["data"] for n in peer.notifications if n["data"].get("kind") == "stopped"
                         and n["data"].get("reason") == "manual"), None)
        transport = status.get("transport")
        result = dict(frames_during_pause=frames_during, pause_seconds=pause,
                      private_bytes_before=memory_before, private_bytes_paused=memory_paused,
                      events_during_pause=None if events(after) is None else events(after) - events(before),
                      transport=transport, stop_notification=stop, status_stop=status.get("stop"),
                      stop_sequence_matches=bool(stop) and status.get("stop", {}).get("sequence") == stop.get("sequence"))
        if transport is not None:
            assert frames_during > 0, result
            assert transport["best_effort_queued"] <= transport["best_effort_limit"], result
            assert transport["dropped"] > 0, result
            assert stop is not None and result["stop_sequence_matches"], result
        return result
    finally:
        process.close()
        peer.close()


def matrix_trial(worker, rom, root, syms, mode, mask, count, frames, batches):
    hello, history = MODES[mode]
    peers = [start(worker, rom, root / f"w{i}", syms, hello, history, mask, 0) for i in range(count)]
    results = [None] * count

    def drive(index):
        peer = peers[index]
        process = Process(peer.process.pid)
        status_times, received = [], 0
        first = peer.call("status")
        cpu = process.usage()["cpu_seconds"]
        start_time = time.perf_counter()
        for _ in range(batches):
            peer.notifications.clear()
            lost = peer.notifications_lost
            peer.call("run_until", {"frames": frames}, expected=None)
            received += len(peer.notifications) + peer.notifications_lost - lost
            t = time.perf_counter()
            peer.call("status")
            status_times.append((time.perf_counter() - t) * 1000)
        elapsed = time.perf_counter() - start_time
        last = peer.call("status")
        results[index] = dict(
            fps=(last["frames"] - first["frames"]) / elapsed,
            events_per_second=None if events(last) is None else (events(last) - events(first)) / elapsed,
            notifications_per_second=received / elapsed,
            notifications_dropped=(last.get("transport") or {}).get("dropped"),
            status_ms_p50=statistics.median(status_times), status_ms_max=max(status_times),
            cpu_seconds=process.usage()["cpu_seconds"] - cpu, seconds=elapsed)
        process.close()

    try:
        threads = [threading.Thread(target=drive, args=(i,)) for i in range(count)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    finally:
        for peer in peers:
            peer.close()
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--worker", action="append", required=True, help="name=ctr-worker.exe")
    parser.add_argument("--rom", type=Path, required=True, help="debugger fixture .3dsx (ELF beside it)")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workers", action="append", type=int)
    parser.add_argument("--rate", action="append", help="log_mask values (hex)")
    parser.add_argument("--mode", action="append", choices=list(MODES))
    parser.add_argument("--frames", type=int, default=120)
    parser.add_argument("--batches", type=int, default=5)
    parser.add_argument("--pause", type=float, default=3)
    parser.add_argument("--skip-slow-reader", action="store_true")
    parser.add_argument("--require-idle", action="store_true", help="refuse to run on a busy host")
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("Windows only")
    workers = [(n, Path(p).resolve()) for n, p in (w.split("=", 1) for w in args.worker)]
    rom = args.rom.resolve()
    syms = symbols(rom.with_suffix(".elf"))
    masks = [int(r, 16) for r in (args.rate or ["0xffff", "0xfff", "0xff"])]
    counts = args.workers or [1, 4, 8]
    modes = args.mode or list(MODES)
    out = args.output
    load = bench.system_load()
    if args.require_idle and load is not None and load > bench.BUSY:
        raise SystemExit(f"host CPU load {load:.0%} > {bench.BUSY:.0%}; refusing (--require-idle)")
    out.mkdir(parents=True, exist_ok=False)
    report = dict(tool="tools/event_flood.py", host=dict(bench.host_metadata(), cpu_load_before=load),
                  workers={n: dict(path=str(p), sha256=hashlib.sha256(p.read_bytes()).hexdigest())
                           for n, p in workers},
                  fixed=dict(frames=args.frames, batches=args.batches, pause=args.pause),
                  slow_reader={}, matrix=[], skipped=[])
    for name, worker in workers:
        if not args.skip_slow_reader:
            report["slow_reader"][name] = slow_reader(worker, rom, out / name / "slow", syms, min(masks), args.pause)
            print(name, "slow_reader", json.dumps(report["slow_reader"][name]), flush=True)
        configurable = configures_events(worker, out / name / "probe")
        for mode in modes:
            if MODES[mode][1] is not None and not configurable:
                report["skipped"].append(dict(worker=name, mode=mode, reason="no events_configure"))
                print(name, mode, "skipped: no events_configure", flush=True)
                continue
            for mask in masks:
                for count in counts:
                    root = out / name / mode / f"{mask:x}-n{count}"
                    sessions = matrix_trial(worker, rom, root, syms, mode, mask, count, args.frames, args.batches)
                    entry = dict(worker=name, mode=mode, log_mask=mask, workers=count, sessions=sessions)
                    report["matrix"].append(entry)
                    print(json.dumps(dict(worker=name, mode=mode, log_mask=hex(mask), workers=count,
                                          fps=[round(s["fps"]) for s in sessions],
                                          events_per_second=sessions[0]["events_per_second"],
                                          status_ms=round(max(s["status_ms_p50"] for s in sessions), 2))), flush=True)
    report["host"]["cpu_load_after"] = bench.system_load()
    loads = [v for v in (load, report["host"]["cpu_load_after"]) if v is not None]
    report["host_busy"] = any(v > bench.BUSY for v in loads)
    (out / "report.json").write_text(json.dumps(report, indent=2))
    lines = ["# Event flood", ""]
    if report["host_busy"]:
        lines += [f"**Not comparable: host CPU load {loads} exceeded {bench.BUSY:.0%}.**", ""]
    for name, slow in report["slow_reader"].items():
        lines.append(f"- {name} slow reader ({args.pause} s): {slow['frames_during_pause']} frames emulated, "
                     f"private bytes {slow['private_bytes_before'] >> 20} -> {slow['private_bytes_paused'] >> 20} MiB, "
                     f"dropped {(slow['transport'] or {}).get('dropped')}, stop delivered "
                     f"{slow['stop_notification'] is not None}, status names it {slow['stop_sequence_matches']}")
    lines += [f"- {s['worker']} {s['mode']}: skipped ({s['reason']})" for s in report["skipped"]]
    lines += ["", "| worker | mode | log_mask | N | fps min / median | events/s per worker | notifications/s | "
              "status p50 ms (max) | CPU s/s |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for e in report["matrix"]:
        s = e["sessions"]
        fps = [x["fps"] for x in s]
        eps = [x["events_per_second"] for x in s if x["events_per_second"] is not None]
        lines.append(f"| {e['worker']} | {e['mode']} | {e['log_mask']:#x} | {e['workers']} | "
                     f"{min(fps):.0f} / {statistics.median(fps):.0f} | "
                     f"{statistics.median(eps) if eps else float('nan'):.0f} | "
                     f"{statistics.median([x['notifications_per_second'] for x in s]):.0f} | "
                     f"{statistics.median([x['status_ms_p50'] for x in s]):.2f} ({max(x['status_ms_max'] for x in s):.1f}) | "
                     f"{statistics.median([x['cpu_seconds'] / x['seconds'] for x in s]):.2f} |")
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    print((out / "summary.md").read_text())


if __name__ == "__main__":
    main()
