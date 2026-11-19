"""Linux AppImage launcher (packaging/linux/AppRun) path resolution (#105).

Runs AppRun against a stand-in AppDir whose programs record how they were
started. Linux only; needs bash.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
RECORDER = """#!/usr/bin/env python3
import json, os, sys
print(json.dumps({"program": os.path.basename(sys.argv[0]), "args": sys.argv[1:],
                  "prefs": os.environ.get("LOTUS_DESKTOP_PREFERENCES")}))
"""


def make_appdir(parent: pathlib.Path) -> pathlib.Path:
    appdir = parent / "App Dir"
    (appdir / "usr/bin").mkdir(parents=True)
    shutil.copy(ROOT / "packaging/linux/AppRun", appdir / "AppRun")
    for name in ("lotus-desktop", "lotus-mcp", "ctr-cli"):
        (appdir / "usr/bin" / name).write_text(RECORDER)
    for backend in ("citnlo", "azahar"):
        (appdir / "runtimes" / backend).mkdir(parents=True)
        (appdir / "runtimes" / backend / "ctr-worker").write_text("#!/bin/sh\n")
    for path in appdir.rglob("*"):
        if path.is_file():
            path.chmod(0o755)
    return appdir


def snapshot(path: pathlib.Path) -> set:
    return {str(p.relative_to(path)) for p in path.rglob("*")}


def check(condition, message):
    if not condition:
        raise AssertionError(message)


class Launcher:
    def __init__(self, appdir: pathlib.Path, home: pathlib.Path, cwd: pathlib.Path):
        self.appdir, self.home, self.cwd = appdir, home, cwd

    def run(self, *args, env=None, ok=True):
        environment = {"PATH": os.environ["PATH"], "HOME": str(self.home)}
        environment.update(env or {})
        result = subprocess.run(["bash", str(self.appdir / "AppRun"), *args], cwd=self.cwd,
                                env=environment, capture_output=True, text=True, timeout=30)
        if ok and result.returncode != 0:
            raise AssertionError(f"{args}: exit {result.returncode}: {result.stderr}")
        if not ok and result.returncode == 0:
            raise AssertionError(f"{args}: expected failure")
        return result

    def start(self, *args, env=None):
        return json.loads(self.run(*args, env=env).stdout)


def main() -> int:
    if sys.platform != "linux":
        print("AppRun tests need Linux; skipped")
        return 0
    with tempfile.TemporaryDirectory() as temp:
        base = pathlib.Path(temp)
        appdir = make_appdir(base / "mnt")
        before = snapshot(appdir)
        home = base / "home dir"
        cwd = base / "elsewhere"
        home.mkdir()
        cwd.mkdir()
        launcher = Launcher(appdir, home, cwd)
        default = home / ".local/lotus"
        config = default / "citnlo/host.json"

        # Default: ~/.local/lotus, CITnLO, Desktop, started from another CWD.
        started = launcher.start()
        check(started == {"program": "lotus-desktop", "args": [str(config)],
                          "prefs": str(default / "desktop.json")}, started)
        host = json.loads(config.read_text())
        check(host == {"worker": str(appdir / "runtimes/citnlo/ctr-worker"),
                       "storage_root": str(default / "citnlo/sessions"),
                       "assets": {}, "allow_control": True}, host)

        # Second backend is isolated; each mode reaches its program.
        started = launcher.start("--backend", "AZAHAR", "--mode", "MCP")
        check(started == {"program": "lotus-mcp", "args": [str(default / "azahar/host.json")],
                          "prefs": str(default / "desktop.json")}, started)
        check(json.loads((default / "azahar/host.json").read_text())["worker"]
              == str(appdir / "runtimes/azahar/ctr-worker"), "azahar worker")
        started = launcher.start("--mode", "WindowedMCP")
        check(started["program"] == "lotus-desktop"
              and started["args"] == ["--mcp", str(config)], started)
        started = launcher.start("cli", "settings", "show")
        check(started["program"] == "ctr-cli" and started["args"] == ["settings", "show"], started)

        # --data-dir beats LOTUS_DATA_DIR beats the default; relative paths are CWD-based.
        env_dir = base / "env data"
        started = launcher.start(env={"LOTUS_DATA_DIR": str(env_dir)})
        check(started["args"] == [str(env_dir / "citnlo/host.json")], started)
        started = launcher.start("--data-dir", "rel data", env={"LOTUS_DATA_DIR": str(env_dir)})
        check(started["args"] == [str(cwd / "rel data/citnlo/host.json")], started)
        # A caller-supplied preferences file is kept.
        started = launcher.start(env={"LOTUS_DESKTOP_PREFERENCES": "/x/p.json"})
        check(started["prefs"] == "/x/p.json", started)

        # A fixture whose name has spaces and quotes is approved by absolute path.
        game = cwd / 'my "game".3dsx'
        game.write_bytes(b"3DSX")
        launcher.run("--configure-only", game.name)
        check(json.loads(config.read_text())["assets"] == {"homebrew": str(game)}, "fixture")
        (cwd / "not-a-game.txt").write_text("x")
        launcher.run("not-a-game.txt", ok=False)
        launcher.run("--backend", "other", ok=False)
        launcher.run("--data-dir", str(appdir / "data"), ok=False)
        launcher.run("--configure-only", env={"HOME": ""}, ok=False)

        # Moved AppImage: host.json follows the new mount and data survives.
        card = default / "citnlo/sessions/cards/main/save.bin"
        card.parent.mkdir(parents=True)
        card.write_bytes(b"save")
        moved = base / "mnt2" / "Other"
        shutil.copytree(appdir, moved)
        Launcher(moved, home, cwd).run("--configure-only")
        check(json.loads(config.read_text())["worker"]
              == str(moved / "runtimes/citnlo/ctr-worker"), "moved worker path")
        check(card.read_bytes() == b"save", "card survived")

        # Opt-in preference import never overwrites.
        legacy = home / ".config/lotus/desktop.json"
        legacy.parent.mkdir(parents=True)
        legacy.write_text('{"legacy": true}')
        fresh = base / "fresh"
        launcher.run("--configure-only", "--data-dir", str(fresh),
                     "--import-preferences", str(legacy))
        check(json.loads((fresh / "desktop.json").read_text()) == {"legacy": True}, "import")
        (fresh / "desktop.json").write_text('{"mine": true}')
        launcher.run("--configure-only", "--data-dir", str(fresh),
                     "--import-preferences", str(legacy), ok=False)
        check(json.loads((fresh / "desktop.json").read_text()) == {"mine": True}, "kept")

        check(snapshot(appdir) == before, f"AppDir changed: {snapshot(appdir) ^ before}")
        roots = (home, env_dir, cwd, fresh, appdir, moved)
        stray = [p for p in base.rglob("*")
                 if p.is_file() and not any(p.is_relative_to(r) for r in roots)]
        check(not stray, f"files outside the data roots: {stray}")
    print("AppRun launcher: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
