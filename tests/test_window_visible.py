"""Issue #3: lotus-desktop must be visible when its parent requests SW_HIDE.

Starts the desktop the way Node's `windowsHide` does (STARTF_USESHOWWINDOW, SW_HIDE),
finds the process's top-level "LOTUS" window with EnumWindows and checks IsWindowVisible.
Windows only. Usage: python tests/test_window_visible.py <lotus-desktop.exe> [host.json]
"""
import ctypes
import ctypes.wintypes as wt
import subprocess
import sys
import time

user32 = ctypes.windll.user32
EnumProc = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)


def windows_of(pid):
    found = []

    def visit(hwnd, _):
        owner = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        title = ctypes.create_unicode_buffer(64)
        user32.GetWindowTextW(hwnd, title, 64)
        if owner.value == pid and title.value == "LOTUS":
            found.append(bool(user32.IsWindowVisible(hwnd)))
        return True

    user32.EnumWindows(EnumProc(visit), 0)
    return found


info = subprocess.STARTUPINFO()
info.dwFlags |= subprocess.STARTF_USESHOWWINDOW
info.wShowWindow = 0  # SW_HIDE
process = subprocess.Popen(sys.argv[1:], startupinfo=info, stdin=subprocess.PIPE,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    deadline = time.monotonic() + 30
    states = []
    while time.monotonic() < deadline and not states:
        time.sleep(0.5)
        states = windows_of(process.pid)
    time.sleep(1)
    states = windows_of(process.pid)
    print({"pid": process.pid, "lotus_windows_visible": states})
    assert states, "no LOTUS window found"
    assert all(states), "LOTUS window started hidden"
finally:
    process.kill()
    process.wait()
