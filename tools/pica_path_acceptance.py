"""PICA command-path acceptance (issue #4) on software and offscreen Vulkan.

Asserts, per renderer:
- passive debugger (CITnLO worker attached, no GPU capture): the batched path runs, i.e.
  `fast_path_commands` (PicaCore counter, pica_core.cpp ProcessCmdList) advances;
- capture on: the counter stops and draws are recorded (per-command path);
- capture off again: the counter advances again;
- a run that toggled capture ends with the same guest ticks as a run that never enabled it.

With --integration, compare three matched windows on the two worker branches,
using deterministic async HLE and no speed limiter. Require equal final guest
ticks and screen hashes for all three runs.

    python tools/pica_path_acceptance.py <ctr-worker.exe> <rom.3dsx> <fresh output dir>
"""
import argparse
import hashlib
import json
from pathlib import Path
from time import perf_counter

from native_debug_probe import Peer

RENDERERS = {"software": 0, "vulkan": 2}
WARMUP, SEGMENT = 60, 30


def boot(worker, rom, root, renderer, compare):
    peer = Peer(worker, root)
    try:
        hello = peer.call("hello")
        peer.debug("settings_set", name="graphics_api", value=RENDERERS[renderer])
        peer.debug("settings_set", name="init_clock", value=1)  # FixedTime, as tools/bench.py
        if compare:
            peer.debug("settings_set", name="deterministic_async_operations", value=True)
            peer.debug("settings_set", name="frame_limit", value=0)
        peer.call("load", {"path": str(rom)})
        run(peer, WARMUP)
        return peer, hello["source"]
    except BaseException:
        peer.close()
        raise


def run(peer, frames):
    """In steps of 10, like tools/bench.py: one long run_until can exceed the worker's host deadline."""
    for _ in range(frames // 10):
        peer.call("run_until", {"frames": 10})


def timed_segment(peer):
    start = perf_counter()
    run(peer, SEGMENT)
    return (perf_counter() - start) * 1000


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
    parser.add_argument("--integration", type=Path, help="matched AZAHAR baseline worker")
    args = parser.parse_args()
    worker, rom, out = args.worker.resolve(), args.rom.resolve(), args.output.resolve()
    integration = args.integration.resolve() if args.integration else None
    report = {}
    for renderer in RENDERERS:
        root = out / renderer / "toggled"
        peer, citnlo_source = boot(worker, rom, root, renderer, compare=integration is not None)
        try:
            passive = [counter(peer)]
            passive_ms = timed_segment(peer)
            passive.append(counter(peer))
            assert passive[1] > passive[0] > 0, f"batched path not taken while passive: {passive}"
            reply = peer.debug("gpu_configure", enabled=True, stop_events=[])
            assert reply["command_path"] == "per_command", reply
            capture_ms = timed_segment(peer)
            listing = peer.debug("gpu_draws")
            assert listing["fast_path_commands"] == passive[1], f"batched path ran during capture: {listing}"
            assert listing["draws"], "capture recorded no draws"
            reply = peer.debug("gpu_configure", enabled=False, stop_events=[])
            assert reply["command_path"] == "batched", reply
            resumed_ms = timed_segment(peer)
            resumed = counter(peer)
            assert resumed > passive[1], f"batched path not restored: {resumed} <= {passive[1]}"
            toggled = finish(peer, root)
        finally:
            peer.close()
        root = out / renderer / "never"
        peer, _ = boot(worker, rom, root, renderer, compare=integration is not None)
        try:
            never_ms = [timed_segment(peer) for _ in range(3)]
            never = finish(peer, root)
        finally:
            peer.close()
        assert toggled[0] == never[0], f"guest ticks differ: toggled {toggled[0]}, never {never[0]}"
        report[renderer] = dict(passive_fast_path=passive, after_capture_fast_path=resumed,
                                captured_draws=len(listing["draws"]), guest_ticks=toggled[0],
                                screens_identical=toggled[1] == never[1],
                                citnlo_source=citnlo_source,
                                toggled_hashes=toggled[1], never_hashes=never[1],
                                toggled_ms=[passive_ms, capture_ms, resumed_ms], never_ms=never_ms)
        if integration:
            root = out / renderer / "integration"
            peer, integration_source = boot(integration, rom, root, renderer, compare=True)
            try:
                integration_ms = [timed_segment(peer) for _ in range(3)]
                baseline = finish(peer, root)
            finally:
                peer.close()
            assert baseline == toggled == never, (baseline, toggled, never)
            report[renderer].update(integration_source=integration_source, integration_ms=integration_ms,
                                    integration_hashes=baseline[1])
        print(renderer, json.dumps(report[renderer]), flush=True)
    (out / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
