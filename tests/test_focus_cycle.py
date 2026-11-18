"""lotus#74 focus-cycle check for the Windows desktop.

Runs `lotus-desktop --mcp` with one running session, then moves window focus the way a
person does: Alt+Tab away and back (real key events), a click on the title bar,
minimize/restore and rapid Alt+Tab cycles. Each phase samples `lotus_status` twice a second
and records the guest frame counter with the stages from `frontend_metrics` (lotus#74):

  published_preview_sequence   controller preview task -> shared state
  ui.polls_completed           window refresh task heartbeat
  ui.installed_preview_sequence  previews turned into window images
  ui.renders                   GPUI render calls

Pass: in every phase where the window is not minimized, the guest, the published and the
installed sequence and the render count all advance, the poll heartbeat is under 500 ms old,
and the installed sequence trails the published one by at most 3. Minimized, renders may stop
(GPUI throttling) but the poll heartbeat must not. A failure names the first stage that
stopped, which is the measurement lotus#74 asks for.

Moves real focus on this desktop for about 30 s. Works with a native worker or the mock
(`target/<profile>/lotus-mock-native.exe`, which writes advancing capture frames).

Usage: test_focus_cycle.py <lotus-desktop.exe> <worker.exe> <content> <output dir>
Output: <output dir>/samples.json and a one-line verdict per phase.
"""
import ctypes
import ctypes.wintypes as wt
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import mcp_client  # noqa: E402

STAGES = ("guest", "published", "installed", "renders")
user32 = ctypes.windll.user32


def main():
    desktop, worker, content, output = sys.argv[1:5]
    output = Path(output)
    output.mkdir(parents=True)
    config = output / "host.json"
    config.write_text(json.dumps({"worker": worker, "storage_root": str(output / "storage"),
                                  "allow_control": True, "max_sessions": 2,
                                  "assets": {"content": content}}))
    # mcp_client.Client launches `<executable> <config>`; the desktop needs --mcp first.
    popen = subprocess.Popen
    subprocess.Popen = lambda args, **kw: popen([args[0], "--mcp", *args[1:]], **kw)
    try:
        client = mcp_client.Client(desktop, config, output / "transcript.jsonl")
    finally:
        subprocess.Popen = popen
    try:
        run(client, output)
    finally:
        # WM_CLOSE shuts the desktop down with its workers; kill only as a fallback.
        hwnd = window_of(client.process.pid)
        if hwnd:
            user32.PostMessageW(hwnd, 0x10, 0, 0)
        try:
            client.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            client.process.kill()


def window_of(pid):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def each(hwnd, _):
        owner = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            found.append(hwnd)
        return True
    user32.EnumWindows(each, 0)
    return found[0] if found else None


def key(vk, up=False):
    user32.keybd_event(vk, 0, 2 if up else 0, 0)


def alt_tab():
    key(0x12); key(0x09); time.sleep(0.05); key(0x09, True); time.sleep(0.15); key(0x12, True)


def foreground(hwnd):
    # A tapped Alt lets this process move the foreground (SetForegroundWindow rules).
    key(0x12); user32.SetForegroundWindow(hwnd); key(0x12, True)


def click_title(hwnd):
    """Click a visible point of the title bar strip, as a person returning to the window
    would. False if another window covers all of it."""
    rect = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    user32.WindowFromPoint.restype = wt.HWND
    user32.WindowFromPoint.argtypes = [wt.POINT]
    user32.GetAncestor.restype = wt.HWND
    for x in range(rect.left + 120, rect.right - 160, 40):
        point = wt.POINT(x, rect.top + 14)
        if user32.GetAncestor(user32.WindowFromPoint(point), 2) == hwnd:
            user32.SetCursorPos(point.x, point.y)
            user32.mouse_event(2, 0, 0, 0, 0); user32.mouse_event(4, 0, 0, 0, 0)
            return True
    return False


