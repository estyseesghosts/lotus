"""Headless vs visible desktop: same worker, ROM, renderer and supervisor (issues #6, #10). Windows.

Each trial starts a fresh frontend, `lotus-mcp` (headless) or `lotus-desktop --mcp` (window
shown, the session focused so it is live-previewed), with its own host config and storage
root. It creates one secondary session over MCP, plays it continuously, waits out a warm-up,
and then counts presented guest frames over a fixed host interval (`lotus_list_sessions`
`frames`). The frontend order alternates per repeat so slow drift on the host affects both.
The desktop gets an isolated preferences file (no Discord presence, default layout).
`--frame-limit` (default 0 = unthrottled, `core/perf_stats.cpp` DoFrameLimiting) goes into the
host profile; at 100 both frontends normally sit at the guest's 59.83 Hz and only a frontend
that cannot keep up shows a difference.

Reported per renderer and frontend: median and spread of guest fps, worker working set,
desktop/headless fps ratio, and fresh previews during the measurement.
Host metadata and CPU load come from tools/bench.py. A busy host (> 15 %) marks
the report as not comparable. `--require-idle` refuses to start under that load.

    python tools/bench_frontends.py --mcp target/release/lotus-mcp.exe \
        --desktop target/release/lotus-desktop.exe --worker <ctr-worker.exe> --rom <abs .3dsx> \
        --output runs/<fresh> [--renderer vulkan --renderer software] [--repeats 3] \
        [--warmup-seconds 5] [--seconds 20]
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import queue
import statistics
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench import BUSY, INIT_CLOCK_FIXED, PROFILE_KEYS, host_metadata, manifest, system_load  # noqa: E402
from desktop_shots import user32, window_of  # noqa: E402

WM_CLOSE = 0x0010


class Frontend:
    def __init__(self, command, env, transcript):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env)
        self.messages = queue.Queue()
        self.transcript = transcript.open("w", encoding="utf-8")
        threading.Thread(target=self._read, daemon=True).start()
        self.sequence = 0
        self.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                 "clientInfo": {"name": "lotus-bench-frontends", "version": "1"}})
        self.call("notifications/initialized", notify=True)

    def _read(self):
        for line in self.process.stdout:
            self.messages.put(json.loads(line))
        self.messages.put(None)

    def call(self, method, params=None, notify=False):
        message = {"jsonrpc": "2.0", "method": method}
        if not notify:
            self.sequence += 1
            message["id"] = self.sequence
        if params is not None:
            message["params"] = params
        self.transcript.write(json.dumps(message) + "\n")
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        if notify:
            return None
        while True:
            reply = self.messages.get(timeout=60)
            assert reply is not None, "frontend exited"
            if reply.get("id") == self.sequence:
                assert "error" not in reply, reply
                return reply["result"]

    def tool(self, name, arguments):
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), result
        return json.loads(result["content"][0]["text"])

    def session(self, session):
        return next(s for s in self.tool("lotus_list_sessions", {}) if s["id"] == session)

    def close(self, hwnd=None):
        try:
            if hwnd:  # the desktop keeps running when stdin closes; close its window like a user
                user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
            self.process.stdin.close()
            self.process.wait(timeout=20)
        except Exception:
            self.process.kill()
            self.process.wait()
        finally:
            self.transcript.close()


def trial(kind, exe, worker, rom, renderer, root, warmup, seconds, requested):
    root.mkdir(parents=True, exist_ok=False)
    config = root / "host.json"
    config.write_text(json.dumps({"worker": str(worker), "storage_root": str(root / "storage"),
                                  "allow_control": True, "assets": {"rom": str(rom)},
                                  "prefer_vulkan": renderer == "vulkan",
                                  "profile": requested}))
    env = dict(os.environ, LOTUS_DESKTOP_PREFERENCES=str(root / "desktop.json"))
    command = [str(exe), "--mcp", str(config)] if kind == "desktop" else [str(exe), str(config)]
    frontend = Frontend(command, env, root / "mcp.jsonl")
    hwnd = None
    try:
        if kind == "desktop":
            deadline = time.monotonic() + 30
            while (hwnd := window_of(frontend.process.pid)) is None:
                assert time.monotonic() < deadline, "no LOTUS window"
                time.sleep(0.2)
        session = frontend.tool("lotus_create_session", {"kind": "secondary", "asset": "rom"})["session"]
        frontend.tool("lotus_focus_session", {"session": session})
        settings = frontend.tool("lotus_settings", {"target": {"target": "session", "session": session}})["settings"]
        profile = {name: entry["value"] for name, entry in settings.items() if name in PROFILE_KEYS}
        mismatches = {name: {"requested": value, "effective": profile.get(name)}
                      for name, value in requested.items() if profile.get(name) != value}
        if mismatches:
            raise RuntimeError(f"{kind} settings differ from the requested profile: {mismatches}")
        frontend.tool("lotus_control", {"session": session, "operation": "run"})
        time.sleep(warmup)
        first = frontend.session(session)["telemetry"]
        start = time.perf_counter()
        time.sleep(seconds)
        last = frontend.session(session)["telemetry"]
        elapsed = time.perf_counter() - start
        for sample in (first, last):
            # A running session can be at its bounded frame target when sampled.
            assert sample["playing"] and not sample.get("failure"), sample
            assert sample["renderer"] == renderer, sample
        frames = last["frames"] - first["frames"]
        assert frames > 0, last
        preview = {name: last["preview"][name] - first["preview"][name]
                   for name in ("requests", "new_frames", "reused")}
        assert preview["requests"] == preview["new_frames"] + preview["reused"], preview
        visible = None
        if kind == "desktop":
            visible = bool(user32.IsWindowVisible(hwnd) and not user32.IsIconic(hwnd))
            assert visible and preview["new_frames"] > 0, (visible, preview)
            before = first["preview"]["worker"] or {}
            after = last["preview"]["worker"]
            if after is not None:
                for name in ("gpu_waits", "gpu_wait_ns", "readbacks", "skipped_busy", "copied_bytes"):
                    preview[name] = after[name] - before.get(name, 0)
        else:
            assert preview["requests"] == 0, preview
        result = dict(fps=frames / elapsed, mean_frame_ms=1000 * elapsed / frames,
                      frames=frames, seconds=elapsed, renderer=last["renderer"],
                      memory_bytes=last.get("memory_bytes"), backend_name=last.get("backend_name"),
                      window_visible=visible, preview=preview, effective_profile=profile)
        frontend.tool("lotus_control", {"session": session, "operation": "stop"})
        return result
    finally:
        frontend.close(hwnd)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--mcp", type=Path, required=True)
    parser.add_argument("--desktop", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--rom", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--renderer", action="append", choices=["vulkan", "software"])
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--warmup-seconds", type=float, default=5)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--require-idle", action="store_true")
    parser.add_argument("--require-nonblocking", action="store_true",
                        help="require zero Vulkan GPU waits during visible playback")
    parser.add_argument("--frame-limit", type=float, default=0, help="percent; 0 = unthrottled")
    parser.add_argument("--new-3ds", action="store_true")
    parser.add_argument("--deterministic-async", action="store_true")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    worker, rom = args.worker.resolve(), args.rom.resolve()
    exes = {"headless": args.mcp.resolve(), "desktop": args.desktop.resolve()}
    load_before = system_load()
    if args.require_idle and load_before is not None and load_before > BUSY:
        raise SystemExit(f"host CPU load {load_before:.0%} > {BUSY:.0%}; refusing (--require-idle)")
    report = dict(tool="tools/bench_frontends.py",
                  started=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  host=dict(host_metadata(), cpu_load_before=load_before),
                  worker=dict(path=str(worker), sha256=hashlib.sha256(worker.read_bytes()).hexdigest(),
                              build_manifest=manifest(worker)),
                  frontends={k: dict(path=str(v), sha256=hashlib.sha256(v.read_bytes()).hexdigest())
                             for k, v in exes.items()},
                  rom=dict(sha256=hashlib.sha256(rom.read_bytes()).hexdigest()),
                  fixed=dict(repeats=args.repeats, warmup_seconds=args.warmup_seconds, seconds=args.seconds,
                             frame_limit=args.frame_limit, init_clock=INIT_CLOCK_FIXED),
                  results={})
    for renderer in args.renderer or ["vulkan", "software"]:
        runs = {"headless": [], "desktop": []}
        requested = dict(graphics_api=2 if renderer == "vulkan" else 0, init_clock=INIT_CLOCK_FIXED,
                         frame_limit=args.frame_limit, is_new_3ds=args.new_3ds)
        if args.deterministic_async:
            requested["deterministic_async_operations"] = True
        for index in range(args.repeats):
            order = ["headless", "desktop"] if index % 2 == 0 else ["desktop", "headless"]
            for kind in order:
                result = trial(kind, exes[kind], worker, rom, renderer, out / renderer / kind / str(index),
                               args.warmup_seconds, args.seconds, requested)
                if args.require_nonblocking and kind == "desktop" and renderer == "vulkan":
                    assert result["preview"].get("gpu_waits") == 0, result
                runs[kind].append(result)
                print(renderer, kind, index, json.dumps(result), flush=True)
        entry = {}
        for kind, results in runs.items():
            fps = [r["fps"] for r in results]
            preview_fps = [r["preview"]["new_frames"] / r["seconds"] for r in results]
            waits = [r["preview"]["gpu_waits"] for r in results if "gpu_waits" in r["preview"]]
            wait_ns = [r["preview"]["gpu_wait_ns"] for r in results if "gpu_wait_ns" in r["preview"]]
            entry[kind] = dict(median_fps=statistics.median(fps), min_fps=min(fps), max_fps=max(fps),
                               median_mean_frame_ms=statistics.median(r["mean_frame_ms"] for r in results),
                               median_preview_fps=statistics.median(preview_fps),
                               median_memory_bytes=statistics.median(r["memory_bytes"] or 0 for r in results),
                               median_preview_gpu_waits=statistics.median(waits) if waits else None,
                               median_preview_gpu_wait_ms=statistics.median(wait_ns) / 1e6 if wait_ns else None,
                               trials=results)
        profiles = [r["effective_profile"] for trials in runs.values() for r in trials]
        entry["profiles_match"] = all(profile == profiles[0] for profile in profiles)
        entry["desktop_vs_headless"] = (round(entry["desktop"]["median_fps"] / entry["headless"]["median_fps"], 3)
                                        if entry["profiles_match"] else None)
        report["results"][renderer] = entry
    report["host"]["cpu_load_after"] = system_load()
    loads = [v for v in (load_before, report["host"]["cpu_load_after"]) if v is not None]
    report["host_busy"] = any(v > BUSY for v in loads)
    (out / "report.json").write_text(json.dumps(report, indent=2))
    lines = [f"# Frontends {report['started']}", ""]
    if report["host_busy"]:
        lines += [f"**Not comparable: host CPU load {loads} exceeded {BUSY:.0%}.**", ""]
    host = report["host"]
    lines += [f"Host: {host.get('cpu_name') or host['processor']}, GPU "
              + (", ".join(f"{g.get('Name')} ({g.get('DriverVersion')})" for g in host.get("gpus") or []) or "unknown"),
              f"Worker sha256 {report['worker']['sha256'][:16]}, ROM {report['rom']['sha256'][:16]}, "
              f"{args.repeats} x {args.seconds:g} s after {args.warmup_seconds:g} s warm-up, "
              f"frame limit {args.frame_limit:g} % (0 = unthrottled)", "",
              "| renderer | frontend | median guest fps | spread | fresh previews/s | preview GPU waits (ms) | worker MiB | desktop / headless |",
              "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for renderer, entry in report["results"].items():
        for kind in ("headless", "desktop"):
            e = entry[kind]
            wait_text = (f"{e['median_preview_gpu_waits']:g} ({e['median_preview_gpu_wait_ms']:.3f})"
                         if e["median_preview_gpu_waits"] is not None else "—")
            lines.append(f"| {renderer} | {kind} | {e['median_fps']:.1f} | {e['min_fps']:.1f}-{e['max_fps']:.1f} | "
                         f"{e['median_preview_fps']:.1f} | {wait_text} | {e['median_memory_bytes'] / 2**20:.0f} | "
                         f"{(entry['desktop_vs_headless'] or 'settings differ') if kind == 'desktop' else ''} |")
    (out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
