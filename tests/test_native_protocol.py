"""Native pipe contract without guest assets: python tests/test_native_protocol.py <worker>."""
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading


class Worker:
    def __init__(self, executable):
        self.root = Path(tempfile.mkdtemp(prefix="lotus-wire-"))
        self.log = (self.root / "test.stderr.log").open("wb")
        self.process = subprocess.Popen([executable], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.log, env={**os.environ, "LOTUS_STORAGE_ROOT": str(self.root)},
                                        creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
        self.messages = queue.Queue()
        def read():
            try:
                while prefix := self.process.stdout.read(4):
                    assert len(prefix) == 4
                    length, = struct.unpack("<I", prefix)
                    assert 0 < length <= 65536
                    payload = self.process.stdout.read(length)
                    assert len(payload) == length
                    self.messages.put(json.loads(payload))
            finally:
                self.messages.put(None)
        threading.Thread(target=read, daemon=True).start()

    def request(self, identity=1, **overrides):
        request = dict(version_major=1, version_minor=0, request_id=identity,
                       session="0" * 32, epoch=1, method="hello", params=None)
        request.update(overrides)
        payload = json.dumps(request).encode()
        self.process.stdin.write(struct.pack("<I", len(payload)) + payload)
        self.process.stdin.flush()
        return self.messages.get(timeout=5)

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        self.log.close()


def main():
    executable = str(Path(sys.argv[1]).resolve())
    worker = Worker(executable)
    try:
        assert worker.request()["status"] == "ok"
        assert worker.request(2, method="status", epoch=0)["status"] == "stale_epoch"
        assert worker.request(3, method="status", session="f" * 32)["status"] == "permission_denied"
        assert worker.request(4, version_major=2)["status"] == "unsupported"
        assert worker.request(5, method="load", params={"path": "missing.3dsx"})["status"] == "invalid_argument"
        assert worker.request(6, method="status")["status"] == "ok"
        assert worker.request(7, method="stop")["state"] == "stopped"
        worker.process.wait(timeout=5)
        assert worker.process.returncode == 0
    finally:
        worker.close()
    for invalid in (b"\0\0\0\0", struct.pack("<I", 65537), struct.pack("<I", 2) + b"{"):
        worker = Worker(executable)
        try:
            worker.process.stdin.write(invalid)
            worker.process.stdin.close()
            assert worker.messages.get(timeout=5) is None
            worker.process.wait(timeout=5)
        finally:
            worker.close()
    for changes in ({"unexpected": 1}, {"request_id": 1}):
        worker = Worker(executable)
        try:
            assert worker.request()["status"] == "ok"
            assert worker.request(2, method="status", **changes) is None
            worker.process.wait(timeout=5)
            assert worker.process.returncode != 0
        finally:
            worker.close()
    print("Native framing/identity/epoch/version/length/truncation/replay: passed")


if __name__ == "__main__":
    main()
