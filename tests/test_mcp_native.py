"""Opt-in end-to-end stdio smoke: python tests/test_mcp_native.py <server> <config> <asset-handle>."""
import base64
import json
import queue
import struct
import subprocess
import sys
import threading
import time


def main():
    server, config, asset = sys.argv[1:]
    messages = queue.Queue()
    flags = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
    with subprocess.Popen([server, config], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, text=True, encoding="utf-8",
                          creationflags=flags) as process:
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
                assert reply is not None, "server exited early"
                if reply.get("id") == request_id:
                    assert "error" not in reply, reply
                    return reply["result"]

        def tool(name, arguments):
            result = send("tools/call", {"name": name, "arguments": arguments})
            assert not result.get("isError", False), result
            return result

        try:
            send("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                "clientInfo": {"name": "lotus-local-acceptance", "version": "1"}})
            send("notifications/initialized", notification=True)
            names = {entry["name"] for entry in send("tools/list")["tools"]}
            assert {"lotus_create_session", "lotus_run_until", "lotus_capture",
                    "lotus_capture_image", "lotus_input"} <= names
            created = tool("lotus_create_session", {"asset": asset})
            session = json.loads(created["content"][0]["text"])["session"]
            tool("lotus_run_until", {"session": session, "frames": 60})
            for scale in (1, 2):
                arguments = {"session": session}
                if scale != 1:
                    arguments["scale"] = scale
                start = time.perf_counter()
                content = tool("lotus_capture_image", arguments)["content"]
                elapsed = (time.perf_counter() - start) * 1000
                assert [block["type"] for block in content] == ["text", "image"] * 2
                sizes = []
                for screen, width, label, image in zip(
                        ("top", "bottom"), (400, 320), content[::2], content[1::2]):
                    dimensions = (width * scale, 240 * scale)
                    assert label["text"] == f"{screen} screen ({dimensions[0]} × {dimensions[1]})"
                    assert image["mimeType"] == "image/png"
                    png = base64.b64decode(image["data"], validate=True)
                    assert png.startswith(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR")
                    assert struct.unpack_from(">II", png, 16) == dimensions
                    sizes.append(len(png))
                print(f"inline PNG {scale}x: {elapsed:.1f} ms, {sizes} bytes")
            assert send("resources/list")["resources"] == []
            for scale in (0, 5):
                invalid = send("tools/call", {"name": "lotus_capture_image",
                               "arguments": {"session": session, "scale": scale}})
                assert invalid.get("isError")
            start = time.perf_counter()
            capture = tool("lotus_capture", {"session": session})
            elapsed = (time.perf_counter() - start) * 1000
            assert len(capture["content"]) == 2
            for link in capture["content"]:
                assert link["type"] == "resource_link"
                resource = send("resources/read", {"uri": link["uri"]})
                data = base64.b64decode(resource["contents"][0]["blob"], validate=True)
                assert len(data) == link["size"]
            assert len(send("resources/list")["resources"]) == 2
            print(f"raw capture 1x: {elapsed:.1f} ms for links, "
                  f"{[link['size'] for link in capture['content']]} bytes")
            denied = send("tools/call", {"name": "lotus_create_session", "arguments": {"asset": "../../arbitrary.3dsx"}})
            assert denied.get("isError")
            tool("lotus_control", {"session": session, "operation": "stop"})
            process.stdin.close()
            process.wait(timeout=5)
            assert process.returncode == 0
            print("MCP stdio/native lifecycle/resources/permissions: passed")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
