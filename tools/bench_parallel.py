"""Concurrent-emulator throughput and interference through the real supervisor (lotus#22). Windows.

Each trial starts a fresh frontend (`lotus-mcp`, or `lotus-desktop --mcp` with session 0 focused
so it is live-previewed) with its own host config and storage root, creates N secondary sessions
(ROMs assigned round-robin), applies one tools/bench.py debug configuration to every session,
plays them all continuously and waits out a warm-up. During the measured window, once per
`--probe-interval` (default 0.25 s), it times per session one worker command (`lotus_registers`,
a safe-point exchange) and one input (`lotus_input`, which also waits for the worker), then polls
`lotus_status`. A fixed rate keeps the probe load per session the same at every N; probing as fast
as possible gave one session all of it and halved it at N = 2 (runs/bp-real-02). When a round
takes longer than the interval the rate drops, and the report says so (`probe_rounds`).

Recorded per session: guest fps over the window (telemetry `frames`, presented frames), command
and input round trips (p50/p95/max), supervision poll counters (busy and late polls, lateness and
status-exchange times, from the session's supervision task), the worker's CPU seconds, disk bytes,
working set and private bytes (GetProcessTimes, GetProcessIoCounters, K32GetProcessMemoryInfo),
startup time and shader seed state, and guest ticks at the end. Per trial: aggregate fps, fps
skew (max / min), available host RAM before and after, and host time to shut the frontend down
with all sessions still running.

Host throughput, guest progress and correctness are kept apart: fps and CPU are host measures;
guest ticks are reported per session but never compared, because free-running sessions are not
deterministic (docs/34, #29). GPU memory (lotus#46) comes from each session's
`telemetry.gpu_memory` (CITnLO VMA statistics): device-local live and high-water block bytes and
failed allocations per session, and per trial the host policy's `gpu_memory` (smallest
device-local headroom, budget source, pressure) before and after the window. Budgets are the
driver's only with `VK_EXT_memory_budget`; otherwise the source reads `vma_estimate`. Not
measured: GPU wait time, host allocation counts.

Host policy (lotus#23): `--workload batch` (default) has no budgets, so every session may run
the same expensive configuration and timings see no scheduling; `--workload interactive` applies
the default budgets (a config over budget is reported `unsupported` with the reason).
`--focus-priority` focuses session 0 (`lotus_focus_session`) with `policy.focus_priority`, so its
worker runs in the above-normal priority class; compare session 0's command latency and the
others' fps with and without it.

`--idle K` adds K sessions that load the same ROM and configuration but stay paused (a debugger
holding sessions open); they get the same probes, are excluded from fps and skew, and their CPU
and command latency are reported separately.

`--stall` suspends session 0's worker (NtSuspendProcess) for the measured window, so the other
sessions show whether one hung worker slows them; it is resumed before shutdown. `--capture`
adds a `lotus_capture` per session per round (screenshot contention). `--shader-cache seeded`
enables `shader_seed` and primes the seed with one session that stops cleanly before the trial;
`cold` leaves it off. `--reference` also runs tools/bench.py's single-worker trial for each
worker/renderer/model/config, so the 1-session row can be compared with the existing harness.

A busy host (> 15 % CPU before or after) marks the report not comparable; `--require-idle`
refuses to start. Raw rounds are kept in report.json; summary.md has the tables and
aggregate.svg plots aggregate fps against N.

    python tools/bench_parallel.py --server target/release/lotus-mcp.exe --worker citnlo=<ctr-worker.exe> \
        --rom t03=<abs .3dsx> --output runs/<fresh> [--workers 1 --workers 2 --workers 4] \
        [--renderer vulkan|software] [--model old|new] [--config off --config pica_events ...] \
        [--desktop target/release/lotus-desktop.exe --frontend headless --frontend desktop] \
        [--idle 0] [--stall] [--capture] [--shader-cache cold|seeded] [--frame-limit 0] [--reference] \
        [--warmup-seconds 5] [--seconds 20] [--repeats 1] [--require-idle]

CI smoke (mock worker, no emulation): `--smoke` with `--worker mock=target/release/lotus-mock-native.exe`
and any file as ROM; it skips settings and debug configuration and shortens the windows.
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import datetime
import hashlib
import json
import os
from pathlib import Path
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402
from bench_frontends import Frontend  # noqa: E402

RENDERERS = {"vulkan": 2, "software": 0}
PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME = 0x1000, 0x0800


class MemoryCounters(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD)] + \
               [(name, ctypes.c_size_t) for name in (
                   "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                   "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage",
                   "PeakPagefileUsage", "PrivateUsage")]


class Process:
    """A worker process by pid: usage counters and suspend/resume. Values that cannot be read are None."""

    def __init__(self, pid):
        self.pid = pid
        kernel32 = ctypes.windll.kernel32
        kernel32.OpenProcess.restype = wt.HANDLE
        self.handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SUSPEND_RESUME, False, pid)

    def usage(self):
        if not self.handle:
            return None
        kernel32 = ctypes.windll.kernel32
        times = [wt.FILETIME() for _ in range(4)]
        io = bench.Counters()
        memory = MemoryCounters(cb=ctypes.sizeof(MemoryCounters))
        if not kernel32.GetProcessTimes(self.handle, *map(ctypes.byref, times)) or \
                not kernel32.GetProcessIoCounters(self.handle, ctypes.byref(io)) or \
                not kernel32.K32GetProcessMemoryInfo(self.handle, ctypes.byref(memory), memory.cb):
            return None
        seconds = lambda t: ((t.dwHighDateTime << 32) | t.dwLowDateTime) / 1e7
        return dict(cpu_seconds=seconds(times[2]) + seconds(times[3]), io_read_bytes=io.ReadTransferCount,
                    io_write_bytes=io.WriteTransferCount, working_set_bytes=memory.WorkingSetSize,
                    private_bytes=memory.PrivateUsage, peak_working_set_bytes=memory.PeakWorkingSetSize,
                    # Peak commit charge of the process: private bytes at their highest (lotus#32).
                    peak_private_bytes=memory.PeakPagefileUsage)

    def suspend(self, on):
        ntdll = ctypes.windll.ntdll
        status = (ntdll.NtSuspendProcess if on else ntdll.NtResumeProcess)(self.handle)
        assert status == 0, f"NtSuspendProcess/NtResumeProcess failed: {status:#x}"

    def close(self):
        if self.handle:
            ctypes.windll.kernel32.CloseHandle(self.handle)


def available_ram():
    status = bench.MemoryStatus(dwLength=ctypes.sizeof(bench.MemoryStatus))
    return status.ullAvailPhys if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)) else None


def call(frontend, name, arguments):
    """(seconds, result or None, error text or None) of one MCP tool call."""
    start = time.perf_counter()
    result = frontend.call("tools/call", {"name": name, "arguments": arguments})
    elapsed = time.perf_counter() - start
    text = result["content"][0]["text"] if result.get("content") else ""
    if result.get("isError"):
        return elapsed, None, text
    try:
        return elapsed, json.loads(text), None
    except json.JSONDecodeError:
        return elapsed, text, None


def tool(frontend, name, arguments):
    _, result, error = call(frontend, name, arguments)
    assert error is None, f"{name}: {error}"
    return result


def gpu_policy(frontend):
    return (tool(frontend, "lotus_status", {}).get("policy") or {}).get("gpu_memory")


def device_local(gpu_memory):
    """One session's device-local block bytes, high-water mark and failed allocations."""
    if not gpu_memory:
        return None
    heaps = [heap for heap in gpu_memory["heaps"] if heap["device_local"]]
    return dict(budget_source=gpu_memory["budget_source"],
                allocation_failures=gpu_memory["allocation_failures"],
                adapter_dedicated_usage_bytes=gpu_memory.get("adapter_dedicated_usage_bytes"),
                block_bytes=sum(heap["block_bytes"] for heap in heaps),
                block_bytes_high_water=sum(heap["block_bytes_high_water"] for heap in heaps))


