"""T45: four offscreen Vulkan workers in parallel; bad-device and no-device paths.

Parallel: four isolated workers boot the same homebrew ROM on Vulkan, run 60
frames concurrently and capture both screens. Structural checks only (frame
sizes, non-blank content); hash equality is recorded, not required.
Failure paths: physical_device=99 and a loader with no ICD must yield a typed
error reply or a clean worker exit, never a crash or hang, and leave a
software session usable afterwards.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import threading
import time
from native_debug_probe import Peer


def boot(worker, rom, root, settings, frames=60):
    peer = Peer(worker, root)
    try:
        peer.call("hello")
        for name, value in settings.items():
            peer.debug("settings_set", name=name, value=value)
        peer.call("load", {"path": str(rom)})
        peer.call("run_until", {"frames": frames})
        capture = peer.call("capture")
        screens = {}
        for lease in capture["frames"]:
            data = (root / lease["path"]).read_bytes()[32:]
            screens[lease["screen"]] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                                            nonblank=len(set(data[::97])) > 1)
        return dict(screens=screens)
    finally:
        peer.close()


def failure(worker, rom, root, settings, env=None):
    """Returns the reply status/reason of a load expected to fail and the exit code."""
    saved = dict(os.environ)
    os.environ.update(env or {})
    try:
        peer = Peer(worker, root)
    finally:
        os.environ.clear()
        os.environ.update(saved)
    start = time.monotonic()
    outcome = {}
    try:
        peer.call("hello")
        for name, value in settings.items():
            peer.debug("settings_set", name=name, value=value)
        try:
            data = peer.call("load", {"path": str(rom)}, expected=None)
            outcome = dict(status=peer.status, reply=data)
            if peer.status != "ok":  # the worker must still answer after the failed load
                outcome["status_after"] = peer.call("status", expected=None) and peer.status
        except (EOFError, OSError) as error:
            outcome = dict(status="worker exited", reply=repr(error))
    finally:
        try:
            peer.close()
        except Exception as error:  # report, do not mask the exit code check
            outcome["close_error"] = repr(error)
    outcome["exit_code"] = peer.process.returncode
    outcome["seconds"] = round(time.monotonic() - start, 2)
    stderr = (root / "probe.stderr.log").read_text(errors="replace")
    outcome["stderr_tail"] = stderr[-300:]
    return outcome


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    worker, rom, out = args.worker.resolve(), args.rom.resolve(), args.output.resolve()
    report = {}
    results, errors = [None] * 4, []

    def run(index):
        try:
            results[index] = boot(worker, rom, out / f"parallel-{index}",
                                  {"graphics_api": 2, "is_new_3ds": bool(index % 2)})
        except Exception as error:
            errors.append(repr(error))

    start = time.monotonic()
    threads = [threading.Thread(target=run, args=(i,)) for i in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(300)
    report["parallel"] = dict(seconds=round(time.monotonic() - start, 1), errors=errors, results=results)
    assert not errors and all(results), report["parallel"]
    for result in results:
        assert set(result["screens"]) == {"top", "bottom"} or len(result["screens"]) == 2, result
        assert all(s["bytes"] > 0 for s in result["screens"].values()), result
        assert any(s["nonblank"] for s in result["screens"].values()), result
    by_model = {}
    for index, result in enumerate(results):
        by_model.setdefault(index % 2, set()).add(json.dumps({k: v["sha256"] for k, v in result["screens"].items()}))
    report["parallel_hashes_equal_per_model"] = {("new" if k else "old"): len(v) == 1 for k, v in by_model.items()}
    print(json.dumps({"parallel": report["parallel"]["seconds"], "equal": report["parallel_hashes_equal_per_model"]}), flush=True)

    report["bad_device"] = failure(worker, rom, out / "bad-device", {"graphics_api": 2, "physical_device": 99})
    empty = out / "no-icd"
    empty.mkdir(parents=True, exist_ok=True)
    (empty / "none.json").write_text('{"file_format_version":"1.0.0","ICD":{"library_path":"lotus-missing-icd.dll","api_version":"1.3.0"}}')
    report["no_device"] = failure(worker, rom, out / "no-device", {"graphics_api": 2},
                                  env={"VK_DRIVER_FILES": str(empty / "none.json"),
                                       "VK_ICD_FILENAMES": str(empty / "none.json")})
    for name in ("bad_device", "no_device"):
        outcome = report[name]
        print(json.dumps({name: {k: outcome[k] for k in ("status", "exit_code", "seconds")},
                          "reply": str(outcome["reply"])[:200]}), flush=True)
        assert outcome["status"] != "ok", outcome
        assert outcome["exit_code"] in (0, 1), outcome  # not a crash code
        assert outcome["seconds"] < 30, outcome
    report["software_after"] = boot(worker, rom, out / "software-after", {"graphics_api": 0}, frames=10)
    (out / "acceptance.json").write_text(json.dumps(report, indent=2))
