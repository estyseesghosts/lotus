"""Verify Windows regression history with two owned builds and independent frontends."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

from mcp_client import Client
from test_settings_window import foreground, key, until, user32, windows


def main():
    cli, server, desktop, worker, output = (Path(arg).resolve() for arg in sys.argv[1:6])
    output.mkdir(parents=True, exist_ok=False)
    storage = output / "storage"
    history = storage / "regressions"
    config = output / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(storage),
        "allow_control": False, "prefer_vulkan": False,
    }))
    build = [sys.executable, str(Path(__file__).resolve().parents[1] / "tools/build-debug-fixture.py")]
    source = output / "fixture"
    changed = output / "changed"
    subprocess.run([*build, str(source)], check=True)
    subprocess.run([*build, str(changed), "--define", "LOTUS_CANARY_VALUE=0xbad"], check=True)
    symbols = subprocess.check_output(["llvm-nm", str(source / "debugger.elf")], text=True)
    address = int(next(row for row in symbols.splitlines() if row.endswith(" canary")).split()[0], 16)
    scenario = output / "canary.yaml"
    scenario.write_text(f"""version: 2
name: canary regression
content: {{ kind: homebrew_3dsx, path_handle: fixture/debugger.3dsx }}
profile: {{ model: old3ds, renderer: software, worker_flavor: integration }}
storage: {{ mode: isolated_copy }}
input: {{ owner: scenario }}
stop: {{ max_frames: 2, wall_timeout_seconds: 15 }}
captures:
  - {{ when: {{ frame: 2 }}, include: [top_raw_frame] }}
assertions:
  - {{ type: memory, at_frame: 2, address: {address}, width: 4, equals: 0xcafebabe }}
