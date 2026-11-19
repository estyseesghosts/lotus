"""Packaged Linux AppImage acceptance (#109): tests the artifact, not the build tree.

    python3 tests/test_appimage.py LOTUS-vX.Y.Z-linux-x86_64.AppImage

Checks name/version/checksum, embedded files against release-manifest.json, both
worker pins and identities, shared-library closure, and then runs the image itself
(APPIMAGE_EXTRACT_AND_RUN, since hosted CI has no FUSE) renamed into a path with
spaces, from another CWD, with an isolated HOME: launcher configuration, CLI --help,
MCP initialize, worker --self-test and the native protocol test for
both backends. Fails if anything is written outside ~/.local/lotus and the CWD.
"""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def run(command, **kwargs):
    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    kwargs.setdefault("timeout", 120)
    result = subprocess.run(command, **kwargs)
    if result.returncode != 0:
        raise AssertionError(f"{command}: exit {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result


def main() -> int:
    image = pathlib.Path(sys.argv[1]).resolve()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    pins = tomllib.loads((ROOT / "config/pins.toml").read_text())["azahar"]
    check(image.name == f"LOTUS-v{version}-linux-x86_64.AppImage", f"name {image.name}")
    check(os.access(image, os.X_OK), "AppImage is not executable")
    sums = image.with_name(image.name + ".sha256").read_text().split()
    check(sums == [digest(image), image.name], f"checksum file {sums}")

    with tempfile.TemporaryDirectory() as temp:
        base = pathlib.Path(temp)
        extract = base / "extract"
        extract.mkdir()
        run([str(image), "--appimage-extract"], cwd=extract)
        appdir = extract / "squashfs-root"
        manifest = json.loads((appdir / "release-manifest.json").read_text())
        check(manifest["version"] == version and manifest["target"] == "linux-x86_64", "manifest identity")
        for relative, sha in manifest["files_sha256"].items():
            check(digest(appdir / relative) == sha, f"{relative} differs from the manifest")
        for name in ("AppRun", "lotus.desktop", "lotus.png", "SOURCE.md", "LICENSE",
                     "usr/bin/lotus-desktop", "usr/bin/lotus-mcp", "usr/bin/ctr-cli",
                     "runtimes/azahar/ctr-worker", "runtimes/citnlo/ctr-worker",
                     "usr/share/licenses/rust-packages.json"):
            check((appdir / name).is_file(), f"missing {name}")
        for name in ("AppRun", "usr/bin/lotus-desktop", "usr/bin/lotus-mcp", "usr/bin/ctr-cli",
                     "runtimes/azahar/ctr-worker", "runtimes/citnlo/ctr-worker"):
            check(os.access(appdir / name, os.X_OK), f"{name} is not executable")
            if name == "AppRun":
                continue
            ldd = run(["ldd", str(appdir / name)]).stdout
            check("not found" not in ldd, f"{name} has unresolved libraries:\n{ldd}")
        for backend, branch, identity in (("azahar", "integration", "AZAHAR"),
                                          ("citnlo", "experimental", "CITnLO")):
            worker_manifest = json.loads((appdir / "runtimes" / backend / "build-manifest.json").read_text())
            check(worker_manifest["source"] == pins[f"{branch}_commit"], f"{backend} pin")
            check(f"-DCTR_BACKEND_NAME={identity}" in worker_manifest["cmake_flags"], f"{backend} identity")

        # Run the image itself, renamed into a path with spaces, from elsewhere.
        install = base / "my apps" / "LOTUS renamed.AppImage"
        install.parent.mkdir()
        shutil.copy2(image, install)
        home = base / "home"
        cwd = base / "cwd"
        home.mkdir()
        cwd.mkdir()
        env = {"PATH": os.environ["PATH"], "HOME": str(home), "APPIMAGE_EXTRACT_AND_RUN": "1",
               "TMPDIR": str(base / "tmp")}
        (base / "tmp").mkdir()
        data = home / ".local/lotus"

        def appimage(*args, **kwargs):
            return run([str(install), *args], cwd=cwd, env=env, **kwargs)

        check("Usage:" in appimage("cli", "--help").stdout, "ctr-cli --help")
        for backend in ("CITnLO", "AZAHAR"):
            config = pathlib.Path(appimage("--backend", backend, "--configure-only").stdout.strip())
            check(config == data / backend.lower() / "host.json", f"config path {config}")
            host = json.loads(config.read_text())
            worker = pathlib.Path(host["worker"])
            check(worker.name == "ctr-worker" and worker.parent.name == backend.lower(), host)
            check(not str(worker).startswith(str(data)), "worker path inside data")
        mcp = subprocess.run(
            [str(install), "--mode", "MCP"], cwd=cwd, env=env, capture_output=True, text=True, timeout=60,
            input=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "test_appimage", "version": "0"}}}) + "\n"
            + json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        reply = json.loads(mcp.stdout.splitlines()[0])
        check(mcp.returncode == 0 and reply.get("id") == 1 and "result" in reply,
              f"MCP initialize: {mcp.returncode} {mcp.stdout[:500]} {mcp.stderr[-500:]}")

        for backend in ("azahar", "citnlo"):
            worker = appdir / "runtimes" / backend / "ctr-worker"
            scratch = base / f"worker-{backend}"
            scratch.mkdir()
            worker_env = dict(env, LOTUS_STORAGE_ROOT=str(scratch))
            run([str(worker), "--self-test"], cwd=scratch, env=worker_env)
            run([sys.executable, str(ROOT / "tests/test_native_protocol.py"), str(worker)],
                cwd=scratch, env=worker_env, timeout=600)

        allowed = (data, cwd, extract, install.parent, base / "tmp", base / "worker-azahar",
                   base / "worker-citnlo")
        stray = [p for p in base.rglob("*")
                 if p.is_file() and not any(p.is_relative_to(a) for a in allowed)]
        check(not stray, f"files outside the data directory: {stray}")
        check(not any(p.is_file() for p in cwd.rglob("*")), "files written in the CWD")
    print(f"AppImage acceptance ({image.name}): passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