def run(client, output):
    session = client.tool("lotus_create_session", {"asset": "content"})["session"]
    client.tool("lotus_control", {"session": session, "operation": "run"})
    hwnd = None
    for _ in range(100):
        hwnd = window_of(client.process.pid)
        if hwnd:
            break
        time.sleep(0.1)
    assert hwnd, "no LOTUS window"
    foreground(hwnd)
    samples = []

    def sample(phase, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            status = client.tool("lotus_status", {})
            metrics = status.get("frontend_metrics", {})
            ui = metrics.get("ui", {})
            guest = next(s["telemetry"]["frames"] for s in status["sessions"]
                         if s["id"] == session)
            samples.append({
                "phase": phase, "time": time.monotonic(),
                "foreground": user32.GetForegroundWindow() == hwnd,
                "minimized": bool(user32.IsIconic(hwnd)),
                "guest": guest,
                "published": metrics.get("published_preview_sequence", 0),
                "installed": ui.get("installed_preview_sequence", 0),
                "renders": ui.get("renders", 0),
                "polls": ui.get("polls_completed", 0),
                "polls_recovered": ui.get("polls_recovered", 0),
                "poll_age_ms": ui.get("poll_age_ms"),
                "activations": ui.get("activations", 0),
                "deactivations": ui.get("deactivations", 0),
            })
            time.sleep(0.5)

    sample("focused", 3)
    alt_tab(); sample("alt-tab away", 3)
    alt_tab(); sample("alt-tab back", 3)
    # Deactivate without covering the window (the desktop shell takes focus), then click
    # back in; skipped if another window still covers the whole title bar.
    user32.GetShellWindow.restype = wt.HWND
    foreground(user32.GetShellWindow()); time.sleep(0.5)
    sample("shell focused", 2)
    if click_title(hwnd):
        sample("click activation", 3)
    else:
        print("click activation: title bar covered, skipped")
        foreground(hwnd)
    user32.ShowWindow(hwnd, 6); sample("minimized", 3)
    user32.ShowWindow(hwnd, 9); foreground(hwnd); sample("restored", 3)
    for _ in range(5):
        alt_tab(); time.sleep(0.4); alt_tab(); time.sleep(0.4)
    foreground(hwnd); sample("after 5 cycles", 4)
    (output / "samples.json").write_text(json.dumps(samples, indent=1))
    failures = verdicts(samples)
    assert not failures, failures


def verdicts(samples):
    failures = []
    phases = dict.fromkeys(sample["phase"] for sample in samples)
    for phase in phases:
        # The first sample of a phase still straddles the focus change.
        rows = [sample for sample in samples if sample["phase"] == phase][1:]
        first, last = rows[0], rows[-1]
        minimized = all(row["minimized"] for row in rows)
        stopped = [stage for stage in STAGES
                   if last[stage] <= first[stage] and not (minimized and stage == "renders")]
        lag = max(row["published"] - row["installed"] for row in rows)
        stale = max(row["poll_age_ms"] or 1e9 for row in rows)
        problems = []
        if stopped:
            problems.append(f"stopped: {', '.join(stopped)}")
        if stale >= 500:
            problems.append(f"poll heartbeat {stale:.0f} ms old")
        if lag > 3 and not minimized:
            problems.append(f"installed trails published by {lag}")
        if last["polls_recovered"]:
            problems.append(f"{last['polls_recovered']} poisoned-state recoveries")
        print(f"{phase:16} foreground={rows[-1]['foreground']!s:5} "
              f"guest+{last['guest'] - first['guest']} "
              f"published+{last['published'] - first['published']} "
              f"installed+{last['installed'] - first['installed']} "
              f"renders+{last['renders'] - first['renders']} lag<={lag} "
              f"{'; '.join(problems) or 'ok'}")
        if problems:
            failures.append(f"{phase}: {'; '.join(problems)}")
    return failures


if __name__ == "__main__":
    main()
