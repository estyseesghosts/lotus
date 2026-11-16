"""MCP-only forked guest filesystem write isolation with the owned FS fixture.

The parent writes sdmc:/before.txt, forks two children, then the parent and
each child write different bytes to sdmc:/shared.txt. Each session root must
hold its own shared.txt; before.txt is inherited by children through the fork
copy; no child write reaches the parent or a sibling.
"""
import json
from pathlib import Path
import struct
import sys
from mcp_client import Client

server, config, rom, elf, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
storage = Path(json.loads(Path(config).read_text())["storage_root"])
client = Client(server, config, output / "transcript.jsonl")


def debug(session, operation, **params):
    return client.tool("lotus_debug", {"session": session, "command": {"operation": operation, **params}})


def u32(session, address):
    return struct.unpack("<I", bytes(debug(session, "read_memory", address=address, length=4)["bytes"]))[0]


def run(session, frames=1):
    client.tool("lotus_run_until", {"session": session, "frames": frames})


def write(session, sym, path, payload):
    debug(session, "write_memory", address=sym["path"], bytes=list(path.encode() + b"\0"))
    debug(session, "write_memory", address=sym["payload"], bytes=list(payload))
    debug(session, "write_memory", address=sym["payload_size"], bytes=list(struct.pack("<I", len(payload))))
    request = u32(session, sym["request"]) + 1
    debug(session, "write_memory", address=sym["request"], bytes=list(struct.pack("<I", request)))
    for _ in range(60):
        run(session)
        if u32(session, sym["done"]) == request:
            assert u32(session, sym["open_result"]) == 0, hex(u32(session, sym["open_result"]))
            return
    raise AssertionError((session, path))


def files(name):
    return {p.parent.parent.name if p.parent.name == "sdmc" else str(p): p.read_bytes()
            for p in storage.rglob(name) if p.parent.name == "sdmc"}


try:
    for new in (False, True):
        before_shared = set(files("shared.txt"))
        parent = client.tool("lotus_create_session", {"kind": "primary", "card": "fs-new" if new else "fs-old"})["session"]
        debug(parent, "settings_set", name="is_new_3ds", value=new)
        client.tool("lotus_load", {"session": parent, "asset": rom})
        debug(parent, "load_symbols", asset=elf)
        sym = {n: debug(parent, "resolve_symbol", query=n)["addresses"][0]
               for n in ("path", "payload", "payload_size", "request", "done", "open_result", "stage")}
        for _ in range(120):
            run(parent)
            if u32(parent, sym["stage"]) == 2:
                break
        assert u32(parent, sym["stage"]) == 2
        tag = b"new" if new else b"old"
        write(parent, sym, "/before.txt", b"before-" + tag)
        forks = [child["id"] for child in debug(parent, "fork", count=2)["forks"]]
        payloads = {parent: b"parent-" + tag, forks[0]: b"child0-" + tag, forks[1]: b"child1-" + tag}
        for session, payload in payloads.items():
            write(session, sym, "/shared.txt", payload)
        shared = {k: v for k, v in files("shared.txt").items() if k not in before_shared}
        assert sorted(shared.values()) == sorted(payloads.values()), shared
        assert len(shared) == 3, shared
        inherited = [v for v in files("before.txt").values() if v == b"before-" + tag]
        assert len(inherited) == 3, files("before.txt")
        for child in forks:
            client.tool("lotus_control", {"session": child, "operation": "stop"})
        client.tool("lotus_control", {"session": parent, "operation": "stop"})
        print(f"{'New' if new else 'Old'} 3DS forked SDMC writes isolated across 3 roots", flush=True)
finally:
    client.close()
