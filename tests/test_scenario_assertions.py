"""lotus#94: scenario version 2 assertions end to end on the owned debugger fixture.

Builds fixtures/debugger, resolves its symbols with llvm-nm, and runs generated scenarios
through `ctr-cli scenario run`:
  - the passing scenario (memory, register, events, coverage, event-triggered capture), twice,
    so repeat mode also compares frame hashes;
  - a frame_hash check against the recorded hash, then against a changed hash;
  - an intentionally changed memory expectation, a coverage target the fixture never runs and
    an event trigger that never fires: each must exit non-zero with passed=false and name the
    failing assertion;
  - a host deadline and a CITnLO-only scenario on the AZAHAR worker: no report.json at all;
  - specs/native-smoke.yaml on the AZAHAR worker still passes.
Usage: test_scenario_assertions.py <ctr-cli> <experimental-worker> <integration-worker> <output>
"""
import json
from pathlib import Path
import shutil
import subprocess
import sys

cli, experimental, integration, output = (Path(a).resolve() for a in sys.argv[1:5])
repo = Path(__file__).resolve().parents[1]
output.mkdir(parents=True, exist_ok=False)
fixture = output / "fixture"
subprocess.run([sys.executable, str(repo / "tools/build-debug-fixture.py"), str(fixture)], check=True)
nm = shutil.which("llvm-nm")
assert nm, "llvm-nm is required to resolve fixture symbols"
symbols = {}
for line in subprocess.check_output([nm, str(fixture / "debugger.elf")], text=True).splitlines():
    parts = line.split()
    if len(parts) == 3:
        symbols[parts[2]] = int(parts[0], 16)
rom = fixture / "debugger.3dsx"


def scenario(name, assertions, flavor="experimental", captures=None, stop="{ max_frames: 60, wall_timeout_seconds: 60 }"):
    captures = captures if captures is not None else [
        "{ when: { frame: 30 }, include: [top_raw_frame, bottom_raw_frame] }",
        '{ when: { event: guest_log, text_contains: "lotus-fixture:ready" }, include: [top_raw_frame, bottom_raw_frame] }',
    ]
    path = output / f"{name}.yaml"
    path.write_text(f"""version: 2
name: {name}
content: {{ kind: homebrew_3dsx, path_handle: {json.dumps(str(rom))} }}
profile: {{ model: old3ds, renderer: software, worker_flavor: {flavor}, audio_output: discard }}
storage: {{ mode: isolated_copy }}
input: {{ owner: scenario, schedule: [] }}
stop: {stop}
captures:
""" + "".join(f"  - {c}\n" for c in captures) + "assertions:\n" + "".join(f"  - {a}\n" for a in assertions))
    return path


def run(path, worker, count=1):
    root = path.with_suffix("")
    done = subprocess.run([str(cli), "scenario", "run", str(path), str(worker), str(root), str(count)],
                          capture_output=True, text=True, timeout=900)
    reports = [json.loads(p.read_text()) for p in sorted(root.glob("*/report.json"))]
    failures = [json.loads(p.read_text()) for p in sorted(root.glob("*/failure.json"))]
    return done, reports, failures


PASSING = [
    "{ type: no_worker_crash }",
    "{ type: frame_count, minimum: 60 }",
    f"{{ type: memory, at_frame: 30, address: {symbols['canary']}, width: 4, equals: 0xcafebabe }}",
    f"{{ type: memory, at_frame: 60, address: {symbols['counter']}, minimum: 2 }}",
    "{ type: register, at_frame: 60, core: 0, register: 15, minimum: 0x00100000, maximum: 0x00110000 }",
    '{ type: event_seen, event: guest_log, text_contains: "lotus-fixture:ready" }',
    "{ type: no_event, event: hio_stdout }",
    f"{{ type: reached, address: {symbols['tick']}, count_at_least: 100 }}",
    f"{{ type: not_reached, address: {symbols['deadlock_main']} }}",
]

