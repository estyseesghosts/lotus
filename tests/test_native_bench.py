"""Validate the native benchmark's timing and comparison gates."""

from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

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


if __name__ == "__main__":
    unittest.main()
