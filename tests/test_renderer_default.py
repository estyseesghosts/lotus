"""Issue #5: supervisor sessions default to Vulkan and fall back to software with a reason.

Through `ctr-cli debug` (shared supervisor), for one worker:
  default            no graphics_api in profile         -> renderer 2 (Vulkan), no fallback
  bad_device         profile physical_device=99          -> renderer 0, fallback reason recorded
  pinned_software    profile graphics_api=0              -> renderer 0, no fallback
  opt_out            prefer_vulkan=false                 -> renderer 0 (worker default), no fallback
Every case must load (the worker creates the renderer at load). Frame presentation on
Vulkan is covered by tools/vulkan_acceptance.py and tools/bench.py.

Usage: python tests/test_renderer_default.py <ctr-cli.exe> <ctr-worker.exe> <abs .3dsx> <fresh output>
"""
import json
from pathlib import Path
import subprocess
import sys

cli, worker, rom, output = (Path(arg).resolve() for arg in sys.argv[1:5])
output.mkdir(parents=True, exist_ok=False)
CASES = {
    "default": dict(),
    "bad_device": dict(profile={"physical_device": 99}),
    "pinned_software": dict(profile={"graphics_api": 0}),
    "opt_out": dict(prefer_vulkan=False),
}
EXPECTED = {"default": (2, False), "bad_device": (0, True), "pinned_software": (0, False), "opt_out": (0, False)}
report = {}
for name, extra in CASES.items():
    root = output / name
    root.mkdir()
    config = root / "host.json"
    config.write_text(json.dumps(dict(worker=str(worker), storage_root=str(root / "host"),
                                      assets={"rom": str(rom)}, allow_control=True, **extra)))
    commands = [{"operation": "registers", "core": 0}]
    result = subprocess.run([str(cli), "debug", str(config), "rom"], capture_output=True, text=True, timeout=120,
                            input="".join(json.dumps(c) + "\n" for c in commands) + "quit\n")
    lines = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    assert result.returncode == 0 and len(lines) == 2, (name, result.stdout[-500:], result.stderr[-500:])
    reply = lines[1]
    assert "error" not in reply, (name, reply)
    renderer, fallback = reply["renderer"], reply.get("renderer_fallback")
    report[name] = dict(renderer=renderer, renderer_fallback=fallback, backend=reply["identity"]["backend"])
    print(json.dumps({name: report[name]}), flush=True)
    assert (renderer, bool(fallback)) == EXPECTED[name], (name, report[name])
(output / "report.json").write_text(json.dumps(report, indent=2))
