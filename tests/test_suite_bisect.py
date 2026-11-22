"""Windows bisect acceptance on locally approved worktrees of an owned fixture."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    cli, worker, output = (Path(arg).resolve() for arg in sys.argv[1:4])
    output.mkdir(parents=True, exist_ok=False)
    root = output / "repository"
    root.mkdir()
    git = shutil.which("git")
    cmake = shutil.which("cmake")
    lotus = Path(__file__).resolve().parents[1]

    def command(*args, cwd=root):
        return subprocess.check_output(args, cwd=cwd, text=True, timeout=45).strip()

    command(git, "init", "--quiet")
    for key in ("user.name", "user.email"):
        command(git, "config", key, command(git, "config", key, cwd=lotus))
    command(git, "config", "core.autocrlf", "false")
    (root / ".gitignore").write_text("/build/\n")
    (root / "fixtures/debugger").mkdir(parents=True)
    (root / "tools").mkdir()
    for relative in ("fixtures/debugger/fixture.c", "fixtures/debugger/link.ld", "tools/build-debug-fixture.py"):
        shutil.copyfile(lotus / relative, root / relative)
    (root / "CMakeLists.txt").write_text(f"""cmake_minimum_required(VERSION 3.20)
project(owned_fixture NONE)
file(STRINGS "${{CMAKE_SOURCE_DIR}}/canary.txt" value)
add_custom_target(fixture ALL
  COMMAND "{Path(sys.executable).as_posix()}" "${{CMAKE_SOURCE_DIR}}/tools/build-debug-fixture.py"
    "${{CMAKE_BINARY_DIR}}" --define "LOTUS_CANARY_VALUE=${{value}}"
  VERBATIM)
add_custom_target(no_fixture COMMAND "${{CMAKE_COMMAND}}" -E echo "No guest produced")
""")
    (root / "CMakePresets.json").write_text(json.dumps({
        "version": 3,
        "configurePresets": [{"name": "regression", "generator": "Ninja",
                              "binaryDir": "${sourceDir}/build"}],
        "buildPresets": [
            {"name": "regression", "configurePreset": "regression"},
            {"name": "no-fixture", "configurePreset": "regression", "targets": ["no_fixture"]},
        ],
    }))
    revisions = []
    for index, value in enumerate(("0xcafebabe", "0xbad", "0xcafebabe", "0xcafebabe", "0xbad")):
        (root / "canary.txt").write_text(value + "\n")
        (root / "build-id.txt").write_text(str(index) + "\n")
        command(git, "add", ".")
        command(git, "commit", "--quiet", "-m", f"Fixture revision {index}")
        revisions.append(command(git, "rev-parse", "HEAD"))
    good, first_bad, _, _, bad = revisions
    # Resolve the memory assertion address with the same owned fixture and toolchain.
    reference = output / "reference"
    command(sys.executable, str(lotus / "tools/build-debug-fixture.py"), str(reference))
    symbols = command("llvm-nm", str(reference / "debugger.elf"))
    address = int(next(line for line in symbols.splitlines() if line.endswith(" canary")).split()[0], 16)
    (output / "fixture.yaml").write_text(f"""version: 2
name: canary regression
content: {{ kind: homebrew_3dsx, path_handle: build/debugger.3dsx }}
profile: {{ model: old3ds, renderer: software, worker_flavor: integration }}
storage: {{ mode: isolated_copy }}
input: {{ owner: scenario }}
stop: {{ max_frames: 2, wall_timeout_seconds: 10 }}
assertions:
  - {{ type: memory, at_frame: 2, address: {address}, width: 4, equals: 0xcafebabe }}
""")
    (output / "suite.yaml").write_text("version: 1\nrepeats: 2\nscenarios: [fixture.yaml]\n")
    before = command(git, "status", "--porcelain")

    def run(label, refusal=None):
        config = output / (label + "-host.json")
        plan = output / (label + "-plan.json")
        storage = output / label
        profiles = {}
        assignments = {}
        for index, revision in enumerate(revisions):
            name = f"{label}-{index}"
            tree = output / name
            command(git, "worktree", "add", "--quiet", "--detach", str(tree), revision)
            command(cmake, "--preset", "regression", cwd=tree)
            preset = refusal if index == 1 and refusal else "regression"
            profiles[name] = {
                "root": str(tree), "output": "build/debugger.3dsx",
                "tool": {"kind": "cmake_preset", "program": cmake, "preset": preset},
                "env": {"Path": os.environ["PATH"], "CMAKE_BUILD_PARALLEL_LEVEL": "1"},
                "timeout_secs": 45,
            }
            assignments[revision] = name
        config.write_text(json.dumps({
            "worker": str(worker), "storage_root": str(storage), "allow_control": True,
            "build_profiles": profiles,
        }))
        plan.write_text(json.dumps({
            "version": 1, "git": git, "repository": str(root), "suite": "suite.yaml",
            "good": good, "bad": bad, "profiles": assignments,
        }))
        done = subprocess.run([str(cli), "suite", "bisect", str(config), str(plan)],
                              capture_output=True, text=True, timeout=180)
        assert done.returncode == (1 if refusal else 0), (done.stdout, done.stderr)
        report = json.loads(done.stdout)
        history = storage / "regressions"
        assert report == json.loads((history / f"bisect-{report['id']}.json").read_text())
        trials = {trial["revision"]: trial for trial in report["trials"]}
        assert trials[good]["status"] == "PASS", trials
        assert trials[bad]["status"] == "FAIL", trials
        pinned = trials[first_bad]
        if refusal:
            assert report["first_bad"] is None and pinned["status"] == "INCOMPLETE", report
            assert pinned["error"] and pinned["fixture_sha256"] is None and pinned["suite"] is None
        else:
            assert report["first_bad"] == first_bad and pinned["status"] == "FAIL", report
            detail = json.loads(command(str(cli), "suite", "detail", str(history), pinned["suite"]))
            assert detail["suite"]["declared_revision"] == first_bad
            case, = detail["cases"]
            assert case["first_failure"]["actual"] == 0xBAD
            assert case["content_sha256"] == pinned["fixture_sha256"]
        assert command(git, "status", "--porcelain") == before
        assert command(git, "rev-parse", "HEAD") == bad

    run("reversal")
    run("missing", "no-fixture")
    run("failed", "invalid-preset")
    print("First bad survived a later repair. Missing output and failed build remained unresolved.")


if __name__ == "__main__":
    main()
