#!/usr/bin/env python3
"""Assemble the Linux x86_64 AppImage from tested build outputs (#108).

    package-linux.py --rust target/release --metadata build/metadata.json \
        --azahar BUILD_DIR --citnlo BUILD_DIR --version X.Y.Z --output build/dist \
        --appimagetool TOOL --runtime RUNTIME

The AppDir follows docs.appimage.org/reference/appdir.html: AppRun, one .desktop
entry and its icon at the root. Libraries the host must provide (glibc, the C++
runtime, X11/Wayland/xkbcommon, udev, graphics and audio drivers) are recorded,
not bundled: they must match the host's drivers and xkb data. A binary needing
anything else fails the build, so every bundled library is a deliberate change.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LICENSE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT)([-._].*)?$", re.IGNORECASE)
# Pinned packaging tools (docs/27 section 11).
TOOLS = {
    "appimagetool": ("https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage",
                     "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"),
    "runtime": ("https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64",
                "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"),
}
HOST = re.compile(
    r"^(linux-vdso|ld-linux-x86-64|libc|libm|libmvec|libdl|librt|libpthread|libresolv|libgcc_s|libstdc\+\+"
    r"|libxcb[-\w]*|libX\w+|libxkbcommon(-x11)?|libwayland-\w+|libudev|libcap|libbsd|libmd"
    r"|libGL\w*|libEGL|libvulkan|libdrm|libgbm|libfontconfig|libfreetype|libasound|libpulse\w*"
    r"|libz|libexpat|libpng16|libbrotli\w*|libuuid)\.so[.\d]*$")
GLIBC = re.compile(r"GLIBC_(\d+)\.(\d+)")


def digest(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            hasher.update(block)
    return hasher.hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def needed(binary):
    """Resolved shared libraries from ldd, name -> path (None for the vDSO)."""
    output = subprocess.run(["ldd", str(binary)], check=True, capture_output=True, text=True).stdout
    libraries = {}
    for line in output.splitlines():
        line = line.strip()
        if "not found" in line:
            raise FileNotFoundError(f"{binary.name}: {line}")
        name = line.split()[0]
        match = re.search(r"=>\s+(\S+)", line)
        libraries[os.path.basename(name)] = match.group(1) if match else None
    return libraries


def glibc_floor(binary):
    output = subprocess.run(["objdump", "-T", str(binary)], check=True, capture_output=True, text=True).stdout
    versions = {(int(a), int(b)) for a, b in GLIBC.findall(output)}
    return max(versions) if versions else (0, 0)


def copy_licenses(source, destination, recursive=True):
    count = 0
    for directory, folders, files in os.walk(source):
        folders[:] = [name for name in folders if name not in (".git", "target", "build")]
        for filename in files:
            if LICENSE.match(filename):
                path = Path(directory) / filename
                target = destination / path.relative_to(source)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
                count += 1
        if not recursive:
            break
    return count


def rust_notices(metadata, destination):
    notices = []
    for package in metadata["packages"]:
        if package["source"] is None:
            continue
        source = Path(package["manifest_path"]).parent
        target = destination / f"{package['name']}-{package['version']}"
        copied = copy_licenses(source, target, recursive=False)
        for parent in source.parents:
            if (parent / ".git").exists():
                copied += copy_licenses(parent, target / "repository", recursive=False)
                break
        if package.get("license_file") and (source / package["license_file"]).is_file():
            target.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / package["license_file"], target)
            copied += 1
        if not copied:
            for filename in ("README.md", "README", "Readme.md"):
                if (source / filename).is_file():
                    target.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(source / filename, target / filename)
                    break
        notices.append({key: package.get(key) for key in ("name", "version", "license", "repository", "source")})
    return notices


def find_worker(build):
    """ctr-worker in a CMake tree (bin/<config>/, bin/) or a CI native archive (root)."""
    for pattern in ("bin/*/ctr-worker", "bin/ctr-worker", "ctr-worker"):
        found = sorted(build.glob(pattern))
        if found:
            return found[0]
    raise FileNotFoundError(f"no ctr-worker under {build}")


def check_tool(path, name):
    url, expected = TOOLS[name]
    actual = digest(path)
    if actual != expected:
        raise ValueError(f"{path}: sha256 {actual}, expected {expected} ({url})")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ("rust", "metadata", "azahar", "citnlo", "output", "appimagetool", "runtime"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--azahar-source", type=Path, help="pinned integration checkout, for notices")
    parser.add_argument("--version", required=True)
    args = parser.parse_args()

    pins = tomllib.loads((ROOT / "config/pins.toml").read_text(encoding="utf-8"))
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if args.version != version:
        raise ValueError(f"Requested {args.version}; Cargo version is {version}")
    check_tool(args.appimagetool, "appimagetool")
    check_tool(args.runtime, "runtime")

    name = f"LOTUS-v{version}-linux-x86_64"
    appdir = args.output / f"{name}.AppDir"
    appdir.mkdir(parents=True, exist_ok=False)
    shutil.copy2(ROOT / "packaging/linux/AppRun", appdir / "AppRun")
    shutil.copy2(ROOT / "packaging/linux/lotus.desktop", appdir / "lotus.desktop")
    shutil.copy2(ROOT / "packaging/linux/lotus.png", appdir / "lotus.png")
    os.symlink("lotus.png", appdir / ".DirIcon")
    (appdir / "AppRun").chmod(0o755)

    binaries = []
    for program in ("lotus-desktop", "lotus-mcp", "ctr-cli"):
        target = appdir / "usr/bin" / program
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(args.rust / program, target)
        binaries.append(target)
    runtimes = {}
    for backend, build, branch, identity in (("azahar", args.azahar, "integration", "AZAHAR"),
                                             ("citnlo", args.citnlo, "experimental", "CITnLO")):
        manifest = json.loads((build / "lotus-build-manifest.json").read_text(encoding="utf-8"))
        if manifest["source"] != pins["azahar"][f"{branch}_commit"] or manifest["branch"] != branch:
            raise ValueError(f"Wrong source/flavor for {backend}: {manifest['source']} {manifest['branch']}")
        if f"-DCTR_BACKEND_NAME={identity}" not in manifest["cmake_flags"]:
            raise ValueError(f"Wrong backend identity for {backend}")
        worker = find_worker(build)
        target = appdir / "runtimes" / backend / "ctr-worker"
        target.parent.mkdir(parents=True)
        shutil.copy2(worker, target)
        manifest.pop("source_root", None)
        runtimes[backend] = manifest
        write_json(target.parent / "build-manifest.json", manifest)
        binaries.append(target)

    host, bundled, floor = {}, {}, (0, 0)
    for binary in binaries:
        floor = max(floor, glibc_floor(binary))
        for library, path in needed(binary).items():
            if HOST.match(library):
                host.setdefault(library, []).append(binary.name)
            elif path:
                bundled[library] = path
    if bundled:
        raise ValueError(f"Libraries outside the host contract; bundle them deliberately: {bundled}")

    licenses = appdir / "usr/share/licenses"
    shutil.copy2(ROOT / "LICENSE", appdir / "LICENSE")
    metadata = json.loads(args.metadata.read_text(encoding="utf-8"))
    write_json(licenses / "rust-packages.json", rust_notices(metadata, licenses / "rust"))
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    copy_licenses(sysroot / "share/doc/rust", licenses / "rust-toolchain")
    if args.azahar_source:
        copy_licenses(args.azahar_source, licenses / "azahar")
    source_sha = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
    (appdir / "SOURCE.md").write_text(
        f"# Corresponding source\n\nLOTUS {version}: https://github.com/estyseesghosts/lotus/tree/{source_sha}\n\n"
        f"AZAHAR: https://github.com/estyseesghosts/azahar/tree/{pins['azahar']['integration_commit']}\n\n"
        f"CITnLO: https://github.com/estyseesghosts/azahar/tree/{pins['azahar']['experimental_commit']}\n\n"
        f"GPUI: https://github.com/zed-industries/zed/tree/{pins['gpui']['rev']}\n\n"
        "Rust dependency versions are fixed by Cargo.lock; usr/share/licenses/rust-packages.json\n"
        "records their licenses. Build steps: docs/27-build-and-run.md section 11.\n",
        encoding="utf-8")
    files = {path.relative_to(appdir).as_posix(): digest(path)
             for path in sorted(appdir.rglob("*")) if path.is_file() and not path.is_symlink()}
    write_json(appdir / "release-manifest.json", {
        "application": "LOTUS", "version": version, "source": source_sha, "target": "linux-x86_64",
        "native_pins": pins["azahar"], "gpui": pins["gpui"], "runtimes": runtimes,
        "minimum_glibc": f"{floor[0]}.{floor[1]}",
        "host_libraries": {name: sorted(set(users)) for name, users in sorted(host.items())},
        "rustc": subprocess.check_output(["rustc", "-vV"], text=True).strip(),
        "appimage_tools": {name: {"url": url, "sha256": sha} for name, (url, sha) in TOOLS.items()},
        "files_sha256": files,
    })

    image = args.output / f"{name}.AppImage"
    environment = dict(os.environ, ARCH="x86_64", APPIMAGE_EXTRACT_AND_RUN="1")
    subprocess.run([str(args.appimagetool), "--no-appstream", "--runtime-file", str(args.runtime),
                    str(appdir), str(image)], check=True, env=environment)
    (args.output / f"{name}.AppImage.sha256").write_text(f"{digest(image)}  {image.name}\n", encoding="utf-8")
    print(image)


if __name__ == "__main__":
    sys.exit(main())
