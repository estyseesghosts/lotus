"""Run a real ARM GDB client through the Rust server on both system profiles."""
import argparse
import json
from pathlib import Path
import queue
import subprocess
import threading

parser = argparse.ArgumentParser(__doc__)
parser.add_argument("gdb", type=Path)
parser.add_argument("cli", type=Path)
parser.add_argument("worker", type=Path)
parser.add_argument("rom", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
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
        commands = ["set pagination off","set confirm off",f"set remotelogfile {(root / 'rsp.log').as_posix()}","set debug remote 1",f"file {args.rom.with_suffix('.elf').resolve().as_posix()}",
            f"target remote {address}","break main","continue","stepi","stepi","break lotus_tick_body",
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
