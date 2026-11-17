"""GPU capture levels (lotus#24) on one CITnLO worker, software and offscreen Vulkan.

Per renderer:
- counters: batched command path, PicaCore counters advance, no draws recorded, no gpu/ files;
- events: per-command path, draws recorded as metadata only, still no gpu/ files; gpu_draw
  returns a metadata document and shader_trace is refused;
- draws with max_draws=2: two snapshots, later draws counted in snapshots_skipped_budget;
- fidelity: draw 1, stopped on PICA event 2 from fresh boots, under `full` and under `draws`
  with draw_range [1, 1] and resources, must give a byte-identical gpu_draw document.
  Identical boots do not always reach draw 1 in the same guest state: of three `full` Vulkan
  boots of t03 one had different guest-written registers and uniforms at the same guest tick
  (runs/gpu-repeat-24-01), and a software boot once reached draw 1 at another guest tick
  (runs/gpu-modes-24-02); this is the run-to-run divergence tracked in lotus#29. So boots
  are repeated (up to ATTEMPTS per mode) until some pair matches, and the attempts and the
  differing fields of mismatches are reported. With --baseline <older ctr-worker>, the same
  draw from that worker (capture omitted) is matched the same way, without asserting.

Host times are reported, not asserted (a busy host is no timing evidence).

    python tools/gpu_capture_modes.py <ctr-worker.exe> <rom.3dsx> <fresh output dir> [--baseline <ctr-worker.exe>]
"""
import argparse
import json
from pathlib import Path
import time

from native_debug_probe import Peer

RENDERERS = {"software": 0, "vulkan": 2}
ATTEMPTS = 4


def boot(worker, rom, root, renderer):
    peer = Peer(worker, root)
    peer.call("hello")
    peer.debug("settings_set", name="graphics_api", value=RENDERERS[renderer])
    peer.debug("settings_set", name="use_hw_shader", value=False)  # vertex outputs, as gpu_acceptance
    peer.debug("settings_set", name="init_clock", value=1)  # FixedTime, as tools/bench.py
    peer.call("load", {"path": str(rom)})
    for _ in range(6):
        peer.call("run_until", {"frames": 10})
    return peer


