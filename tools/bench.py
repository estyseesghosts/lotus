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

Metadata (report.json `host`, `workers`): CPU, GPU and driver, RAM, OS build, each worker's
SHA-256, source SHA and build manifest (CMake flags, compiler). Build differences between
workers (e.g. ENABLE_LTO) are listed at the top of summary.md; Debug builds are refused.
Host CPU load is sampled before and after; a busy host (> 15 %) marks the whole report as
not comparable, and `--require-idle` refuses to start.

Shader cache: `--shader-cache cold` (default) boots each Vulkan trial with an empty disk cache;
`warm` first primes one root per worker and copies its `shaders/` into every trial before load.
Warm-up frames warm the in-process pipeline cache either way. Software has no shader cache.

Not covered here (stated in the report): Qt Azahar (no headless frontend with matching
settings), allocation counts, GPU wait time. `--capture-every N` models a live preview by
capturing both screens every N frames; tools/bench_frontends.py measures the real headless
and desktop frontends.

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
import shutil
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_debug_probe import Peer  # noqa: E402

INIT_CLOCK_FIXED = 1  # Settings::InitClock::FixedTime; init_time keeps its default
RENDERERS = {"software": 0, "vulkan": 2}  # Settings::GraphicsAPI values used by settings_set
UNHIT = 0x0F000000  # execute breakpoints here are never reached by 3DSX homebrew (outside its code)
REWIND_FAST_LEVEL = 1  # zstd level for the *_fast rewind configs; default checkpoints use 3
CONFIGS = {
    "off": [],
    "svc_ipc_trace": [("trace", dict(enabled=True))],
    # Worker default capture=full: every draw snapshotted with resources (pre-#24 behaviour).
    "pica_events": [("gpu_configure", dict(enabled=True, stop_events=[]))],
    # GPU capture levels (lotus#24); older CITnLO workers ignore `capture` and run full.
    **{f"gpu_{level}": [("gpu_configure", dict(enabled=True, stop_events=[], capture=level))]
       for level in ("counters", "events")},
    "gpu_draws_8": [("gpu_configure", dict(enabled=True, stop_events=[], capture="draws", max_draws=8))],
    "rewind_frames": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64))],
    **{f"rewind_interval_{n}": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                          interval_frames=n))] for n in (2, 5, 10)},
    # Same checkpoints at a faster zstd level (issue #9); every level restores the same state.
    **{f"rewind_interval_{n}_fast": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                               interval_frames=n,
                                                               compression_level=REWIND_FAST_LEVEL))]
       for n in (1, 2, 5, 10)},
    **{f"bp_execute_{n}": [("breakpoint_set", dict(address=UNHIT + 4 * i, kind="execute", length=4))
                           for i in range(n)] for n in (1, 8, 64)},
}
CITNLO_ONLY = {name for name in CONFIGS if name != "off"}
BUSY = 0.15  # whole-host CPU load above which timings are labelled not comparable


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


def system_load(seconds=2.0):
    """Whole-host CPU busy fraction over `seconds` (Windows GetSystemTimes; None elsewhere)."""
    if os.name != "nt":
        return None

    def sample():
        idle, kernel, user = wt.FILETIME(), wt.FILETIME(), wt.FILETIME()
        ctypes.windll.kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user))
        value = lambda t: (t.dwHighDateTime << 32) | t.dwLowDateTime
        return value(idle), value(kernel) + value(user)  # kernel time includes idle

    idle0, total0 = sample()
    time.sleep(seconds)
    idle1, total1 = sample()
    return round(1 - (idle1 - idle0) / max(1, total1 - total0), 3)


class MemoryStatus(ctypes.Structure):
    _fields_ = [("dwLength", wt.DWORD), ("dwMemoryLoad", wt.DWORD)] + \
               [(name, ctypes.c_ulonglong) for name in ("ullTotalPhys", "ullAvailPhys", "ullTotalPageFile",
                                                        "ullAvailPageFile", "ullTotalVirtual", "ullAvailVirtual",
                                                        "ullAvailExtendedVirtual")]


