"""Secondary copies of a card in use match one guest timeline point (#28).

The owned filesystem fixture in churn mode keeps sdmc:/lotus-churn.bin open and rewrites
all 256 words with an incrementing generation, without FS_WRITE_FLUSH. Checks:

1. Observation, not a pass condition: the card read directly at a frame boundary. It can
   differ from the guest's last completed generation while an FSFILE_Write still runs on a
   host thread (HLERequestContext::RunAsync); otherwise it matches, because File::Write
   calls GetSize, whose seek flushes stdio (src/common/file_util.cpp, GetSize(FILE*)).
2. A secondary copied from the paused primary holds one whole generation, the last one the
   guest completed or the one it just wrote. A pending guest FS operation refuses the copy;
   the test then advances one frame and retries.
3. Five secondaries copied while the primary runs each hold one whole generation (all
   words equal, non-zero), and the primary keeps running afterwards.
4. Three forks from one paused checkpoint hold byte-identical files and the source's
   guest counters in memory; the file is the generation the source completed or issued.

Usage: test_mcp_card_clone.py <abs lotus-mcp.exe> <abs CITnLO worker> <abs filesystem.3dsx>
       <abs filesystem.elf> <abs output dir>
"""
import json
from pathlib import Path
import struct
import sys
from mcp_client import Client

server, worker, rom, elf, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
storage = output / "storage"
config = output / "host.json"
config.write_text(json.dumps({"worker": worker, "storage_root": str(storage), "allow_control": True,
                              "max_sessions": 8, "assets": {"fs": rom, "fs-elf": elf},
                              "profile": {"graphics_api": 0}}))
client = Client(server, config, output / "transcript.jsonl")
report = {}


def debug(session, operation, **params):
    return client.tool("lotus_debug", {"session": session, "command": {"operation": operation, **params}})


def u32(session, address):
    return struct.unpack("<I", bytes(debug(session, "read_memory", address=address, length=4)["bytes"]))[0]


def words(path):
    data = Path(path).read_bytes()
    assert len(data) == 1024, (path, len(data))
    return struct.unpack("<256I", data)


def generation_of(path):
    values = set(words(path))
    assert len(values) == 1, (path, sorted(values)[:4])
    value = values.pop()
    assert value != 0, path
    return value


def clone_root(session):
    return storage.resolve() / "scratch" / session / "sdmc" / "lotus-churn.bin"


def create_clone(retry_paused):
    """Create a secondary copied from the card; while paused, retry over pending FS work."""
    for attempt in range(10):
        result = client.send("tools/call", {"name": "lotus_create_session",
                                            "arguments": {"kind": "secondary", "card": "churn"}})
        if not result.get("isError"):
            return json.loads(result["content"][0]["text"])["session"], attempt
        text = result["content"][0]["text"]
        assert retry_paused and "not quiescent" in text, text
        client.tool("lotus_run_until", {"session": primary, "frames": 1})
    raise AssertionError("guest FS work never settled")


try:
    primary = client.tool("lotus_create_session", {"kind": "primary", "card": "churn", "asset": "fs"})["session"]
    debug(primary, "load_symbols", asset="fs-elf")
    sym = {n: debug(primary, "resolve_symbol", query=n)["addresses"][0]
           for n in ("stage", "churn", "generation", "generation_done", "churn_result")}
    for _ in range(120):
        client.tool("lotus_run_until", {"session": primary, "frames": 1})
        if u32(primary, sym["stage"]) == 2:
            break
    assert u32(primary, sym["stage"]) == 2
    debug(primary, "write_memory", address=sym["churn"], bytes=list(struct.pack("<I", 1)))
    client.tool("lotus_run_until", {"session": primary, "frames": 30})
    assert u32(primary, sym["churn_result"]) == 0, hex(u32(primary, sym["churn_result"]))

    # 1. Observation: the card read directly at a frame boundary, without the supervisor.
    done = u32(primary, sym["generation_done"])
    card_file = storage.resolve() / "cards" / "churn" / "sdmc" / "lotus-churn.bin"
    on_disk = set(struct.unpack("<256I", card_file.read_bytes().ljust(1024, b"\0")))
    report["direct_read"] = {"guest_generation_done": done, "disk_values": sorted(on_disk)[:4],
                             "matches_guest": on_disk == {done}}

    # 2. Paused primary: the copy is one whole generation at this instant. `generation` is
    # bumped before the write is issued and `generation_done` set after the guest thread runs
    # again, so with no write in flight the file holds one of the two.
    clone, retries = create_clone(retry_paused=True)
    done = u32(primary, sym["generation_done"])
    issued = u32(primary, sym["generation"])
    assert issued in (done, done + 1)
    copied = generation_of(clone_root(clone))
    report["paused_clone"] = {"guest_generation_done": done, "guest_generation": issued,
                              "copied": copied, "retries": retries}
    assert copied in (done, issued), report
    client.tool("lotus_control", {"session": clone, "operation": "stop"})

    # 3. Running primary: each copy is one whole generation and the primary keeps running.
    client.tool("lotus_control", {"session": primary, "operation": "run"})
    seen = []
    for _ in range(5):
        clone, _ = create_clone(retry_paused=False)
        seen.append(generation_of(clone_root(clone)))
        client.tool("lotus_control", {"session": clone, "operation": "stop"})
    status = client.tool("lotus_status", {})
    row = next(s for s in status["sessions"] if s["id"] == primary)
    report["running_clones"] = {"generations": seen, "primary_playing": row["telemetry"]["playing"]}
    assert row["telemetry"]["playing"], report
    assert seen == sorted(seen) and len(set(seen)) == len(seen), report

    # 4. Forks from one paused checkpoint are identical on disk and in memory.
    client.tool("lotus_control", {"session": primary, "operation": "pause"})
    for _ in range(10):
        fork = client.send("tools/call", {"name": "lotus_debug", "arguments": {
            "session": primary, "command": {"operation": "fork", "count": 3}}})
        if not fork.get("isError"):
            break
        client.tool("lotus_run_until", {"session": primary, "frames": 1})
    forks = [child["id"] for child in json.loads(fork["content"][0]["text"])["forks"]]
    done = u32(primary, sym["generation_done"])
    issued = u32(primary, sym["generation"])
    files = {f: clone_root(f).read_bytes() for f in forks}
    memory = {f: (u32(f, sym["generation_done"]), u32(f, sym["generation"])) for f in forks}
    disk = {f: generation_of(clone_root(f)) for f in forks}
    report["forks"] = {"source_generation_done": done, "source_generation": issued,
                       "memory": memory, "disk": disk}
    assert len(set(files.values())) == 1, report
    assert set(memory.values()) == {(done, issued)}, report
    assert set(disk.values()) <= {done, issued} and len(set(disk.values())) == 1, report
    for session in forks + [primary]:
        client.tool("lotus_control", {"session": session, "operation": "stop"})
    client.close()
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
except BaseException:
    (output / "acceptance.json").write_text(json.dumps(report, indent=2))
    client.process.kill()
    raise