""")
    manifest = output / "suite.yaml"
    manifest.write_text("version: 1\nrepeats: 2\nscenarios: [canary.yaml]\n")

    def suite(command, *args, exit_code=0):
        result = subprocess.run([str(cli), "suite", command, *map(str, args)],
                                capture_output=True, text=True, timeout=120)
        assert result.returncode == exit_code, (result.stdout, result.stderr)
        return json.loads(result.stdout)

    def run(revision, exit_code=0):
        return suite("run", manifest, worker, history, revision, exit_code=exit_code)

    baseline = run("a" * 40)
    assert baseline["status"] == "PASS", baseline
    content = source / "debugger.3dsx"
    baseline_content = content.read_bytes()
    content.write_bytes((changed / content.name).read_bytes())
    candidate = run("b" * 40, 1)
    assert candidate["status"] == "FAIL", candidate

    baseline_id, candidate_id = baseline["id"], candidate["id"]
    (history / ("c" * 32)).mkdir()  # An interrupted run does not join history.
    listing = suite("list", history)
    assert [row["id"] for row in listing] == [candidate_id, baseline_id]
    detail = suite("detail", history, candidate_id)
    case, = detail["cases"]
    assert case["content_sha256"] == hashlib.sha256(content.read_bytes()).hexdigest()
    failure = case["first_failure"]
    assert (failure["type"], failure["expected"], failure["actual"]) == (
        "memory", {"equals": 0xCAFEBABE}, 0xBAD,
    ), failure
    diff = suite("compare", history, baseline_id, candidate_id)
    change, = diff["cases"]
    assert (change["baseline"], change["candidate"], change["observation"]) == (
        "PASS", "FAIL", "compared",
    ), change
    assert change["first_failure"] == failure
    assert change["assertion_change"]["baseline"]["actual"] == 0xCAFEBABE
    assert change["assertion_change"]["candidate"]["actual"] == 0xBAD

    # Export after the original scenario and guest path are gone.
    scenario.unlink()
    content.unlink()
    bundle = output / "repro bundle"

    def export(destination, expected=0):
        result = subprocess.run([str(cli), "suite", "export", str(history), baseline_id,
                                 candidate_id, str(destination)], capture_output=True, text=True,
                                timeout=15)
        assert result.returncode == expected, (result.stdout, result.stderr)

    export(bundle)
    assert {p.name for p in bundle.iterdir()} == {
        "README.md", "report.json", "replay.ps1", "case-01-baseline.json", "case-01-candidate.json",
    }
    report = json.loads((bundle / "report.json").read_text())
    assert report["comparison"] == diff
    summary = (bundle / "README.md").read_text(encoding="utf-8")
    assert "canary.yaml" in summary and "2989" in summary
    replay, = report["replays"]
    assert replay["case"] == 1 and replay["unavailable"] is None
    assert report["baseline"]["revision"] == "a" * 40
    assert report["candidate"]["revision"] == "b" * 40
    for side, fixture, actual in (
        ("baseline", baseline_content, 0xCAFEBABE),
        ("candidate", (changed / "debugger.3dsx").read_bytes(), 0xBAD),
    ):
        recipe = replay[side]
        assert recipe["content_sha256"] == hashlib.sha256(fixture).hexdigest()
        assert recipe["repeats"] == 2
        assert report[side]["worker_sha256"] == baseline["worker_sha256"]
        spec = json.loads((bundle / recipe["scenario"]).read_text())
        assert spec["content"]["path_handle"] == recipe["content"]
        assert spec["content"]["sha256"] == recipe["content_sha256"]
        assert spec["profile"]["worker_sha256"] == baseline["worker_sha256"]
        (bundle / recipe["content"]).write_bytes(fixture)
        result = subprocess.run([
            shutil.which("powershell.exe"), "-NoProfile", "-ExecutionPolicy", "Bypass",
            "-File", str(bundle / "replay.ps1"), "-Cli", str(cli), "-Worker", str(worker),
            "-Output", str(output / ("replay-" + side)), "-Case", "1", "-Side", side,
        ], capture_output=True, text=True, timeout=45)
        assert result.returncode == int(side == "candidate"), (result.stdout, result.stderr)
        replayed = json.loads(result.stdout)
        expected_status = "PASS" if side == "baseline" else "FAIL"
        assert replayed["status"] == expected_status, replayed
        assert len(replayed["attempts"]) == 2
        assert all(a["status"] == expected_status for a in replayed["attempts"])
        if side == "candidate":
            assert all(a["first_failure"]["actual"] == actual for a in replayed["attempts"])

    export(bundle, 1)  # An export cannot overwrite an existing package.
    (bundle / replay["candidate"]["content"]).write_bytes(baseline_content)
    for binary, expected in ((worker, "fixture SHA-256 mismatch"),
                             (cli, "worker executable SHA-256 mismatch")):
        recipe = replay["candidate"] if binary == worker else replay["baseline"]
        result = subprocess.run([str(cli), "scenario", "assess", str(bundle / recipe["scenario"]),
                                 str(binary), str(output / "replay-refused"), "2"],
                                capture_output=True, text=True, timeout=15)
        assert result.returncode == 1, (result.stdout, result.stderr)
        refused = json.loads(result.stdout)
        assert refused["status"] == "INCOMPLETE"
        assert all(a["error"] == expected and a["report"] is None for a in refused["attempts"])

    retained = history / candidate_id / Path(candidate["cases"][0]["assessment"]).parent / "scenario.json"
    retained.write_bytes(retained.read_bytes() + b" ")
    corrupt = output / "corrupt export"
    export(corrupt)
    refused, = json.loads((corrupt / "report.json").read_text())["replays"]
    assert refused["unavailable"] == "retained scenario SHA-256 mismatch"
    assert not (corrupt / "replay.ps1").exists()
    retained.unlink()
    legacy = output / "legacy export"
    export(legacy)
    refused, = json.loads((legacy / "report.json").read_text())["replays"]
    assert refused["unavailable"] == "retained scenario unavailable"

    def verify(client):
        tool = "lotus_regression_history"
        assert client.tool(tool, {"operation": {"op": "list"}}) == listing
        assert client.tool(tool, {"operation": {"op": "detail", "id": candidate_id}}) == detail
        assert client.tool(tool, {"operation": {"op": "compare", "baseline": baseline_id,
                                                "candidate": candidate_id}}) == diff
        client.tool(tool, {"operation": {"op": "detail", "id": "../outside"}}, expected_error=True)
        assert not client.tool("lotus_list_sessions", {})

    client = Client(server, config, output / "headless.jsonl", stderr_path=output / "headless.stderr")
    try:
        verify(client)
    finally:
        client.close()

    client = Client(desktop, config, output / "desktop.jsonl", arguments=["--mcp-stdio"],
                    stderr_path=output / "desktop.stderr")
    try:
        pid = client.process.pid
        until("shared-shell owner visible", lambda: any(title == "lotus" for _, title in windows(pid)), 10)
        owner = next(hwnd for hwnd, title in windows(pid) if title == "lotus")
        verify(client)
        for _ in range(2):
            foreground(owner)
            key(0x11); key(0x10); key(0x52)
            key(0x52, True); key(0x10, True); key(0x11, True)
            until("Regression History window visible",
                  lambda: sum(title == "regression history" for _, title in windows(pid)) == 1)
        user32.PostMessageW(owner, 0x10, 0, 0)
        until("Regression History closes with owner", lambda: not windows(pid))
    finally:
        client.close()

    path = history / candidate_id / candidate["cases"][0]["assessment"]
    report = path.parent / "attempt-02/report.json"
    changed_report = json.loads(report.read_text())
    changed_report["assertions"][0]["actual"] = 123
    report.write_text(json.dumps(changed_report))
    unavailable, = suite("compare", history, baseline_id, candidate_id)["cases"]
    assert unavailable["candidate"] == "INCOMPLETE" and unavailable["observation"] == "inconclusive"
    print("PASS and FAIL builds replayed from the export. CLI, headless MCP and windowed MCP agree.")


if __name__ == "__main__":
    main()
