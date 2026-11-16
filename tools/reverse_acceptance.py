"""Continuous-run reverse continue against the owned debugger fixture.

Runs forward with rewind enabled and collects several breakpoint and write
watchpoint stops, then asks for reverse_continue. A breakpoint lands on the
previous hit; a watchpoint lands before the access that caused the current
stop, so memory shows the old value. Going forward re-hits the same access.
"""
import argparse
import json
from pathlib import Path
import struct
from native_debug_probe import Peer
from debugger_acceptance import symbols


def u32(peer, address):
    return struct.unpack("<I", bytes(peer.debug("read_memory", address=address, length=4)["bytes"]))[0]


def forward(peer, reason):
    result = peer.call("run_until", {"frames": 4})
    assert result["stop"]["reason"] == reason, result
    return result["stop"]


def run(worker, rom, root, new):
    syms = symbols(rom.with_suffix(".elf"))
    peer = Peer(worker, root)
    evidence = dict(new_3ds=new, checks={})
    try:
        peer.call("hello")
        peer.debug("settings_set", name="is_new_3ds", value=new)
        peer.call("load", {"path": str(rom)})
        peer.debug("rewind_configure", enabled=True, capacity=32, budget_mib=128)

        # Write watchpoint: values v, v+1, v+2 observed after each access.
        bp = peer.debug("breakpoint_set", address=syms["watched"], kind="write", length=4)
        seen = []
        for _ in range(3):
            forward(peer, "watchpoint")
            seen.append(u32(peer, syms["watched"]))
        assert seen[1] == seen[0] + 1 and seen[2] == seen[1] + 1, seen
        back = peer.debug("reverse_continue")
        stop = back["stop"]
        assert stop["reason"] == "watchpoint" and stop["reverse"] and not stop["access_completed"], back
        assert u32(peer, syms["watched"]) == seen[1], (back, seen)
        again = forward(peer, "watchpoint")
        assert u32(peer, syms["watched"]) == seen[2], again
        back = peer.debug("reverse_continue")
        assert u32(peer, syms["watched"]) == seen[1], back
        back = peer.debug("reverse_continue")
        assert u32(peer, syms["watched"]) == seen[0], back
        evidence["checks"]["watchpoint"] = dict(values=seen, landed=back)
        peer.debug("breakpoint_remove", id=bp["id"])

        # Execute breakpoint: reverse lands on the previous hit, counter one lower.
        bp = peer.debug("breakpoint_set", address=syms["lotus_tick_body"], kind="execute", length=4)
        counters = []
        for _ in range(3):
            stop = forward(peer, "breakpoint")
            assert stop["address"] == syms["lotus_tick_body"], stop
            counters.append(u32(peer, syms["counter"]))
        assert counters[2] == counters[0] + 2, counters
        back = peer.debug("reverse_continue")
        assert back["stop"]["reason"] == "breakpoint" and back["stop"]["address"] == syms["lotus_tick_body"], back
        assert u32(peer, syms["counter"]) == counters[1], (back, counters)
        stop = forward(peer, "breakpoint")
        assert u32(peer, syms["counter"]) == counters[2], stop
        evidence["checks"]["breakpoint"] = dict(counters=counters, landed=back)
        peer.debug("breakpoint_remove", id=bp["id"])

        # No breakpoints: reverse continue is refused and keeps the present.
        before = u32(peer, syms["counter"])
        evidence["checks"]["no_breakpoint_refused"] = peer.call(
            "debug", dict(operation="reverse_continue"), expected="invalid_argument")
        assert u32(peer, syms["counter"]) == before
        evidence["checks"]["status"] = peer.debug("rewind_status")
        (root / "acceptance.json").write_text(json.dumps(evidence, indent=2))
    finally:
        peer.close()
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    for new in (False, True):
        result = run(args.worker.resolve(), args.rom.resolve(), args.output.resolve() / ("new" if new else "old"), new)
        print(json.dumps({"new_3ds": new, "checks": list(result["checks"])}), flush=True)
