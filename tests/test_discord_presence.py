"""Issue #18: Discord Rich Presence from lotus-desktop, against a fake Discord IPC server.

python tests/test_discord_presence.py <lotus-desktop> <ctr-worker> <fixture-a.3dsx> <fixture-b.3dsx> <out-dir>

Windows only; needs pywin32. Serves `\\\\.\\pipe\\discord-ipc-0` with Discord's IPC framing
(little-endian u32 opcode, u32 length, JSON; opcode 0 handshake, 1 frame, 2 close) and
answers the handshake with READY and each command with an echo, as Discord does. Refuses
to run if a real Discord already owns the pipe. Drives the window over MCP and checks:

- presence disabled: no connection while a game runs;
- enabled: Playing / Debugging (same canonical file) / multiple games (two different files
  that are both named game.3dsx) / back to Playing / cleared at zero sessions;
- pausing and duplicate states send nothing new;
- the server going away and coming back: LOTUS keeps running and reconnects once;
- shutdown clears the activity.
The fixtures are copied to out-dir/a/game.3dsx and out-dir/b/game.3dsx.
"""
import json
import os
import pathlib
import queue
import shutil
import struct
import subprocess
import sys
import threading
import time

import pywintypes
import win32file
import win32pipe

PIPE = r"\\.\pipe\discord-ipc-0"
APP_ID = "123456789012345678"


class FakeDiscord:
    def __init__(self):
        self.events = queue.Queue()
        self.connections = 0
        self.stop = False
        self.handle = None
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while not self.stop:
            pipe = win32pipe.CreateNamedPipe(
                PIPE, win32pipe.PIPE_ACCESS_DUPLEX,
                win32pipe.PIPE_TYPE_BYTE | win32pipe.PIPE_READMODE_BYTE | win32pipe.PIPE_WAIT,
                1, 65536, 65536, 0, None)
            self.handle = pipe
            try:
                win32pipe.ConnectNamedPipe(pipe, None)
            except pywintypes.error:
                win32file.CloseHandle(pipe)
                continue
            if self.stop:
                win32file.CloseHandle(pipe)
                return
            self.connections += 1
            try:
                while True:
                    _, header = win32file.ReadFile(pipe, 8)
                    opcode, length = struct.unpack("<II", header)
                    _, body = win32file.ReadFile(pipe, length) if length else (0, b"{}")
                    message = json.loads(body)
                    if opcode == 0:
                        assert message == {"v": 1, "client_id": APP_ID}, message
                        self.reply(pipe, 1, {"cmd": "DISPATCH", "evt": "READY", "data": {"v": 1}})
                        self.events.put(("connect", None))
                    elif opcode == 1:
                        assert message["cmd"] == "SET_ACTIVITY", message
                        activity = message["args"]["activity"]
                        self.events.put(("activity", activity))
                        self.reply(pipe, 1, {"cmd": "SET_ACTIVITY", "nonce": message["nonce"],
                                             "evt": None, "data": activity})
                    elif opcode == 2:
                        self.events.put(("close", None))
                        break
            except pywintypes.error:
                self.events.put(("disconnect", None))
            finally:
                win32file.CloseHandle(pipe)

    @staticmethod
    def reply(pipe, opcode, data):
        body = json.dumps(data).encode()
        win32file.WriteFile(pipe, struct.pack("<II", opcode, len(body)) + body)

    def shutdown(self):
        """Drop the client and stop listening, as if Discord quit."""
        self.stop = True
        try:
            # Unblock a pending ConnectNamedPipe.
            win32file.CloseHandle(win32file.CreateFile(
                PIPE, win32file.GENERIC_READ | win32file.GENERIC_WRITE, 0, None,
                win32file.OPEN_EXISTING, 0, None))
        except pywintypes.error:
            pass
        if self.handle is not None:
            try:
                win32pipe.DisconnectNamedPipe(self.handle)
            except pywintypes.error:
                pass
        self.thread.join(5)

    def next(self, timeout=15):
        return self.events.get(timeout=timeout)

    def drain(self, seconds):
        time.sleep(seconds)
        items = []
        while not self.events.empty():
            items.append(self.events.get())
        return items


