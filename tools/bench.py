"""Repeatable native-worker benchmark: AZAHAR vs CITnLO, software vs Vulkan, per debugger feature.

Each trial boots a fresh worker in a fresh root, applies the renderer/model settings before
load, warms up, applies the configuration's debug operations, then times a fixed number of
frames in `run_until` calls. Recorded per trial: host wall seconds, worker CPU seconds and
I/O bytes (Windows GetProcessTimes/GetProcessIoCounters, deltas for the measured window), guest
ticks and both screen hashes after the window. Host seconds are wall-clock on this machine:
not guest time, not hardware cycles.

Comparability rules (enforced, see `compare`): ratios are only computed between trials with
the same ROM SHA-256, renderer, model, warm-up, frames and capture cadence. Guest identity
(ticks + screens) is checked against the same worker's `off` run in that group, and across
workers against the first worker given. Trials alternate worker order every repeat.

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
PROFILE_KEYS = ("graphics_api", "resolution_factor", "use_hw_shader", "use_disk_shader_cache",
                "is_new_3ds", "use_cpu_jit", "cpu_clock_percentage", "deterministic_async_operations",
                "audio_emulation", "output_type", "volume", "frame_limit", "init_clock",
                "init_ticks_type", "init_ticks_override", "init_time")
UNHIT = 0x0F000000  # execute breakpoints here are never reached by 3DSX homebrew (outside its code)
# Memory::PROCESS_IMAGE_VADDR (src/core/memory.h), where the 3DSX loader puts .text; never written,
# but an armed watchpoint switches CITnLO to instruction stepping ("execution_mode").
TEXT_BASE = 0x00100000
REWIND_FAST_LEVEL = 1  # zstd level for the *_fast rewind configs; default checkpoints use 3
HISTORY_WITHOUT_PICA = ("stopped", "log", "native_log", "svc", "ipc")
CONFIGS = {
    "off": [],
    "svc_ipc_trace": [("trace", dict(enabled=True))],
    # Worker default capture=full: every draw snapshotted with resources (pre-#24 behaviour).
    "pica_events": [("gpu_configure", dict(enabled=True, stop_events=[]))],
    # GPU capture levels (lotus#24); older CITnLO workers ignore `capture` and run full.
    **{f"gpu_{level}": [("gpu_configure", dict(enabled=True, stop_events=[], capture=level))]
       for level in ("counters", "events")},
    **{f"gpu_{level}_no_pica_history": [
        ("events_configure", dict(notify=["stopped"], history=list(HISTORY_WITHOUT_PICA))),
        ("gpu_configure", dict(enabled=True, stop_events=[], capture=level)),
    ] for level in ("events", "full")},
    "gpu_draws_8": [("gpu_configure", dict(enabled=True, stop_events=[], capture="draws", max_draws=8))],
    "rewind_frames": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64))],
    **{f"rewind_interval_{n}": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                          interval_frames=n))] for n in (2, 5, 10)},
    # Same checkpoints at a faster zstd level (issue #9); every level restores the same state.
    **{f"rewind_interval_{n}_fast": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                               interval_frames=n,
                                                               compression_level=REWIND_FAST_LEVEL))]
       for n in (1, 2, 5, 10)},
    # lotus#32: the worker default (compression_queue 1) compresses checkpoints off the
    # emulation thread; `_sync` compresses on it, as every worker before #32 did, and
    # `_queue_2` allows two checkpoints in compression at once.
    **{f"rewind_interval_{n}_{name}": [("rewind_configure", dict(enabled=True, capacity=32, budget_mib=64,
                                                                 interval_frames=n, compression_queue=queue))]
       for n in (1, 2, 5, 10) for name, queue in (("sync", 0), ("queue_2", 2))},
    **{f"bp_execute_{n}": [("breakpoint_set", dict(address=UNHIT + 4 * i, kind="execute", length=4))
                           for i in range(n)] for n in (1, 8, 64)},
    "watch_write_1": [("breakpoint_set", dict(address=TEXT_BASE, kind="write", length=4))],
}
CITNLO_ONLY = {name for name in CONFIGS if name != "off"}
# Settings the workers accept only before load (settings_bindings.h "boot"), applied with settings_set
# before `load`. Both workers bind them. deterministic_async runs HLE async sections on the emulation
# thread, so the guest wake tick does not depend on host thread timing (lotus#29); CITnLO's
# DefaultProfile turns it on, AZAHAR keeps the upstream default (off).
BOOT_CONFIGS = {
    "deterministic_async": [("deterministic_async_operations", True)],
    "host_async": [("deterministic_async_operations", False)],
}
BUSY = 0.15  # whole-host CPU load above which timings are labelled not comparable


class UnsupportedConfiguration(Exception):
    """A worker explicitly refused or did not implement a requested debug option."""


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


# A fresh root gets a random MAC (cfg.cpp GenerateRandomMAC, also in the guest's shared page)
# and console unique ID (GenerateConsoleUniqueId) at first load (lotus#29). Trials copy one
# template's copies so every trial boots the same console.
IDENTITY = ("sysdata", "nand")


def configuration_mismatch(operation, params, reply):
    """Older workers may silently ignore an option; never time a different workload."""
    checked = {
        "rewind_configure": ("interval_frames", "compression_level", "compression_queue"),
        "gpu_configure": ("capture", "draw_range", "max_draws", "resources"),
    }
    if operation == "events_configure":
        for key in ("notify", "history"):
            if key in params and set(reply.get(key, ())) != set(params[key]):
                return f"{operation}: requested {key}={params[key]!r}, worker reported {reply.get(key)!r}"
    for key in checked.get(operation, ()):
        if key in params and reply.get(key) != params[key]:
            return f"{operation}: requested {key}={params[key]!r}, worker reported {reply.get(key)!r}"
    return None


def identity_template(worker, rom, root, new_3ds):
    peer = Peer(worker, root)
    try:
        peer.call("hello")
        peer.debug("settings_set", name="is_new_3ds", value=new_3ds)
        peer.call("load", {"path": str(rom)})
    finally:
        peer.close()
    return root


def trial(worker, rom, root, renderer, new_3ds, operations, warmup, frames, capture_every, shaders=None,
          identity=None, pokes=(), reads=(), boot=(), frame_limit=None):
    spawned = time.perf_counter()
    peer = Peer(worker, root)
    try:
        hello = peer.call("hello")
        startup_seconds = time.perf_counter() - spawned
        if shaders:  # warm disk shader cache; it is read at load, so copying before load is enough
            shutil.copytree(shaders, root / "shaders")
        for name in IDENTITY if identity else ():  # also read at load
            shutil.copytree(identity / name, root / name, dirs_exist_ok=True)
        # SystemTime seeds the guest date/time from the host clock (shared_page.cpp:40-58), so
        # trials booted at different host seconds can diverge with identical guest ticks.
        requested = dict(graphics_api=RENDERERS[renderer], is_new_3ds=new_3ds, init_clock=INIT_CLOCK_FIXED)
        if frame_limit is not None:
            requested["frame_limit"] = frame_limit
        requested.update(boot)
        for setting, value in requested.items():
            peer.debug("settings_set", name=setting, value=value)
        loading = time.perf_counter()
        peer.call("load", {"path": str(rom)})
        load_seconds = time.perf_counter() - loading
        for address, value in pokes:  # guest workload switches, set before warm-up
            peer.debug("write_memory", address=address, bytes=list(value.to_bytes(4, "little")))
        profile = {}
        offset = 0
        while offset is not None:
            page = peer.debug("settings_list", offset=offset, limit=32)
            profile.update({entry["name"]: entry["value"] for entry in page["settings"]
                            if entry["name"] in PROFILE_KEYS or entry["name"] in requested})
            offset = page["next"]
        if any(key not in profile or profile[key] != value for key, value in requested.items()):
            raise AssertionError(f"loaded profile {profile!r} does not match {requested!r}")
        # The native worker accepts up to 600 frames per request and can also
        # return early with status=ok after a debugger stop.
        frame = 0
        while frame < warmup:
            target = min(frame + 600, warmup)
            warmed = peer.call("run_until", {"frames": target - frame})
            if warmed.get("frames") != target:
                raise AssertionError(f"early warm-up stop at {warmed.get('frames')}, expected {target}")
            frame = target
        for operation, params in operations:
            reply = peer.call("debug", dict(operation=operation, **params), expected=None)
            if peer.status == "unsupported":
                raise UnsupportedConfiguration(f"{operation}: {reply!r}")
            if peer.status != "ok":
                raise AssertionError(f"{operation}: {peer.status}, {reply!r}")
            mismatch = configuration_mismatch(operation, params, reply)
            if mismatch:
                raise UnsupportedConfiguration(mismatch)
        before = process_usage(peer.process)
        start = time.perf_counter()
        done = 0
        while done < frames:
            until_capture = capture_every - done % capture_every if capture_every else 10
            step = min(10, frames - done, until_capture)
            result = peer.call("run_until", {"frames": step}, expected=None) or {}
            if peer.status != "ok" or result.get("frames") != frame + done + step:
                return dict(status=peer.status if peer.status != "ok" else "early_stop",
                            frames_done=max(0, result.get("frames", frame + done) - frame),
                            stop=result.get("stop"), last_error=result.get("last_error"),
                            host_seconds_lower_bound=time.perf_counter() - start)
            done += step
            if capture_every and done % capture_every == 0:
                for lease in peer.call("capture")["frames"]:
                    (root / lease["path"]).read_bytes()
        seconds = time.perf_counter() - start
        after = process_usage(peer.process)
        ticks = peer.debug("registers", core=0)["guest_ticks"]
        guest = {name: int.from_bytes(bytes(peer.debug("read_memory", address=address, length=4)["bytes"]),
                                      "little") for name, address in reads}
        capture = peer.call("capture")
        screens = {lease["screen"]: hashlib.sha256((root / lease["path"]).read_bytes()[32:]).hexdigest()
                   for lease in capture["frames"]}
        usage = {key: after[key] - before[key] for key in after} if before and after else None
        rewind = None
        if any(operation == "rewind_configure" for operation, _ in operations):
            # Checkpoint phase costs and memory, then one reverse replay (restore + replay to
            # the previous instruction), timed on the host. Taken after the screens are hashed.
            status = peer.debug("rewind_status")
            # Frame checkpoints have no core. An instruction step records the
            # checkpoint that reverse_step for this core needs.
            peer.debug("step", core=0, count=1)
            stepping = time.perf_counter()
            peer.debug("reverse_step", core=0)
            reverse_step_seconds = time.perf_counter() - stepping
            restored = peer.debug("rewind_status")
            rewind = dict(status=status, reverse_step_seconds=reverse_step_seconds,
                          restore_ns=restored.get("restore_ns", 0) - status.get("restore_ns", 0))
        return dict(status="ok", host_seconds=seconds, fps=frames / seconds, guest_ticks=ticks, profile=profile,
                    screens=screens, usage=usage, source=hello.get("source"),
                    backend_name=hello.get("backend_name"), startup_seconds=startup_seconds,
                    load_seconds=load_seconds, rewind=rewind, guest=guest)
    finally:
        peer.close()


def manifest(worker):
    packaged = worker.with_name("build-manifest.json")
    if packaged.is_file():
        return json.loads(packaged.read_text(encoding="utf-8"))
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
        for key in ("io_read_bytes", "io_write_bytes"):
            values = [r["usage"][key] for r in runs if r["status"] == "ok" and r["usage"]]
            if values:
                entry[f"median_{key}"] = statistics.median(values)
        profiles = [r["profile"] for r in runs if r["status"] == "ok"]
        entry["effective_profile"] = profiles[0] if all(p == profiles[0] for p in profiles) else None
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
                median_restore_ms=statistics.median(r["restore_ns"] / 1e6 for r in rewinds),
                median_reverse_step_seconds=statistics.median(r["reverse_step_seconds"] for r in rewinds))
    else:
        entry["host_seconds_lower_bound"] = max(r["host_seconds_lower_bound"] for r in runs)
    return entry


def identity(runs):
    if not runs or any(r["status"] != "ok" for r in runs):
        return None
    states = [(r["guest_ticks"], r["screens"], r["guest"]) for r in runs]
    return states[0] if all(item == states[0] for item in states) else None


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
                    profile = entry["effective_profile"]
                    baseline = ref["effective_profile"]
                    if profile is None or baseline is None:
                        entry["profile_comparison"] = "settings varied across repeats"
                    else:
                        differences = {key: {reference_worker: baseline.get(key), name: profile.get(key)}
                                       for key in baseline.keys() | profile.keys()
                                       if baseline.get(key) != profile.get(key)}
                        if differences:
                            entry["profile_differences"] = differences
                        elif entry["completed"] == entry["trials"] and ref["completed"] == ref["trials"]:
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
    parser.add_argument("--config", action="append", choices=sorted(CONFIGS | BOOT_CONFIGS))
    parser.add_argument("--new-3ds", action="store_true")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=60)
    parser.add_argument("--frames", type=int, default=300)
    parser.add_argument("--capture-every", type=int, default=0, help="capture both screens every N frames")
    parser.add_argument("--shader-cache", choices=["cold", "warm"], default="cold")
    parser.add_argument("--require-idle", action="store_true", help="refuse to run on a busy host")
    parser.add_argument("--allow-debug-build", action="store_true")
    parser.add_argument("--frame-limit", type=int, help="percent, 0 unthrottled; default keeps the worker's 100")
    parser.add_argument("--setting", action="append", default=[], metavar="NAME=JSON",
                        help="apply the same native setting to every worker before loading")
    parser.add_argument("--poke", action="append", default=[], metavar="SYMBOL=VALUE",
                        help="write a u32 guest variable (from <rom>.elf) after load, e.g. churn=1")
    parser.add_argument("--read", action="append", default=[], metavar="SYMBOL",
                        help="record a u32 guest variable after the window, e.g. generation_done")
    args = parser.parse_args()
    if args.repeats < 1 or args.frames < 1 or args.warmup < 0 or args.capture_every < 0:
        parser.error("repeats and frames must be positive, warmup and capture-every nonnegative")
    workers = [(spec.split("=", 1)[0], Path(spec.split("=", 1)[1]).resolve()) for spec in args.worker]
    if len({name for name, _ in workers}) != len(workers):
        parser.error("worker labels must be unique")
    rom = args.rom.resolve()
    try:
        common_settings = {key: json.loads(value) for key, value in
                           (setting.split("=", 1) for setting in args.setting)}
    except ValueError as error:
        parser.error(f"invalid --setting NAME=JSON: {error}")
    if {"graphics_api", "is_new_3ds", "frame_limit", "init_clock"} & common_settings.keys():
        parser.error("renderer, console model, frame limit and fixed clock are controlled by benchmark options")
    pokes, reads = [], []
    if args.poke or args.read:
        from debugger_acceptance import symbols
        table = symbols(rom.with_suffix(".elf"))
        pokes = [(table[name], int(value, 0)) for name, value in (p.split("=", 1) for p in args.poke)]
        reads = [(name, table[name]) for name in args.read]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    renderers = args.renderer or ["software", "vulkan"]
    configs = args.config or list(CONFIGS)  # boot configs only when named
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
                   pokes=args.poke, reads=args.read, frame_limit=args.frame_limit, settings=common_settings,
                   profile="worker DefaultProfile plus graphics_api/is_new_3ds/init_clock=FixedTime via settings_set; "
                           "sysdata and nand copied from one template boot (same MAC and console ID)"),
        workers=workers_meta,
        not_covered=["Qt Azahar frontend", "allocation counts", "GPU wait time",
                     "frontends (tools/bench_frontends.py)"],
        results={})
    console = identity_template(workers[0][1], rom, out / "identity", args.new_3ds)
    primed = {}
    if args.shader_cache == "warm" and "vulkan" in renderers:
        for name, worker in workers:
            root = out / "prime" / name
            prime = trial(worker, rom, root, "vulkan", args.new_3ds, [], args.warmup, args.frames, 0,
                          identity=console, boot=common_settings, frame_limit=args.frame_limit)
            if prime["status"] != "ok" or not (root / "shaders").is_dir():
                raise SystemExit(f"{name}: failed to prime Vulkan shaders: {prime['status']}")
            primed[name] = root / "shaders"
    for renderer in renderers:
        for config in configs:
            boot = common_settings | dict(BOOT_CONFIGS.get(config, ()))
            runs_by_worker = {name: [] for name, _ in workers}
            skipped_by_worker = {}
            for index in range(args.repeats):
                start = index % len(workers)
                for name, worker in workers[start:] + workers[:start]:
                    if name in skipped_by_worker:
                        continue
                    root = out / renderer / name / config / str(index)
                    try:
                        runs_by_worker[name].append(trial(
                            worker, rom, root, renderer, args.new_3ds, CONFIGS.get(config, []),
                            args.warmup, args.frames, args.capture_every,
                            primed.get(name) if renderer == "vulkan" else None,
                            console, pokes, reads, boot, args.frame_limit))
                    except UnsupportedConfiguration as error:
                        if config not in CITNLO_ONLY:
                            raise SystemExit(f"{name}/{renderer}/{config} failed: {error}") from error
                        skipped_by_worker[name] = str(error)[:300]
            for name, runs in runs_by_worker.items():
                if name in skipped_by_worker:
                    entry = dict(skipped=skipped_by_worker[name], runs_detail=runs)
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
    profile_notes = [f"- {renderer}/{name}/{config}: {e.get('profile_differences', e.get('profile_comparison'))}"
                     for renderer, by_worker in report["results"].items()
                     for name, by_config in by_worker.items() for config, e in by_config.items()
                     if "profile_differences" in e or "profile_comparison" in e]
    if profile_notes:
        lines += ["", "Effective settings prevented a cross-worker ratio for these rows:", *profile_notes]
    reference = workers[0][0]
    lines += ["", f"Shader cache: {args.shader_cache} (Vulkan only). A CITnLO worker's `off` has the "
                  "debugger attached with every feature disabled (the passive-debugger case).", "",
              f"ROM {report['rom']['sha256'][:16]}, {'New' if args.new_3ds else 'Old'} 3DS, "
              f"warm-up {args.warmup}, {args.frames} frames x {args.repeats}, capture every {args.capture_every or '-'}, frame limit {'worker default' if args.frame_limit is None else f'{args.frame_limit} %'}", "",
              f"| renderer | worker | config | median s | fps | spread s | ratio vs off | ratio vs {reference} "
              "| guest identical | startup s | completed |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for renderer, by_worker in report["results"].items():
        for name, by_config in by_worker.items():
            for config, e in by_config.items():
                if "skipped" in e:
                    lines.append(f"| {renderer} | {name} | {config} | skipped | | | | | | | |")
                    continue
                median = e.get("median_host_seconds")
                lines.append("| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |".format(
                    renderer, name, config,
                    f"{median:.3f}" if median else f">= {e['host_seconds_lower_bound']:.1f}",
                    f"{args.frames / median:.1f}" if median else "",
                    f"{e['min_host_seconds']:.3f}-{e['max_host_seconds']:.3f}" if median else "",
                    e.get("ratio_vs_off", e.get("ratio_vs_off_lower_bound", "")),
                    e.get(f"ratio_vs_{reference}", ""),
                    e.get("guest_identical_to_off", "") if config != "off" else
                    e.get(f"guest_identical_to_{reference}", e["guest_repeatable"]),
                    f"{e['median_startup_seconds']:.2f}" if "median_startup_seconds" in e else "",
                    f"{e['completed']}/{e['trials']}"))
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