def host_metadata():
    """Hardware identity for the report. Values that cannot be read are None, never guessed."""
    host = dict(platform=platform.platform(), processor=platform.processor(), cpu_count=os.cpu_count(),
                python=sys.version.split()[0], cpu_name=None, memory_bytes=None, gpus=None)
    if os.name != "nt":
        return host
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") as key:
            host["cpu_name"] = winreg.QueryValueEx(key, "ProcessorNameString")[0].strip()
    except OSError:
        pass
    status = MemoryStatus(dwLength=ctypes.sizeof(MemoryStatus))
    if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)):
        host["memory_bytes"] = status.ullTotalPhys
    try:
        listing = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion | ConvertTo-Json"],
            capture_output=True, text=True, timeout=30, check=True).stdout
        gpus = json.loads(listing) if listing.strip() else []
        host["gpus"] = gpus if isinstance(gpus, list) else [gpus]
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
        pass
    return host


def build_differences(workers_meta):
    """CMake flags (and compilers) that differ between workers' build manifests."""
    flags = {}
    for name, meta in workers_meta.items():
        for flag in (meta.get("build_manifest") or {}).get("cmake_flags") or []:
            if flag.startswith("-D") and "=" in flag:
                key, value = flag[2:].split("=", 1)
                flags.setdefault(key, {})[name] = value
    differing = {key: values for key, values in flags.items()
                 if len(set(values.values())) > 1 or len(values) != len(workers_meta)}
    compilers = {name: (meta.get("build_manifest") or {}).get("compiler") for name, meta in workers_meta.items()}
    if len(set(compilers.values())) > 1:
        differing["compiler"] = compilers
    missing = [name for name, meta in workers_meta.items() if not meta.get("build_manifest")]
    if missing:
        differing["build_manifest"] = {name: "missing (flags unknown)" for name in missing}
    return differing