def rows(frontend):
    return {row["id"]: row for row in tool(frontend, "lotus_status", {})["sessions"]}


def percentiles(values):
    if not values:
        return None
    ordered = sorted(values)
    rank = lambda q: ordered[min(len(ordered) - 1, max(0, round(q * len(ordered) + 0.5) - 1))]
    return dict(p50_ms=round(rank(0.5) * 1e3, 2), p95_ms=round(rank(0.95) * 1e3, 2),
                max_ms=round(ordered[-1] * 1e3, 2), count=len(ordered))


def frontend_for(kind, exes, config, root):
    env = dict(os.environ, LOTUS_DESKTOP_PREFERENCES=str(root / "desktop.json"))
    command = [str(exes["desktop"]), "--mcp", str(config)] if kind == "desktop" else [str(exes["headless"]), str(config)]
    return Frontend(command, env, root / "mcp.jsonl")


def trial(args, exes, kind, worker, roms, renderer, model, config_name, count, root):
    root.mkdir(parents=True, exist_ok=False)
    # The supervisor applies the profile with settings_set, which the mock does not answer.
    profile = {} if args.smoke else dict(frame_limit=args.frame_limit, graphics_api=RENDERERS[renderer],
                                         is_new_3ds=model == "new", init_clock=bench.INIT_CLOCK_FIXED)
    config = root / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(root / "storage"), "allow_control": True,
        "max_sessions": max(count + args.idle, 4), "assets": {name: str(path) for name, path in roms},
        "prefer_vulkan": renderer == "vulkan", "shader_seed": args.shader_cache == "seeded",
        "profile": profile,
        "policy": {"workload": args.workload, "focus_priority": args.focus_priority}}))
    frontend = frontend_for(kind, exes, config, root)
    processes = []
    try:
        if kind == "desktop":
            time.sleep(3)  # window creation; the MCP surface answers before it is shown
        if args.shader_cache == "seeded":
            primer = tool(frontend, "lotus_create_session", {"kind": "secondary", "asset": roms[0][0]})["session"]
            tool(frontend, "lotus_run_until", {"session": primer, "frames": 120})
            tool(frontend, "lotus_control", {"session": primer, "operation": "stop"})
        sessions = []
        for index in range(count + args.idle):
            asset = roms[index % len(roms)][0]
            created_at = time.perf_counter()
            _, reply, error = call(frontend, "lotus_create_session", {"kind": "secondary", "asset": asset})
            if error:
                return dict(status="create_failed", error=error, sessions_created=len(sessions),
                            gpu_memory_after=gpu_policy(frontend))
            session = reply["session"]
            sessions.append(dict(id=session, asset=asset, create_seconds=time.perf_counter() - created_at,
                                 idle=index >= count))
        if kind == "desktop" or args.focus_priority:
            tool(frontend, "lotus_focus_session", {"session": sessions[0]["id"]})
        for session in sessions:
            for operation, params in ([] if args.smoke else bench.CONFIGS[config_name]):
                _, reply, error = call(frontend, "lotus_debug", {"session": session["id"],
                                                                 "command": {"operation": operation, **params}})
                if error:
                    return dict(status="unsupported", error=f"{operation}: {error}")
                for key in ("interval_frames", "compression_level", "compression_queue"):
                    if key in params and reply.get(key) != params[key]:
                        return dict(status="unsupported", error=f"worker does not support rewind {key}")
        for session in sessions:
            if not session["idle"]:
                tool(frontend, "lotus_control", {"session": session["id"], "operation": "run"})
        status = rows(frontend)
        for session in sessions:
            telemetry = status[session["id"]]["telemetry"]
            session.update(startup_ms=telemetry["startup_ms"], shader_seed=telemetry["shader_seed"],
                           pid=telemetry["pid"])
            processes.append(Process(telemetry["pid"]))
        time.sleep(args.warmup_seconds)

        stalled = {sessions[0]["id"]} if args.stall else set()
        if args.stall:
            processes[0].suspend(True)
        ram_before = available_ram()
        gpu_before = gpu_policy(frontend)
        first = rows(frontend)
        usage_before = [p.usage() for p in processes]
        start = time.perf_counter()
        rounds, status_seconds = [], []
        commands = {s["id"]: [] for s in sessions}
        inputs = {s["id"]: [] for s in sessions}
        errors = {s["id"]: [] for s in sessions}
        while time.perf_counter() - start < args.seconds:
            round_start = time.perf_counter()
            if rounds:
                time.sleep(max(0.0, rounds[-1]["start"] + args.probe_interval - round_start))
                round_start = time.perf_counter()
            for index, session in enumerate(sessions):
                if session["id"] in stalled:
                    continue
                elapsed, reply, error = call(frontend, "lotus_registers", {"session": session["id"], "core": 0})
                (errors[session["id"]].append(error) if error else commands[session["id"]].append(elapsed))
                elapsed, _, error = call(frontend, "lotus_input", {
                    "session": session["id"], "owner": "live", "buttons": len(rounds) % 2, "x": 0, "y": 0,
                    "touch": False})
                (errors[session["id"]].append(error) if error else inputs[session["id"]].append(elapsed))
                if args.capture:
                    _, _, error = call(frontend, "lotus_capture", {"session": session["id"]})
                    if error:
                        errors[session["id"]].append(error)
            polled = time.perf_counter()
            status = rows(frontend)
            status_seconds.append(time.perf_counter() - polled)
            rounds.append(dict(start=round_start, t=round(polled - start, 3),
                               round_seconds=round(polled - round_start, 4),
                               frames={s["id"]: status[s["id"]]["telemetry"]["frames"] for s in sessions}))
        elapsed = time.perf_counter() - start
        last = rows(frontend)
        usage_after = [p.usage() for p in processes]
        # Checkpoint costs, pending compression and loss reasons per session (lotus#32).
        rewind = {}
        if not args.smoke and any(op == "rewind_configure" for op, _ in bench.CONFIGS[config_name]):
            for session in sessions:
                if session["idle"] or (args.stall and session is sessions[0]):
                    continue
                _, reply, error = call(frontend, "lotus_debug", {"session": session["id"],
                                                                 "command": {"operation": "rewind_status"}})
                if error:
                    errors[session["id"]].append(f"rewind_status: {error}")
                rewind[session["id"]] = reply
        ram_after = available_ram()
        gpu_after = gpu_policy(frontend)
        if args.stall:
            processes[0].suspend(False)

        results = []
        for session, before, after in zip(sessions, usage_before, usage_after):
            sid = session["id"]
            t0, t1 = first[sid]["telemetry"], last[sid]["telemetry"]
            supervision = {key: t1["supervision"][key] - t0["supervision"][key]
                           for key in ("polls", "busy_polls", "late_polls", "continuations", "status_failures")}
            supervision.update(lateness_max_ms=t1["supervision"]["lateness_max_ms"],
                               status_max_ms=t1["supervision"]["status_max_ms"])
            _, registers, _ = call(frontend, "lotus_registers", {"session": sid, "core": 0}) \
                if sid not in stalled else (0, None, None)
            results.append(dict(
                session, stalled=sid in stalled, fps=(t1["frames"] - t0["frames"]) / elapsed,
                state=t1["state"], failure=t1.get("failure"), renderer=t1["renderer"],
                renderer_fallback=t1["renderer_fallback"],
                gpu_memory=device_local(t1.get("gpu_memory")),
                command=percentiles(commands[sid]), input=percentiles(inputs[sid]),
                errors=errors[sid][:4], error_count=len(errors[sid]), supervision=supervision,
                usage=None if not (before and after) else dict(
                    cpu_seconds=after["cpu_seconds"] - before["cpu_seconds"],
                    io_read_bytes=after["io_read_bytes"] - before["io_read_bytes"],
                    io_write_bytes=after["io_write_bytes"] - before["io_write_bytes"],
                    working_set_bytes=after["working_set_bytes"], private_bytes=after["private_bytes"],
                    peak_working_set_bytes=after["peak_working_set_bytes"],
                    peak_private_bytes=after["peak_private_bytes"]),
                guest_ticks=(registers or {}).get("guest_ticks"), rewind=rewind.get(sid)))
        running = [r["fps"] for r in results if not (r["stalled"] or r["idle"])]
        closing = time.perf_counter()
        frontend.close()
        shutdown_seconds = time.perf_counter() - closing
        return dict(status="ok", seconds=elapsed, sessions=results, aggregate_fps=sum(running),
                    fps_skew=max(running) / min(running) if running and min(running) > 0 else None,
                    min_fps=min(running) if running else None, status_poll=percentiles(status_seconds),
                    host_ram_available_before=ram_before, host_ram_available_after=ram_after,
                    gpu_memory_before=gpu_before, gpu_memory_after=gpu_after,
                    gpu_allocation_failures=sum((r["gpu_memory"] or {}).get("allocation_failures", 0)
                                                for r in results),
                    shutdown_seconds=shutdown_seconds, probe_rounds=len(rounds),
                    probe_rounds_expected=int(elapsed / args.probe_interval) + 1, rounds=rounds)
    finally:
        for process in processes:
            try:
                process.suspend(False)
            except AssertionError:
                pass
            process.close()
        if frontend.process.poll() is None:
            frontend.close()


