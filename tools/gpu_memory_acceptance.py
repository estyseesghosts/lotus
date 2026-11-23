"""Measure Windows worker GPU telemetry RPC latency at a range of session counts."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
from statistics import mean, median
from threading import Barrier
from time import monotonic, perf_counter_ns, sleep

from native_debug_probe import Peer


def latency(samples):
    ordered = sorted(samples)

    def percentile(p):
        return ordered[(len(ordered) * p + 99) // 100 - 1] / 1000

    return {"avg_us": mean(ordered) / 1000, "p95_us": percentile(95),
            "p99_us": percentile(99), "max_us": percentile(100)}


def trial(worker, guest, root, count, renderer, polls, mode):
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
                snapshots = []
                cache_reads = []
                sampled = {}
                running = 0
                for _ in range(polls):
                    begin = perf_counter_ns()
                    latest = peer.call("status")
                    elapsed.append(perf_counter_ns() - begin)
                    running += peer.state == "running"
                    gpu = latest["gpu_memory"]
                    if gpu is not None and mode != "legacy":
                        assert gpu["sample_age_ms"] < gpu["sample_ttl_ms"], gpu
                        assert gpu["sample_state"] == "fresh", gpu
                        snapshots.append(gpu["snapshot_ns"])
                        cache_reads.append(gpu["timings_ns"]["cache_read"])
                        sampled[gpu["sample_sequence"]] = {
                            key: value for key, value in gpu["timings_ns"].items()
                            if key != "cache_read"
                        }
                timings[phase] = {"latency": latency(elapsed), "rpc_ns": elapsed,
                                  "running_replies": running,
                                  "frames": latest["frames"], "gpu_memory": latest["gpu_memory"]}
                if snapshots:
                    if mode == "sync":
                        assert len(sampled) == polls, sampled
                    else:
                        # Two samples can fall into a short test window at a cadence boundary.
                        assert len(sampled) <= 2 + sum(elapsed) // 500_000_000, sampled
                    timings[phase].update(snapshot_ns=snapshots, cache_read_ns=cache_reads,
                                          samples=sampled)
            peer.call("pause")
            start.wait(timeout=45)
            begin = perf_counter_ns()
            done = peer.call("run_until", {"frames": 60})
            duration = perf_counter_ns() - begin
            assert done["frames"] >= latest["frames"] + 60
            begin = perf_counter_ns()
            peer.call("stop")
            stop_rpc_ns = perf_counter_ns() - begin
            assert peer.process.wait(timeout=5) == 0
            return {"source": hello["source"], "backend": hello["backend_name"], "polls": timings,
                    "run_until_wall_ns": duration, "stop_rpc_ns": stop_rpc_ns,
                    "audio_hash_fnv1a64": done["audio_hash_fnv1a64"]}

        with ThreadPoolExecutor(max_workers=count) as pool:
            results = list(pool.map(measure, peers))
        pooled = {
            phase: latency([ns for result in results for ns in result["polls"][phase]["rpc_ns"]])
            for phase in ("paused", "running")
        }
        return {"renderer": renderer, "workers": count, "mode": mode,
                "sessions": results, "pooled_latency": pooled,
                "median_run_until_wall_ns": median(result["run_until_wall_ns"] for result in results)}
    finally:
        for peer in peers:
            peer.close()


def lifecycle(worker, guest, root, new_model, fault_cause):
    peers = []
    try:
        for name in ("fault", "sibling"):
            peer = Peer(worker, root / name)
            peers.append(peer)
            hello = peer.call("hello")
            assert "gpu_memory_refresh" in hello["features"]
            assert hello["gpu_memory"] is None
            peer.debug("settings_set", name="graphics_api", value=2)
            peer.debug("settings_set", name="is_new_3ds", value=new_model)
            peer.debug("settings_set", name="frame_limit", value=0)
        fault, sibling = peers
        load = {"path": str(guest)}
        initial = sibling.call("load", load)["gpu_memory"]
        assert initial["sample_state"] == "fresh" and initial["sample_sequence"] > 0
        deadline = monotonic() + 2
        latest = initial
        while latest["sample_sequence"] == initial["sample_sequence"] and monotonic() < deadline:
            sleep(0.05)
            latest = sibling.call("status")["gpu_memory"]
        assert latest["sample_sequence"] > initial["sample_sequence"], latest
        assert latest["sample_state"] == "fresh", latest
        explicit = sibling.call("gpu_memory_refresh")["gpu_memory"]
        assert explicit["sample_sequence"] > latest["sample_sequence"], explicit
        assert explicit["sample_state"] == "fresh" and explicit["sample_age_ms"] < 500, explicit
        old_epoch = sibling.epoch
        reset = sibling.call("reset")["gpu_memory"]
        assert sibling.epoch > old_epoch and reset["sample_sequence"] > explicit["sample_sequence"]
        assert reset["sample_state"] == "fresh" and reset["sample_age_ms"] < 500, reset

        failure_sample = fault.call("load", load)["gpu_memory"]
        (fault.root / "vk-fail-alloc").write_text("1", encoding="ascii")
        try:
            fault.call("run_until", {"frames": 60})
        except EOFError:
            assert fault.process.wait(timeout=5) == 4
        else:
            raise AssertionError("the fixture did not trigger the injected draw allocation failure")
        prefix = "worker_gpu_error: "
        log = (fault.root / "probe.stderr.log").read_text(encoding="utf-8", errors="replace")
        report = json.loads(next(line[len(prefix):] for line in log.splitlines() if line.startswith(prefix)))
        assert report["cause"] == fault_cause, report
        assert report["vk_result_name"] == {
            "gpu_out_of_memory": "VK_ERROR_OUT_OF_DEVICE_MEMORY",
            "gpu_device_lost": "VK_ERROR_DEVICE_LOST",
        }[fault_cause]
        cached = report["gpu_memory"]
        assert cached["allocation_failures"] == int(fault_cause == "gpu_out_of_memory"), cached
        assert cached["sample_state"] in ("stale", "unknown"), cached
        if cached["sample_state"] == "stale":
            assert cached["sample_sequence"] >= failure_sample["sample_sequence"], cached
        done = sibling.call("run_until", {"frames": 60})
        assert done["frames"] == 60 and done["gpu_memory"]["sample_state"] == "fresh"
        assert done["gpu_memory"]["allocation_failures"] == 0
        sibling.call("stop")
        assert sibling.process.wait(timeout=5) == 0
        return {"source": hello["source"], "new_3ds": new_model, "initial": initial,
                "periodic": latest, "explicit": explicit, "reset": reset,
                "fault": report, "sibling_audio_hash_fnv1a64": done["audio_hash_fnv1a64"]}
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
    parser.add_argument("--mode", choices=("legacy", "sync", "cache"), default="legacy")
    parser.add_argument("--lifecycle", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.polls <= 10000:
        parser.error("--polls must be between 1 and 10000")
    os.environ["CTR_WORKER_GPU_MEMORY_PROFILE"] = str(int(args.mode != "legacy"))
    os.environ["CTR_WORKER_GPU_MEMORY_SYNC"] = str(int(args.mode == "sync"))
    args.output.mkdir(parents=True)
    runs = []
    if args.lifecycle:
        if args.mode != "cache":
            parser.error("--lifecycle requires --mode cache")
        for injection, cause in (("file", "gpu_out_of_memory"), ("device_lost_file", "gpu_device_lost")):
            os.environ["CTR_WORKER_VK_FAIL_ALLOC"] = injection
            for new_model in (False, True):
                root = args.output.resolve() / cause / ("new" if new_model else "old")
                runs.append(lifecycle(args.worker.resolve(), args.guest.resolve(),
                                      root, new_model, cause))
        (args.output / "report.json").write_text(json.dumps(runs, indent=2) + "\n", encoding="utf-8")
        return
    for renderer in args.renderer or ("software", "vulkan"):
        for count in args.workers or (1, 2, 4, 8):
            runs.append(trial(args.worker.resolve(), args.guest.resolve(), args.output.resolve() /
                              f"{renderer}-{count}", count, renderer, args.polls, args.mode))
            (args.output / "report.json").write_text(json.dumps(runs, indent=2) + "\n", encoding="utf-8")
            print(f"{renderer} {count}: {runs[-1]['pooled_latency']}", flush=True)


if __name__ == "__main__":
    main()
