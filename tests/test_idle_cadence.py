"""Measure Windows redraw and controller cadence with native workers (lotus#158)."""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import time

from mcp_client import Client
from test_focus_cycle import click_inspector, foreground, user32, wait_state, window_of


def cpu_time(process):
    times = [wintypes.FILETIME() for _ in range(4)]
    assert ctypes.windll.kernel32.GetProcessTimes(
        wintypes.HANDLE(int(process._handle)), *(ctypes.byref(value) for value in times)
    )
    return sum(value.dwHighDateTime * (1 << 32) + value.dwLowDateTime
               for value in times[2:]) / 10_000_000


def sample(client, seconds):
    def snapshot():
        status = client.tool("lotus_status", {})
        metrics = status["frontend_metrics"]
        ui = metrics["ui"]
        return (time.monotonic(), cpu_time(client.process), {
            "renders": ui["renders"], "polls": ui["polls_completed"],
            "previews": ui["previews_installed"], "ticks": metrics["ticks"],
            "preview_requests": metrics["preview"]["count"],
            **{name: metrics[name] for name in ("metrics_publications", "session_publications")
               if name in metrics},
        }, {row["id"]: row["telemetry"]["frames"] for row in status["sessions"]})

    start, cpu_start, first, frames = snapshot()
    time.sleep(seconds)
    end, cpu_end, last, end_frames = snapshot()
    return {
        "seconds": end - start,
        "cpu_percent": 100 * (cpu_end - cpu_start) / (end - start),
        **{name: last[name] - first[name] for name in first},
        "guest_frames": [end_frames[session] - value for session, value in frames.items()],
    }


def checkpoint(client, session):
    results = client.tool("lotus_capture", {"session": session})["content"]
    return [hashlib.sha256(base64.b64decode(client.send("resources/read", {
        "uri": result["uri"],
    })["contents"][0]["blob"])).hexdigest() for result in results]


def run(args):
    root = args.output.resolve()
    root.mkdir()
    config = root / "host.json"
    config.write_text(json.dumps({
        "worker": str(args.worker.resolve()), "storage_root": str(root / "storage"),
        "allow_control": True, "max_sessions": 4, "assets": {"owned": str(args.content.resolve())},
        "prefer_vulkan": args.vulkan,
    }), encoding="utf-8")
    os.environ["LOTUS_DESKTOP_PREFERENCES"] = str(root / "desktop.json")
    client = Client(args.desktop.resolve(), config, root / "mcp.jsonl",
                    arguments=["--mcp-stdio"], stderr_path=root / "desktop.stderr")
    results = []

    def record(count, phase):
        time.sleep(0.9)
        result = {"sessions": count, "phase": phase, **sample(client, args.seconds)}
        results.append(result)
        (root / "samples.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
        print(json.dumps(result), flush=True)
        assert result["ticks"] / result["seconds"] >= 24, result
        if args.baseline:
            return
        if phase.startswith("hidden"):
            assert result["previews"] == result["preview_requests"] == 0, result
        if phase in ("empty", "paused", "paused inspector"):
            assert result["renders"] <= 2 and result["polls"] / result["seconds"] < 12, result
        if phase == "running":
            assert result["previews"] > 0 and result["renders"] > 0, result
        if phase in ("running", "hidden running"):
            assert sum(value > 0 for value in result["guest_frames"]) == min(count, 2), result

    def workspace(op, **kwargs):
        return client.tool("lotus_workspace", {"operation": {"op": op, **kwargs}})["workspace"]

    def rendered(revision, sessions, after=None):
        seen = {}

        def check():
            ui = client.tool("lotus_status", {})["frontend_metrics"]["ui"]
            shown = ui.get("rendered_workspace") or {}
            tiles = shown.get("tiles", [])
            if (shown.get("revision") != revision
                    or [tile["session"] for tile in tiles] != sessions):
                return False
            for tile in tiles:
                preview = tile["preview"]
                if (not preview or preview["epoch"] != tile["summary_epoch"]
                        or (after and preview["sequence"] <= after[tile["session"]])):
                    return False
            seen.update((tile["session"], tile["preview"]["sequence"]) for tile in tiles)
            return True

        wait_state("current workspace and preview", check)
        return seen

    def control(session, operation):
        client.tool("lotus_control", {"session": session, "operation": operation})

    hwnd = None
    try:
        def find_window():
            nonlocal hwnd
            hwnd = window_of(client.process.pid)
            return hwnd is not None
        wait_state("Lotus appears", find_window)
        # Move the pointer out of the window to keep hover timers out of the sample.
        user32.SetCursorPos(0, 0)
        foreground(hwnd)
        record(0, "empty")
        user32.ShowWindow(hwnd, 6)
        wait_state("Lotus minimizes", lambda: user32.IsIconic(hwnd))
        record(0, "hidden empty")
        user32.ShowWindow(hwnd, 9)
        foreground(hwnd)
        sessions = []
        for count in (1, 2, 4):
            while len(sessions) < count:
                sessions.append(client.tool("lotus_create_session", {
                    "kind": "primary" if not sessions else "secondary",
                    "asset": "owned", "card": "cadence",
                })["session"])
                client.tool("lotus_run_until", {"session": sessions[-1], "frames": 60})
            shown = sessions[:min(count, 2)]
            if count == 1:
                layout = workspace("single", session=shown[0])
            else:
                layout = workspace("tile", first=shown[0], second=shown[1], axis="horizontal")
            rendered(layout["revision"], shown)
            rows = client.tool("lotus_list_sessions", {})
            renderer = "vulkan" if args.vulkan else "software"
            assert len(rows) == count and all(
                row["telemetry"]["renderer"].casefold() == renderer for row in rows
            ), rows
            if count >= 2:
                # Show one paused tile for the idle comparison and restore the split for playback.
                single = workspace("single", session=shown[0])
                rendered(single["revision"], shown[:1])
            record(count, "paused")
            if count == 1 and args.inspector:
                click_inspector(hwnd, opening=True)
                record(count, "paused inspector")
                click_inspector(hwnd, opening=False)
            if count >= 2:
                layout = workspace("tile", first=shown[0], second=shown[1], axis="horizontal")
                rendered(layout["revision"], shown)
            (root / f"checkpoint-{count}.json").write_text(
                json.dumps(checkpoint(client, shown[-1])), encoding="utf-8")
            for session in shown:
                control(session, "run")
            record(count, "running")
            before_hide = rendered(layout["revision"], shown)
            user32.ShowWindow(hwnd, 6)
            wait_state("Lotus minimizes", lambda: user32.IsIconic(hwnd))
            record(count, "hidden running")
            user32.ShowWindow(hwnd, 9)
            foreground(hwnd)
            for session in shown:
                control(session, "pause")
            rendered(layout["revision"], shown, after=before_hide)
    finally:
        if hwnd:
            user32.PostMessageW(hwnd, 0x10, 0, 0)
        client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("desktop", "worker", "content", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--seconds", default=3.0, type=float)
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--inspector", action="store_true")
    parser.add_argument("--vulkan", action="store_true")
    run(parser.parse_args())


if __name__ == "__main__":
    main()
