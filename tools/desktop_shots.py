"""Screenshot the LOTUS desktop at chosen sizes and states (Windows).

python tools/desktop_shots.py <lotus-desktop> <host.json> <asset-handle> <out-dir> [scenario...]

Launches `lotus-desktop --mcp`, creates and runs a session through MCP, then for
each scenario writes preferences, resizes the window to a *logical* client size
(physical = logical x DPI/96), posts mouse clicks or drags to the LOTUS window
handle only (PostMessage; GPUI reads the button from wParam, gpui_windows
`events.rs` handle_mouse_move_msg) and captures only that window with
PrintWindow(PW_RENDERFULLCONTENT), so other windows are never read or clicked.
Saves `<out>/<scenario>.png` plus `<out>/report.json` (client size, DPI, preference
file after the scenario). GPUI lays out in logical pixels, so a logical 640 x 440
client is what a 1280 x 880 window gives at 200% display scale.

Preferences are isolated through LOTUS_DESKTOP_PREFERENCES; scenarios that set
`prefs` restart the window so the state is read at startup like a real launch.
No keyboard input is synthesised (GPUI reads modifiers with GetKeyState, so a
posted Ctrl would not work, and SendInput would reach whatever window is in front).
"""
import ctypes
import ctypes.wintypes as wt
import json
import os
import pathlib
import queue
import subprocess
import sys
import threading
import time


ctypes.windll.shcore.SetProcessDpiAwareness(2)
user32 = ctypes.windll.user32
EnumProc = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)

DEFAULT_PREFS = {"sessions_open": True, "sessions_width": 264.0,
                 "inspector_open": False, "inspector_width": 420.0}

# name: (logical width, height, preferences, actions[, extra sessions])
# extra sessions: lotus_create_session arguments run (and left running) before the
# main one, for crowded sidebars; their assets must exist in host.json.
# actions: ("drag", x0, x1) logical client x at mid height
#          | ("click", x, y) logical, negative = from right/bottom
# Inspector rail button centres (logical y), from the rail layout in lotus_window.rs:
# 52 px header, 8 px padding, 32 px buttons, 4 px gaps, 9 px group separators.
RAIL = {"cpu": 76, "breakpoints": 112, "threads": 148, "memory": 197, "gpu": 246,
        "logs": 295, "snapshots": 344, "more": 393}
rail = lambda name: ("click", -22, RAIL[name])
LONG_CARD = "an-unusually-long-card-name-0001"  # 32 characters, the card-name maximum
LONG_ASSET = "pokemon-heartgold-randomizer-nuzlocke-build-0042-debug"
LONG_SESSIONS = [
    {"kind": "primary", "card": LONG_CARD, "asset": LONG_ASSET},
    {"kind": "secondary", "card": LONG_CARD, "asset": LONG_ASSET},
    {"kind": "secondary", "asset": LONG_ASSET},
]
SCENARIOS = {
    "wide-default": (1280, 880, {}, []),
    "wide-inspector": (1280, 880, {"inspector_open": True}, [rail("cpu")]),
    "both-1000-memory": (1000, 720, {"inspector_open": True}, [rail("memory")]),
    "both-900": (900, 700, {"inspector_open": True}, [rail("breakpoints")]),
    "min-both-squeezed": (720, 560, {"inspector_open": True}, [rail("cpu")]),
    "min-inspector-only": (720, 560, {"inspector_open": True, "sessions_open": False}, [rail("gpu")]),
    "min-sessions-only": (720, 560, {}, []),
    "compact-gauges-expanded": (900, 640, {"sessions_open": False}, [("click", -44 - 20, -22)]),
    "toolbar-overflow-menu": (720, 600, {"inspector_open": True}, [("click", -44 - 300 - 30, 26)]),
    "inspector-utility-menu": (1280, 880, {"inspector_open": True}, [("click", -44 - 22, 26)]),
    "inspector-more-menu": (1280, 880, {}, [rail("more")]),
    "scale125-1280x880": (1024, 704, {"inspector_open": True}, []),
    "scale150-1280x880": (853, 587, {"inspector_open": True}, []),
    "scale200-1440x1120": (720, 560, {"inspector_open": True}, []),
    "drag-sidebar-snap": (1280, 880, {}, [("drag", 263, 60)]),
    "drag-sidebar-reopen": (1280, 880, {"sessions_open": False}, [("drag", 49, 330)]),
    "drag-inspector-wider": (1280, 880, {"inspector_open": True}, [("drag", 1280 - 44 - 420 + 2, 1280 - 44 - 600)]),
    "drag-inspector-snap": (1280, 880, {"inspector_open": True}, [("drag", 1280 - 44 - 420 + 2, 1280 - 44 - 100)]),
    "drag-inspector-open": (1280, 880, {}, [("drag", 1280 - 44 + 2, 1280 - 44 - 360)]),
    "multi-long-labels": (900, 700, {"inspector_open": True}, [rail("memory")], LONG_SESSIONS),
    "multi-long-labels-min": (720, 560, {"inspector_open": True}, [], LONG_SESSIONS),
    "multi-long-labels-sidebar": (1280, 880, {}, [], LONG_SESSIONS),
}

WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON = 0x200, 0x201, 0x202, 0x1


def post_mouse(hwnd, message, x, y, buttons):
    user32.PostMessageW(hwnd, message, buttons, (int(y) << 16) | (int(x) & 0xFFFF))


def capture(hwnd, path):
    _, _, width, height = client_rect(hwnd)
    gdi32 = ctypes.windll.gdi32
    window_dc = user32.GetDC(hwnd)
    memory_dc = gdi32.CreateCompatibleDC(window_dc)
    bitmap = gdi32.CreateCompatibleBitmap(window_dc, width, height)
    gdi32.SelectObject(memory_dc, bitmap)
    # PW_CLIENTONLY | PW_RENDERFULLCONTENT: this window's own pixels, even if covered.
    assert user32.PrintWindow(hwnd, memory_dc, 0x1 | 0x2), "PrintWindow failed"
    header = (ctypes.c_uint32 * 10)(40, width, (-height) & 0xFFFFFFFF, 1 | (32 << 16), 0, 0, 0, 0, 0, 0)
    buffer = ctypes.create_string_buffer(width * height * 4)
    gdi32.GetDIBits(memory_dc, bitmap, 0, height, buffer, header, 0)
    from PIL import Image  # lazy: headless callers (CI smoke) have no Pillow
    Image.frombuffer("RGBA", (width, height), buffer, "raw", "BGRA", 0, 1).convert("RGB").save(path)
    gdi32.DeleteObject(bitmap)
    gdi32.DeleteDC(memory_dc)
    user32.ReleaseDC(hwnd, window_dc)


def window_of(pid):
    found = []

    def visit(hwnd, _):
        owner = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        title = ctypes.create_unicode_buffer(64)
        user32.GetWindowTextW(hwnd, title, 64)
        if owner.value == pid and title.value == "LOTUS" and user32.IsWindowVisible(hwnd):
            found.append(hwnd)
        return True

    user32.EnumWindows(EnumProc(visit), 0)
    return found[0] if found else None


def client_rect(hwnd):
    rect = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(rect))
    origin = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(origin))
    return origin.x, origin.y, rect.right, rect.bottom


def resize(hwnd, width, height):
    scale = user32.GetDpiForWindow(hwnd) / 96
    outer, inner = wt.RECT(), wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(outer))
    user32.GetClientRect(hwnd, ctypes.byref(inner))
    extra_w = (outer.right - outer.left) - inner.right
    extra_h = (outer.bottom - outer.top) - inner.bottom
    user32.SetWindowPos(hwnd, 0, 40, 20, round(width * scale) + extra_w,
                        round(height * scale) + extra_h, 0x0004 | 0x0010)  # NOZORDER|NOACTIVATE
    return scale


