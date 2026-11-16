"""Repeatable native-worker benchmark: AZAHAR vs CITnLO, software vs Vulkan, per debugger feature.

Each trial boots a fresh worker in a fresh root, applies the renderer/model settings before
load, warms up, applies the configuration's debug operations, then times a fixed number of
frames in `run_until` calls. Recorded per trial: host wall seconds, worker CPU seconds and
disk bytes (Windows GetProcessTimes/GetProcessIoCounters, whole process lifetime), guest
ticks and both screen hashes after the window. Host seconds are wall-clock on this machine:
not guest time, not hardware cycles.

Comparability rules (enforced, see `compare`): ratios are only computed between trials with
the same ROM SHA-256, renderer, model, warm-up, frames and capture cadence. Guest identity
(ticks + screens) is checked against the same worker's `off` run in that group, and across
workers against the first worker given. A mismatch is reported, never averaged away.

Not covered here (stated in the report): Qt Azahar (no headless frontend with matching
settings), GPUI desktop presentation, allocation counts. `--capture-every N` models a live
preview by capturing both screens every N frames. Vulkan trials start with an empty disk
shader cache (fresh root); warm-up frames warm the in-process pipeline cache.

    python tools/bench.py --worker integration=<ctr-worker.exe> --worker citnlo=<ctr-worker.exe> \
        --rom <abs .3dsx> --output runs/<fresh> [--renderer software --renderer vulkan] \
        [--config off --config bp_execute_1 ...] [--repeats 5] [--warmup 60] [--frames 300]
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_debug_probe import Peer  # noqa: E402

INIT_CLOCK_FIXED = 1  # Settings::InitClock::FixedTime; init_time keeps its default
RENDERERS = {"software": 0, "vulkan": 2}  # Settings::GraphicsAPI values used by settings_set
UNHIT = 0x0F000000  # execute breakpoints here are never reached by 3DSX homebrew (outside its code)
CONFIGS = {
    "off": [],
    "svc_ipc_trace": [("trace", dict(enabled=True))],
    "pica_events": [("gpu_configure", dict(enabled=True, stop_events=[]))],
    "rewind_frames": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64))],
    **{f"rewind_interval_{n}": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                          interval_frames=n))] for n in (2, 5, 10)},
    **{f"bp_execute_{n}": [("breakpoint_set", dict(address=UNHIT + 4 * i, kind="execute", length=4))
                           for i in range(n)] for n in (1, 8, 64)},
}
CITNLO_ONLY = {name for name in CONFIGS if name != "off"}


class Counters(ctypes.Structure):
    _fields_ = [(name, ctypes.c_ulonglong) for name in
                ("ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                 "ReadTransferCount", "WriteTransferCount", "OtherTransferCount")]


def process_usage(process):
    """CPU seconds and I/O bytes of a live child process (Windows only; None elsewhere)."""
    if os.name != "nt":
        return None
    handle = wt.HANDLE(process._handle)
    times = [wt.FILETIME() for _ in range(4)]
    io = Counters()
    kernel32 = ctypes.windll.kernel32
    if not kernel32.GetProcessTimes(handle, *map(ctypes.byref, times)) or \
            not kernel32.GetProcessIoCounters(handle, ctypes.byref(io)):
        return None
    seconds = lambda t: ((t.dwHighDateTime << 32) | t.dwLowDateTime) / 1e7
    return dict(cpu_seconds=seconds(times[2]) + seconds(times[3]),
                io_read_bytes=io.ReadTransferCount, io_write_bytes=io.WriteTransferCount)


def trial(worker, rom, root, renderer, new_3ds, operations, warmup, frames, capture_every):
    peer = Peer(worker, root)
    try:
        hello = peer.call("hello")
        peer.debug("settings_set", name="graphics_api", value=RENDERERS[renderer])
        peer.debug("settings_set", name="is_new_3ds", value=new_3ds)
        # SystemTime seeds the guest date/time from the host clock (shared_page.cpp:40-58), so
        # trials booted at different host seconds can diverge with identical guest ticks.
        peer.debug("settings_set", name="init_clock", value=INIT_CLOCK_FIXED)
        peer.call("load", {"path": str(rom)})
        peer.call("run_until", {"frames": warmup})
        for operation, params in operations:
            reply = peer.debug(operation, **params)
            # Older workers ignore unknown parameters; refuse rather than mislabel the trial.
            if "interval_frames" in params and reply.get("interval_frames") != params["interval_frames"]:
                raise AssertionError("worker does not support rewind interval_frames")
        before = process_usage(peer.process)
        start = time.perf_counter()
        done = 0
        while done < frames:
            step = min(10, frames - done, capture_every or 10)
            peer.call("run_until", {"frames": step}, expected=None)
            if peer.status != "ok":
                return dict(status=peer.status, frames_done=done,
                            host_seconds_lower_bound=time.perf_counter() - start)
            done += step
            if capture_every and done % capture_every == 0:
                for lease in peer.call("capture")["frames"]:
                    (root / lease["path"]).read_bytes()
        seconds = time.perf_counter() - start
        after = process_usage(peer.process)
        ticks = peer.debug("registers", core=0)["guest_ticks"]
        capture = peer.call("capture")
        screens = {lease["screen"]: hashlib.sha256((root / lease["path"]).read_bytes()[32:]).hexdigest()
                   for lease in capture["frames"]}
        usage = {key: after[key] - before[key] for key in after} if before and after else None
        return dict(status="ok", host_seconds=seconds, fps=frames / seconds, guest_ticks=ticks,
                    screens=screens, usage=usage, source=hello.get("source"),
                    backend_name=hello.get("backend_name"))
    finally:
        peer.close()


def manifest(worker):
    for parent in worker.parents:
        candidate = parent / "lotus-build-manifest.json"
        if candidate.is_file():
            return json.loads(candidate.read_text())
    return None


def summarize(runs):
    timed = [r["host_seconds"] for r in runs if r["status"] == "ok"]
    entry = dict(trials=len(runs), completed=len(timed))
    if timed:
        entry.update(median_host_seconds=statistics.median(timed), min_host_seconds=min(timed),
                     max_host_seconds=max(timed),
                     stdev_host_seconds=statistics.stdev(timed) if len(timed) > 1 else 0.0)
        cpu = [r["usage"]["cpu_seconds"] for r in runs if r["status"] == "ok" and r["usage"]]
        if cpu:
            entry["median_cpu_seconds"] = statistics.median(cpu)
        writes = [r["usage"]["io_write_bytes"] for r in runs if r["status"] == "ok" and r["usage"]]
        if writes:
            entry["median_io_write_bytes"] = statistics.median(writes)
    else:
        entry["host_seconds_lower_bound"] = max(r["host_seconds_lower_bound"] for r in runs)
    return entry


def identity(runs):
    ok = [(r["guest_ticks"], r["screens"]) for r in runs if r["status"] == "ok"]
    return ok[0] if ok and all(item == ok[0] for item in ok) else None


def compare(results, workers):
    """Ratios and guest identity within one renderer group only."""
    for renderer, by_worker in results.items():
        reference_worker = workers[0][0]
        for name, by_config in by_worker.items():
            off = by_config.get("off")
            for config, entry in by_config.items():
                runs = entry.pop("runs_detail")
                if "skipped" in entry:
                    continue
                entry["guest_repeatable"] = identity(runs) is not None
                if off and "median_host_seconds" in off and config != "off":
                    if "median_host_seconds" in entry:
                        entry["ratio_vs_off"] = round(entry["median_host_seconds"] / off["median_host_seconds"], 3)
                    else:
                        entry["ratio_vs_off_lower_bound"] = round(
                            entry["host_seconds_lower_bound"] / off["median_host_seconds"], 1)
                    entry["guest_identical_to_off"] = (identity(runs) == off["_identity"]
                                                       if identity(runs) else None)
                ref = by_worker.get(reference_worker, {}).get(config)
                if name != reference_worker and ref and "median_host_seconds" in ref and \
                        "median_host_seconds" in entry:
                    entry[f"ratio_vs_{reference_worker}"] = round(
                        entry["median_host_seconds"] / ref["median_host_seconds"], 3)
                    entry[f"guest_identical_to_{reference_worker}"] = (
                        identity(runs) == ref["_identity"] if identity(runs) else None)
        for by_config in by_worker.values():
            for entry in by_config.values():
                entry.pop("_identity", None)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--worker", action="append", required=True, help="NAME=path to ctr-worker.exe")
    parser.add_argument("--rom", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--renderer", action="append", choices=sorted(RENDERERS))
    parser.add_argument("--config", action="append", choices=sorted(CONFIGS))
    parser.add_argument("--new-3ds", action="store_true")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=60)
    parser.add_argument("--frames", type=int, default=300)
    parser.add_argument("--capture-every", type=int, default=0, help="capture both screens every N frames")
    args = parser.parse_args()
    workers = [(spec.split("=", 1)[0], Path(spec.split("=", 1)[1]).resolve()) for spec in args.worker]
    rom = args.rom.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    renderers = args.renderer or ["software", "vulkan"]
    configs = args.config or list(CONFIGS)
    report = dict(
        tool="tools/bench.py", started=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        host=dict(platform=platform.platform(), processor=platform.processor(), cpu_count=os.cpu_count(),
                  python=sys.version.split()[0]),
        rom=dict(path=str(rom), sha256=hashlib.sha256(rom.read_bytes()).hexdigest()),
        fixed=dict(new_3ds=args.new_3ds, warmup_frames=args.warmup, frames=args.frames,
                   repeats=args.repeats, capture_every=args.capture_every,
                   profile="worker DefaultProfile plus graphics_api/is_new_3ds/init_clock=FixedTime via settings_set"),
        workers={name: dict(path=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                            build_manifest=manifest(path)) for name, path in workers},
        not_covered=["Qt Azahar frontend", "GPUI desktop presentation", "allocation counts",
                     "GPU wait time"],
        results={})
    for renderer in renderers:
        for name, worker in workers:
            for config in configs:
                skipped = None
                runs = []
                for index in range(args.repeats):
                    root = out / renderer / name / config / str(index)
                    try:
                        runs.append(trial(worker, rom, root, renderer, args.new_3ds, CONFIGS[config],
                                          args.warmup, args.frames, args.capture_every))
                    except AssertionError as error:  # typed refusal, e.g. CITnLO-only operation
                        skipped = str(error)[:300]
                        break
                if skipped:
                    entry = dict(skipped=skipped)
                    if config not in CITNLO_ONLY:
                        raise SystemExit(f"{name}/{renderer}/{config} failed: {skipped}")
                else:
                    entry = summarize(runs)
                    entry["runs_detail"] = runs
                    entry["_identity"] = identity(runs)
                    entry["source"] = runs[0].get("source")
                    entry["backend_name"] = runs[0].get("backend_name")
                report["results"].setdefault(renderer, {}).setdefault(name, {})[config] = entry
                (out / "partial.json").write_text(json.dumps(report, indent=2, default=str))
                print(json.dumps({"renderer": renderer, "worker": name, "config": config,
                                  **{k: v for k, v in entry.items() if k not in ("runs_detail", "_identity")}},
                                 default=str), flush=True)
    # Keep per-trial detail in its own file; the summary carries ratios and identity flags.
    detail = {renderer: {name: {config: entry.get("runs_detail") for config, entry in by_config.items()}
                         for name, by_config in by_worker.items()}
              for renderer, by_worker in report["results"].items()}
    (out / "trials.json").write_text(json.dumps(detail, indent=2))
    for by_worker in report["results"].values():
        for by_config in by_worker.values():
            for entry in by_config.values():
                entry.setdefault("runs_detail", [])
    compare(report["results"], workers)
    (out / "report.json").write_text(json.dumps(report, indent=2))
    (out / "partial.json").unlink()
    lines = [f"# Bench {report['started']}", "",
             f"ROM {report['rom']['sha256'][:16]}, {'New' if args.new_3ds else 'Old'} 3DS, "
             f"warm-up {args.warmup}, {args.frames} frames x {args.repeats}, capture every {args.capture_every or '-'}", "",
             "| renderer | worker | config | median s | fps | spread s | ratio vs off | guest identical |",
             "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for renderer, by_worker in report["results"].items():
        for name, by_config in by_worker.items():
            for config, e in by_config.items():
                if "skipped" in e:
                    lines.append(f"| {renderer} | {name} | {config} | skipped | | | | |")
                    continue
                median = e.get("median_host_seconds")
                lines.append("| {} | {} | {} | {} | {} | {} | {} | {} |".format(
                    renderer, name, config,
                    f"{median:.3f}" if median else f">= {e['host_seconds_lower_bound']:.1f}",
                    f"{args.frames / median:.1f}" if median else "",
                    f"{e['min_host_seconds']:.3f}-{e['max_host_seconds']:.3f}" if median else "",
                    e.get("ratio_vs_off", e.get("ratio_vs_off_lower_bound", "")),
                    e.get("guest_identical_to_off", "") if config != "off" else e["guest_repeatable"]))
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    print((out / "summary.md").read_text())


if __name__ == "__main__":
    main()
