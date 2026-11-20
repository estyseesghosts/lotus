"""Verify native rewind in three surviving sessions after a fourth worker dies (#32)."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

from bench import INIT_CLOCK_FIXED
from bench_parallel import frontend_for, rows, tool


def wait_until(observe):
    deadline = time.monotonic() + 30
    while not (result := observe()):
        assert time.monotonic() < deadline, "timed out waiting for worker progress"
        time.sleep(0.1)
    return result


def run(server, worker, rom, root):
    root.mkdir(parents=True, exist_ok=False)
    config = root / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(root / "storage"),
        "allow_control": True, "assets": {"rom": str(rom)}, "max_sessions": 4,
        "prefer_vulkan": True, "policy": {"workload": "batch"},
        "profile": {"graphics_api": 2, "init_clock": INIT_CLOCK_FIXED, "frame_limit": 100},
    }))
    frontend = frontend_for("headless", {"headless": server}, config, root)
    try:
        def rewind(session, operation="rewind_status", **params):
            return tool(frontend, "lotus_debug", {
                "session": session, "command": {"operation": operation, **params},
            })

        sessions = [
            tool(frontend, "lotus_create_session", {"kind": "secondary", "asset": "rom"})["session"]
            for _ in range(4)
        ]
        for session in sessions:
            status = rewind(session, "rewind_configure", enabled=True, capacity=32, budget_mib=64,
                            interval_frames=5, compression_queue=1)
            assert status["compression_queue"] == 1, status
            tool(frontend, "lotus_control", {"session": session, "operation": "run"})

        baseline = {}

        def all_recording():
            for session, row in rows(frontend).items():
                telemetry = row["telemetry"]
                assert not telemetry.get("failure"), row
                if not telemetry["frames"]:
                    continue
                assert telemetry["playing"] and telemetry["renderer"] == "vulkan", row
                history = rewind(session)
                if history["checkpoints"] >= 3:
                    baseline[session] = {"frames": telemetry["frames"], "history": history}
            return len(baseline) == len(sessions)

        wait_until(all_recording)
        victim, *survivors = sessions
        victim_pid = rows(frontend)[victim]["telemetry"]["pid"]
        subprocess.run(["taskkill", "/F", "/PID", str(victim_pid)], check=True, timeout=10)
        failure = wait_until(lambda: rows(frontend)[victim]["telemetry"].get("failure"))
        assert failure["cause"] == "abort" and failure["exit_code"], failure
        observed_failure = rows(frontend)
        at_failure = {
            session: {"frames": observed_failure[session]["telemetry"]["frames"], "history": rewind(session)}
            for session in survivors
        }
        after = {}

        def isolated_failure():
            status = rows(frontend)
            for session in survivors:
                telemetry = status[session]["telemetry"]
                assert telemetry["playing"] and not telemetry.get("failure"), status[session]
                history = rewind(session)
                losses = history["lost"]
                assert losses["budget"] == losses["async_operations"] == losses["oversize"] == 0, history
                assert history["checkpoint_compress_failures"] == 0, history
                assert history["compression_pending_peak_bytes"] > 0, history
                before = at_failure[session]
                if (telemetry["frames"] >= before["frames"] + 10
                        and history["checkpoints"] >= before["history"]["checkpoints"] + 2):
                    after[session] = {"frames": telemetry["frames"], "history": history}
            return len(after) == len(survivors)

        wait_until(isolated_failure)
        for session in survivors:
            tool(frontend, "lotus_registers", {"session": session, "core": 0})
            capture = frontend.call("tools/call", {
                "name": "lotus_capture", "arguments": {"session": session},
            })
            assert not capture.get("isError") and len(capture["content"]) == 2, capture
            assert all(item["type"] == "resource_link" for item in capture["content"]), capture
        for session in survivors:
            tool(frontend, "lotus_control", {"session": session, "operation": "stop"})
        return {"failed_session": victim, "failure": failure, "before": baseline,
                "at_failure": at_failure, "after": after}
    finally:
        frontend.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "worker", "rom", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args()
    root = args.output.resolve()
    report = run(args.server.resolve(), args.worker.resolve(), args.rom.resolve(), root)
    report["sha256"] = {name: hashlib.sha256(path.read_bytes()).hexdigest()
                        for name, path in vars(args).items() if name != "output"}
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print("One worker failed and all three siblings kept recording rewind and returned both screens.")


if __name__ == "__main__":
    main()
