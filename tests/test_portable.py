"""Verify ZIP relocation and startup with a Windows-only PATH, without guest assets."""
import importlib.util
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package_windows", ROOT / "scripts/package-windows.py")
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


def main():
    with tempfile.TemporaryDirectory(prefix="LOTUS portable ") as temporary:
        relocated = Path(temporary) / "Moved installation"
        with zipfile.ZipFile(sys.argv[1]) as archive:
            archive.extractall(relocated)
        roots = list(relocated.iterdir())
        assert len(roots) == 1 and roots[0].is_dir()
        root = roots[0]
        manifest = json.loads((root / "release-manifest.json").read_text(encoding="utf-8"))
        for name, digest in manifest["files_sha256"].items():
            assert packaging.digest(root / name) == digest, name
        for path in root.rglob("*"):
            assert path.suffix.lower() not in (".3dsx", ".3ds", ".cdc", ".log"), path
            if path.suffix.lower() in (".exe", ".dll"):
                for dependency in packaging.imports(path):
                    assert (dependency.startswith(("api-ms-", "ext-ms-"))
                            or (path.parent / dependency).exists()
                            or (Path(os.environ["SystemRoot"]) / "System32" / dependency).exists()), dependency
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith(("LOTUS_", "CARGO_", "RUST"))}
        environment["PATH"] = os.pathsep.join([
            str(Path(os.environ["SystemRoot"]) / "System32"), os.environ["SystemRoot"]])

        def run(*command):
            return subprocess.run(command, cwd=temporary, env=environment, capture_output=True,
                                  text=True, encoding="utf-8", check=True, timeout=30)

        version = run(str(root / "bin/ctr-cli.exe"), "version").stdout
        assert f"lotus {manifest['version']}" in version
        # The file is only a launcher path fixture; it is never emulated.
        fixture = Path(temporary) / "homebrew path with spaces.3dsx"
        fixture.write_bytes(b"")
        for backend, branch in (("azahar", "integration"), ("citnlo", "experimental")):
            worker = root / "runtimes" / backend / "ctr-worker.exe"
            assert manifest["native_pins"][f"{branch}_commit"] in run(str(worker), "--version").stdout
            run(str(worker), "--probe-core")
            assert "=ok" in run(str(worker), "--self-test").stdout
            # Exercise Windows PowerShell 5.1, the version invoked by the .cmd launchers.
            run("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                str(root / "launch.ps1"), "-Backend", backend.upper() if backend == "azahar" else "CITnLO",
                "-Fixture", str(fixture), "-ConfigureOnly")
            config = json.loads((root / "data" / backend / "host.json").read_text(encoding="utf-8"))
            # Windows may expose Temp through RUNNER~1 while PowerShell expands
            # the same directory to runneradmin. Compare filesystem identity.
            assert Path(config["worker"]).samefile(worker)
            assert Path(config["assets"]["homebrew"]).samefile(fixture)
            assert Path(config["storage_root"]).resolve().is_relative_to(root.resolve())
            subprocess.run([sys.executable, str(ROOT / "tests/test_native_protocol.py"), str(worker)],
                           cwd=temporary, env=environment, check=True, timeout=60)
        with subprocess.Popen([str(root / "bin/lotus-mcp.exe")], cwd=temporary, env=environment,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True, encoding="utf-8") as server:
            messages = queue.Queue()

            def read():
                for line in server.stdout:
                    messages.put(json.loads(line))
                messages.put(None)

            threading.Thread(target=read, daemon=True).start()
            try:
                request = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "lotus-portable-test", "version": "1"}}}
                server.stdin.write(json.dumps(request) + "\n")
                server.stdin.flush()
                reply = messages.get(timeout=10)
                assert reply is not None and "result" in reply, reply
                server.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
                server.stdin.flush()
                server.stdin.close()
                server.wait(timeout=10)
                assert server.returncode == 0, server.stderr.read()
            finally:
                if server.poll() is None:
                    server.kill()
                    server.wait(timeout=5)
    print("Portable relocation, checksums, DLL closure, launch paths and CLI/MCP/native startup: passed")


if __name__ == "__main__":
    main()