def trial(worker, rom, root, renderer, new_3ds, operations, warmup, frames, capture_every, shaders=None):
    spawned = time.perf_counter()
    peer = Peer(worker, root)
    try:
        if shaders:  # warm disk shader cache; it is read at load, so copying before load is enough
            shutil.copytree(shaders, root / "shaders")
        hello = peer.call("hello")
        startup_seconds = time.perf_counter() - spawned
        peer.debug("settings_set", name="graphics_api", value=RENDERERS[renderer])
        peer.debug("settings_set", name="is_new_3ds", value=new_3ds)
        # SystemTime seeds the guest date/time from the host clock (shared_page.cpp:40-58), so
        # trials booted at different host seconds can diverge with identical guest ticks.
        peer.debug("settings_set", name="init_clock", value=INIT_CLOCK_FIXED)
        loading = time.perf_counter()
        peer.call("load", {"path": str(rom)})
        load_seconds = time.perf_counter() - loading
        peer.call("run_until", {"frames": warmup})
        for operation, params in operations:
            reply = peer.debug(operation, **params)
            # Older workers ignore unknown parameters; refuse rather than mislabel the trial.
            for key in ("interval_frames", "compression_level"):
                if key in params and reply.get(key) != params[key]:
                    raise AssertionError(f"worker does not support rewind {key}")
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
        rewind = None
        if any(operation == "rewind_configure" for operation, _ in operations):
            # Checkpoint phase costs and memory, then one reverse replay (restore + replay to
            # the previous instruction), timed on the host. Taken after the screens are hashed.
            status = peer.debug("rewind_status")
            stepping = time.perf_counter()
            peer.debug("reverse_step", core=0)
            rewind = dict(status=status, reverse_step_seconds=time.perf_counter() - stepping)
        return dict(status="ok", host_seconds=seconds, fps=frames / seconds, guest_ticks=ticks,
                    screens=screens, usage=usage, source=hello.get("source"),
                    backend_name=hello.get("backend_name"), startup_seconds=startup_seconds,
                    load_seconds=load_seconds, rewind=rewind)
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
        for key in ("startup_seconds", "load_seconds"):
            values = [r[key] for r in runs if r["status"] == "ok" and key in r]
            if values:
                entry[f"median_{key}"] = statistics.median(values)
        rewinds = [r["rewind"] for r in runs if r["status"] == "ok" and r.get("rewind")]
        if rewinds:
            def per_checkpoint(rewind, key):
                status = rewind["status"]
                return status.get(key, 0) / max(1, status.get("checkpoints", 0))
            entry["rewind"] = dict(
                median_checkpoints=statistics.median(r["status"].get("checkpoints", 0) for r in rewinds),
                median_serialize_ms=statistics.median(per_checkpoint(r, "checkpoint_serialize_ns") / 1e6 for r in rewinds),
                median_compress_ms=statistics.median(per_checkpoint(r, "checkpoint_compress_ns") / 1e6 for r in rewinds),
                median_serialized_mib=statistics.median(per_checkpoint(r, "checkpoint_serialized_bytes") / 2**20
                                                        for r in rewinds),
                median_stored_mib=statistics.median(per_checkpoint(r, "checkpoint_stored_bytes") / 2**20
                                                    for r in rewinds),
                median_history_mib=statistics.median(r["status"]["bytes"] / 2**20 for r in rewinds),
                median_restore_ms=statistics.median(r["status"].get("restore_ns", 0) / 1e6
                                                    / max(1, r["status"].get("restores", 0)) for r in rewinds),
                median_reverse_step_seconds=statistics.median(r["reverse_step_seconds"] for r in rewinds))
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
    parser.add_argument("--shader-cache", choices=["cold", "warm"], default="cold")
    parser.add_argument("--require-idle", action="store_true", help="refuse to run on a busy host")
    parser.add_argument("--allow-debug-build", action="store_true")
    args = parser.parse_args()
    workers = [(spec.split("=", 1)[0], Path(spec.split("=", 1)[1]).resolve()) for spec in args.worker]
    rom = args.rom.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    renderers = args.renderer or ["software", "vulkan"]
    configs = args.config or list(CONFIGS)
    load_before = system_load()
    if args.require_idle and load_before is not None and load_before > BUSY:
        raise SystemExit(f"host CPU load {load_before:.0%} > {BUSY:.0%}; refusing (--require-idle)")
    workers_meta = {name: dict(path=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                               build_manifest=manifest(path)) for name, path in workers}
    for name, meta in workers_meta.items():
        if "-DCMAKE_BUILD_TYPE=Debug" in ((meta["build_manifest"] or {}).get("cmake_flags") or []) \
                and not args.allow_debug_build:
            raise SystemExit(f"{name} is a Debug build; timings are not comparable (--allow-debug-build)")
    report = dict(
        tool="tools/bench.py", started=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        host=dict(host_metadata(), cpu_load_before=load_before),
        build_differences=build_differences(workers_meta),
        rom=dict(path=str(rom), sha256=hashlib.sha256(rom.read_bytes()).hexdigest()),
        fixed=dict(new_3ds=args.new_3ds, warmup_frames=args.warmup, frames=args.frames,
                   repeats=args.repeats, capture_every=args.capture_every, shader_cache=args.shader_cache,
                   profile="worker DefaultProfile plus graphics_api/is_new_3ds/init_clock=FixedTime via settings_set"),
        workers=workers_meta,
        not_covered=["Qt Azahar frontend", "allocation counts", "GPU wait time",
                     "frontends (tools/bench_frontends.py)"],
        results={})
    primed = {}
    if args.shader_cache == "warm" and "vulkan" in renderers:
        for name, worker in workers:
            root = out / "prime" / name
            trial(worker, rom, root, "vulkan", args.new_3ds, [], args.warmup, args.frames, 0)
            primed[name] = root / "shaders"
    for renderer in renderers:
        for name, worker in workers:
            for config in configs:
                skipped = None
                runs = []
                for index in range(args.repeats):
                    root = out / renderer / name / config / str(index)
                    try:
                        runs.append(trial(worker, rom, root, renderer, args.new_3ds, CONFIGS[config],
                                          args.warmup, args.frames, args.capture_every,
                                          primed.get(name) if renderer == "vulkan" else None))
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
    report["host"]["cpu_load_after"] = system_load()
    loads = [v for v in (report["host"]["cpu_load_before"], report["host"]["cpu_load_after"]) if v is not None]
    report["host_busy"] = any(v > BUSY for v in loads)
    (out / "report.json").write_text(json.dumps(report, indent=2))
    (out / "partial.json").unlink()
    host = report["host"]
    lines = [f"# Bench {report['started']}", ""]
    if report["host_busy"]:
        lines += [f"**Not comparable: host CPU load {loads} exceeded {BUSY:.0%} (another process was busy).**", ""]
    gpus = ", ".join(f"{g.get('Name')} ({g.get('DriverVersion')})" for g in host.get("gpus") or []) or "unknown"
    lines += [f"Host: {host.get('cpu_name') or host['processor']}, {host['cpu_count']} threads, "
              f"{(host.get('memory_bytes') or 0) / 2**30:.1f} GiB, GPU {gpus}, {host['platform']}", ""]
    for name, meta in report["workers"].items():
        build = meta["build_manifest"] or {}
        reported = {e.get("source") for by_worker in report["results"].values()
                    for e in by_worker.get(name, {}).values() if e.get("source")}
        lines.append(f"Worker {name}: reports source {', '.join(sorted(reported)) or 'unknown'}, "
                     f"sha256 {meta['sha256'][:16]}, {build.get('compiler', 'compiler unknown')}")
        if build and reported and reported != {build.get("source")}:
            report["build_manifest_stale"] = True
            lines.append(f"  **Build manifest is stale: it records source {build.get('source')}; "
                         "its flags may not describe this binary.**")
    if report["build_differences"]:
        lines += ["", "Build differences between workers (cross-worker ratios include them):"]
        lines += [f"- {key}: " + ", ".join(f"{w}={v}" for w, v in values.items())
                  for key, values in report["build_differences"].items()]
    reference = workers[0][0]
    lines += ["", f"Shader cache: {args.shader_cache} (Vulkan only). A CITnLO worker's `off` has the "
                  "debugger attached with every feature disabled (the passive-debugger case).", "",
              f"ROM {report['rom']['sha256'][:16]}, {'New' if args.new_3ds else 'Old'} 3DS, "
              f"warm-up {args.warmup}, {args.frames} frames x {args.repeats}, capture every {args.capture_every or '-'}", "",
              f"| renderer | worker | config | median s | fps | spread s | ratio vs off | ratio vs {reference} "
              "| guest identical | startup s |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for renderer, by_worker in report["results"].items():
        for name, by_config in by_worker.items():
            for config, e in by_config.items():
                if "skipped" in e:
                    lines.append(f"| {renderer} | {name} | {config} | skipped | | | | | | |")
                    continue
                median = e.get("median_host_seconds")
                lines.append("| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |".format(
                    renderer, name, config,
                    f"{median:.3f}" if median else f">= {e['host_seconds_lower_bound']:.1f}",
                    f"{args.frames / median:.1f}" if median else "",
                    f"{e['min_host_seconds']:.3f}-{e['max_host_seconds']:.3f}" if median else "",
                    e.get("ratio_vs_off", e.get("ratio_vs_off_lower_bound", "")),
                    e.get(f"ratio_vs_{reference}", ""),
                    e.get("guest_identical_to_off", "") if config != "off" else e["guest_repeatable"],
                    f"{e['median_startup_seconds']:.2f}" if "median_startup_seconds" in e else ""))
    rewind_rows = [(renderer, name, config, e["rewind"]) for renderer, by_worker in report["results"].items()
                   for name, by_config in by_worker.items() for config, e in by_config.items() if e.get("rewind")]
    if rewind_rows:
        lines += ["", "Rewind checkpoints (per checkpoint, medians over trials; host time):", "",
                  "| renderer | worker | config | checkpoints | serialize ms | compress ms | serialized MiB "
                  "| stored MiB | history MiB | restore ms | reverse step s |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        lines += [f"| {renderer} | {name} | {config} | {r['median_checkpoints']:.0f} | {r['median_serialize_ms']:.1f} | "
                  f"{r['median_compress_ms']:.1f} | {r['median_serialized_mib']:.1f} | {r['median_stored_mib']:.2f} | "
                  f"{r['median_history_mib']:.1f} | {r['median_restore_ms']:.1f} | {r['median_reverse_step_seconds']:.3f} |"
                  for renderer, name, config, r in rewind_rows]
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    (out / "report.json").write_text(json.dumps(report, indent=2))  # with build_manifest_stale
    print((out / "summary.md").read_text())


if __name__ == "__main__":
    main()
