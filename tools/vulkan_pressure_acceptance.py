"""Verify Vulkan admission under an independent 2 GiB WDDM load on Windows.

Build tools/wddm_pressure.cpp and pass that executable as --allocator. Choose
--reserve-mib between the adapter's baseline headroom and the expected headroom
under the load. Four Vulkan sessions must keep advancing in both runs.
"""

import argparse
import json
from pathlib import Path
from queue import Empty, Queue
import subprocess
from threading import Thread
import time

from bench_parallel import call, frontend_for, gpu_policy, rows, tool


def wait_until(observe, seconds=15):
    deadline = time.monotonic() + seconds
    while (result := observe()) is None:
        if time.monotonic() >= deadline:
            raise AssertionError("condition timed out")
        time.sleep(0.25)
    return result


def run(server, worker, rom, allocator, root, explicit, reserve_mib):
    root.mkdir(parents=True)
    evidence = {}
    config = root / "host.json"
    profile = {"frame_limit": 100, "init_clock": 1}
    if explicit:
        profile["graphics_api"] = 2
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(root / "storage"), "allow_control": True,
        "max_sessions": 6, "assets": {"rom": str(rom)}, "prefer_vulkan": True, "profile": profile,
        "policy": {"workload": "batch", "gpu_reserve_mib": reserve_mib},
    }), encoding="utf-8")
    frontend = frontend_for("headless", {"headless": server}, config, root)
    allocation = None
    create = {"kind": "secondary", "asset": "rom"}

    def observe_renderers(sessions, renderer):
        current = rows(frontend)
        for session in sessions:
            telemetry = current[session]["telemetry"]
            assert not telemetry["failure"], telemetry["failure"]
            if telemetry["renderer"].lower() != renderer or telemetry["frames"] < 10:
                return None
        return current

    def play(session):
        tool(frontend, "lotus_control", {"session": session, "operation": "run"})

    try:
        sessions = [tool(frontend, "lotus_create_session", create)["session"] for _ in range(4)]
        for session in sessions:
            play(session)
        wait_until(lambda: observe_renderers(sessions, "vulkan"))
        evidence["before"] = gpu_policy(frontend)
        assert evidence["before"]["budget_source"] == "VK_EXT_memory_budget", evidence
        assert not evidence["before"]["pressure"], evidence
        ready = Queue()
        with (root / "allocator.log").open("w", encoding="utf-8") as stderr:
            allocation = subprocess.Popen([str(allocator), "2048"], stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, stderr=stderr, text=True)
            Thread(target=lambda: ready.put(allocation.stdout.readline()), daemon=True).start()
            try:
                evidence["external_load"] = json.loads(ready.get(timeout=15))
            except (Empty, ValueError) as error:
                raise AssertionError("external allocation helper did not report readiness") from error
            deadline = time.monotonic() + 15
            while True:
                evidence["under_load"] = gpu_policy(frontend)
                if evidence["under_load"]["pressure"] or time.monotonic() >= deadline:
                    break
                time.sleep(0.25)
            assert evidence["under_load"]["pressure"], evidence
            assert evidence["under_load"]["headroom_source"] == "WDDM dedicated usage", evidence
            start = rows(frontend)
            _, reply, error = call(frontend, "lotus_create_session", create)
            if explicit:
                assert error and "device-local headroom" in error, (reply, error)
                evidence["admission"] = error
            else:
                assert not error, error
                fallback = reply["session"]
                play(fallback)
                status = wait_until(lambda: observe_renderers([fallback], "software"))
                reason = status[fallback]["telemetry"]["renderer_fallback"]
                assert "device-local headroom" in reason, reason
                evidence["admission"] = reason
            evidence["survivors"] = []
            for session in sessions:
                tool(frontend, "lotus_input", {"session": session, "owner": "live", "buttons": 1,
                                              "x": 0, "y": 0, "touch": False})
                capture = frontend.call("tools/call", {"name": "lotus_capture",
                                                     "arguments": {"session": session}})
                assert not capture.get("isError") and len(capture["content"]) == 2, capture
                assert all(x["type"] == "resource_link" for x in capture["content"]), capture
            end = wait_until(lambda: observe_renderers(sessions, "vulkan"))
            for session in sessions:
                advance = end[session]["telemetry"]["frames"] - start[session]["telemetry"]["frames"]
                assert advance > 0, advance
                evidence["survivors"].append({"frames_advanced": advance, "screens": 2})
            allocation.stdin.close()
            assert allocation.wait(timeout=10) == 0
        evidence["recovered"] = wait_until(lambda: current if not (current := gpu_policy(frontend))["pressure"]
                                           else None)
        admitted = tool(frontend, "lotus_create_session", create)["session"]
        play(admitted)
        wait_until(lambda: observe_renderers([admitted], "vulkan"))
        evidence["vulkan_admitted_after_release"] = True
    finally:
        if allocation is not None and allocation.poll() is None:
            allocation.terminate()
            allocation.wait(timeout=10)
        frontend.close()
        (root / "report.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "worker", "rom", "allocator", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--reserve-mib", required=True, type=int)
    args = parser.parse_args()
    if args.reserve_mib <= 0:
        parser.error("--reserve-mib must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    for name, explicit in (("explicit", True), ("preferred", False)):
        run(args.server.resolve(), args.worker.resolve(), args.rom.resolve(), args.allocator.resolve(),
            args.output.resolve() / name, explicit, args.reserve_mib)
        print(f"{name} Vulkan admission under load passed", flush=True)
