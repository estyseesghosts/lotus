"""Control request backpressure on a real worker (lotus#160).

python tests/test_control_backpressure.py <output> <guest.3dsx> <worker.exe>... [--baseline]

Cases, each in a fresh worker:
- flood: 40 status requests queued behind a load. All are answered once, in order.
- unread: 200 requests while the client does not read replies. The worker stops reading at 16
  unwritten replies, then answers all of them once the client reads.
- mixed: stale epochs, input, preview, debug-free status and pause interleaved in one burst.
- stalled_stop: stop queued behind a blocked reader; closing the reply pipe ends the worker.
- drained_stop: the same, but reading resumes and stop completes with exit 0.
- frames: malformed frames and clean EOF exit with distinct stderr reasons.
--baseline runs only `flood` and expects the unfixed worker to exit.
"""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import threading
import time
import uuid

LIMIT = 16


class Raw:
    def __init__(self, worker, root):
        root.mkdir(parents=True)
        self.root = root
        self.stderr_path = root / "stderr.log"
        self.stderr = self.stderr_path.open("wb")
        self.process = subprocess.Popen([str(worker)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.stderr, env={**os.environ, "LOTUS_STORAGE_ROOT": str(root)})
        self.session = uuid.uuid4().hex
        self.epoch = 1
        self.sequence = 0

    def frame(self, method, params=None, epoch=None):
        self.sequence += 1
        data = json.dumps(dict(version_major=1, version_minor=0, request_id=self.sequence, session=self.session,
                               epoch=self.epoch if epoch is None else epoch, method=method,
                               params=params)).encode()
        return self.sequence, struct.pack("<I", len(data)) + data

    def send(self, frames):
        self.process.stdin.write(b"".join(frames))
        self.process.stdin.flush()

    def send_async(self, frames):
        """Write from a thread: the pipe blocks once the worker stops reading. A worker that
        exits mid-write ends the write with OSError, kept in `thread.error`."""
        def write():
            try:
                self.send(frames)
            except OSError as error:
                thread.error = error

        thread = threading.Thread(target=write, daemon=True)
        thread.error = None
        thread.start()
        return thread

    def receive(self):
        header = self.process.stdout.read(4)
        if len(header) != 4:
            raise EOFError("worker closed stdout")
        length, = struct.unpack("<I", header)
        reply = json.loads(self.process.stdout.read(length))
        assert "notification" not in reply, reply
        return reply

    def call(self, method, params=None, expected="ok"):
        identity, frame = self.frame(method, params)
        self.send([frame])
        reply = self.receive()
        assert reply["request_id"] == identity and reply["status"] == expected, reply
        self.epoch = reply["epoch"]
        return reply["data"]

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=10)
        self.stderr.close()

    def stderr_text(self):
        self.stderr.flush()
        return self.stderr_path.read_text(errors="replace")


def replies_in_order(raw, identities):
    statuses = []
    for identity in identities:
        reply = raw.receive()
        assert reply["request_id"] == identity, (reply["request_id"], identity)
        statuses.append(reply["status"])
    return statuses


def requests(raw):
    return raw.call("status")["transport"]["requests"]


def boot(worker, guest, root):
    raw = Raw(worker, root)
    raw.call("hello")
    raw.call("debug", {"operation": "settings_set", "name": "graphics_api", "value": 0})
    raw.call("load", {"path": str(guest)})
    return raw


def flood(worker, guest, root, baseline):
    raw = Raw(worker, root)
    try:
        raw.call("hello")
        raw.call("debug", {"operation": "settings_set", "name": "graphics_api", "value": 0})
        load_id, load = raw.frame("load", {"path": str(guest)})
        statuses = [raw.frame("status", epoch=2) for _ in range(40)]
        raw.send([load, *(frame for _, frame in statuses)])
        try:
            first = raw.receive()
        except EOFError:
            first = None
        if baseline:
            answered = 0
            try:
                while first is not None and answered < len(statuses):
                    raw.receive()
                    answered += 1
            except EOFError:
                pass
            raw.process.wait(timeout=10)
            assert answered < len(statuses), "the baseline worker answered the whole flood"
            return {"exit_code": raw.process.returncode, "load_replied": first is not None,
                    "statuses_answered": answered, "stderr_tail": raw.stderr_text()[-400:]}
        assert first and first["request_id"] == load_id and first["status"] == "ok", first
        raw.epoch = first["epoch"]
        assert set(replies_in_order(raw, [identity for identity, _ in statuses])) == {"ok"}
        stats = requests(raw)
        assert stats["limit"] == LIMIT and stats["peak"] == LIMIT, stats
        # This status, plus at most one reply the client read before the writer returned its credit.
        assert 1 <= stats["in_flight"] <= 2 and stats["backpressure_waits"] > 0, stats
        return {"requests": stats}
    finally:
        raw.close()


