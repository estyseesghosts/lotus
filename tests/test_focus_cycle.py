"""lotus#74 focus-cycle check for the Windows desktop.

Runs `lotus-desktop --mcp-stdio` with one running session, then moves window focus the way a
person does: Alt+Tab away and back (real key events), a click on the title bar,
minimize/restore and rapid Alt+Tab cycles. It then uses Windows UI Automation
to locate and click the Inspector toolbar button with accessibility active.
Each phase samples `lotus_status` twice a second
and records the guest frame counter with the stages from `frontend_metrics` (lotus#74):

  published_preview_sequence   controller preview task -> shared state
  ui.polls_completed           window refresh task heartbeat
  ui.installed_preview_sequence  previews turned into window images
  ui.renders                   GPUI render calls

Pass: in every phase where the window is not minimized, the guest, the published and the
installed sequence and the render count all advance, the poll heartbeat is under 500 ms old,
and the installed sequence trails the published one by at most 3. While minimized, the guest
and poll heartbeat keep advancing, but preview requests and image updates stop (#25).

Moves real focus on this desktop for about 30 s. Works with a native worker or the mock
(`target/<profile>/lotus-mock-native.exe`, which writes advancing capture frames).

Usage: test_focus_cycle.py <lotus-desktop.exe> <worker.exe> <content> <output dir> [theme] [--inspector-only]
Output: <output dir>/samples.json and a one-line verdict per phase.
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import mcp_client  # noqa: E402

STAGES = ("guest", "controller_ticks", "polls", "published", "installed",
          "rendered_preview", "rendered_worker_sequence", "rendered_gauge_frames", "renders")
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wt.HWND


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("desktop", type=Path)
    parser.add_argument("worker", type=Path)
    parser.add_argument("content", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("theme", nargs="?")
    parser.add_argument("--inspector-only", action="store_true")
    args = parser.parse_args()
    theme = ["--theme", args.theme] if args.theme else []
    output = args.output.resolve()
    output.mkdir(parents=True)
    os.environ["LOTUS_DESKTOP_PREFERENCES"] = str(output / "desktop.json")
    config = output / "host.json"
    config.write_text(json.dumps({"worker": str(args.worker.resolve()),
                                  "storage_root": str(output / "storage"),
                                  "allow_control": True, "max_sessions": 2,
                                  "assets": {"content": str(args.content.resolve())}}))
    client = mcp_client.Client(args.desktop.resolve(), config, output / "transcript.jsonl",
                               arguments=["--mcp-stdio", *theme], stderr_path=output / "desktop.stderr")
    try:
        run(client, output, args.worker.stem == "lotus-mock-native", args.inspector_only)
    finally:
        # Close the window, then close stdio so the windowed MCP server exits.
        hwnd = window_of(client.process.pid)
        if hwnd:
            user32.PostMessageW(hwnd, 0x10, 0, 0)
        client.close()


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
    previous = user32.GetForegroundWindow()
    other = user32.GetWindowThreadProcessId(previous, None)
    current = ctypes.windll.kernel32.GetCurrentThreadId()
    attached = other and other != current and user32.AttachThreadInput(current, other, True)
    try:
        user32.BringWindowToTop(hwnd)
        user32.SetForegroundWindow(hwnd)
    finally:
        if attached:
            user32.AttachThreadInput(current, other, False)


def wait_state(description, check):
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError(description)


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


def click_inspector(hwnd, *, opening):
    # Discovering the button through UI Automation activates GPUI's accessibility
    # tree. Keep that client connected through the click and the following frames.
    script = rf"""
        $ErrorActionPreference = 'Stop'
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]{hwnd})
        $name = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::NameProperty, 'Inspector')
        $button = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::ControlTypeProperty,
            [Windows.Automation.ControlType]::Button)
        $condition = [Windows.Automation.AndCondition]::new($name, $button)
        for ($i = 0; $i -lt 20; $i++) {{
            $element = $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            if ($element) {{ break }}
            Start-Sleep -Milliseconds 100
        }}
        if (!$element) {{ throw 'Inspector toolbar button missing from the accessibility tree' }}
        $rect = $element.Current.BoundingRectangle
        Write-Output ("{{0}} {{1}}" -f [int]($rect.X + $rect.Width / 2),
                                      [int]($rect.Y + $rect.Height / 2))
        [void][Console]::ReadLine()
        $pin = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::NameProperty, 'Pin')
        $unpin = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::NameProperty, 'Unpin')
        $header = [Windows.Automation.OrCondition]::new($pin, $unpin)
        $expected = ${str(opening).lower()}
        for ($i = 0; $i -lt 20; $i++) {{
            $shown = $null -ne $window.FindFirst(
                [Windows.Automation.TreeScope]::Descendants, $header)
            if ($shown -eq $expected) {{ exit 0 }}
            Start-Sleep -Milliseconds 100
        }}
        throw 'Inspector accessibility subtree did not reach the expected visibility'
    """
    with subprocess.Popen(["powershell.exe", "-NoProfile", "-MTA", "-Command", script],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True,
                          creationflags=subprocess.CREATE_NO_WINDOW) as helper:
        stdout = queue.Queue()
        threading.Thread(target=lambda: stdout.put(helper.stdout.readline()), daemon=True).start()
        try:
            try:
                coordinates = stdout.get(timeout=10)
            except queue.Empty:
                raise AssertionError("timed out locating the Inspector through UI Automation")
            if not coordinates:
                _, error = helper.communicate(timeout=5)
                raise AssertionError(error)
            x, y = map(int, coordinates.split())
            user32.SetCursorPos(x, y)
            user32.mouse_event(2, 0, 0, 0, 0)
            user32.mouse_event(4, 0, 0, 0, 0)
            time.sleep(0.5)
            _, error = helper.communicate(input="done\n", timeout=5)
        finally:
            if helper.poll() is None:
                helper.kill()
        assert helper.returncode == 0, error


def run(client, output, mock, inspector_only):
    session = client.tool("lotus_create_session",
                          {"kind": "primary", "card": "focus", "asset": "content"})["session"]
    if mock:
        (output / "storage" / "cards" / "focus" / "mock-continuous-run").touch()
    client.tool("lotus_control", {"session": session, "operation": "run"})
    hwnd = None
    for _ in range(100):
        hwnd = window_of(client.process.pid)
        if hwnd:
            break
        time.sleep(0.1)
    assert hwnd, "no LOTUS window"
    foreground(hwnd)
    active = lambda: user32.GetForegroundWindow() == hwnd
    wait_state("LOTUS did not take foreground", active)
    samples = []

    def sample(phase, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            status = client.tool("lotus_status", {})
            metrics = status.get("frontend_metrics", {})
            ui = metrics.get("ui", {})
            guest = next(s["telemetry"] for s in status["sessions"] if s["id"] == session)
            workspace = ui.get("rendered_workspace") or {}
            tile = next((tile for tile in workspace.get("tiles", [])
                         if tile["session"] == session), {})
            preview = tile.get("preview") or {}
            samples.append({
                "phase": phase, "time": time.monotonic(),
                "foreground": user32.GetForegroundWindow() == hwnd,
                "minimized": bool(user32.IsIconic(hwnd)),
                "guest": guest["frames"],
                "guest_epoch": guest["epoch"],
                "controller_ticks": metrics.get("ticks", 0),
                "published": metrics.get("published_preview_sequence", 0),
                "preview_requests": metrics.get("preview", {}).get("count", 0),
                "installed": ui.get("installed_preview_sequence", 0),
                "rendered_preview": preview.get("sequence", 0),
                "rendered_worker_sequence": preview.get("worker_sequence", 0),
                "rendered_preview_epoch": preview.get("epoch"),
                "rendered_summary_epoch": tile.get("summary_epoch"),
                "rendered_gauge_frames": tile.get("guest_frames", 0),
                "renders": ui.get("renders", 0),
                "polls": ui.get("polls_completed", 0),
                "polls_started": ui.get("polls_started", 0),
                "polls_recovered": ui.get("polls_recovered", 0),
                "poll_age_ms": ui.get("poll_age_ms"),
                "activations": ui.get("activations", 0),
                "deactivations": ui.get("deactivations", 0),
            })
            time.sleep(0.5)
        (output / "samples.json").write_text(json.dumps(samples, indent=1))

    sample("focused", 3)
    if not inspector_only:
        cycle_focus(hwnd, active, sample)
    click_inspector(hwnd, opening=True)
    sample("inspector open", 3)
    click_inspector(hwnd, opening=False)
    sample("inspector closed", 3)
    failures = verdicts(samples)
    assert not failures, failures


def cycle_focus(hwnd, active, sample):
    alt_tab()
    wait_state("Alt+Tab did not leave LOTUS", lambda: not active())
    sample("alt-tab away", 3)
    alt_tab()
    wait_state("Alt+Tab did not return to LOTUS", active)
    sample("alt-tab back", 3)
    # Deactivate without covering the window (the desktop shell takes focus), then click
    # back in; skipped if another window still covers the whole title bar.
    user32.GetShellWindow.restype = wt.HWND
    foreground(user32.GetShellWindow()); time.sleep(0.5)
    sample("shell focused", 2)
    if click_title(hwnd):
        wait_state("title click did not activate LOTUS", active)
        sample("click activation", 3)
    else:
        print("click activation: title bar covered, skipped")
        foreground(hwnd)
    user32.ShowWindow(hwnd, 6)
    wait_state("LOTUS did not minimize", lambda: user32.IsIconic(hwnd))
    sample("minimized", 3)
    user32.ShowWindow(hwnd, 9)
    foreground(hwnd)
    wait_state("LOTUS did not restore to foreground", active)
    sample("restored", 3)
    for _ in range(5):
        alt_tab(); time.sleep(0.4); alt_tab(); time.sleep(0.4)
    foreground(hwnd); sample("after 5 cycles", 4)


def verdicts(samples):
    failures = []
    phases = dict.fromkeys(sample["phase"] for sample in samples)
    for phase in phases:
        # The first sample of a phase still straddles the focus change.
        rows = [sample for sample in samples if sample["phase"] == phase][1:]
        if len(rows) < 2:
            failures.append(f"{phase}: insufficient status samples")
            continue
        first, last = rows[0], rows[-1]
        minimized = all(row["minimized"] for row in rows)
        required = ("guest", "controller_ticks", "polls") if minimized else STAGES
        stopped = [stage for stage in required if last[stage] <= first[stage]]
        lag = max(row["published"] - row["installed"] for row in rows)
        stale = max(
            float("inf") if row["poll_age_ms"] is None else row["poll_age_ms"]
            for row in rows
        )
        problems = []
        if stopped:
            problems.append(f"stopped: {', '.join(stopped)}")
        if minimized:
            continued = [stage for stage in ("preview_requests", "published", "installed")
                         if last[stage] != first[stage]]
            if continued:
                problems.append(f"continued while minimized: {', '.join(continued)}")
        if stale >= 500:
            problems.append(f"poll heartbeat {stale:.0f} ms old")
        if lag > 3 and not minimized:
            problems.append(f"installed trails published by {lag}")
        if not minimized:
            mismatched = [row for row in rows if row["rendered_preview_epoch"] is not None
                          and row["rendered_preview_epoch"] != row["rendered_summary_epoch"]]
            if mismatched:
                problems.append("rendered preview epoch differs from the gauge snapshot epoch")
        if len({row["guest_epoch"] for row in samples}) != 1:
            problems.append("guest epoch changed during focus cycling")
        if last["polls_recovered"]:
            problems.append(f"{last['polls_recovered']} poisoned-state recoveries")
        print(f"{phase:16} foreground={rows[-1]['foreground']!s:5} "
              f"guest+{last['guest'] - first['guest']} "
              f"published+{last['published'] - first['published']} "
              f"installed+{last['installed'] - first['installed']} "
              f"native sequence+{last['rendered_worker_sequence'] - first['rendered_worker_sequence']} "
              f"gauge frames+{last['rendered_gauge_frames'] - first['rendered_gauge_frames']} "
              f"renders+{last['renders'] - first['renders']} lag<={lag} "
              f"{'; '.join(problems) or 'ok'}")
        if problems:
            failures.append(f"{phase}: {'; '.join(problems)}")
    return failures


if __name__ == "__main__":
    main()