def reference(args, worker, rom, renderer, model, config_name, root):
    """tools/bench.py's single-worker trial: no supervisor, fixed frames, run_until steps.

    bench.py leaves `frame_limit` at the worker default (100 %); the reference applies this
    run's limit so its fps is comparable with the 1-session row."""
    frames = 300
    operations = [("settings_set", dict(name="frame_limit", value=args.frame_limit))] + bench.CONFIGS[config_name]
    result = bench.trial(worker, rom, root, renderer, model == "new", operations,
                         warmup=60, frames=frames, capture_every=0)
    return {key: result.get(key) for key in ("status", "fps", "host_seconds", "usage")}


def svg(points, title):
    """Aggregate fps against session count, one polyline per series; no plotting dependency."""
    width, height, pad = 640, 360, 48
    series = {}
    for label, n, fps in points:
        series.setdefault(label, []).append((n, fps))
    xs = sorted({n for _, n, _ in points}) or [1]
    top = max([fps for _, _, fps in points] + [1]) * 1.1
    x = lambda n: pad + (width - 2 * pad) * (xs.index(n) / max(1, len(xs) - 1))
    y = lambda fps: height - pad - (height - 2 * pad) * fps / top
    colors = ["#2563eb", "#dc2626", "#16a34a", "#9333ea", "#ea580c", "#0891b2"]
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" font-family="sans-serif" font-size="12">',
             '<rect width="100%" height="100%" fill="white"/>',
             f'<text x="{pad}" y="20">{title}</text>',
             f'<line x1="{pad}" y1="{height - pad}" x2="{width - pad}" y2="{height - pad}" stroke="black"/>',
             f'<line x1="{pad}" y1="{pad}" x2="{pad}" y2="{height - pad}" stroke="black"/>',
             f'<text x="{width / 2}" y="{height - 12}" text-anchor="middle">sessions</text>',
             f'<text x="12" y="{height / 2}" transform="rotate(-90 12 {height / 2})" text-anchor="middle">aggregate fps</text>']
    for n in xs:
        parts.append(f'<text x="{x(n)}" y="{height - pad + 16}" text-anchor="middle">{n}</text>')
    for tick in range(5):
        value = top * tick / 4
        parts.append(f'<text x="{pad - 6}" y="{y(value) + 4}" text-anchor="end">{value:.0f}</text>')
    for index, (label, values) in enumerate(sorted(series.items())):
        color = colors[index % len(colors)]
        values.sort()
        parts.append(f'<polyline fill="none" stroke="{color}" stroke-width="2" points="'
                     + " ".join(f"{x(n):.1f},{y(f):.1f}" for n, f in values) + '"/>')
        parts.append(f'<text x="{width - pad + 4}" y="{pad + 14 * index}" fill="{color}" font-size="10">{label}</text>')
    return "\n".join(parts + ["</svg>"])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--server", type=Path, required=True, help="lotus-mcp executable")
    parser.add_argument("--desktop", type=Path, help="lotus-desktop executable, for --frontend desktop")
    parser.add_argument("--worker", action="append", required=True, help="name=ctr-worker.exe")
    parser.add_argument("--rom", action="append", required=True, help="name=absolute ROM path")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workers", action="append", type=int, help="session counts (default 1, 2, 4)")
    parser.add_argument("--renderer", action="append", choices=list(RENDERERS))
    parser.add_argument("--model", action="append", choices=["old", "new"])
    parser.add_argument("--config", action="append", choices=list(bench.CONFIGS))
    parser.add_argument("--frontend", action="append", choices=["headless", "desktop"])
    parser.add_argument("--workload", choices=["batch", "interactive"], default="batch")
    parser.add_argument("--focus-priority", action="store_true")
    parser.add_argument("--idle", type=int, default=0, help="extra sessions kept loaded and paused")
    parser.add_argument("--stall", action="store_true")
    parser.add_argument("--capture", action="store_true")
    parser.add_argument("--shader-cache", choices=["cold", "seeded"], default="cold")
    parser.add_argument("--frame-limit", type=float, default=0, help="percent; 0 = unthrottled")
    parser.add_argument("--reference", action="store_true")
    parser.add_argument("--probe-interval", type=float, default=0.25, help="seconds between probe rounds")
    parser.add_argument("--warmup-seconds", type=float, default=5)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--require-idle", action="store_true")
    parser.add_argument("--smoke", action="store_true")
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("Windows only (process counters and suspension)")
    pairs = lambda values: [(name, Path(path).resolve()) for name, path in (v.split("=", 1) for v in values)]
    workers, roms = pairs(args.worker), pairs(args.rom)
    counts = args.workers or [1, 2, 4]
    # supervisor.rs accepts at most 16 sessions, including paused sessions.
    if args.idle < 0 or any(count < 1 or count + args.idle > 16 for count in counts):
        parser.error("each --workers count must be positive and total sessions including --idle must be at most 16")
    renderers = args.renderer or ["vulkan"]
    models = args.model or ["old"]
    configs = args.config or ["off"]
    frontends = args.frontend or ["headless"]
    if "desktop" in frontends and not args.desktop:
        raise SystemExit("--frontend desktop needs --desktop")
    if args.smoke:
        args.warmup_seconds, args.seconds = min(args.warmup_seconds, 1), min(args.seconds, 2)
    exes = {"headless": args.server.resolve(), "desktop": args.desktop.resolve() if args.desktop else None}
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    load_before = bench.system_load()
    if args.require_idle and load_before is not None and load_before > bench.BUSY:
        raise SystemExit(f"host CPU load {load_before:.0%} > {bench.BUSY:.0%}; refusing (--require-idle)")
    digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    report = dict(
        tool="tools/bench_parallel.py", started=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        host=dict(bench.host_metadata(), cpu_load_before=load_before),
        workers={name: dict(path=str(path), sha256=digest(path), build_manifest=bench.manifest(path))
                 for name, path in workers},
        frontends={kind: dict(path=str(path), sha256=digest(path)) for kind, path in exes.items() if path},
        roms={name: dict(sha256=digest(path)) for name, path in roms},
        fixed=dict(warmup_seconds=args.warmup_seconds, seconds=args.seconds, repeats=args.repeats,
                   frame_limit=args.frame_limit, shader_cache=args.shader_cache, stall=args.stall, idle=args.idle,
                   capture=args.capture, smoke=args.smoke, init_clock="FixedTime",
                   probe_interval=args.probe_interval, workload=args.workload,
                   focus_priority=args.focus_priority),
        build_differences=bench.build_differences({name: dict(build_manifest=bench.manifest(path))
                                                    for name, path in workers}),
        trials=[], references=[])
    for name, worker in workers:
        for kind in frontends:
            for renderer in renderers:
                for model in models:
                    for config_name in configs:
                        if args.reference and not args.smoke:
                            root = out / "reference" / name / renderer / model / config_name
                            entry = dict(worker=name, renderer=renderer, model=model, config=config_name,
                                         **reference(args, worker, roms[0][1], renderer, model, config_name, root))
                            report["references"].append(entry)
                            print("reference", json.dumps(entry), flush=True)
                        for count in counts:
                            for repeat in range(args.repeats):
                                root = out / name / kind / renderer / model / config_name / f"n{count}-{repeat}"
                                result = trial(args, exes, kind, worker, roms, renderer, model, config_name,
                                               count, root)
                                entry = dict(worker=name, frontend=kind, renderer=renderer, model=model,
                                             config=config_name, sessions_requested=count, repeat=repeat, **result)
                                report["trials"].append(entry)
                                brief = {k: entry.get(k) for k in ("worker", "frontend", "renderer", "model", "config",
                                                                   "sessions_requested", "status", "aggregate_fps",
                                                                   "min_fps", "fps_skew", "shutdown_seconds", "error")}
                                print(json.dumps(brief), flush=True)
    report["host"]["cpu_load_after"] = bench.system_load()
    loads = [v for v in (load_before, report["host"]["cpu_load_after"]) if v is not None]
    report["host_busy"] = any(v > bench.BUSY for v in loads)
    (out / "report.json").write_text(json.dumps(report, indent=2))
    write_summary(out, report, loads)
    if args.smoke:
        bad = [t for t in report["trials"] if t["status"] != "ok"
               or any(s["error_count"] or (s["fps"] <= 0 and not s["idle"]) for s in t["sessions"]
                      if not s["stalled"])]
        if bad:
            raise SystemExit(f"smoke failed: {json.dumps(bad)[:2000]}")


