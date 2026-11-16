"""Guest SDMC writes stay inside the session root (T31 confinement).

The owned filesystem fixture opens SDMC paths chosen by the host through
FSUSER_OpenFileDirectly. A plain path must land in <root>/sdmc; paths that
climb above the archive root must fail and leave no file anywhere above the
session root. Requires CITnLO (write_memory).
"""
import argparse
import json
from pathlib import Path
import struct
from native_debug_probe import Peer
from debugger_acceptance import symbols

ESCAPES = ["/../escape-1.txt", "/a/../../escape-2.txt", "/..\\escape-3.txt",
           "//../escape-4.txt", "/../../../../escape-5.txt", "/../sdmc-sibling/escape-6.txt"]


def u32(peer, address):
    return struct.unpack("<I", bytes(peer.debug("read_memory", address=address, length=4)["bytes"]))[0]


def guest_write(peer, syms, path, payload):
    peer.debug("write_memory", address=syms["path"], bytes=list(path.encode() + b"\0"))
    peer.debug("write_memory", address=syms["payload"], bytes=list(payload))
    peer.debug("write_memory", address=syms["payload_size"], bytes=list(struct.pack("<I", len(payload))))
    request = u32(peer, syms["request"]) + 1
    peer.debug("write_memory", address=syms["request"], bytes=list(struct.pack("<I", request)))
    for _ in range(60):
        peer.call("run_until", {"frames": 1})
        if u32(peer, syms["done"]) == request:
            return dict(open_result=u32(peer, syms["open_result"]), write_result=u32(peer, syms["write_result"]),
                        written=u32(peer, syms["written"]))
    raise AssertionError(("guest write did not complete", path))


def boot(peer, rom, syms, new):
    peer.call("hello")
    peer.debug("settings_set", name="is_new_3ds", value=new)
    peer.call("load", {"path": str(rom)})
    for _ in range(120):
        peer.call("run_until", {"frames": 1})
        assert not u32(peer, syms["failure"]), hex(u32(peer, syms["failure"]))
        if u32(peer, syms["stage"]) == 2:
            return
    raise AssertionError("fs:USER not initialized")


def escaped(root):
    """Escape-named files at or above the session root, excluding its sdmc tree."""
    found = [p for p in root.parent.rglob("escape-*") if root / "sdmc" not in p.parents]
    for ancestor in list(root.parents)[1:4]:
        found += list(ancestor.glob("escape-*"))
    return [str(p) for p in found]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    syms = symbols(args.rom.with_suffix(".elf"))
    for new in (False, True):
        root = args.output.resolve() / ("new" if new else "old")
        peer = Peer(args.worker.resolve(), root)
        try:
            boot(peer, args.rom.resolve(), syms, new)
            payload = b"lotus-owned-fs:" + (b"new" if new else b"old")
            ok = guest_write(peer, syms, "/lotus-fs.txt", payload)
            landed = [p for p in root.rglob("lotus-fs.txt")]
            assert ok["open_result"] == 0 and ok["written"] == len(payload), ok
            assert len(landed) == 1 and landed[0].read_bytes() == payload, landed
            assert (root / "sdmc") in landed[0].parents, landed
            escapes = {path: guest_write(peer, syms, path, b"escape") for path in ESCAPES}
            leaked = escaped(root)
            refused = {path: r["open_result"] != 0 for path, r in escapes.items()}
            report = dict(new_3ds=new, plain=dict(result=ok, file=str(landed[0].relative_to(root))),
                          escapes={p: dict(r, open_result=hex(r["open_result"])) for p, r in escapes.items()},
                          refused=refused, leaked=leaked)
            (root / "acceptance.json").write_text(json.dumps(report, indent=2))
            print(json.dumps(dict(new_3ds=new, refused=refused, leaked=leaked)), flush=True)
            assert not leaked, leaked
            assert all(refused.values()), refused
        finally:
            peer.close()
