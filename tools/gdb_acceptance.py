"""Run a real ARM GDB client through the Rust server on both system profiles."""
import argparse
import json
from pathlib import Path
import re
import socket
import subprocess

parser = argparse.ArgumentParser(__doc__)
parser.add_argument("gdb", type=Path)
parser.add_argument("cli", type=Path)
parser.add_argument("worker", type=Path)
parser.add_argument("rom", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
# ADR-008: Azahar's INADDR_ANY stub must not be linked into the worker.
worker_bytes = args.worker.read_bytes()
for marker in (b"command_buffer overflow in SendReply", b"Failed to set non-blocking gdb socket"):
    assert marker not in worker_bytes, f"Azahar GDB stub marker {marker!r} present in worker"


def exposed(port):
    """Non-loopback host addresses that accept a TCP connection on the GDB port."""
    addresses = {info[4][0] for info in socket.getaddrinfo(socket.gethostname(), None)
                 if not info[4][0].startswith(("127.", "::1"))}
    accepted = []
    for address in sorted(addresses):
        try:
            with socket.create_connection((address.split("%")[0], int(port)), timeout=1):
                accepted.append(address)
        except OSError:
            pass
    return addresses, accepted

for new in (False,True):
    root = args.output.resolve() / ("new" if new else "old")
    root.mkdir(parents=True,exist_ok=False)
    config = root / "host.json"
    config.write_text(json.dumps(dict(worker=str(args.worker.resolve()),storage_root=str(root / "host"),
        assets={"rom":str(args.rom.resolve()),"elf":str(args.rom.with_suffix('.elf').resolve())},
        allow_control=True,profile={"is_new_3ds":new})))
    host = subprocess.Popen([str(args.cli.resolve()),"debug",str(config),"rom","--gdb","0"],
        stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        session = json.loads(host.stdout.readline())
        line = host.stderr.readline()
        assert line.startswith("GDB listening on "),line
        address = line.strip().split()[-1]
        port = address.rsplit(":", 1)[1]
        checked, accepted = exposed(port)
        assert checked and not accepted, ("GDB port reachable off loopback", accepted)
        commands = ["set pagination off","set confirm off",f"set remotelogfile {(root / 'rsp.log').as_posix()}","set debug remote 1",f"file {args.rom.with_suffix('.elf').resolve().as_posix()}",
            f"target remote {address}","break main","continue","info threads","info os processes",
            "monitor rewind on","stepi","p/x $pc","stepi","p/x $pc","reverse-stepi","p/x $pc","monitor rewind status",
            "stepi","stepi",
            "break thumb_tick","continue",'printf "thumb-cpsr=%x\\n", $cpsr & 0x20','printf "thumb-pc0=%x\\n", $pc',
            "stepi",'printf "thumb-pc1=%x\\n", $pc',"delete breakpoints","break lotus_tick_body",
            "continue","bt","delete breakpoints","watch watched","continue","p/x watched","stepi",
            "delete breakpoints","set variable mode = 5","continue","p hio.result","detach"]
        script = root / "acceptance.gdb"
        script.write_text("\n".join(commands)+"\n")
        result = subprocess.run([str(args.gdb.resolve()),"-nx","-batch","-x",str(script)],
            capture_output=True,text=True,timeout=45)
        (root / "gdb.stdout.log").write_text(result.stdout)
        (root / "gdb.stderr.log").write_text(result.stderr)
        assert result.returncode == 0,(result.stdout,result.stderr)
        assert "lotus-hio:hello" in result.stdout + result.stderr,(result.stdout,result.stderr)
        assert "failure reply" not in result.stderr,(result.stdout,result.stderr)
        assert "Hardware watchpoint" in result.stdout
        assert "#1" in result.stdout and "main" in result.stdout
        pcs = re.findall(r"^\$\d+ = (0x[0-9a-f]+)$", result.stdout, re.M)
        # stepi -> P1, stepi -> P2, reverse-stepi (GDB bs) must return to P1.
        assert len(pcs) >= 3 and pcs[0] != pcs[1] and pcs[2] == pcs[0], ("reverse-stepi", pcs)
        # Thumb: execute breakpoint (Z0 kind 2) stops in Thumb state; stepi advances 2 or 4 bytes.
        thumb = dict(re.findall(r"^(thumb-\w+)=([0-9a-f]+)$", result.stdout, re.M))
        assert thumb.get("thumb-cpsr") == "20", ("Thumb state", thumb)
        assert int(thumb["thumb-pc1"], 16) - int(thumb["thumb-pc0"], 16) in (2, 4), ("Thumb stepi", thumb)
        assert "Thread" in result.stdout, "info threads (qXfer:threads) listed nothing"
        # The same host's ELF/DWARF API resolves symbols and real source lines.
        def debug(command):
            host.stdin.write(json.dumps(command)+"\n");host.stdin.flush()
            reply = json.loads(host.stdout.readline())
            assert "error" not in reply,reply
            return reply
        debug({"operation":"load_symbols","asset":"elf"})
        addresses = debug({"operation":"resolve_symbol","query":"tick"})["addresses"]
        locations = debug({"operation":"symbolize","addresses":addresses})
        assert locations["locations"][0]["line"] > 0,locations
        (root / "symbols.json").write_text(json.dumps(locations,indent=2))
        print(json.dumps({"new_3ds":new,"gdb":"passed","source_lines":locations}),flush=True)
    finally:
        host.stdin.write("quit\n");host.stdin.flush()
        try:host.wait(timeout=10)
        except subprocess.TimeoutExpired:host.kill();host.wait()
