"""Owned guest reads every scripted peripheral value back through its services.

Covers HID buttons/circle pad/touch/accelerometer/gyroscope, IR ZL/ZR and
C-stick, 3D and volume sliders, headphone, fixed battery state, a private BMP
camera frame and a synthetic plain NFC tag. Home/Power reach APT only when
HOME Menu is registered (applet_manager.cpp), so they are checked as accepted
input state only. All assets are generated here; nothing is bundled.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
from native_debug_probe import Peer
from debugger_acceptance import symbols

HID_MEMORY, IR_MEMORY = 0x10000000, 0x10002000
WIDTH, HEIGHT = 160, 120


def bmp(path):
    """24-bit bottom-up BMP with a deterministic gradient (owned test data)."""
    width, height = 64, 48
    stride = (width * 3 + 3) & ~3
    rows = []
    for y in range(height - 1, -1, -1):
        row = bytearray()
        for x in range(width):
            row += bytes(((x * 4) & 255, (y * 5) & 255, ((x + y) * 3) & 255))  # B, G, R
        rows.append(bytes(row) + bytes(stride - width * 3))
    pixels = b"".join(rows)
    header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, len(pixels), 2835, 2835, 0, 0)
    path.write_bytes(header + info + pixels)
    return width, height


def expected_camera(width, height):
    out = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            sx, sy = x * width // WIDTH, y * height // HEIGHT
            r, g, b = ((sx + sy) * 3) & 255, (sy * 5) & 255, (sx * 4) & 255
            out.append(((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3))
    return struct.pack(f"<{len(out)}H", *out)


def amiibo(path):
    """Synthetic decoded NTAG215File (nfc_types.h, packed, 0x21C bytes) that
    satisfies native IsAmiiboValid; no keys or retail data involved."""
    data = bytearray(0x21C)
    uid = [0x04, 0x11, 0x22, 0, 0x33, 0x44, 0x55]
    uid[3] = 0x88 ^ uid[0] ^ uid[1] ^ uid[2]
    nintendo_id = 0x48
    data[0] = uid[4] ^ uid[5] ^ uid[6] ^ nintendo_id      # lock_bytes[0]
    struct.pack_into("<HI", data, 2, 0xE00F, 0xEEFF10F1)  # static lock, capability container
    data[0x28] = 0xA5                                     # constant value
    data[0x1D4:0x1DB] = bytes(uid)                        # uid
    data[0x1DB] = nintendo_id
    data[0x1DC + 7] = 2                                   # model_info.tag_type = Type2
    struct.pack_into("<III", data, 0x208, 0x0F0001, 0x04000000, 0x5F)  # dynamic lock, CFG0, CFG1
    path.write_bytes(bytes(data))
    return bytes(uid)


def read(peer, address, length):
    out = b""
    while len(out) < length:
        chunk = min(4096, length - len(out))
        out += bytes(peer.debug("read_memory", address=address + len(out), length=chunk)["bytes"])
    return out


def run_to_stage(peer, addresses, target, limit=240):
    for _ in range(limit):
        peer.call("run_until", {"frames": 1})
        stage, = struct.unpack("<I", read(peer, addresses["stage"], 4))
        failure, = struct.unpack("<I", read(peer, addresses["failure"], 4))
        assert not failure, ("guest failure", hex(failure), read(peer, addresses["failed_stage"], 4))
        if stage >= target:
            return stage
    raise AssertionError(f"stage {target} not reached")


parser = argparse.ArgumentParser(__doc__)
parser.add_argument("worker", type=Path)
parser.add_argument("rom", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
addresses = symbols(args.rom.with_suffix(".elf"))
state = dict(buttons=(1 << 0) | (1 << 10) | (1 << 14) | (1 << 15) | (1 << 16),  # A, Start, ZL, ZR, Home
             x=100, y=60, touch=True, circle_pad=[0.5, -0.25], c_stick=[0.0, 1.0],
             acceleration=[0.25, -0.5, 1.0], gyroscope=[10.0, -20.0, 30.0], slider_3d=0.75, volume=0.5)
reports = []
for new in (False, True):
    peer = Peer(args.worker.resolve(), args.output.resolve() / ("new" if new else "old"))
    try:
        peer.call("hello")
        media = peer.root / "media"; media.mkdir()
        width, height = bmp(media / "camera.bmp")
        uid = amiibo(media / "nfc.bin")
        peer.debug("settings_set", name="is_new_3ds", value=new)
        for name, value in (("battery_state_source", 1), ("battery_level", 2), ("battery_charging", True),
                            ("simulate_headphones_plugged", True)):
            peer.debug("settings_set", name=name, value=value)
        peer.debug("asset_mount", kind="camera_bmp", asset="owned-generated-gradient")
        peer.call("load", {"path": str(args.rom.resolve())})
        echoed = peer.debug("input_state", owner="scenario", state=state)
        run_to_stage(peer, addresses, 7)
        results = struct.unpack("<32I", read(peer, addresses["results"], 128))
        hid = read(peer, HID_MEMORY, 0x238)
        ir = read(peer, IR_MEMORY, 0x98)
        pad_index, = struct.unpack_from("<I", hid, 0x10)
        slider, current = struct.unpack_from("<fI", hid, 0x18)
        pad_state, _, _, cx, cy = struct.unpack_from("<IIIhh", hid, 0x28 + 16 * pad_index)
        touch_index, = struct.unpack_from("<I", hid, 0xB8)
        tx, ty, valid = struct.unpack_from("<HHI", hid, 0xC8 + 8 * touch_index)
        accel_index, = struct.unpack_from("<I", hid, 0x118)
        accel = struct.unpack_from("<hhh", hid, 0x128 + 6 * accel_index)
        gyro_index, = struct.unpack_from("<I", hid, 0x168)
        gyro = struct.unpack_from("<hhh", hid, 0x178 + 6 * gyro_index)
        ir_index, = struct.unpack_from("<I", ir, 0x10)
        ir_state, _, _, csx, csy = struct.unpack_from("<IIIhh", ir, 0x18 + 16 * ir_index)
        camera = read(peer, addresses["camera_buffer"], WIDTH * HEIGHT * 2)
        checks = {
            "buttons_a_start": pad_state & 0x9 == 0x9,
            "circle_pad": cx > 0 and cy < 0,
            "touch": (tx, ty, valid & 0x7F) == (100, 60, 1),
            "accelerometer_nonzero": any(accel),
            "gyroscope_nonzero": any(gyro),
            "zl_zr": ir_state & 0xC000 == 0xC000,
            "c_stick": csx == 0 and csy == 0x9C,
            "slider_hid": abs(slider - 0.75) < 1e-6,
            "volume": results[0] == int(0x3F * 0.5),
            "adapter": results[1] == 1,
            "battery_level": results[2] == 3,
            "charging": results[3] == 1,
            "headphones": results[4] == 1,
            "camera_event_signalled": results[6] == 0,
            "camera_frame": camera == expected_camera(width, height),
            "home_accepted": echoed["input"]["buttons"] & (1 << 16) != 0,
        }
        peer.debug("asset_mount", kind="nfc", asset="owned-synthetic-plain-tag")
        run_to_stage(peer, addresses, 8)
        results = struct.unpack("<32I", read(peer, addresses["results"], 128))
        info = struct.pack("<III", results[8], results[9], results[10])
        checks["slider_shared_page"] = abs(struct.unpack("<f", struct.pack("<I", results[5]))[0] - 0.75) < 1e-6
        checks["nfc_in_range"] = results[7] == 3
        checks["nfc_uid"] = info[4:11] == uid
        report = dict(new_3ds=new, checks=checks, accel=accel, gyro=gyro, circle=(cx, cy),
                      camera_sha256=hashlib.sha256(camera).hexdigest(),
                      nfc_info=info.hex(), raw_results=results[:11])
        (peer.root / "acceptance.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report), flush=True)
        reports.append(report)
        failed = [name for name, ok in checks.items() if not ok]
        assert not failed, failed
    finally:
        peer.close()
