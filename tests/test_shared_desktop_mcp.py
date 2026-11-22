"""Windows shared desktop acceptance with the owned mock worker.

Usage: python tests/test_shared_desktop_mcp.py <lotus-desktop.exe> <worker.exe> <output-dir> [fixture.3dsx]
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import ctypes
import ctypes.wintypes as wt
import json
import os
from pathlib import Path
import subprocess

from mcp_client import Client
from test_settings_window import foreground, key, until, user32, windows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("desktop", type=Path)
    parser.add_argument("worker", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("fixture", type=Path, nargs="?")
    parser.add_argument("--clients", type=int, choices=range(4, 65), default=8)
    parser.add_argument("--check-config-mismatch", action="store_true")
    args = parser.parse_args()
    desktop, worker, output = (path.resolve() for path in (args.desktop, args.worker, args.output))
    output.mkdir(parents=True, exist_ok=False)
    config = output / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(output / "storage"),
        "allow_control": True, "prefer_vulkan": False,
        "assets": {"owned": str(args.fixture.resolve())} if args.fixture else {},
    }), encoding="utf-8")
    os.environ["LOTUS_DESKTOP_PREFERENCES"] = str(output / "desktop.json")
    user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
    user32.IsIconic.argtypes = [wt.HWND]
    user32.ShowWindow.argtypes = [wt.HWND, ctypes.c_int]
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
    kernel.OpenProcess.restype = wt.HANDLE
    kernel.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
    kernel.CloseHandle.argtypes = [wt.HANDLE]

    def connect(index):
        return Client(desktop, config, output / f"client-{index}.jsonl",
                      arguments=["--mcp"], stderr_path=output / f"client-{index}.stderr")

    clients = []
    process_handle = None
    worker_handle = None
    hwnd = None

    def quit_window(window):
        user32.ShowWindow(window, 9)  # SW_RESTORE
        foreground(window)
        key(0x11); key(0x10); key(0x51)
        key(0x51, True); key(0x10, True); key(0x11, True)

    try:
        with ThreadPoolExecutor(max_workers=args.clients) as pool:
            futures = [pool.submit(connect, index) for index in range(args.clients)]
            for future in futures:
                clients.append(future.result())
        status = [client.tool("lotus_status", {}) for client in clients]
        (host_pid,) = {entry["frontend_pid"] for entry in status}
        assert all(entry["frontend"] == "windowed" for entry in status)
        assert all(client.process.pid != host_pid for client in clients)
        assert all(not windows(client.process.pid) for client in clients)
        process_handle = kernel.OpenProcess(0x00100000, False, host_pid)  # SYNCHRONIZE
        assert process_handle
        until("shared window visible", lambda: any(title == "lotus" for _, title in windows(host_pid)), 10)
        (hwnd,) = [handle for handle, title in windows(host_pid) if title == "lotus"]
        print(f"{args.clients} concurrent clients share one visible desktop, PID {host_pid}", flush=True)

        # A direct desktop launch joins the same singleton instead of opening a second window.
        duplicate = subprocess.run([desktop, "--mcp-host", config], capture_output=True, timeout=8)
        assert duplicate.returncode == 0 and b"already has a shared desktop" in duplicate.stderr

        if args.check_config_mismatch:
            read_only = output / "read-only.json"
            read_only.write_text(json.dumps(dict(json.loads(config.read_text()), allow_control=False)))
            mismatch = subprocess.run([desktop, "--mcp", read_only], input=b"", capture_output=True, timeout=25)
            assert mismatch.returncode != 0 and not mismatch.stdout
            assert b"same host configuration" in mismatch.stderr

        request = {"kind": "secondary"}
        if args.fixture:
            request["asset"] = "owned"
        session = clients[0].tool("lotus_create_session", request)["session"]
        own_session = lambda client: next(
            row for row in client.tool("lotus_list_sessions", {}) if row["id"] == session
        )
        worker_pid = own_session(clients[1])["telemetry"]["pid"]
        assert worker_pid
        worker_handle = kernel.OpenProcess(0x00100000, False, worker_pid)
        assert worker_handle
        assert user32.PostMessageW(hwnd, 0x10, 0, 0)  # WM_CLOSE
        until("Close minimizes the original window", lambda: user32.IsIconic(hwnd), 6)
        assert own_session(clients[2])["telemetry"]["pid"] == worker_pid

        # An agent killed without an orderly protocol shutdown loses its bridge only.
        disconnected = clients.pop()
        disconnected.process.kill()
        disconnected.process.wait(timeout=6)
        disconnected.transcript.close()
        assert own_session(clients[1])["telemetry"]["pid"] == worker_pid
        for client in clients:
            client.close()
        clients.clear()
        assert kernel.WaitForSingleObject(process_handle, 0) == 0x102  # WAIT_TIMEOUT

        clients.append(connect("reconnected"))
        status = clients[0].tool("lotus_status", {})
        assert status["frontend_pid"] == host_pid
        assert own_session(clients[0])["telemetry"]["pid"] == worker_pid
        assert user32.IsIconic(hwnd), "a reconnect raised the window"
        print(f"Close and all disconnects preserve session {session} and worker PID {worker_pid}", flush=True)

        quit_window(hwnd)
        assert kernel.WaitForSingleObject(process_handle, 12000) == 0, "Quit must stop the shared desktop"
        assert kernel.WaitForSingleObject(worker_handle, 0) == 0, "Quit left a worker behind"
        assert clients[0].process.wait(timeout=6) == 0, "bridge did not exit with the desktop"
        clients.pop().close()
        kernel.CloseHandle(process_handle)
        process_handle, hwnd = None, None

        clients.append(connect("fresh"))
        new_pid = clients[0].tool("lotus_status", {})["frontend_pid"]
        assert new_pid != host_pid
        process_handle = kernel.OpenProcess(0x00100000, False, new_pid)
        assert process_handle
        assert clients[0].tool("lotus_list_sessions", {}) == []
        until("fresh window visible", lambda: any(title == "lotus" for _, title in windows(new_pid)), 10)
        (hwnd,) = [handle for handle, title in windows(new_pid) if title == "lotus"]
        quit_window(hwnd)
        assert kernel.WaitForSingleObject(process_handle, 12000) == 0
        print("Quit stopped the host, worker and bridge. The next launch starts clean.", flush=True)
    finally:
        for client in clients:
            try:
                client.close()
            except (AssertionError, OSError, subprocess.TimeoutExpired):
                if client.process.poll() is None:
                    client.process.kill()
                    client.process.wait(timeout=6)
        if process_handle:
            if hwnd and kernel.WaitForSingleObject(process_handle, 0) == 0x102:
                quit_window(hwnd)
                kernel.WaitForSingleObject(process_handle, 12000)
            kernel.CloseHandle(process_handle)
        if worker_handle:
            kernel.CloseHandle(worker_handle)


if __name__ == "__main__":
    main()
