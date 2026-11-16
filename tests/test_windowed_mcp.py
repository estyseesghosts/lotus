"""Opt-in windowed acceptance: an MCP client launches the LOTUS window as its server.

python tests/test_windowed_mcp.py <lotus-desktop> <host-config> <asset-handle> [hold-seconds]

Drives the same tools as headless LOTUS against `lotus-desktop --mcp`: one primary
and one secondary copied from it run concurrently, are listed with gauges, are
addressed independently, and are stopped asynchronously. The secondary's scratch
root must be gone afterwards; the primary card must remain. `hold-seconds`
keeps both sessions running before shutdown so a person can watch the window.
"""
import json
import pathlib
import queue
import subprocess
import sys
import threading
import time


def main():
    desktop, config, asset = sys.argv[1:4]
    desktop = str(pathlib.Path(desktop).resolve())  # CreateProcess rejects relative "a/b" paths
    hold = float(sys.argv[4]) if len(sys.argv) > 4 else 0.0
    storage = pathlib.Path(json.loads(pathlib.Path(config).read_text(encoding="utf-8"))["storage_root"])
    messages = queue.Queue()
    with subprocess.Popen([desktop, "--mcp", config], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, text=True, encoding="utf-8") as process:
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

        def tool(name, arguments=None):
            result = send("tools/call", {"name": name, "arguments": arguments or {}})
            assert not result.get("isError", False), result
            return json.loads(result["content"][0]["text"])

        def sessions():
            return {entry["id"]: entry for entry in tool("lotus_list_sessions")}

        def wait(what, predicate, seconds=30):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                listed = sessions()
                if predicate(listed):
                    return listed
                time.sleep(0.25)
            raise AssertionError(f"timed out waiting for {what}: {sessions()}")

        try:
            send("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                "clientInfo": {"name": "lotus-windowed-acceptance", "version": "1"}})
            send("notifications/initialized", notification=True)
            names = {entry["name"] for entry in send("tools/list")["tools"]}
            assert {"lotus_create_session", "lotus_load", "lotus_control", "lotus_registers",
                    "lotus_list_sessions", "lotus_focus_session", "lotus_capture"} <= names, names
            status = json.loads(send("tools/call", {"name": "lotus_status", "arguments": {}})
                                ["content"][0]["text"])
            assert status["frontend"] == "windowed", status

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
            tool("lotus_focus_session", {"session": secondary})

            # Independently addressed: pausing one leaves the other running.
            tool("lotus_control", {"session": secondary, "operation": "pause"})
            registers = tool("lotus_registers", {"session": secondary})
            assert len(registers["registers"]) == 16, registers
            wait("secondary paused, primary running", lambda s:
                 not s[secondary]["telemetry"]["playing"] and s[primary]["telemetry"]["playing"])
            tool("lotus_control", {"session": secondary, "operation": "run"})
            if hold > 0:
                print(f"holding {hold:.0f}s for observation")
                time.sleep(hold)

            # Stop asynchronously in either order; secondary data is discarded.
            tool("lotus_control", {"session": secondary, "operation": "stop"})
            assert not (storage / "scratch" / secondary).exists(), "secondary root discarded"
            assert set(sessions()) == {primary}
            tool("lotus_control", {"session": primary, "operation": "stop"})
            assert (storage / "cards" / "dev").is_dir(), "primary card persists"
            print("Windowed MCP: primary/secondary concurrency, addressing, gauges, discard: passed")
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()


if __name__ == "__main__":
    main()
