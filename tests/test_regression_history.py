"""Verify Windows regression history with two owned builds and independent frontends."""
import hashlib
import json
from pathlib import Path
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
    print("PASS and FAIL builds retained. CLI, fresh headless MCP and windowed MCP agree.")


if __name__ == "__main__":
    main()
