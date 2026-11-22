"""Opt-in windowed acceptance: an MCP client launches the LOTUS window as its server.

python tests/test_windowed_mcp.py <lotus-desktop> <host-config> <asset-handle>
    [hold-seconds] [--screenshots <directory>]

One primary and one secondary run in a single GPUI window. Check each tile's
rendered preview, frame counter and epoch alongside MCP layout revisions and
worker PIDs. The secondary's scratch root must be discarded. `hold-seconds`
leaves both tiles visible for observation before untiling.
"""
import argparse
import json
import pathlib
import queue
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("desktop", type=pathlib.Path)
    parser.add_argument("config", type=pathlib.Path)
    parser.add_argument("asset")
    parser.add_argument("hold", nargs="?", type=float, default=0)
    parser.add_argument("--screenshots", type=pathlib.Path,
                        help="save horizontal, vertical and auto layouts (Windows with Pillow)")
    args = parser.parse_args()
    desktop, config, asset = str(args.desktop.resolve()), str(args.config.resolve()), args.asset
    storage = pathlib.Path(json.loads(pathlib.Path(config).read_text(encoding="utf-8"))["storage_root"])
    messages = queue.Queue()
    with subprocess.Popen([desktop, "--mcp-stdio", config], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, text=True, encoding="utf-8") as process:
        def screenshot(name):
            if args.screenshots is None:
                return
            import ctypes
            from ctypes.wintypes import RECT
            from PIL import ImageGrab
            from test_focus_cycle import foreground, user32, wait_state, window_of

            hwnd = window_of(process.pid)
            assert hwnd, "no visible LOTUS window to capture"
            foreground(hwnd)
            wait_state("LOTUS did not take foreground for capture",
                       lambda: user32.GetForegroundWindow() == hwnd)
            bounds = RECT()
            # DWM reports physical frame bounds, including on a scaled display.
            assert ctypes.windll.dwmapi.DwmGetWindowAttribute(
                ctypes.c_void_p(hwnd), 9, ctypes.byref(bounds), ctypes.sizeof(bounds)) == 0
            ctypes.windll.dwmapi.DwmFlush()
            picture = ImageGrab.grab(bbox=(bounds.left, bounds.top, bounds.right, bounds.bottom),
                                     all_screens=True)
            assert any(low < high for low, high in picture.getextrema()), "blank window screenshot"
            args.screenshots.mkdir(parents=True, exist_ok=True)
            picture.save(args.screenshots / f"{name}.png")

        def read():
            for line in process.stdout:
                messages.put(json.loads(line))
            messages.put(None)
        threading.Thread(target=read, daemon=True).start()
        request_id = 0

        def send(method, params=None, notification=False):
            nonlocal request_id
            request_id += 1
            message = {"jsonrpc": "2.0", "method": method}
            if not notification:
                message["id"] = request_id
            if params is not None:
                message["params"] = params
            process.stdin.write(json.dumps(message) + "\n")
            process.stdin.flush()
            if notification:
                return None
            deadline = time.monotonic() + 35
            while True:
                reply = messages.get(timeout=max(0.001, deadline - time.monotonic()))
                assert reply is not None, "window exited early"
                if reply.get("id") == request_id:
                    assert "error" not in reply, reply
                    return reply["result"]

        def tool(name, arguments=None, *, error=None):
            result = send("tools/call", {"name": name, "arguments": arguments or {}})
            if error is not None:
                assert result.get("isError") and error in result["content"][0]["text"], result
                return
            assert not result.get("isError", False), result
            return json.loads(result["content"][0]["text"])

        def sessions():
            return {entry["id"]: entry for entry in tool("lotus_list_sessions")}

        def wait(what, predicate, seconds=30, read=sessions):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                value = read()
                if predicate(value):
                    return value
                time.sleep(0.25)
            raise AssertionError(f"timed out waiting for {what}: {value}")

        def status():
            return tool("lotus_status")

        def sample_metrics(label):
            snapshot = status()
            preview = snapshot["frontend_metrics"]["preview"]
            workers = snapshot["sessions"]
            gauges = ", ".join(
                f"{row['kind']} {row['telemetry']['fps']:.1f} fps "
                f"{row['telemetry']['memory_bytes'] / 2**20:.1f} MiB" for row in workers)
            print(f"{label}: preview RPC {preview['last_ms']:.2f} ms, {gauges}")

        def workspace(op="get", revision=None, **fields):
            args = {"operation": {"op": op, **fields}}
            if revision is not None:
                args["expected_revision"] = revision
            return tool("lotus_workspace", args)

        def rendered(revision, ids, after=None):
            def read():
                snapshot = status()
                return (snapshot["frontend_metrics"]["ui"].get("rendered_workspace") or {},
                        snapshot["workspace"])

            def matches(sample):
                drawn, workspace_state = sample
                tiles = drawn.get("tiles", [])
                if drawn.get("revision") != revision or workspace_state["revision"] != revision:
                    return False
                if [tile["session"] for tile in tiles] != ids:
                    return False
                if len(ids) == 2 and not workspace_state["resolved"]:
                    return False
                return all(tile["preview"] and tile["preview"]["epoch"] == tile["summary_epoch"]
                           and (after is None or (
                               tile["preview"]["worker_sequence"] >
                               after[tile["session"]]["preview"]["worker_sequence"]
                               and tile["guest_frames"] > after[tile["session"]]["guest_frames"]))
                           for tile in tiles)

            drawn, workspace_state = wait(f"rendered revision {revision} for {ids}", matches, read=read)
            return {tile["session"]: tile for tile in drawn["tiles"]}, workspace_state

        try:
            send("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                "clientInfo": {"name": "lotus-windowed-acceptance", "version": "1"}})
            send("notifications/initialized", notification=True)
            names = {entry["name"] for entry in send("tools/list")["tools"]}
            assert {"lotus_create_session", "lotus_load", "lotus_control", "lotus_registers",
                    "lotus_list_sessions", "lotus_focus_session", "lotus_capture"} <= names, names
            assert status()["frontend"] == "windowed"

            primary = tool("lotus_create_session",
                           {"kind": "primary", "card": "dev", "asset": asset})["session"]
            secondary = tool("lotus_create_session",
                             {"kind": "secondary", "card": "dev", "asset": asset})["session"]
            for session in (primary, secondary):
                tool("lotus_control", {"session": session, "operation": "run"})
            listed = wait("both sessions running", lambda s: all(
                s[i]["telemetry"]["playing"] and s[i]["telemetry"]["fps"] > 0 for i in (primary, secondary)))
            assert listed[primary]["kind"] == "primary" and listed[primary]["card"] == "dev"
            assert listed[secondary]["kind"] == "secondary" and listed[secondary]["parent"] == "dev"
            for session in (primary, secondary):
                gauges = listed[session]["telemetry"]
                print(f"{listed[session]['kind']:9} fps={gauges['fps']:.1f} speed={gauges['speed']:.2f} "
                      f"frames={gauges['frames']} memory={gauges['memory_bytes']}")

            single = workspace("single", session=primary)["workspace"]
            rendered(single["revision"], [primary])
            time.sleep(1)
            sample_metrics("one visible")
            pids = {i: listed[i]["telemetry"]["pid"] for i in (primary, secondary)}
            assert all(pids.values()) and len(set(pids.values())) == 2, pids

            started = time.monotonic()
            tiled = workspace("tile", single["revision"],
                              first=primary, second=secondary, axis="horizontal")
            revision = tiled["workspace"]["revision"]
            before, window = rendered(revision, [primary, secondary])
            assert window["resolved"]["axis"] == "horizontal", window
            print(f"both tile previews reached GPUI in {(time.monotonic() - started) * 1000:.0f} ms")
            time.sleep(1)
            after, _ = rendered(revision, [primary, secondary], before)
            sample_metrics("two visible")
            screenshot("horizontal")
            print("horizontal: " + ", ".join(
                f"{i[:8]} guest+{after[i]['guest_frames'] - before[i]['guest_frames']} "
                f"preview+{after[i]['preview']['worker_sequence'] - before[i]['preview']['worker_sequence']}"
                for i in (primary, secondary)))
            for session in (primary, secondary):
                reply = send("tools/call", {"name": "lotus_capture", "arguments": {"session": session}})
                assert not reply.get("isError"), reply
                links = reply["content"]
                assert len(links) == 2 and all(link.get("type") == "resource_link" for link in links), links
            tool("lotus_focus_session", {"session": secondary})
            assert workspace()["revision"] == revision, "inspector focus changed the split"
            vertical = workspace("set_axis", revision, axis="vertical")["workspace"]
            tool("lotus_workspace", {"operation": {"op": "swap"}, "expected_revision": revision},
                 error="workspace changed")
            assert workspace()["revision"] == vertical["revision"], "stale update changed revision"
            after, window = rendered(vertical["revision"], [primary, secondary], after)
            assert window["resolved"]["axis"] == "vertical", window
            screenshot("vertical")
            swapped = workspace("swap", vertical["revision"])["workspace"]
            after, _ = rendered(swapped["revision"], [secondary, primary], after)
            auto = workspace("set_axis", swapped["revision"], axis="auto")["workspace"]
            _, window = rendered(auto["revision"], [secondary, primary], after)
            assert window["resolved"]["axis"] in ("horizontal", "vertical"), window
            screenshot(f"auto-{window['resolved']['axis']}")

            # Independently addressed: pausing one leaves the other running.
            tool("lotus_control", {"session": secondary, "operation": "pause"})
            registers = tool("lotus_registers", {"session": secondary})
            assert len(registers["registers"]) == 16, registers
            wait("secondary paused, primary running", lambda s:
                 not s[secondary]["telemetry"]["playing"] and s[primary]["telemetry"]["playing"])
            tool("lotus_control", {"session": secondary, "operation": "run"})
            if args.hold > 0:
                print(f"holding {args.hold:.0f}s for observation")
                time.sleep(args.hold)

            single = workspace("untile", auto["revision"], keep=primary)["workspace"]
            rendered(single["revision"], [primary])
            before = sessions()
            wait("both original workers advancing after untile", lambda rows: all(
                rows[i]["telemetry"]["pid"] == pids[i]
                and rows[i]["telemetry"]["frames"] > before[i]["telemetry"]["frames"]
                for i in pids))
            tool("lotus_control", {"session": secondary, "operation": "pause"})
            retiling = workspace("tile", single["revision"], first=primary, second=secondary)
            rendered(retiling["workspace"]["revision"], [primary, secondary])
            assert not sessions()[secondary]["telemetry"]["playing"], "retiling resumed the guest"

            # Stop asynchronously in either order; secondary data is discarded.
            tool("lotus_control", {"session": secondary, "operation": "stop"})
            assert not (storage / "scratch" / secondary).exists(), "secondary root discarded"
            assert set(sessions()) == {primary}
            remaining = workspace()
            rendered(remaining["revision"], [primary])
            tool("lotus_control", {"session": primary, "operation": "stop"})
            assert (storage / "cards" / "dev").is_dir(), "primary card persists"
            print("Windowed MCP: preview isolation, CAS, swap, untile, capture and cleanup passed")
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()


if __name__ == "__main__":
    main()
