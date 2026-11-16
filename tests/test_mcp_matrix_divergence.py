"""T41/T18: a JIT-sensitive owned fixture is flagged by the profile matrix.

The divergence fixture parks CPU-backend probe results in r4-r9. A matrix of
JIT, interpreter and a second JIT profile (two repeats each) must report a
repeatable divergence at the first checkpoint for every JIT/interpreter pair
and none between the two JIT profiles. The worker labels comparisons as
observations; nothing here says which backend matches hardware.
"""
import json
from pathlib import Path
import sys
from mcp_client import Client

server, config, rom, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
client = Client(server, config, output / "transcript.jsonl")
try:
    session = client.tool("lotus_create_session", {"kind": "primary", "card": "matrix-divergence"})["session"]
    client.tool("lotus_load", {"session": session, "asset": rom})
    profiles = [{"name": name, "backend": None, "settings": {"use_cpu_jit": jit, "deterministic_async_operations": True}}
                for name, jit in (("jit", True), ("interpreter", False), ("jit_copy", True))]
    matrix = client.tool("lotus_debug", {"session": session, "command": {
        "operation": "matrix", "profiles": profiles, "frames": 20, "checkpoints": [10, 20], "repeats": 2}})
    report = matrix["report"]
    (output / "matrix.json").write_text(json.dumps(matrix, indent=2))
    assert all(p["repeatable_at_checkpoints"] for p in report["profiles"]), report["profiles"]
    pairs = {(c["left"], c["right"]): c for c in report["comparisons"]}
    for pair in (("jit", "interpreter"), ("interpreter", "jit_copy")):
        assert pairs[pair]["earliest_sampled_divergence"] == 10 and pairs[pair]["repeatability_confirmed"], pairs[pair]
    assert pairs[("jit", "jit_copy")]["earliest_sampled_divergence"] is None, pairs[("jit", "jit_copy")]
    rows = {p["name"]: p["runs"][0]["checkpoints"][0]["registers"][4:10] for p in report["profiles"]}
    differing = [f"r{4 + i}" for i, (a, b) in enumerate(zip(rows["jit"], rows["interpreter"])) if a != b]
    summary = dict(probe_registers=rows, differing=differing,
                   verdicts={f"{a}/{b}": c["earliest_sampled_divergence"] for (a, b), c in pairs.items()})
    print(json.dumps(summary), flush=True)
    client.tool("lotus_control", {"session": session, "operation": "stop"})
finally:
    client.close()
