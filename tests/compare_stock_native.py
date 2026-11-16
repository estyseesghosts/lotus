"""Compare stock oracle RGBA with a stopped native --boot frame ring (slot 0)."""
import hashlib
from pathlib import Path
import struct
import sys


def exact_read(path, size):
    with path.open("rb") as source:
        data = source.read(size + 1)
    assert len(data) == size, f"unexpected byte count: {path}"
    return data


def main():
    stock, native = map(Path, sys.argv[1:])
    for screen, width in (("top", 400), ("bottom", 320)):
        size = width * 240 * 4
        expected = exact_read(stock / f"{screen}.rgba", size)
        slot = exact_read(native / "frames" / f"{screen}-0.bgra", size + 32)
        magic, version, epoch, sequence, actual_width, height = struct.unpack(
            "<4sIQQII", slot[:32]
        )
        assert (magic, version, actual_width, height) == (b"LTRF", 1, width, 240)
        assert epoch > 0 and sequence > 0
        rgba = bytearray(slot[32:])
        rgba[0::4], rgba[2::4] = rgba[2::4], rgba[0::4]
        assert rgba == expected, f"{screen}: native pixels differ from stock"
        print(f"{screen}: identical RGBA SHA-256 {hashlib.sha256(expected).hexdigest()}")


if __name__ == "__main__":
    main()
