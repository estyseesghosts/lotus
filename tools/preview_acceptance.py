"""Shared-memory live preview acceptance (issue #6), software and offscreen Vulkan. Windows.

Per renderer, against one worker:
- paused: `preview` pixels equal an explicit `capture` byte for byte;
- running: each preview names the frame it shows, never older than the previous preview's,
  always of the current epoch. Vulkan never waits for the GPU while running (lotus#25): the
  worker's `gpu_waits` counter does not move during 120 back-to-back previews, which also
  exercise backpressure (`reused` answers repeat the previous slot, `skipped_busy` counts
  requests that found every staging slot in flight). Reported, not asserted: lag in frames;
  under Vulkan it is no longer bounded by one request, since a busy GPU leaves the older
  frame on screen;
- after `reset` (new epoch) no preview carries the old epoch, and a paused preview again
  equals capture;
- 120 previews while running leave every `frames/` file untouched, and the worker's write
  counter (GetProcessIoCounters, which also counts the reply pipe) grows by under 4 KiB per
  preview, against 691,200 bytes of files per explicit capture.

With `--require-nonblocking`, a preview at a bounded run stop must also leave the
GPU-wait count unchanged. The following exact preview must still match capture.

    python tools/preview_acceptance.py <ctr-worker.exe> <rom.3dsx> <fresh output dir>
"""
import argparse
import json
import mmap
from pathlib import Path

from bench import process_usage
from native_debug_probe import Peer

RENDERERS = {"software": 0, "vulkan": 2}


class Ring:
    def __init__(self):
        self.name = None
        self.view = None

    def read(self, reply):
        if reply["name"] != self.name:
            self.view = mmap.mmap(-1, reply["bytes"], tagname=reply["name"], access=mmap.ACCESS_READ)
            self.name = reply["name"]
        screens = {}
        for lease in reply["frames"]:
            at = lease["offset"]
            header = self.view[at:at + 32]
            assert header[:4] == b"LTRF", header
            assert int.from_bytes(header[8:16], "little") == lease["epoch"]
            assert int.from_bytes(header[16:24], "little") == lease["sequence"]
            screens[lease["screen"]] = self.view[at + 32:at + 32 + lease["bytes"]]
        return screens


def captured(peer, root):
    return {lease["screen"]: (root / lease["path"]).read_bytes()[32:] for lease in peer.call("capture")["frames"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--require-nonblocking", action="store_true",
                        help="also verify preview can avoid a GPU wait at a bounded run stop")
    args = parser.parse_args()
    report = {}
    for renderer, api in RENDERERS.items():
        root = args.output.resolve() / renderer
        peer = Peer(args.worker.resolve(), root)
        ring = Ring()
        try:
            hello = peer.call("hello")
            assert hello.get("preview") == "shared_memory_v1", hello
            peer.debug("settings_set", name="graphics_api", value=api)
            peer.call("load", {"path": str(args.rom.resolve())})
            for _ in range(3):
                peer.call("run_until", {"frames": 10})
            # Paused: exact.
            for _ in range(2):
                reply = peer.call("preview")
                assert reply["frame"] == reply["current_frame"], reply
                assert ring.read(reply) == captured(peer, root), "paused preview differs from capture"
            bounded_stop = None
            if args.require_nonblocking:
                before_stop = reply
                peer.call("run_until", {"frames": 10})
                assert peer.state == "paused", peer.state
                preview = peer.call("preview", {"nonblocking": True})
                assert preview["current_frame"] > before_stop["current_frame"]
                waits = preview["stats"]["gpu_waits"] - before_stop["stats"]["gpu_waits"]
                assert waits == 0, (before_stop["stats"], preview["stats"])
                assert all(lease["epoch"] == peer.epoch for lease in preview["frames"])
                assert ring.read(preview).keys() == {"top", "bottom"}, preview
                bounded_stop = dict(frame=preview["frame"], current_frame=preview["current_frame"],
                                    gpu_waits=waits)
                exact = peer.call("preview")
                assert exact["frame"] == exact["current_frame"], exact
                assert ring.read(exact) == captured(peer, root), (
                    "exact preview after nonblocking request differs")
            # Running: the current frame or the previous request's, labelled.
            peer.call("run", {"frames": 600})
            lags, last = [], None
            files = {path: path.stat().st_mtime_ns for path in (root / "frames").glob("*.bgra")}
            before = process_usage(peer.process)
            preview_params = {"nonblocking": True} if args.require_nonblocking else None
            stats_before = peer.call("preview", preview_params).get("stats")
            fresh = 0
            for _ in range(120):
                reply = peer.call("preview", preview_params)
                ring.read(reply)
                lag = reply["current_frame"] - reply["frame"]
                assert lag >= 0 and (last is None or reply["frame"] >= last), reply
                assert all(lease["epoch"] == reply["current_epoch"] for lease in reply["frames"]), reply
                last = reply["frame"]
                lags.append(lag)
                fresh += reply.get("new_frame", True)
            stats = reply.get("stats")
            if stats_before and stats:
                assert stats["gpu_waits"] == stats_before["gpu_waits"], (stats_before, stats)
            after = process_usage(peer.process)
            written = after["io_write_bytes"] - before["io_write_bytes"]
            assert written < 4096 * len(lags), f"{written} bytes written during previews"
            assert files and all(path.stat().st_mtime_ns == mtime for path, mtime in files.items()),                 "a preview rewrote a capture file"
            peer.call("pause")
            # New epoch: never the old one, and exact again once paused.
            old_epoch = peer.epoch
            peer.call("reset")
            assert peer.epoch != old_epoch
            for _ in range(2):
                peer.call("run_until", {"frames": 10})
            for _ in range(2):
                reply = peer.call("preview")
                assert all(lease["epoch"] == peer.epoch for lease in reply["frames"]), reply
                assert reply["frame"] == reply["current_frame"], reply
                assert ring.read(reply) == captured(peer, root), "preview after reset differs from capture"
        finally:
            ring.view and ring.view.close()
            peer.close()
        report[renderer] = dict(previews=len(lags), max_lag_frames=max(lags),
                                bounded_stop=bounded_stop,
                                backend=hello.get("backend_name"), source=hello.get("source"),
                                lagged_previews=sum(1 for lag in lags if lag), io_write_bytes=written,
                                new_frames=fresh, worker_stats=stats,
                                running_stats_delta=None if not (stats_before and stats) else
                                {k: stats[k] - stats_before[k] for k in stats})
        print(renderer, json.dumps(report[renderer]), flush=True)
    (args.output.resolve() / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