def unread(worker, guest, root):
    raw = boot(worker, guest, root)
    try:
        before = requests(raw)
        frames = [raw.frame("status") for _ in range(200)]
        writer = raw.send_async([frame for _, frame in frames])
        time.sleep(1.0)
        assert raw.process.poll() is None, "worker exited under backpressure"
        assert writer.is_alive(), "worker kept reading although replies were not read"
        assert set(replies_in_order(raw, [identity for identity, _ in frames])) == {"ok"}
        writer.join(timeout=10)
        assert writer.error is None, writer.error
        status = raw.call("status")
        stats, transport = status["transport"]["requests"], status["transport"]
        assert stats["peak"] == LIMIT and stats["backpressure_waits"] > before["backpressure_waits"], stats
        assert stats["completed"] >= before["completed"] + 200, stats
        assert transport["peak"] <= LIMIT, transport  # writer queue bounded by the window
        return {"requests": stats, "writer_peak": transport["peak"]}
    finally:
        raw.close()


def mixed(worker, guest, root):
    raw = boot(worker, guest, root)
    try:
        raw.call("run_until", {"frames": 2})
        plan = []
        for index in range(48):
            kind = index % 4
            if kind == 0:
                plan.append(("stale_epoch", raw.frame("status", epoch=raw.epoch - 1)))
            elif kind == 1:
                plan.append(("ok", raw.frame("input", {"owner": "live", "buttons": index & 1,
                                                       "x": 0, "y": 0, "touch": False})))
            elif kind == 2:
                plan.append((None, raw.frame("preview", {"nonblocking": True})))
            else:
                plan.append(("ok", raw.frame("status")))
        plan.append(("ok", raw.frame("run", {"frames": 30})))
        plan.append(("ok", raw.frame("pause")))
        writer = raw.send_async([frame for _, (_, frame) in plan])
        statuses = replies_in_order(raw, [identity for _, (identity, _) in plan])
        writer.join(timeout=10)
        assert writer.error is None, writer.error
        for (expected, _), status in zip(plan, statuses):
            assert expected is None or status == expected, (expected, status)
            assert expected is not None or status in ("ok", "unsupported"), status
        stats = requests(raw)
        assert stats["peak"] <= LIMIT, stats
        return {"requests": stats, "statuses": {s: statuses.count(s) for s in set(statuses)}}
    finally:
        raw.close()


def stalled_stop(worker, guest, root, drain):
    raw = boot(worker, guest, root)
    try:
        frames = [raw.frame("status") for _ in range(100)]
        stop_id, stop = raw.frame("stop")
        writer = raw.send_async([*(frame for _, frame in frames), stop])
        time.sleep(1.0)
        assert writer.is_alive() and raw.process.poll() is None
        start = time.monotonic()
        if drain:
            replies_in_order(raw, [identity for identity, _ in frames])
            last = raw.receive()
            assert last["request_id"] == stop_id and last["state"] == "stopped", last
            raw.process.wait(timeout=10)
            assert raw.process.returncode == 0, raw.process.returncode
        else:
            raw.process.stdout.close()  # forced pipe close while the reader waits for credits
            raw.process.wait(timeout=10)
            assert raw.process.returncode != 0
        writer.join(timeout=10)
        assert not drain or writer.error is None, writer.error
        stderr = raw.stderr_text()
        if not drain:
            assert "worker_control_error: control pipe disconnected" in stderr, stderr[-400:]
        return {"exit_code": raw.process.returncode, "seconds_to_exit": time.monotonic() - start}
    finally:
        raw.close()


def frames(worker, root):
    results = {}
    cases = {
        "bad_length": (struct.pack("<I", 65537), "request frame length outside 1..65536", 1),
        "zero_length": (b"\0\0\0\0", "request frame length outside 1..65536", 1),
        "bad_json": (struct.pack("<I", 2) + b"{x", "request frame is not JSON", 1),
        "truncated": (struct.pack("<I", 8) + b"{}", "request frame truncated", 1),
        "eof": (b"", "worker_control_closed: request stream closed", 0),
    }
    for name, (data, reason, code) in cases.items():
        raw = Raw(worker, root / name)
        try:
            raw.call("hello")
            raw.process.stdin.write(data)
            raw.process.stdin.close()
            raw.process.wait(timeout=10)
            stderr = raw.stderr_text()
            assert raw.process.returncode == code, (name, raw.process.returncode)
            prefix = "" if name == "eof" else "worker_control_error: "
            assert prefix + reason in stderr, (name, stderr[-400:])
            results[name] = raw.process.returncode
        finally:
            raw.close()
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("guest", type=Path)
    parser.add_argument("workers", type=Path, nargs="+")
    parser.add_argument("--baseline", action="store_true")
    args = parser.parse_args()
    guest = args.guest.resolve()
    report = []
    for index, worker in enumerate(args.workers):
        worker = worker.resolve()
        root = args.output.resolve() / f"w{index}"
        result = {"worker": str(worker), "flood": flood(worker, guest, root / "flood", args.baseline)}
        if args.baseline:
            assert result["flood"]["exit_code"] is not None
        else:
            result["unread"] = unread(worker, guest, root / "unread")
            result["mixed"] = mixed(worker, guest, root / "mixed")
            result["stalled_stop"] = stalled_stop(worker, guest, root / "stalled-stop", False)
            result["drained_stop"] = stalled_stop(worker, guest, root / "drained-stop", True)
            result["frames"] = frames(worker, root / "frames")
        report.append(result)
        print(json.dumps(result))
    (args.output / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