done, reports, _ = run(scenario("pass", PASSING), experimental, 2)
assert done.returncode == 0, done.stderr
assert len(reports) == 2 and all(r["passed"] and r["schema_version"] == 2 for r in reports), reports
for report in reports:
    assert [a["outcome"] for a in report["assertions"]] == ["pass"] * len(PASSING), report["assertions"]
    assert report["event_window"]["loss"] is None
    triggered = [c for c in report["checkpoints"] if c["trigger"]]
    assert {c["screen"] for c in triggered} == {"top", "bottom"}, report["checkpoints"]
top = next(c["sha256"] for c in reports[0]["checkpoints"] if c["frame"] == 60 and c["screen"] == "top" and not c["trigger"])
print("pass: 2 runs,", len(PASSING), "assertions each, guest_log capture at frame",
      triggered[0]["frame"], "tick hits", reports[0]["assertions"][7]["actual"])

done, reports, _ = run(scenario("hash-pass", [f"{{ type: frame_hash, at_frame: 60, screen: top, sha256: {top} }}"]), experimental)
assert done.returncode == 0 and reports[0]["passed"], done.stderr

wrong = ("0" if top[0] != "0" else "1") + top[1:]


def expect_failure(name, assertions, index, outcome, **kwargs):
    done, reports, failures = run(scenario(name, assertions, **kwargs), experimental)
    assert done.returncode != 0, f"{name} must not pass"
    report = reports[0]
    assert not report["passed"] and report["first_failure"] == index, report
    result = next(a for a in report["assertions"] if a["index"] == index)
    assert result["outcome"] == outcome, result
    assert failures[0]["assertion"]["index"] == index
    print(f"{name}: {result['type']} {outcome}, expected {result['expected']} actual {result['actual']}")
    return result


expect_failure("hash-changed", [f"{{ type: frame_hash, at_frame: 60, screen: top, sha256: {wrong} }}"], 0, "fail")
changed = expect_failure("memory-changed", [
    "{ type: no_worker_crash }",
    f"{{ type: memory, at_frame: 30, address: {symbols['canary']}, width: 4, equals: 0xbad }}",
], 1, "fail")
assert changed["actual"] == 0xcafebabe
coverage = expect_failure("coverage-missed", [f"{{ type: reached, address: {symbols['deadlock_main']} }}"], 0, "fail")
assert coverage["actual"] == 0
expect_failure("trigger-unfired", ["{ type: no_worker_crash }"], 1, "fail", captures=[
    '{ when: { event: guest_log, text_contains: "never-printed" }, include: [top_raw_frame] }'])

# Host deadline: the run cannot finish, so there is no report to read as a pass.
done, reports, failures = run(scenario("deadline", PASSING[:1], captures=[],
                                       stop="{ max_frames: 100000, wall_timeout_seconds: 2 }"), experimental)
assert done.returncode != 0 and not reports and "deadline" in failures[0]["error"], (done, failures)
print("deadline:", failures[0]["error"])

# CITnLO-only checks: rejected before a worker exists when declared for AZAHAR, and refused by
# the AZAHAR worker's feature check when the declaration is wrong.
mismatch = scenario("azahar-declared", PASSING, flavor="integration")
done = subprocess.run([str(cli), "scenario", "validate", str(mismatch)], capture_output=True, text=True)
assert done.returncode != 0 and "experimental" in done.stderr, done.stderr
done, reports, failures = run(scenario("azahar-runtime", PASSING), integration)
assert done.returncode != 0 and not reports, done
print("azahar runtime:", failures[0]["error"])

smoke = repo / "specs/native-smoke.yaml"
done = subprocess.run([str(cli), "scenario", "run", str(smoke), str(integration), str(output / "smoke"), "1"],
                      capture_output=True, text=True, timeout=900)
assert done.returncode == 0, done.stderr
report = json.loads(next((output / "smoke").glob("*/report.json")).read_text())
assert report["passed"] and report["schema_version"] == 1
print("native-smoke: pass")