def write_summary(out, report, loads):
    host = report["host"]
    lines = [f"# Parallel sessions {report['started']}", ""]
    if report["host_busy"]:
        lines += [f"**Not comparable: host CPU load {loads} exceeded {bench.BUSY:.0%}.**", ""]
    lines += [f"Host: {host.get('cpu_name') or host['processor']}, {host.get('cpu_count')} logical CPUs, GPU "
              + (", ".join(f"{g.get('Name')} ({g.get('DriverVersion')})" for g in host.get("gpus") or []) or "unknown"),
              "Workers: " + ", ".join(f"{n} {w['sha256'][:16]}" for n, w in report["workers"].items()),
              f"Fixed: {json.dumps(report['fixed'])}", "",
              "Host throughput (fps = presented guest frames per host second). Guest ticks are in report.json, "
              "per session, and are not comparable across free-running sessions.", "",
              "| worker | frontend | renderer | model | config | N | aggregate fps | per-session min / median / max | "
              "skew | cmd p50 / p95 ms | input p95 ms | late polls | CPU s/s per worker | working set MiB | GPU high-water MiB / failed allocs | shutdown s |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    points = []
    for t in report["trials"]:
        head = f"| {t['worker']} | {t['frontend']} | {t['renderer']} | {t['model']} | {t['config']} | {t['sessions_requested']} |"
        if t["status"] != "ok":
            lines.append(head + f" {t['status']}: {t.get('error', '')} |" + " |" * 9)
            continue
        live = [s for s in t["sessions"] if not (s["stalled"] or s["idle"])]
        fps = [s["fps"] for s in live]
        median = lambda values: statistics.median(values) if values else float("nan")
        command = [s["command"] for s in live if s["command"]]
        inputs = [s["input"]["p95_ms"] for s in live if s["input"]]
        usage = [s["usage"] for s in live if s["usage"]]
        lines.append(head + f" {t['aggregate_fps']:.1f} | {min(fps):.1f} / {median(fps):.1f} / {max(fps):.1f} | "
                     f"{t['fps_skew'] or float('nan'):.2f} | "
                     f"{median([c['p50_ms'] for c in command]):.1f} / {max([c['p95_ms'] for c in command] or [float('nan')]):.1f} | "
                     f"{max(inputs or [float('nan')]):.1f} | {sum(s['supervision']['late_polls'] for s in live)} | "
                     f"{median([u['cpu_seconds'] / t['seconds'] for u in usage]):.2f} | "
                     f"{median([u['working_set_bytes'] / 2**20 for u in usage]):.0f} | "
                     f"{max([s['gpu_memory']['block_bytes_high_water'] / 2**20 for s in live if s.get('gpu_memory')] or [float('nan')]):.0f}"
                     f" / {t.get('gpu_allocation_failures', 0)} | {t['shutdown_seconds']:.2f} |")
        points.append((f"{t['worker']} {t['frontend']} {t['renderer']} {t['model']} {t['config']}",
                       t["sessions_requested"], t["aggregate_fps"]))
    if report["references"]:
        lines += ["", "tools/bench.py single-worker reference (no supervisor, run_until steps):", "",
                  "| worker | renderer | model | config | fps |", "| --- | --- | --- | --- | --- |"]
        for r in report["references"]:
            lines.append(f"| {r['worker']} | {r['renderer']} | {r['model']} | {r['config']} | "
                         f"{(r.get('fps') or float('nan')):.1f} |")
    idle = [(t, s) for t in report["trials"] if t["status"] == "ok" for s in t["sessions"] if s["idle"]]
    if idle:
        lines += ["", f"Paused sessions ({report['fixed']['idle']} per trial), not counted above:", "",
                  "| worker | renderer | config | N running | CPU s/s | cmd p50 / p95 ms | working set MiB |",
                  "| --- | --- | --- | --- | --- | --- | --- |"]
        for t, s in idle:
            usage, command = s["usage"] or {}, s["command"] or {}
            lines.append(f"| {t['worker']} | {t['renderer']} | {t['config']} | {t['sessions_requested']} | "
                         f"{usage.get('cpu_seconds', float('nan')) / t['seconds']:.3f} | "
                         f"{command.get('p50_ms', float('nan')):.1f} / {command.get('p95_ms', float('nan')):.1f} | "
                         f"{usage.get('working_set_bytes', float('nan')) / 2**20:.0f} |")
    if report["fixed"]["stall"]:
        lines += ["", "Session 0's worker was suspended for the window; its row is excluded from fps, skew and latency."]
    (out / "aggregate.svg").write_text(svg(points, "Aggregate fps by session count"))
    lines += ["", "![aggregate fps](aggregate.svg)"]
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    print((out / "summary.md").read_text())


if __name__ == "__main__":
    main()
