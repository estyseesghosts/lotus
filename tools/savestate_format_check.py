"""Savestate format check for the run-length page table (issue #9).

A state saved by a reference worker (the previous page-table format) must load in the candidate
worker and continue exactly like the reference: same registers, guest ticks, fixture memory and
screens right after loading and again after more frames. A state saved by the candidate must
round-trip through itself the same way. Both workers must report the same source SHA, because
`load_state` refuses checkpoints from another source (src/ctr_worker/debug.h). Software
renderer, fixed guest clock, Old 3DS and New 3DS. The candidate boots from a copy of the reference
root's nand/sysdata, because the checkpoint metadata includes the per-root console unique ID. Records checkpoint size and save wall time.

    python tools/savestate_format_check.py <reference ctr-worker.exe> <candidate ctr-worker.exe> \
        <abs debugger fixture .3dsx> runs/<fresh>
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import time
from native_debug_probe import Peer
from debugger_acceptance import symbols

SOFTWARE, FIXED_CLOCK = 0, 1


def boot(worker, rom, root, new, seed=None):
    peer = Peer(worker, root)
    if seed:  # console unique ID and the checkpoint come from the reference root
        for name in ("nand", "sysdata", "checkpoints"):
            shutil.copytree(seed / name, root / name, dirs_exist_ok=True)
    peer.call("hello")
    peer.debug("settings_set", name="graphics_api", value=SOFTWARE)
    peer.debug("settings_set", name="is_new_3ds", value=new)
    peer.debug("settings_set", name="init_clock", value=FIXED_CLOCK)
    peer.call("load", {"path": str(rom)})
    return peer


def run(peer, frames):
    for _ in range(frames // 10):
        peer.call("run_until", {"frames": 10})


def observe(peer, root, syms, screens=True):
    registers = peer.debug("registers", core=0)
    state = dict(registers=registers,
                 memory={name: peer.debug("read_memory", address=syms[name], length=4)["bytes"]
                         for name in ("counter", "watched", "mode")})
    if screens:
        state["screens"] = {lease["screen"]: hashlib.sha256((root / lease["path"]).read_bytes()[32:]).hexdigest()
                            for lease in peer.call("capture")["frames"]}
    return state


def same_at_load(loaded, saved):
    """Everything but guest_ticks must match right after load_state; returns the tick delta.
    The reported ticks right after a load differ from the value read right after the save
    (observed with one worker on both sides), so ticks are compared after continuing."""
    strip = lambda s: dict(s, registers={k: v for k, v in s["registers"].items() if k != "guest_ticks"})
    assert strip(loaded) == strip(saved), ("state after load differs", loaded, saved)
    return loaded["registers"]["guest_ticks"] - saved["registers"]["guest_ticks"]


def save(peer, root, name):
    start = time.perf_counter()
    peer.debug("save_state", name=name, worker_frame=0)
    return dict(seconds=time.perf_counter() - start,
                bytes=(root / "checkpoints" / f"{name}.bin").stat().st_size)


def check(reference, candidate, rom, out, new):
    syms = symbols(rom.with_suffix(".elf"))
    evidence = dict(new_3ds=new)
    ref_root, cand_root = out / "reference", out / "candidate"
    peer = boot(reference, rom, ref_root, new)
    try:
        run(peer, 30)
        evidence["reference_save"] = save(peer, ref_root, "fmt")
        at_save = observe(peer, ref_root, syms, screens=False)
        run(peer, 20)
        later = observe(peer, ref_root, syms)
    finally:
        peer.close()

    peer = boot(candidate, rom, cand_root, new, seed=ref_root)
    try:
        peer.debug("load_state", name="fmt")
        old_delta = same_at_load(observe(peer, cand_root, syms, screens=False), at_save)
        run(peer, 20)
        continued = observe(peer, cand_root, syms)
        assert continued == later, ("old-format continuation differs", continued, later)
        evidence["old_format_load"] = dict(at_save=at_save, after_20_frames=later, ticks_delta_at_load=old_delta)

        evidence["candidate_save"] = save(peer, cand_root, "fmt2")
        at_save = observe(peer, cand_root, syms, screens=False)
        run(peer, 20)
        later = observe(peer, cand_root, syms)
        peer.debug("load_state", name="fmt2")
        new_delta = same_at_load(observe(peer, cand_root, syms, screens=False), at_save)
        run(peer, 20)
        assert observe(peer, cand_root, syms) == later, "new-format continuation differs"
        evidence["new_format_roundtrip"] = dict(at_save=at_save, after_20_frames=later, ticks_delta_at_load=new_delta)
    finally:
        peer.close()
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("reference", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    for new in (False, True):
        result = check(args.reference.resolve(), args.candidate.resolve(), args.rom.resolve(),
                       args.output / ("new" if new else "old"), new)
        (args.output / f"{'new' if new else 'old'}.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(dict(new_3ds=new, reference_save=result["reference_save"],
                              candidate_save=result["candidate_save"])), flush=True)
