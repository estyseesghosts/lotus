"""Build the owned freestanding ARM fixture with clang, or devkitARM GCC.
The 3DSX segment container follows native loader/3dsx.cpp. Absolute link addresses
match the worker's homebrew base; relocation tables are intentionally empty.
"""
from pathlib import Path
import argparse
import hashlib
import json
import shutil
import struct
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("output", type=Path)
parser.add_argument("--fixture",choices=["debugger","audio","microphone","dsp","peripherals","filesystem","divergence","source"],default="debugger")
parser.add_argument("--gcc", help="devkitARM arm-none-eabi-gcc executable (uses matching objcopy)")
parser.add_argument("--define", action="append", default=[], help="preprocessor macro for a fixture variant")
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=True)
fixtures = Path(__file__).resolve().parents[1] / "fixtures"
source = fixtures / args.fixture
compiler = args.gcc or shutil.which("clang")
if not compiler:
    raise SystemExit("clang or --gcc <devkitARM GCC> is required")
objcopy = str(Path(compiler).with_name("arm-none-eabi-objcopy.exe")) if args.gcc else shutil.which("llvm-objcopy")
elf = out / (args.fixture+".elf")
command = [compiler, "-mcpu=mpcore", "-marm", "-mfloat-abi=soft", "-O1", "-g", "-gdwarf-4",
           "-ffreestanding", "-fno-builtin", "-fno-omit-frame-pointer", "-fno-unwind-tables",
           "-fno-asynchronous-unwind-tables", "-nostdlib", "-Wl,--build-id=none",
           "-Wl,-T,"+str(fixtures / "debugger/link.ld"), str(next(source.glob("fixture.c*"))), "-o", str(elf)]
if args.fixture == "divergence":  # VFP probes; other fixtures keep their soft-float builds
    command[command.index("-mfloat-abi=soft")] = "-mfloat-abi=softfp"
    command.insert(1, "-mfpu=vfpv2")
if args.fixture == "source":  # lotus#77: C++, one row per statement, DWARF 5 file MD5s
    command[command.index("-O1")] = "-O0"
    command[command.index("-gdwarf-4")] = "-gdwarf-5"
    command[1:1] = ["-fno-exceptions", "-fno-rtti"]
command[1:1] = ["-D"+name for name in args.define]
if args.gcc:  # devkitARM defaults to -mtp=cp15, which GCC rejects for 16-bit Thumb; fixtures use no TLS
    command.insert(1, "-mtp=soft")
if not args.gcc:
    command[1:1] = ["--target=armv6k-none-eabi", "-fuse-ld=lld"]
subprocess.run(command, check=True)
segments = []
for section in ("text", "rodata", "data"):
    path = out / (section + ".bin")
    subprocess.run([objcopy, "--dump-section", "."+section+"="+str(path), str(elf)], check=True)
    segments.append(path.read_bytes())
header = struct.pack("<IHHIIIIIIIII", 0x58534433, 44, 8, 0, 0,
                     *(len(s) for s in segments), 0, 0, 0, 0)
rom = header + bytes(24) + b"".join(segments)
(out / (args.fixture+".3dsx")).write_bytes(rom)
(out / "build.json").write_text(json.dumps(dict(compiler=subprocess.check_output([compiler,"--version"],text=True).splitlines()[0],
    command=command, sha256=hashlib.sha256(rom).hexdigest(), elf_sha256=hashlib.sha256(elf.read_bytes()).hexdigest()),indent=2))
print(out / (args.fixture+".3dsx"))