def timed_run(peer, frames):
    start = time.perf_counter()
    for _ in range(frames // 10):
        peer.call("run_until", {"frames": 10})
    return round((time.perf_counter() - start) * 1e3, 1)


def gpu_files(root):
    folder = root / "gpu"
    return sorted(p.name for p in folder.iterdir()) if folder.exists() else []


def levels(worker, rom, root, renderer):
    peer = boot(worker, rom, root, renderer)
    report = {}
    try:
        before = peer.debug("gpu_draws")["counters"]
        reply = peer.debug("gpu_configure", enabled=True, stop_events=[], capture="counters")
        assert reply["command_path"] == "batched", reply
        ms = timed_run(peer, 30)
        listing = peer.debug("gpu_draws")
        after = listing["counters"]
        assert not listing["draws"], listing
        assert after["draw_calls"] > before["draw_calls"], (before, after)
        assert after["command_lists"] > before["command_lists"], (before, after)
        assert after["fast_path_commands"] > before["fast_path_commands"], (before, after)
        assert not gpu_files(root)
        report["counters"] = dict(host_ms_30_frames=ms, draw_calls=after["draw_calls"] - before["draw_calls"],
                                  command_lists=after["command_lists"] - before["command_lists"])

        refused = peer.call("debug", dict(operation="gpu_configure", enabled=True, stop_events=[2],
                                          capture="counters"), expected=None)
        assert peer.status != "ok", refused

        reply = peer.debug("gpu_configure", enabled=True, stop_events=[], capture="events")
        assert reply["command_path"] == "per_command" and reply["resources"] is False, reply
        ms = timed_run(peer, 30)
        listing = peer.debug("gpu_draws")
        assert listing["draws"] and not any(row["snapshot"] for row in listing["draws"]), listing
        assert listing["fast_path_commands"] == after["fast_path_commands"], listing
        assert not gpu_files(root), gpu_files(root)
        draw = listing["draws"][-1]["draw"]
        document = json.loads((root / peer.debug("gpu_draw", draw=draw)["artifacts"][0]["path"]).read_text())
        assert document["snapshot"] == "metadata_only" and "registers" not in document, document
        peer.call("debug", dict(operation="shader_trace", draw=draw, vertex=0), expected=None)
        assert peer.status != "ok"
        report["events"] = dict(host_ms_30_frames=ms, draws_retained=len(listing["draws"]),
                                lost_draws=listing["lost_draws"])

        reply = peer.debug("gpu_configure", enabled=True, stop_events=[], capture="draws", max_draws=2)
        ms = timed_run(peer, 30)
        listing = peer.debug("gpu_draws")
        assert listing["snapshots_taken"] == 2 and listing["snapshots_skipped_budget"] > 0, listing
        assert sum(row["snapshot"] for row in listing["draws"]) <= 2, listing
        report["draws_budget_2"] = dict(host_ms_30_frames=ms, snapshots_taken=listing["snapshots_taken"],
                                        snapshots_skipped_budget=listing["snapshots_skipped_budget"])

        peer.debug("gpu_configure", enabled=True, stop_events=[], capture="full")
        ms = timed_run(peer, 30)
        listing = peer.debug("gpu_draws")
        assert all(row["snapshot"] for row in listing["draws"]), listing
        report["full"] = dict(host_ms_30_frames=ms)

        peer.debug("gpu_configure", enabled=False, stop_events=[])
        report["off"] = dict(host_ms_30_frames=timed_run(peer, 30))
    finally:
        peer.close()
    return report


def first_draw(worker, rom, root, renderer, configure):
    """gpu_draw document of draw 1 after a stop on PICA event 2, from a fresh boot."""
    peer = boot(worker, rom, root, renderer)
    try:
        peer.debug("gpu_configure", enabled=True, stop_events=[2], **configure)
        stopped = peer.call("run_until", {"frames": 1})
        assert stopped["stop"]["reason"] == "pica" and stopped["stop"]["draw"] == 1, stopped
        artifact = peer.debug("gpu_draw", draw=1)["artifacts"][0]
        return (root / artifact["path"]).read_bytes()
    finally:
        peer.close()


def differences(old, new, path=""):
    if isinstance(old, dict) and isinstance(new, dict):
        out = []
        for key in sorted(set(old) | set(new)):
            if key not in new:
                out.append(f"{path}/{key}: removed")
            elif key not in old:
                out.append(f"{path}/{key}: added")
            else:
                out += differences(old[key], new[key], f"{path}/{key}")
        return out
    return [] if old == new else [f"{path}: changed"]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()
    worker, rom, out = args.worker.resolve(), args.rom.resolve(), args.output.resolve()
    report = {}
    for renderer in RENDERERS:
        row = levels(worker, rom, out / renderer / "levels", renderer)
        fulls = []

        def match(name, candidate_worker, configure):
            seen = []
            for attempt in range(ATTEMPTS):
                if len(fulls) <= attempt:
                    fulls.append(first_draw(worker, rom, out / renderer / f"full-{attempt}", renderer,
                                            dict(capture="full")))
                seen.append(first_draw(candidate_worker, rom, out / renderer / f"{name}-{attempt}",
                                       renderer, configure))
                if set(seen) & set(fulls):
                    return dict(identical=True, attempts=attempt + 1, document_bytes=len(seen[-1]))
            return dict(identical=False, attempts=ATTEMPTS,
                        differences=differences(json.loads(fulls[-1]), json.loads(seen[-1])))

        row["selected_vs_full"] = match("selected", worker,
                                        dict(capture="draws", draw_range=[1, 1], resources=True))
        if args.baseline:
            row["baseline_vs_full"] = match("baseline", args.baseline.resolve(), {})
        assert row["selected_vs_full"]["identical"], row
        report[renderer] = row
        print(renderer, json.dumps(row), flush=True)
    (out / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
