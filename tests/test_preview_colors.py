"""Compare rendered Windows preview colors with native BGRA capture.

Usage: test_preview_colors.py <desktop> <native worker> <colored 3dsx> <output>
Requires Pillow and a visible Windows desktop. Runs software and Vulkan separately.
"""
import base64
from collections import Counter
import ctypes
from ctypes.wintypes import RECT
import json
import os
from pathlib import Path
import sys

from PIL import Image, ImageGrab

from mcp_client import Client
from test_focus_cycle import foreground, user32, wait_state, window_of


def screenshot(hwnd):
    foreground(hwnd)
    wait_state("Lotus foreground for pixel check", lambda: user32.GetForegroundWindow() == hwnd)
    bounds = RECT()
    assert ctypes.windll.dwmapi.DwmGetWindowAttribute(
        ctypes.c_void_p(hwnd), 9, ctypes.byref(bounds), ctypes.sizeof(bounds)
    ) == 0
    ctypes.windll.dwmapi.DwmFlush()
    return ImageGrab.grab(bbox=(bounds.left, bounds.top, bounds.right, bounds.bottom),
                          all_screens=True).convert("RGB")


def palettes(client, session):
    links = client.tool("lotus_capture", {"session": session})["content"]
    assert len(links) == 2
    result = []
    for width, link in zip((400, 320), links):
        resource, = client.send("resources/read", {"uri": link["uri"]})["contents"]
        raw = base64.b64decode(resource["blob"], validate=True)
        assert len(raw) == width * 240 * 4
        pixels = Image.frombytes("RGB", (width, 240), raw, "raw", "BGRX")
        counts = Counter(pixels.get_flattened_data())
        samples = {rgb: count for rgb, count in counts.most_common(20)
                   if abs(rgb[0] - rgb[2]) >= 6 and count >= 500
                   and counts[(rgb[2], rgb[1], rgb[0])] * 4 < count}
        assert samples, "fixture must supply stable asymmetric red/blue colors on both screens"
        result.append(samples)
    return result


def compare(picture, reference, label):
    counts = Counter(picture.get_flattened_data())
    for screen, samples in zip(("top", "bottom"), reference):
        matches = []
        for (red, green, blue), native in samples.items():
            expected = counts[(red, green, blue)]
            swapped = counts[(blue, green, red)]
            if expected >= max(30, native // 20) and expected >= 4 * swapped:
                matches.append((red, green, blue, expected, swapped))
        assert matches, (label, screen, [(rgb, counts[rgb]) for rgb in samples])
        print(f"{label} {screen}: RGB swatch and pixel counts {matches[0]}")


def run(desktop, worker, content, root, renderer):
    root.mkdir()
    os.environ["LOTUS_DESKTOP_PREFERENCES"] = str(root / "desktop.json")
    config = root / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(root / "storage"),
        "allow_control": True, "max_sessions": 2, "assets": {"owned": str(content)},
        "prefer_vulkan": renderer == "vulkan",
    }))
    client = Client(desktop, config, root / "mcp.jsonl", arguments=["--mcp-stdio"],
                    stderr_path=root / "desktop.stderr")
    try:
        def workspace(op, revision=None, **kwargs):
            request = {"operation": {"op": op, **kwargs}}
            if revision is not None:
                request["expected_revision"] = revision
            return client.tool("lotus_workspace", request)["workspace"]

        def rendered(revision, ids):
            def check():
                status = client.tool("lotus_status", {})
                ui = status["frontend_metrics"]["ui"].get("rendered_workspace") or {}
                tiles = ui.get("tiles", [])
                return (ui.get("revision") == revision
                        and [tile["session"] for tile in tiles] == ids
                        and all(tile["preview"] and
                                tile["preview"]["epoch"] == tile["summary_epoch"] for tile in tiles))
            wait_state("workspace shows every expected preview", check)

        first = client.tool("lotus_create_session", {
            "kind": "primary", "asset": "owned", "card": "colors",
        })["session"]
        client.tool("lotus_run_until", {"session": first, "frames": 60})
        single = workspace("single", session=first)
        rendered(single["revision"], [first])
        reference = palettes(client, first)
        hwnd = window_of(client.process.pid)
        assert hwnd
        single_image = screenshot(hwnd)
        single_image.save(root / "single.png")
        compare(single_image, reference, f"{renderer} single")

        second = client.tool("lotus_create_session", {
            "kind": "secondary", "asset": "owned", "card": "colors",
        })["session"]
        client.tool("lotus_run_until", {"session": second, "frames": 60})
        sessions = client.tool("lotus_list_sessions", {})
        assert all(row["telemetry"]["renderer"].casefold() == renderer for row in sessions), sessions
        tiled = workspace("tile", single["revision"], first=first, second=second, axis="horizontal")
        rendered(tiled["revision"], [first, second])
        split_image = screenshot(hwnd)
        split_image.save(root / "split.png")
        middle = split_image.width // 2
        for index, (left, right, expected) in enumerate(
            [(0, middle, reference), (middle, split_image.width, palettes(client, second))]
        ):
            compare(split_image.crop((left, 0, right, split_image.height)), expected,
                    f"{renderer} split {index + 1}")
    finally:
        if hwnd := window_of(client.process.pid):
            user32.PostMessageW(hwnd, 0x10, 0, 0)
        client.close()


if __name__ == "__main__":
    desktop, worker, content, output = (Path(value).resolve() for value in sys.argv[1:5])
    output.mkdir(parents=True, exist_ok=False)
    for renderer in ("software", "vulkan"):
        run(desktop, worker, content, output / renderer, renderer)
