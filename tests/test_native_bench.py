"""Validate the native benchmark's timing and comparison gates."""

import hashlib
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import bench


class NativeBenchTest(unittest.TestCase):
    def test_ok_reply_can_stop_before_the_measured_frame_target(self):
        peer = Mock(status="ok")
        settings = {}

        def debug(operation, **params):
            if operation == "settings_list":
                return {"settings": [{"name": key, "value": value} for key, value in settings.items()],
                        "next": None}
            key = params["name"]
            if operation == "settings_set":
                settings[key] = params["value"]
            return {"value": settings.get(key)}

        peer.debug.side_effect = debug
        peer.call.side_effect = [
            {}, {}, {"frames": 20}, {"frames": 24, "stop": {"reason": "breakpoint"}}
        ]
        with tempfile.TemporaryDirectory() as root, patch.object(bench, "Peer", return_value=peer), \
                patch.object(bench, "process_usage", return_value=None):
            row = bench.trial(Path("worker"), Path("guest"), Path(root), "software", False, [], 20, 10, 0)
        self.assertEqual(row["status"], "early_stop")
        self.assertEqual(row["frames_done"], 4)
        self.assertEqual(row["stop"], {"reason": "breakpoint"})
        self.assertNotIn("fps", row)
        peer.close.assert_called_once()

    def test_profile_mismatch_suppresses_cross_worker_ratio(self):
        run = dict(status="ok", host_seconds=1, profile={"frame_limit": 0},
                   usage=None, guest_ticks=100, screens={"top": "test"}, guest={})
        default = dict(bench.summarize([run]), runs_detail=[run], _identity=bench.identity([run]))
        limited = dict(run, profile={"frame_limit": 100})
        candidate = dict(bench.summarize([limited]), runs_detail=[limited], _identity=bench.identity([limited]))
        bench.compare({"software": {"default": {"off": default}, "candidate": {"off": candidate}}},
                      [("default", None), ("candidate", None)])
        self.assertEqual(candidate["profile_differences"], {"frame_limit": {"default": 0, "candidate": 100}})
        self.assertTrue(candidate["guest_identical_to_default"])
        self.assertNotIn("ratio_vs_default", candidate)

    def test_stock_oracle_hashes_the_same_pixel_order_as_native(self):
        process = MagicMock()
        process.__enter__.return_value = process
        process.wait.return_value = 0
        args = SimpleNamespace(warmup=20, frames=10, frame_limit=0, new_3ds=False)

        def run(command, *, stdout, stderr):
            self.assertEqual(command[3:], ["--benchmark", "20", "10", "0", "0", "1"])
            root = Path(command[2])
            self.assertTrue((root / "sysdata").is_dir())
            for name, width in (("top", 400), ("bottom", 320)):
                (root / f"{name}.rgba").write_bytes(bytes((1, 2, 3, 255)) * width * 240)
            stock_profile = {key: 0 for key in bench.STOCK_PROFILE_KEYS} | {"output_type": 1}
            (root / "benchmark.txt").write_text(
                "frames=10\nguest_ticks=500\nhost_seconds=0.01\nload_seconds=1.0\nheadless_system_fps=1000\n"
                "gpu_seconds_per_frame=0\nsvc_seconds_per_frame=0\n"
                "ipc_seconds_per_frame=0\nswap_seconds_per_frame=0\n" +
                "".join(f"{key}={value}\n" for key, value in stock_profile.items()), encoding="utf-8")
            return process

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            console = root / "identity"
            for name in bench.IDENTITY:
                (console / name).mkdir(parents=True)
            with patch.object(bench.subprocess, "Popen", side_effect=run), \
                    patch.object(bench, "process_usage", return_value=None):
                row = bench.stock_trial(Path("oracle"), Path("guest"), root / "stock", args, console, True)
        self.assertEqual(row["guest_ticks"], 500)
        self.assertEqual(row["screens"]["top"], hashlib.sha256(bytes((3, 2, 1, 255)) * 400 * 240).hexdigest())
        self.assertEqual(row["profile"]["output_type"], 1)


if __name__ == "__main__":
    unittest.main()
