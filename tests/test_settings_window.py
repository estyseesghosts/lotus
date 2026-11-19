"""lotus#53 Settings window check for the Windows desktop.

Runs `lotus-desktop --mcp`, then with real key events: Ctrl+, three times must leave exactly
one visible "LOTUS Settings" window; Esc closes it; Ctrl+, opens it again; closing the main
window (WM_CLOSE) must close Settings with it. Each close saves the Settings bounds to
desktop.json (here <output dir>/desktop.json, via LOTUS_DESKTOP_PREFERENCES), and reopening
at saved bounds must not move the window. The process is killed afterwards: with `--mcp`
it keeps running after its window closes, with or without Settings.

Moves real focus on this desktop for a few seconds.

Usage: test_settings_window.py <lotus-desktop.exe> <worker.exe> <output dir>
"""
import ctypes
import ctypes.wintypes as wt
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import mcp_client  # noqa: E402

user32 = ctypes.windll.user32
SETTINGS = "LOTUS Settings"


def main():
    desktop, worker, output = sys.argv[1:4]
    output = Path(output)
    output.mkdir(parents=True)
    config = output / "host.json"
    config.write_text(json.dumps({"worker": worker, "storage_root": str(output / "storage"),
                                  "allow_control": True}))
    preferences = output / "desktop.json"
    os.environ["LOTUS_DESKTOP_PREFERENCES"] = str(preferences)
    popen = subprocess.Popen
    subprocess.Popen = lambda args, **kw: popen([args[0], "--mcp", *args[1:]], **kw)
    try:
        client = mcp_client.Client(desktop, config, output / "transcript.jsonl")
    finally:
        subprocess.Popen = popen
    try:
        run(client, preferences)
    finally:
        if client.process.poll() is None:
            client.process.kill()


def windows(pid):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def each(hwnd, _):
        owner = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            title = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(hwnd, title, 256)
            found.append((hwnd, title.value))
        return True
    user32.EnumWindows(each, 0)
    return found


def settings(pid):
    return [hwnd for hwnd, title in windows(pid) if title == SETTINGS]


def key(vk, up=False):
    user32.keybd_event(vk, 0, 2 if up else 0, 0)


def foreground(hwnd):
    key(0x12); user32.SetForegroundWindow(hwnd); key(0x12, True)
    time.sleep(0.3)


def ctrl_comma():
    key(0x11); key(0xBC); key(0xBC, True); key(0x11, True)


def until(what, check, seconds=5):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if check():
            return
        time.sleep(0.1)
    raise AssertionError(what)


def saved_bounds(preferences):
    try:
        return json.loads(preferences.read_text()).get("settings_bounds")
    except FileNotFoundError:
        return None


def run(client, preferences):
    pid = client.process.pid
    until("main window", lambda: any(t != SETTINGS for _, t in windows(pid)), 10)
    main_hwnd = next(h for h, t in windows(pid) if t != SETTINGS)
    for attempt in range(3):
        foreground(main_hwnd)
        ctrl_comma()
        until(f"Settings after Ctrl+, #{attempt + 1}", lambda: settings(pid))
        time.sleep(0.5)
        assert len(settings(pid)) == 1, f"{len(settings(pid))} Settings windows"
    print("Ctrl+, x3: one Settings window")
    foreground(settings(pid)[0])
    key(0x1B); key(0x1B, True)
    until("Esc closes Settings", lambda: not settings(pid))
    until("Esc saves bounds", lambda: saved_bounds(preferences))
    first = saved_bounds(preferences)
    print("Esc: closed, bounds", first)
    preferences.unlink()
    foreground(main_hwnd)
    ctrl_comma()
    until("reopened", lambda: len(settings(pid)) == 1)
    print("Ctrl+, again: reopened")
    user32.PostMessageW(main_hwnd, 0x10, 0, 0)
    until("main window close takes Settings with it", lambda: not windows(pid), 10)
    until("main window close saves bounds", lambda: saved_bounds(preferences))
    print("main window closed: no window left, bounds", saved_bounds(preferences))
    assert saved_bounds(preferences) == first, "reopened Settings moved: saved bounds drift"


if __name__ == "__main__":
    main()
