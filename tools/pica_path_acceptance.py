"""PICA command-path acceptance (issue #4) on software and offscreen Vulkan.

Asserts, per renderer:
- passive debugger (CITnLO worker attached, no GPU capture): the batched path runs, i.e.
  `fast_path_commands` (PicaCore counter, pica_core.cpp ProcessCmdList) advances;
- capture on: the counter stops and draws are recorded (per-command path);
- capture off again: the counter advances again;
- a run that toggled capture ends with the same guest ticks as a run that never enabled it.

Screen hashes are recorded but only reported: final screens can differ between identical
runs for a reason not yet found (docs/34-performance-and-issue-triage.md).

    python tools/pica_path_acceptance.py <ctr-worker.exe> <rom.3dsx> <fresh output dir>
"""
import argparse
import hashlib
import json
from pathlib import Path

from native_debug_probe import Peer

RENDERERS = {"software": 0, "vulkan": 2}
WARMUP, SEGMENT = 60, 30


def boot(worker, rom, root, renderer):
    peer = Peer(worker, root)
    peer.call("hello")
    peer.debug("settings_set", name="graphics_api", value=RENDERERS[renderer])
    peer.debug("settings_set", name="init_clock", value=1)  # FixedTime, as tools/bench.py
    peer.call("load", {"path": str(rom)})
    run(peer, WARMUP)
    return peer


def run(peer, frames):
    """In steps of 10, like tools/bench.py: one long run_until can exceed the worker's host deadline."""
    for _ in range(frames // 10):
        peer.call("run_until", {"frames": 10})


def counter(peer):
    return peer.debug("gpu_draws")["fast_path_commands"]


def finish(peer, root):
    ticks = peer.debug("registers", core=0)["guest_ticks"]
    screens = {lease["screen"]: hashlib.sha256((root / lease["path"]).read_bytes()[32:]).hexdigest()
               for lease in peer.call("capture")["frames"]}
    return ticks, screens


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    worker, rom, out = args.worker.resolve(), args.rom.resolve(), args.output.resolve()
    report = {}
    for renderer in RENDERERS:
        root = out / renderer / "toggled"
        peer = boot(worker, rom, root, renderer)
        try:
            passive = [counter(peer)]
            run(peer, SEGMENT)
            passive.append(counter(peer))
            assert passive[1] > passive[0] > 0, f"batched path not taken while passive: {passive}"
            reply = peer.debug("gpu_configure", enabled=True, stop_events=[])
            assert reply["command_path"] == "per_command", reply
            run(peer, SEGMENT)
            listing = peer.debug("gpu_draws")
            assert listing["fast_path_commands"] == passive[1], f"batched path ran during capture: {listing}"
            assert listing["draws"], "capture recorded no draws"
            reply = peer.debug("gpu_configure", enabled=False, stop_events=[])
            assert reply["command_path"] == "batched", reply
            run(peer, SEGMENT)
            resumed = counter(peer)
            assert resumed > passive[1], f"batched path not restored: {resumed} <= {passive[1]}"
            toggled = finish(peer, root)
        finally:
            peer.close()
        root = out / renderer / "never"
        peer = boot(worker, rom, root, renderer)
        try:
            run(peer, 3 * SEGMENT)
            never = finish(peer, root)
        finally:
            peer.close()
        assert toggled[0] == never[0], f"guest ticks differ: toggled {toggled[0]}, never {never[0]}"
        report[renderer] = dict(passive_fast_path=passive, after_capture_fast_path=resumed,
                                captured_draws=len(listing["draws"]), guest_ticks=toggled[0],
                                screens_identical=toggled[1] == never[1])
        print(renderer, json.dumps(report[renderer]), flush=True)
    (out / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
