"""Measure Windows worker GPU telemetry RPC latency at a range of session counts."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
from statistics import mean, median
from threading import Barrier
from time import perf_counter_ns

from native_debug_probe import Peer


def latency(samples):
    ordered = sorted(samples)

    def percentile(p):
        return ordered[(len(ordered) * p + 99) // 100 - 1] / 1000

    return {"avg_us": mean(ordered) / 1000, "p95_us": percentile(95),
            "p99_us": percentile(99), "max_us": percentile(100)}


def trial(worker, guest, root, count, renderer, polls):
    start = Barrier(count)
    peers = []
    try:
        for index in range(count):
            peers.append(Peer(worker, root / f"worker-{index}"))

        def measure(peer):
            hello = peer.call("hello")
            peer.debug("settings_set", name="graphics_api", value=2 if renderer == "vulkan" else 0)
            peer.debug("settings_set", name="frame_limit", value=0)
            peer.call("load", {"path": str(guest)})
            start.wait(timeout=45)
            initial = peer.call("status")
            assert (initial["gpu_memory"] is not None) == (renderer == "vulkan")

            timings = {}
            for phase in ("paused", "running"):
                start.wait(timeout=45)
                if phase == "running":
                    peer.call("run", {"frames": 600})
                elapsed = []
                running = 0
                for _ in range(polls):
                    begin = perf_counter_ns()
                    latest = peer.call("status")
                    elapsed.append(perf_counter_ns() - begin)
                    running += peer.state == "running"
                timings[phase] = {"latency": latency(elapsed), "rpc_ns": elapsed,
                                  "running_replies": running,
                                  "frames": latest["frames"], "gpu_memory": latest["gpu_memory"]}
            peer.call("pause")
            start.wait(timeout=45)
            begin = perf_counter_ns()
            done = peer.call("run_until", {"frames": 60})
            duration = perf_counter_ns() - begin
            assert done["frames"] >= latest["frames"] + 60
            return {"source": hello["source"], "backend": hello["backend_name"], "polls": timings,
                    "run_until_wall_ns": duration}

        with ThreadPoolExecutor(max_workers=count) as pool:
            results = list(pool.map(measure, peers))
        pooled = {
            phase: latency([ns for result in results for ns in result["polls"][phase]["rpc_ns"]])
            for phase in ("paused", "running")
        }
        return {"renderer": renderer, "workers": count, "sessions": results, "pooled_latency": pooled,
                "median_run_until_wall_ns": median(result["run_until_wall_ns"] for result in results)}
    finally:
        for peer in peers:
            peer.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("guest", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--workers", type=int, action="append", choices=(1, 2, 4, 8))
    parser.add_argument("--renderer", choices=("software", "vulkan"), action="append")
    parser.add_argument("--polls", type=int, default=160)
    args = parser.parse_args()
    if not 1 <= args.polls <= 10000:
        parser.error("--polls must be between 1 and 10000")
    args.output.mkdir(parents=True)
    runs = []
    for renderer in args.renderer or ("software", "vulkan"):
        for count in args.workers or (1, 2, 4, 8):
            runs.append(trial(args.worker.resolve(), args.guest.resolve(), args.output.resolve() /
                              f"{renderer}-{count}", count, renderer, args.polls))
            (args.output / "report.json").write_text(json.dumps(runs, indent=2) + "\n", encoding="utf-8")
            print(f"{renderer} {count}: {runs[-1]['pooled_latency']}", flush=True)


if __name__ == "__main__":
    main()
