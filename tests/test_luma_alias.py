"""Luma bit-31 alias routing on a real worker (lotus#155).

python tests/test_luma_alias.py <output> <fixture.3dsx> <worker.exe>... [--vulkan] [--baseline]

Builds nothing: pass target/<dir>/alias.3dsx from
`python tools/build-debug-fixture.py <dir> --fixture alias`, with alias.elf beside it.
Each worker runs the fixture on Old and New 3DS models. The test reads the fixture's results
with read_memory and requires one rejection log line for each invalid access, with the guest
PC inside the fixture's text. --baseline expects the unfixed worker to abort at the reported
address instead.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from native_debug_probe import Peer  # noqa: E402

DONE = 0x454E4F44
NM = Path("C:/devkitPro/devkitARM/bin/arm-none-eabi-nm.exe")
OUTSIDE = "not FCRAM or the I/O area"
UNEMULATED = "I/O address outside the emulated LCD/GPU registers"
REJECTION = re.compile(r"Luma alias (Read|Write)(\d+)(?: 0x([0-9A-F]+))? @ 0x([0-9A-F]{8}) "
                       r"\(PA 0x([0-9A-F]{8})\) at PC 0x([0-9A-F]{8}) rejected: (.+) \(rejection (\d+)\)")


def symbols(elf):
    nm = str(NM) if NM.exists() else "llvm-nm"
    table = {}
    for line in subprocess.check_output([nm, str(elf)], text=True).splitlines():
        parts = line.split()
        if len(parts) == 3:
            table[parts[2]] = int(parts[0], 16)
    return table


def expected_rejections(new_3ds):
    """(operation, bits, VA) -> reason for every access the fixture makes that must be refused."""
    expected = {
        ("Write", 32, 0x986483CA): OUTSIDE,  # the reported abort
        ("Read", 32, 0x986483CA): OUTSIDE,
        ("Read", 32, 0x900FFFFC): OUTSIDE,
        ("Read", 32, 0x90100000): UNEMULATED,
        ("Read", 32, 0x904FFFFC): UNEMULATED,
        ("Read", 32, 0x90500000): OUTSIDE,
        ("Read", 16, 0x90400000): "I/O register access is not 32-bit",
        ("Read", 32, 0x90400002): "misaligned I/O register access",
        # Dynarmic hands the unaligned LDR to Read32 whole.
        ("Read", 32, 0x904FFFFE): "crosses the end of the I/O area",
    }
    if not new_3ds:
        expected[("Read", 32, 0xA8000000)] = "beyond this model's FCRAM"
        expected[("Read", 32, 0xAFFFFFFC)] = "beyond this model's FCRAM"
    return expected


def run(worker, fixture, root, new_3ds, vulkan, baseline):
    peer = Peer(worker, root)
    try:
        hello = peer.call("hello")
        peer.debug("settings_set", name="is_new_3ds", value=new_3ds)
        peer.debug("settings_set", name="graphics_api", value=2 if vulkan else 0)
        peer.call("load", {"path": str(fixture)})
        try:
            peer.call("run_until", {"frames": 30})
        except (EOFError, OSError, AssertionError) as error:
            if not baseline:
                raise
            peer.process.wait(timeout=10)
            return {"source": hello["source"], "aborted": True, "exit_code": peer.process.returncode,
                    "error": str(error)}
        assert not baseline, "the baseline worker completed the fixture"
        table = symbols(fixture.with_suffix(".elf"))

        def word(name, index=0):
            data = peer.call("read_memory", {"address": table[name] + 4 * index, "length": 4})["bytes"]
            return int.from_bytes(bytes(data), "little")

        state = {"done": word("done"), "step": word("step"), "fcram_pa": word("fcram_pa"),
                 "values": [word("values", index) for index in range(24)]}
        status = peer.call("status")
        return {"source": hello["source"], "aborted": False, "state": state,
                "text": [0x100000, table["_start"] + 0x1000], "status_last_error": status["last_error"]}
    finally:
        peer.close()


def check(result, root, new_3ds):
    state = result["state"]
    values = state["values"]
    assert state["done"] == DONE, state
    pa = state["fcram_pa"]
    assert pa and pa % 0x1000 == 0 and 0x20000000 <= pa < 0x28000000, hex(pa)
    assert values[1] == pa
    assert values[2] == 0x12345678, "alias read sees a VA write"
    assert values[3] == 0xCAFEF00D, "VA read sees an alias write"
    assert values[4] == 0x4C and values[5] == 0x414D, "8- and 16-bit FCRAM aliases"
    # Rejected reads return 0; on New 3DS steps 7 and 8 read real FCRAM.
    for step in (10, 11, 12, 13, 14, 16, 17) + (() if new_3ds else (7, 8)):
        assert values[step] == 0, (step, hex(values[step]))
    assert values[18] == 0x01020304 and values[19] == 0x11111111 and values[20] == 0x22222222
    assert values[21] == (1 << 9) | 2, hex(values[21])  # trigger cleared, finished set
    assert values[22] == 0 and values[23] == 0xA5A5A5A5, "memory fill reached VRAM"

    log = (root / "log" / "azahar_log.txt").read_text(errors="replace")
    assert "Unreachable" not in log and "unknown GPU address" not in log
    seen = {}
    low, high = result["text"]
    for match in REJECTION.finditer(log):
        operation, bits, value, va, pa_text, pc, reason, _ = match.groups()
        va, pc = int(va, 16), int(pc, 16)
        assert int(pa_text, 16) == va & 0x7FFFFFFF
        assert low <= pc < high, f"guest PC 0x{pc:08X} outside fixture text"
        if operation == "Write":
            assert value == "5A5A5A5A", value
        seen[(operation, int(bits), va)] = reason
    expected = expected_rejections(new_3ds)
    assert seen == expected, {"unexpected": {k: v for k, v in seen.items() if expected.get(k) != v},
                              "missing": {k: v for k, v in expected.items() if seen.get(k) != v}}
    for operation, bits, va in expected:
        assert f"unmapped {operation}{bits}" in log and f"@ 0x{va:08X}" in log, (operation, bits, hex(va))
    return {"rejections": len(seen)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("workers", type=Path, nargs="+")
    parser.add_argument("--vulkan", action="store_true")
    parser.add_argument("--baseline", action="store_true")
    args = parser.parse_args()
    fixture = args.fixture.resolve()
    report = []
    for index, worker in enumerate(args.workers):
        for new_3ds in (False, True):
            root = args.output.resolve() / f"w{index}-{'n3ds' if new_3ds else 'o3ds'}"
            result = run(worker.resolve(), fixture, root, new_3ds, args.vulkan, args.baseline)
            result.update(worker=str(worker), new_3ds=new_3ds, renderer="vulkan" if args.vulkan else "software")
            if args.baseline:
                log = (root / "log" / "azahar_log.txt").read_text(errors="replace")
                assert result["aborted"] and result["exit_code"] != 0, result
                fatal = [line for line in log.splitlines() if "unknown GPU address" in line]
                assert fatal and "0X271483CA" in fatal[-1].upper(), fatal
                result["fatal"] = fatal[-1].strip()
            else:
                result.update(check(result, root, new_3ds))
            report.append(result)
            print(json.dumps(result))
    (args.output / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
