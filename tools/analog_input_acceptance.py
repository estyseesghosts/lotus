"""lotus#60: analog sticks through the normal fast `input` call, two sessions at once.

Runs two workers of one build side by side on the owned peripherals fixture and drives
their Circle Pads and C-sticks in opposite directions with `input` (not the debugger's
`input_state`). The guest-visible result is read back from HID and IR shared memory (layout:
tools/peripheral_acceptance.py, native service/hid/hid.cpp and service/ir/ir_rst.cpp):
the pad entry's circle x/y and direction bits, and the IR entry's C-stick x/y. Also checks
that `features` lists `analog_input`, that out-of-range sticks are refused, and that a
button-only `input` keeps both the sticks and the accelerometer set earlier by `input_state`.
Usage: analog_input_acceptance.py <worker> <peripherals.3dsx> <output>
"""
import argparse
import json
from pathlib import Path
import struct
from native_debug_probe import Peer
from debugger_acceptance import symbols

HID_MEMORY, IR_MEMORY = 0x10000000, 0x10002000
# HID PadState bits (native service/hid/hid.h): circle right/left/up/down.
CIRCLE_BITS = {"right": 1 << 28, "left": 1 << 29, "up": 1 << 30, "down": 1 << 31}
DIRECTIONS = {"neutral": [0.0, 0.0], "left": [-1.0, 0.0], "right": [1.0, 0.0],
              "up": [0.0, 1.0], "down": [0.0, -1.0]}
OPPOSITE = {"neutral": "neutral", "left": "right", "right": "left", "up": "down", "down": "up"}


def bmp(path):
    """Minimal 24-bit BMP so the fixture's camera stage has a source (owned test data)."""
    width, height = 8, 8
    pixels = bytes(width * height * 3)
    header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, len(pixels), 2835, 2835, 0, 0)
    path.write_bytes(header + info + pixels)


def read(peer, address, length):
    return bytes(peer.debug("read_memory", address=address, length=length)["bytes"])


def observe(peer):
    hid = read(peer, HID_MEMORY, 0x238)
    ir = read(peer, IR_MEMORY, 0x98)
    pad_index, = struct.unpack_from("<I", hid, 0x10)
    pad, _, _, cx, cy = struct.unpack_from("<IIIhh", hid, 0x28 + 16 * pad_index)
    accel_index, = struct.unpack_from("<I", hid, 0x118)
    accel = struct.unpack_from("<hhh", hid, 0x128 + 6 * accel_index)
    ir_index, = struct.unpack_from("<I", ir, 0x10)
    _, _, _, csx, csy = struct.unpack_from("<IIIhh", ir, 0x18 + 16 * ir_index)
    return dict(pad=pad, circle=(cx, cy), c_stick=(csx, csy), accel=accel)


def expect(seen, direction):
    """Signs of circle and C-stick match the direction; circle direction bits agree."""
    x, y = DIRECTIONS[direction]
    sign = lambda v: (v > 0) - (v < 0)
    bits = {name for name, bit in CIRCLE_BITS.items() if seen["pad"] & bit}
    return (tuple(map(sign, seen["circle"])) == (sign(x), sign(y))
            and tuple(map(sign, seen["c_stick"])) == (sign(x), sign(y))
            and bits == (set() if direction == "neutral" else {direction}))


parser = argparse.ArgumentParser(__doc__)
parser.add_argument("worker", type=Path)
parser.add_argument("rom", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
addresses = symbols(args.rom.with_suffix(".elf"))
peers = []
try:
    for name in ("a", "b"):
        peer = Peer(args.worker.resolve(), args.output.resolve() / name)
        peers.append(peer)
        hello = peer.call("hello")
        assert "analog_input" in hello["features"], hello["features"]
        media = peer.root / "media"; media.mkdir()
        bmp(media / "camera.bmp")
        peer.debug("asset_mount", kind="camera_bmp", asset="owned-generated-blank")
        peer.call("load", {"path": str(args.rom.resolve())})
        # Motion via the debugger; the fast calls below must not reset it.
        peer.debug("input_state", owner="live", state=dict(
            buttons=0, x=0, y=0, touch=False, circle_pad=[0, 0], c_stick=[0, 0],
            acceleration=[0.25, -0.5, 1.0], gyroscope=[0, 0, 0], slider_3d=0, volume=1))
    for peer in peers:
        for _ in range(240):
            peer.call("run_until", {"frames": 1})
            stage, failure = struct.unpack("<II", read(peer, addresses["stage"], 8))
            assert not failure, hex(failure)
            if stage >= 7:
                break
        assert stage >= 7, stage
    results = []
    for direction in DIRECTIONS:
        pair = (direction, OPPOSITE[direction])
        for peer, chosen in zip(peers, pair):
            stick = DIRECTIONS[chosen]
            peer.call("input", dict(owner="live", buttons=0, x=0, y=0, touch=False,
                                    circle_pad=stick, c_stick=stick))
        for peer in peers:
            peer.call("run_until", {"frames": 2})
        seen = [observe(peer) for peer in peers]
        ok = all(expect(s, d) for s, d in zip(seen, pair)) and all(any(s["accel"]) for s in seen)
        results.append(dict(directions=pair, seen=seen, ok=ok))
        print(json.dumps(results[-1]), flush=True)
    # A button-only update from an older host leaves the sticks where they are.
    a = peers[0]
    a.call("input", dict(owner="live", buttons=0, x=0, y=0, touch=False,
                         circle_pad=[0.0, 1.0], c_stick=[0.0, 1.0]))
    a.call("input", dict(owner="live", buttons=1, x=0, y=0, touch=False))
    a.call("run_until", {"frames": 2})
    kept = observe(a)
    results.append(dict(directions=("buttons only after up", "-"), seen=[kept],
                        ok=expect(kept, "up") and kept["pad"] & 1 == 1 and any(kept["accel"])))
    print(json.dumps(results[-1]), flush=True)
    for bad in ([1.5, 0.0], [0.0], ["x", 0.0]):
        a.call("input", dict(owner="live", buttons=0, x=0, y=0, touch=False, circle_pad=bad),
               expected="invalid_argument")
    report = dict(worker=str(args.worker), results=results,
                  out_of_range_refused=True, passed=all(r["ok"] for r in results))
    (args.output / "analog-input.json").write_text(json.dumps(report, indent=2))
    assert report["passed"], [r for r in results if not r["ok"]]
    print("PASS", args.worker)
finally:
    for peer in peers:
        peer.close()
