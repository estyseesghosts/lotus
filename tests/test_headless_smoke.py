"""Headless smoke of release binaries: help text, mock worker protocol, MCP handshake.

Usage: python tests/test_headless_smoke.py target/release
Needs no display, ROM or native worker (#102).
"""
import json
import subprocess
import sys
from pathlib import Path

SUFFIX = ".exe" if sys.platform == "win32" else ""


def run(*command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (command, result.returncode, result.stderr)
    return result.stdout + result.stderr


def main():
    binaries = Path(sys.argv[1]).resolve()
    exe = lambda name: str(binaries / f"{name}{SUFFIX}")
    assert "Usage:" in run(exe("ctr-cli"), "--help")
    assert "usage: lotus-desktop" in run(exe("lotus-desktop"), "--help")
    for worker in ("lotus-mock-worker", "lotus-mock-native"):
        status = json.loads(run(exe("ctr-cli"), "mock", exe(worker)).strip().splitlines()[-1])
        assert isinstance(status, dict), status
    initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-03-26", "capabilities": {},
        "clientInfo": {"name": "lotus-headless-smoke", "version": "1"}}}
    stdin = json.dumps(initialize) + "\n" + '{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
    server = subprocess.run([exe("lotus-mcp")], input=stdin, capture_output=True, text=True, timeout=30)
    assert server.returncode == 0, server.stderr
    reply = json.loads(server.stdout.splitlines()[0])
    assert reply.get("id") == 1 and "result" in reply, reply
    print("Headless smoke (CLI, desktop --help, mock workers, MCP initialize): passed")


if __name__ == "__main__":
    main()