class Desktop:
    def __init__(self, exe, config, prefs):
        env = dict(os.environ, LOTUS_DESKTOP_PREFERENCES=str(prefs))
        self.process = subprocess.Popen([exe, "--mcp-stdio", str(config)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, encoding="utf-8", env=env)
        self.messages = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.request = 0
        self.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                 "clientInfo": {"name": "lotus-presence-test", "version": "1"}})
        self.call("notifications/initialized", notify=True)

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

    def tool(self, name, **arguments):
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), result
        return json.loads(result["content"][0]["text"])

    def close(self):
        """Close the window like a user would (WM_CLOSE), so shutdown paths run."""
        import ctypes
        import ctypes.wintypes as wt
        user32 = ctypes.windll.user32
        found = []

        @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
        def visit(hwnd, _):
            owner = wt.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            title = ctypes.create_unicode_buffer(64)
            user32.GetWindowTextW(hwnd, title, 64)
            if owner.value == self.process.pid and title.value == "LOTUS":
                found.append(hwnd)
            return True

        user32.EnumWindows(visit, 0)
        assert found, "no LOTUS window"
        user32.PostMessageW(found[0], 0x0010, 0, 0)  # WM_CLOSE
        self.process.wait(timeout=30)


def expect(server, details, state=None):
    kind, activity = server.next()
    while kind == "connect":
        kind, activity = server.next()
    assert kind == "activity", (kind, activity)
    if details is None:
        assert activity is None, activity
    else:
        assert activity and activity.get("details") == details, activity
        assert activity.get("state") == state, activity
    print("presence:", details, state or "", flush=True)


def main():
    exe, worker, fixture_a, fixture_b, out = sys.argv[1:6]
    exe = str(pathlib.Path(exe).resolve())
    out = pathlib.Path(out).resolve()
    try:
        win32file.CloseHandle(win32file.CreateFile(PIPE, win32file.GENERIC_READ, 0, None,
                                                   win32file.OPEN_EXISTING, 0, None))
        sys.exit("a Discord IPC pipe already exists; close Discord before running this test")
    except pywintypes.error:
        pass
    for name, source in (("a", fixture_a), ("b", fixture_b)):
        (out / name).mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, out / name / "game.3dsx")
    config = out / "host.json"
    config.write_text(json.dumps({
        "worker": str(pathlib.Path(worker).resolve()), "storage_root": str(out / "storage"),
        "allow_control": True, "prefer_vulkan": False,
        "assets": {"first": str(out / "a" / "game.3dsx"), "alias": str(out / "a" / "game.3dsx"),
                   "second": str(out / "b" / "game.3dsx")}}))
    prefs = out / "desktop.json"

    server = FakeDiscord()
    # Disabled (the default): never connects, even with a game running.
    prefs.write_text(json.dumps({"discord_presence": False, "discord_application_id": APP_ID}))
    desktop = Desktop(exe, config, prefs)
    session = desktop.tool("lotus_create_session", kind="secondary", asset="first")["session"]
    desktop.tool("lotus_control", session=session, operation="run")
    assert server.drain(4) == [] and server.connections == 0, "disabled presence connected"
    desktop.close()
    print("disabled: no connection", flush=True)

    prefs.write_text(json.dumps({"discord_presence": True, "discord_application_id": APP_ID}))
    desktop = Desktop(exe, config, prefs)
    expect(server, None)  # connected with nothing loaded: clear
    a = desktop.tool("lotus_create_session", kind="secondary", asset="first")["session"]
    expect(server, "Playing game.3dsx...")
    desktop.tool("lotus_control", session=a, operation="run")
    desktop.tool("lotus_control", session=a, operation="pause")
    assert server.drain(1.5) == [], "pause or run changed nothing visible"
    # Same canonical file through a second handle counts as the same game.
    b = desktop.tool("lotus_create_session", kind="secondary", asset="alias")["session"]
    expect(server, "Debugging game.3dsx...", "2 sessions")
    desktop.tool("lotus_load", session=b, asset="second")
    expect(server, "Debugging multiple games", "2 sessions")
    desktop.tool("lotus_control", session=b, operation="stop")
    expect(server, "Playing game.3dsx...")

    # Discord quits and comes back: LOTUS keeps going and reconnects.
    server.shutdown()
    desktop.tool("lotus_control", session=a, operation="run")
    time.sleep(1)
    status = desktop.tool("lotus_list_sessions")
    assert any(s["id"] == a for s in status), status
    before = server.connections
    server = FakeDiscord()
    kind, _ = server.next(timeout=70)
    assert kind == "connect", kind
    expect(server, "Playing game.3dsx...")
    assert server.drain(8) == [], "reconnect is followed by one update only"
    print("reconnected after", before, "earlier connection(s)", flush=True)

    desktop.tool("lotus_control", session=a, operation="stop")
    expect(server, None)
    c = desktop.tool("lotus_create_session", kind="secondary", asset="second")["session"]
    expect(server, "Playing game.3dsx...")
    desktop.close()
    expect(server, None)  # shutdown clears
    kind, _ = server.next()
    assert kind == "close", kind
    server.shutdown()
    print("Discord presence: disabled, policy, dedupe, reconnect, shutdown clear: passed")


if __name__ == "__main__":
    main()
