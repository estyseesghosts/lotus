"""Verify and stage the Silvestris resources embedded in the Lotus desktop."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def stage(desktop, metadata, licenses):
    pins = tomllib.loads((ROOT / "config/pins.toml").read_text(encoding="utf-8"))["silvestris"]
    source = (ROOT / pins["path"]).resolve()
    git = ["git", "-C", str(ROOT)]
    tree = subprocess.check_output([*git, "rev-parse", f"HEAD:{pins['path']}"], text=True).strip()
    if tree != pins["tree"]:
        raise ValueError(f"Silvestris tree {tree} does not match the pin")
    subprocess.run([*git, "diff", "--exit-code", "HEAD", "--", pins["path"]],
                   check=True, stdout=subprocess.DEVNULL)
    desktop_package = next(package for package in metadata["packages"]
                           if package["name"] == "lotus-desktop")
    resolved = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    node = resolved[desktop_package["id"]]
    if "silvestris-ui" not in node["features"]:
        raise ValueError("Portable LOTUS requires a Silvestris desktop build")
    dependency = next(dep for dep in node["deps"] if dep["name"] == "silvestris")
    framework = next(package for package in metadata["packages"] if package["id"] == dependency["pkg"])
    if Path(framework["manifest_path"]).resolve() != source / "crates/silvestris/Cargo.toml":
        raise ValueError("Silvestris build did not use the pinned source snapshot")

    fonts = source / "crates/silvestris-fonts/fonts"
    manifest = json.loads((fonts / "manifest.json").read_text(encoding="utf-8"))
    executable = desktop.read_bytes()
    destination = licenses / "silvestris"
    for family in manifest["families"]:
        for face in family["faces"]:
            data = (fonts / face["file"]).read_bytes()
            if hashlib.sha256(data).hexdigest() != face["sha256"] or data not in executable:
                raise ValueError(f"Desktop does not contain the pinned font: {face['file']}")
        notice = destination / family["license"]
        notice.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(fonts / family["license"], notice)
    shutil.copy2(fonts / "manifest.json", destination / "fonts.json")

    assets = ROOT / "crates/desktop/assets"
    art = [assets / "lotus.png", *sorted((assets / "prefs").glob("*.svg"))]
    hashes = {}
    for path in art:
        data = path.read_bytes()
        if data not in executable:
            raise ValueError(f"Desktop does not contain the pinned artwork: {path.name}")
        hashes[path.relative_to(assets).as_posix()] = hashlib.sha256(data).hexdigest()
    return {"revision": pins["rev"], "tree": pins["tree"], "embedded_art_sha256": hashes}