class Desktop:
    def __init__(self, exe, config, prefs_path):
        env = dict(os.environ, LOTUS_DESKTOP_PREFERENCES=str(prefs_path))
        self.process = subprocess.Popen([exe, "--mcp", config], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, encoding="utf-8", env=env)
        self.messages = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.request = 0
        self.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                 "clientInfo": {"name": "lotus-desktop-shots", "version": "1"}})
        self.call("notifications/initialized", notify=True)
        deadline = time.monotonic() + 30
        while (hwnd := window_of(self.process.pid)) is None:
            assert time.monotonic() < deadline, "no LOTUS window"
            time.sleep(0.2)
        self.hwnd = hwnd

    def _read(self):
        for line in self.process.stdout:
            self.messages.put(json.loads(line))
        self.messages.put(None)

    def call(self, method, params=None, notify=False):
        message = {"jsonrpc": "2.0", "method": method}
        if not notify:
            self.request += 1
            message["id"] = self.request
        if params is not None:
            message["params"] = params
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        if notify:
            return None
        while True:
            reply = self.messages.get(timeout=35)
            assert reply is not None, "window exited"
            if reply.get("id") == self.request:
                assert "error" not in reply, reply
                return reply["result"]

    def tool(self, name, arguments):
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), result
        return json.loads(result["content"][0]["text"])

    def close(self):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=8)
        except Exception:
            self.process.kill()


def act(hwnd, scale, action):
    _, _, width, height = client_rect(hwnd)
    to_client = lambda x, y: (round((x if x >= 0 else width / scale + x) * scale),
                              round((y if y >= 0 else height / scale + y) * scale))
    kind = action[0]
    if kind == "click":
        x, y = to_client(action[1], action[2])
        post_mouse(hwnd, WM_MOUSEMOVE, x, y, 0)
        time.sleep(0.15)
        post_mouse(hwnd, WM_LBUTTONDOWN, x, y, MK_LBUTTON)
        post_mouse(hwnd, WM_LBUTTONUP, x, y, 0)
    elif kind == "drag":
        y = height / scale / 2
        x0, y0 = to_client(action[1], y)
        x1, _ = to_client(action[2], y)
        post_mouse(hwnd, WM_MOUSEMOVE, x0, y0, 0)
        time.sleep(0.2)
        post_mouse(hwnd, WM_LBUTTONDOWN, x0, y0, MK_LBUTTON)
        steps = 24
        for step in range(1, steps + 1):
            time.sleep(0.02)
            post_mouse(hwnd, WM_MOUSEMOVE, round(x0 + (x1 - x0) * step / steps), y0, MK_LBUTTON)
        time.sleep(0.2)
        post_mouse(hwnd, WM_LBUTTONUP, x1, y0, 0)
    time.sleep(0.6)


def main():
    exe, config, asset, out = sys.argv[1:5]
    exe = str(pathlib.Path(exe).resolve())
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    names = sys.argv[5:] or list(SCENARIOS)
    prefs_path = out / "desktop.json"
    report = {}
    for name in names:
        width, height, prefs, actions, *extra = SCENARIOS[name]
        prefs_path.write_text(json.dumps({**DEFAULT_PREFS, **prefs}))
        desktop = Desktop(exe, config, prefs_path)
        try:
            for arguments in (extra[0] if extra else []):
                other = desktop.tool("lotus_create_session", arguments)["session"]
                desktop.tool("lotus_control", {"session": other, "operation": "run"})
            session = desktop.tool("lotus_create_session", {"kind": "secondary", "asset": asset})["session"]
            desktop.tool("lotus_control", {"session": session, "operation": "run"})
            desktop.tool("lotus_focus_session", {"session": session})
            scale = resize(desktop.hwnd, width, height)
            time.sleep(2.5)
            for action in actions:
                act(desktop.hwnd, scale, action)
            # Move the (posted) pointer off every control so no tooltip covers the shot.
            post_mouse(desktop.hwnd, WM_MOUSEMOVE, 300, 300, 0)
            time.sleep(0.8)
            _, _, w, h = client_rect(desktop.hwnd)
            capture(desktop.hwnd, out / f"{name}.png")
            desktop.tool("lotus_control", {"session": session, "operation": "stop"})
        finally:
            desktop.close()
        report[name] = {"logical": [width, height], "client_px": [w, h], "dpi_scale": scale,
                        "actions": actions,
                        "preferences_after": json.loads(prefs_path.read_text())}
        print(name, report[name]["client_px"], report[name]["preferences_after"], flush=True)
    (out / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
