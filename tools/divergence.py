"""First divergent guest state across identical runs (lotus#29). CITnLO workers (`state_hash`).

Boots `--runs` workers with the same ROM and settings (renderer, model, init_clock=FixedTime,
frame_limit), advances them in lockstep with `run_until` of `--step` frames, and after every
step compares each run's `state_hash` with run 0: guest ticks, per-core context, PC, thread and
timer ticks, FCRAM/VRAM/DSP/extra-RAM chunk hashes, async HLE state, and the hash of each
top-level System::serialize section. The first frame with any difference is reported with the
differing components (memory as physical address ranges); stepping continues for `--after`
more checkpoints to show how the difference spreads, then stops.

`--concurrent` runs the workers' steps in parallel threads (contention); otherwise one worker
steps at a time. `--load` adds busy workers that keep running the same ROM unthrottled (reissuing 600-frame
runs), to test host contention without hashing them.

    python tools/divergence.py <ctr-worker.exe> <abs rom> runs/<fresh> [--runs 4] [--frames 600]
        [--step 1] [--renderer software|vulkan] [--new-3ds] [--frame-limit 0] [--concurrent]
        [--load 0] [--deterministic-async | --host-async] [--same-identity] [--no-sections] [--poke SYMBOL=VALUE] [--chunk-kib 1024] [--after 5] [--repeat 1]

Writes report.json (every checkpoint's differences and the hashes at the first divergence) and
summary.md. Exit status 0 when all runs agree to the end, 3 when they diverge.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import shutil
import struct
import threading

sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_debug_probe import Peer  # noqa: E402
import bench  # noqa: E402


def boot(worker, rom, root, args, identity=None):
    peer = Peer(worker, root)
    if identity is not None:
        # CFG reads these at load (cfg.cpp GetMacAddress, config savefile); copying them before
        # load gives every run the template's MAC and console unique ID.
        for name in IDENTITY:
            shutil.copytree(identity / name, root / name, dirs_exist_ok=True)
    peer.call("hello")
    peer.debug("settings_set", name="graphics_api", value=bench.RENDERERS[args.renderer])
    peer.debug("settings_set", name="is_new_3ds", value=args.new_3ds)
    peer.debug("settings_set", name="init_clock", value=bench.INIT_CLOCK_FIXED)
    peer.debug("settings_set", name="frame_limit", value=args.frame_limit)
    if args.deterministic_async or args.host_async:  # unset keeps the worker's DefaultProfile value
        peer.debug("settings_set", name="deterministic_async_operations", value=args.deterministic_async)
    peer.call("load", {"path": str(rom)})
    for address, value in args.pokes:
        peer.debug("write_memory", address=address, bytes=list(struct.pack("<I", value)))
    return peer


# A fresh root gets a random MAC (cfg.cpp GenerateRandomMAC, also in the shared page) and a
# random console unique ID (GenerateConsoleUniqueId) on first load.
IDENTITY = ("sysdata", "nand")


def template(worker, rom, root, args):
    peer = boot(worker, rom, root, args)
    peer.close()
    return root


def state(peer, args):
    status = peer.call("status")
    hashed = peer.debug("state_hash", chunk_kib=args.chunk_kib, sections=not args.no_sections)
    hashed["frames"] = status["frames"]
    return hashed


def differences(a, b):
    """Components of `b` that differ from `a`."""
    found = []
    for key in ("frames", "guest_ticks", "async_pending"):
        if a.get(key) != b.get(key):
            found.append(dict(component=key, a=a.get(key), b=b.get(key)))
    for ca, cb in zip(a["cpu"], b["cpu"]):
        for key in ("pc", "thread", "ticks", "idle_ticks", "context"):
            if ca[key] != cb[key]:
                found.append(dict(component=f"cpu{ca['core']}.{key}", a=ca[key], b=cb[key]))
    for name, ra in a["memory"].items():
        rb = b["memory"].get(name)
        if rb is None or len(rb["chunks"]) != len(ra["chunks"]):
            found.append(dict(component=f"memory.{name}", a="present", b="layout differs"))
            continue
        chunk = a["chunk_kib"] << 10
        diff = [i for i, (x, y) in enumerate(zip(ra["chunks"], rb["chunks"])) if x != y]
        if diff:
            found.append(dict(component=f"memory.{name}", chunks=len(diff),
                              ranges=[f"{ra['base'] + i * chunk:#010x}-{ra['base'] + (i + 1) * chunk:#010x}"
                                      for i in diff[:16]]))
    sa, sb = a.get("sections"), b.get("sections")
    if sa is not None and sb is not None:
        for x, y in zip(sa, sb):
            if x["name"] != y["name"]:
                found.append(dict(component="sections", a=x["name"], b=y["name"]))
                break
            if x["hash"] != y["hash"]:
                found.append(dict(component=f"section.{x['name']}", a_bytes=x["bytes"], b_bytes=y["bytes"]))
    return found


def printable(data):
    return "".join(chr(c) if 32 <= c < 127 else "." for c in data)


def byte_differences(peers, states, diffs, frame):
    """Dump the serialized state of run 0 and every differing run, and list the differing byte
    ranges of each differing section with the printable bytes before them (names, paths)."""
    dumps = {}
    for index in [0, *diffs]:
        dumps[index] = Path(peers[index].debug("state_hash", sections=True, dump=f"f{frame}")["dump"])
    base = dumps[0].read_bytes()
    marks = json.loads(dumps[0].with_suffix(".json").read_text())
    found = {}
    for index in diffs:
        other = dumps[index].read_bytes()
        ranges, offset = [], 0
        for mark in marks:
            end = offset + mark["bytes"]
            a, b = base[offset:end], other[offset:end]
            if a != b:
                position = 0
                while position < len(a) and len(ranges) < 64:
                    if a[position] == b[position]:
                        position += 1
                        continue
                    stop = position
                    while stop < len(a) and (a[stop] != b[stop] or a[stop:stop + 8] != b[stop:stop + 8]):
                        stop += 1
                    ranges.append(dict(section=mark["name"], offset=position, length=stop - position,
                                       a=a[position:stop][:32].hex(), b=b[position:stop][:32].hex(),
                                       context=printable(a[max(0, position - 96):position])))
                    position = stop
            offset = end
        found[index] = ranges
    for path in dumps.values():
        path.unlink()
    return found


def run(worker, rom, out, args, repeat):
    peers, load, spinners, stop = [], [], [], threading.Event()
    identity = template(worker, rom, out / f"r{repeat}" / "template", args) if args.same_identity else None
    try:
        for index in range(args.runs):
            peers.append(boot(worker, rom, out / f"r{repeat}" / f"w{index}", args, identity))
        for index in range(args.load):
            busy = boot(worker, rom, out / f"r{repeat}" / f"load{index}", args)
            busy.debug("settings_set", name="frame_limit", value=0)
            load.append(busy)
            # A worker run ends after at most 600 frames or 20 s (control.h `run`), so keep reissuing.
            spinners.append(threading.Thread(target=spin, args=(busy, stop)))
            spinners[-1].start()
        checkpoints, first, remaining = [], None, None
        done = 0
        while done < args.frames and remaining != 0:
            step = min(args.step, args.frames - done)
            if args.concurrent:
                threads = [threading.Thread(target=p.call, args=("run_until", {"frames": step})) for p in peers]
                for t in threads:
                    t.start()
                for t in threads:
                    t.join()
            else:
                for peer in peers:
                    peer.call("run_until", {"frames": step})
            done += step
            states = [state(p, args) for p in peers]
            diffs = {i: differences(states[0], s) for i, s in enumerate(states[1:], 1)}
            diffs = {i: d for i, d in diffs.items() if d}
            entry = dict(frame=done, async_pending=[s["async_pending"] for s in states],
                         differing_runs=sorted(diffs), differences=diffs)
            checkpoints.append(entry)
            if diffs and first is None:
                first = dict(frame=done, states=states)
                if not args.no_sections and not any(s["async_pending"] for s in states):
                    first["bytes"] = byte_differences(peers, states, sorted(diffs), done)
                remaining = args.after
                print(json.dumps(dict(repeat=repeat, first_divergence=done, differences=diffs))[:4000], flush=True)
            elif remaining is not None:
                remaining -= 1
            if done % 60 == 0:
                print(f"repeat {repeat} frame {done}: {'diverged' if first else 'identical'}", flush=True)
        screens = []
        for peer in peers:
            leases = peer.call("capture")["frames"]
            screens.append(hashlib.sha256(b"".join((peer.root / lease["path"]).read_bytes()
                                                   for lease in leases)).hexdigest())
        print(f"repeat {repeat} final screens: {len(set(screens))} distinct", flush=True)
        return dict(checkpoints=checkpoints, first=first, frames=done, screens=screens)
    finally:
        stop.set()
        for spinner in spinners:
            spinner.join()
        for peer in peers + load:
            peer.close()


def spin(peer, stop):
    while not stop.is_set():
        peer.call("run_until", {"frames": 600}, expected=None)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("worker", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--runs", type=int, default=4)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--step", type=int, default=1)
    parser.add_argument("--renderer", choices=sorted(bench.RENDERERS), default="software")
    parser.add_argument("--new-3ds", action="store_true")
    parser.add_argument("--frame-limit", type=int, default=0, help="percent; 0 is unthrottled")
    parser.add_argument("--concurrent", action="store_true")
    parser.add_argument("--load", type=int, default=0)
    parser.add_argument("--no-sections", action="store_true")
    parser.add_argument("--same-identity", action="store_true",
                        help="copy one template boot's sysdata and nand (MAC, console ID) into every run")
    parser.add_argument("--deterministic-async", action="store_true",
                        help="run HLE async sections on the emulation thread (hle_ipc.h RunAsync)")
    parser.add_argument("--host-async", action="store_true",
                        help="run them on host threads (upstream and AZAHAR default; CITnLO defaults to deterministic)")
    parser.add_argument("--chunk-kib", type=int, default=1024)
    parser.add_argument("--after", type=int, default=5)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--poke", action="append", default=[],
                        help="SYMBOL=VALUE: 32-bit write after load, symbol from the ELF beside the ROM")
    args = parser.parse_args()
    if args.deterministic_async and args.host_async:
        parser.error("--deterministic-async and --host-async are exclusive")
    worker, rom = args.worker.resolve(), args.rom.resolve()
    args.pokes = []
    if args.poke:
        from debugger_acceptance import symbols
        table = symbols(rom.with_suffix(".elf"))
        args.pokes = [(table[name], int(value, 0)) for name, value in (p.split("=", 1) for p in args.poke)]
    out = args.output
    out.mkdir(parents=True, exist_ok=False)
    report = dict(tool="tools/divergence.py", args={k: str(v) for k, v in vars(args).items()},
                  worker_sha256=hashlib.sha256(worker.read_bytes()).hexdigest(),
                  rom_sha256=hashlib.sha256(rom.read_bytes()).hexdigest(),
                  host=bench.host_metadata(), repeats=[])
    for repeat in range(args.repeat):
        report["repeats"].append(run(worker, rom, out, args, repeat))
    (out / "report.json").write_text(json.dumps(report, indent=1))
    lines = ["# Divergence", "", f"{args.runs} runs, {args.renderer}, new_3ds={args.new_3ds}, "
             f"frame_limit={args.frame_limit}, deterministic_async={"on" if args.deterministic_async else "off" if args.host_async else "worker default"}, same_identity={args.same_identity}, step={args.step}, concurrent={args.concurrent}, load={args.load}", ""]
    diverged = False
    for index, r in enumerate(report["repeats"]):
        if r["first"] is None:
            lines.append(f"- repeat {index}: identical through frame {r['frames']}; "
                         f"{len(set(r['screens']))} distinct final screen(s)")
            continue
        diverged = True
        first = next(c for c in r["checkpoints"] if c["differing_runs"])
        lines.append(f"- repeat {index}: first divergence at frame {first['frame']} "
                     f"(runs {first['differing_runs']} differ from run 0); "
                     f"{len(set(r['screens']))} distinct final screen(s)")
        for run_index, diffs in first["differences"].items():
            lines.append(f"  - run {run_index}: " + "; ".join(
                d["component"] + (f" {d['ranges'][:4]}" if "ranges" in d else "") for d in diffs))
        for run_index, ranges in (r["first"].get("bytes") or {}).items():
            for x in ranges[:12]:
                lines.append(f"    - run {run_index} {x['section']}+{x['offset']} ({x['length']} B) "
                             f"{x['a']} / {x['b']} after `{x['context'][-48:]}`")
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    print((out / "summary.md").read_text())
    raise SystemExit(3 if diverged else 0)


if __name__ == "__main__":
    main()
