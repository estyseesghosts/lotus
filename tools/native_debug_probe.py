"""Bounded wire-level acceptance probe; private roots, no uploaded user assets."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import threading
import time
import uuid


class Peer:
    def __init__(self, worker, root):
        root.mkdir(parents=True, exist_ok=False)
        self.root = root
        self.stderr = (root / "probe.stderr.log").open("wb")
        self.process = subprocess.Popen([str(worker)], env={**os.environ, "LOTUS_STORAGE_ROOT": str(root)}, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.stderr)
        self.session = uuid.uuid4().hex
        self.epoch = 1
        self.sequence = 0
        self.notifications = []
        self.output = queue.Queue(32)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        try:
            while True:
                header = self.process.stdout.read(4)
                if len(header) != 4:
                    raise EOFError("worker closed stdout")
                length, = struct.unpack("<I", header)
                assert 0 < length <= 65536
                self.output.put(json.loads(self.process.stdout.read(length)))
        except Exception as error:
            self.output.put(error)

    def call(self, method, params=None, expected="ok"):
        self.sequence += 1
        data = json.dumps(dict(version_major=1, version_minor=0, request_id=self.sequence,
                               session=self.session, epoch=self.epoch, method=method, params=params)).encode()
        self.process.stdin.write(struct.pack("<I", len(data)) + data)
        self.process.stdin.flush()
        while True:
            reply = self.output.get(timeout=35)
            if isinstance(reply, Exception):
                raise reply
            assert reply["session"] == self.session
            if reply.get("notification"):
                self.notifications.append(reply)
                continue
            assert reply["request_id"] == self.sequence, reply
            self.epoch = reply["epoch"]
            self.state = reply["state"]
            assert reply["status"] == expected, reply
            return reply["data"]

    def debug(self, operation, **params):
        return self.call("debug", dict(operation=operation, **params))

    def close(self):
        try:
            if self.process.poll() is None:
                try:self.call("stop")
                except (EOFError,OSError):pass
            self.process.wait(timeout=5)
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.stderr.close()


def probe(worker, asset, root, experimental, new, vulkan, frames=60):
    peer = Peer(worker, root)
    start = time.monotonic()
    try:
        peer.call("hello")  # Regression: omitted notification negotiation remains legal.
        peer.call("hello", {"notifications": True})
        settings = []
        offset = 0
        while True:
            page = peer.debug("settings_list", offset=offset, limit=32)
            settings.extend(page["settings"])
            offset = page["next"]
            if offset is None:
                break
        assert len(settings) >= 125
        peer.debug("settings_set", name="is_new_3ds", value=new)
        peer.debug("settings_set", name="graphics_api", value=2 if vulkan else 0)
        peer.call("load", {"path": str(asset)})
        peer.call("debug", {"operation": "settings_set", "name": "is_new_3ds", "value": not new}, "invalid_argument")
        result = peer.call("run_until", {"frames": frames})
        capture = peer.call("capture")
        hashes = {}
        for lease in capture["frames"]:
            data = (root / lease["path"]).read_bytes()
            assert data[:4] == b"LTRF" and len(data) == lease["bytes"] + 32
            hashes[lease["screen"]] = hashlib.sha256(data[32:]).hexdigest()
        registers = peer.debug("registers", core=0)
        assert len(registers["registers"]) == 16 and len(registers["vfp"]) == 64
        threads = peer.debug("threads")
        if experimental:
            peer.call("pause")
            pc = registers["registers"][15]
            # A one-instruction self-loop makes breakpoint/skip semantics independent of app scheduling.
            peer.debug("write_memory", address=pc & ~3, bytes=[254, 255, 255, 234])
            peer.debug("write_register", core=0, register=15, value=pc & ~3)
            bp = peer.debug("breakpoint_set", address=pc & ~3, kind="execute", length=4)
            stopped = peer.call("run_until", {"frames": 1})
            assert stopped["stop"]["reason"] == "breakpoint", stopped
            assert stopped["stop"]["breakpoint"] == bp["id"]
            peer.debug("breakpoint_remove", id=bp["id"])
            step = peer.debug("step", core=0, count=1)
            assert step["instructions"] == 1 and step["stop"]["reason"] == "step", step
        return dict(new_3ds=new, renderer="vulkan" if vulkan else "software", source=result["source"],
                    hashes=hashes, threads=len(threads["threads"]), seconds=time.monotonic()-start,
                    notifications=len(peer.notifications), root=str(root))
    finally:
        peer.close()
        for log in [root / "probe.stderr.log", *root.glob("log/*.txt")]:
            assert "Failed to find title id for ROM (Error 0)" not in log.read_text(errors="replace"), log


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("worker", type=Path)
    parser.add_argument("asset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--experimental", action="store_true")
    parser.add_argument("--vulkan", action="store_true")
    args = parser.parse_args()
    reports = []
    for new in (False, True):
        report = probe(args.worker.resolve(), args.asset.resolve(),
                       args.output.resolve() / ("new" if new else "old"),
                       args.experimental, new, args.vulkan)
        reports.append(report)
        print(json.dumps(report), flush=True)
    (args.output / "report.json").write_text(json.dumps(reports, indent=2))
