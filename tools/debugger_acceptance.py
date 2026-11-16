"""Known-address debugger regression tests against an owned ARM ELF/3DSX pair."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import time
from native_debug_probe import Peer


def symbols(elf):
    rows = subprocess.check_output(["llvm-nm", "--defined-only", str(elf)], text=True)
    return {parts[2]: int(parts[0], 16) for row in rows.splitlines()
            if len(parts := row.split()) == 3}


def wait(peer):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        result = peer.call("status")
        if peer.state == "paused" and result["stop"].get("reason"):
            return result
        time.sleep(.01)
    raise TimeoutError("no debugger stop within five seconds")


def cpu(worker, rom, root, new):
    syms = symbols(rom.with_suffix(".elf"))
    peer = Peer(worker, root)
    evidence = dict(new_3ds=new, checks={})
    def save(name, result):
        evidence["checks"][name] = result
        (root / "acceptance.json").write_text(json.dumps(evidence, indent=2))
    try:
        peer.call("hello", {"notifications": True})
        peer.debug("settings_set", name="is_new_3ds", value=new)
        peer.call("load", {"path": str(rom)})
        bp = peer.debug("breakpoint_set", address=syms["main"], kind="execute", length=4)
        result = peer.call("run_until", {"frames": 1})
        assert result["stop"]["reason"] == "breakpoint", result
        assert result["stop"]["address"] == syms["main"]
        save("main_breakpoint", result)
        peer.debug("breakpoint_remove", id=bp["id"])
        peer.debug("run_to", core=0, address=syms["lotus_tick_body"])
        result = wait(peer)
        assert result["stop"]["address"] == syms["lotus_tick_body"], result
        assert not peer.debug("breakpoints")["breakpoints"], "temporary breakpoint leaked"
        trace = peer.debug("backtrace", core=0, limit=8)
        assert len(trace["frames"]) >= 3, trace
        assert syms["main"] <= trace["frames"][1]["pc"] < syms["main"] + 128, trace
        save("frame_pointer_backtrace", trace)
        bp = peer.debug("breakpoint_set", address=syms["watched"], kind="write", length=4)
        peer.debug("breakpoint_configure", id=bp["id"], process=None, hit_after=1,
                   condition={"kind":"memory_equals", "address":syms["counter"], "width":4,"value":2}, log=None)
        result = peer.debug("step", core=0, count=100)
        assert result["stop"]["reason"] == "watchpoint", result
        assert result["stop"]["address"] == syms["watched"]
        assert peer.debug("read_memory", address=syms["watched"], length=4)["bytes"] == [2,0,0,0]
        save("watchpoint_during_step", result)
        peer.debug("breakpoint_remove", id=bp["id"])
        peer.debug("save_state", name="roundtrip", worker_frame=0)
        forward = peer.debug("step", core=0, count=20)
        memory = peer.debug("read_memory", address=syms["watched"], length=4)
        peer.debug("load_state", name="roundtrip")
        replay = peer.debug("step", core=0, count=20)
        assert forward["registers"] == replay["registers"], (forward,replay)
        assert memory["bytes"] == peer.debug("read_memory", address=syms["watched"], length=4)["bytes"]
        save("checkpoint_replay", {"registers":replay["registers"],"memory":memory["bytes"],"epoch":peer.epoch})
        logs = peer.debug("logs", after=0, limit=128)
        assert "lotus-fixture:ready" in json.dumps(logs), logs
        save("guest_log", logs)
        for mode, reason in ((1,"exception"),(3,"svc_break"),(4,"exception")):
            peer.call("reset")
            peer.debug("write_memory", address=syms["mode"], bytes=list(struct.pack("<I",mode)))
            result = peer.call("run_until", {"frames":1})
            assert result["stop"]["reason"] == reason, result
            report = peer.debug("crash_report", core=0)
            assert report["failure_domain"] == "guest"
            save(f"crash_mode_{mode}", report)
    finally:
        peer.close()
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    for new in (False,True):
        result = cpu(args.worker.resolve(),args.rom.resolve(),args.output.resolve() / ("new" if new else "old"),new)
        print(json.dumps({"new_3ds":new,"checks":list(result["checks"])}),flush=True)
