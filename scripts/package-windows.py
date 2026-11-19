"""Stage and assemble portable x64 builds; fail on unresolved non-system DLLs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tomllib
import zipfile
import package_silvestris

ROOT = Path(__file__).resolve().parents[1]
LICENSE = re.compile(r"^(licen[cs]e|copying|notice|copyright|authors|patents)", re.I)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def imports(path):
    """Read normal/delay PE imports. Only AMD64 PE32+ binaries belong in this ZIP."""
    data = path.read_bytes()
    pe, = struct.unpack_from("<I", data, 0x3C)
    if data[:2] != b"MZ" or data[pe:pe + 4] != b"PE\0\0":
        raise ValueError(f"Not a Windows executable: {path}")
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    optional = pe + 24
    optional_size, = struct.unpack_from("<H", data, pe + 20)
    magic, = struct.unpack_from("<H", data, optional)
    if (machine, magic) != (0x8664, 0x20B):
        raise ValueError(f"Not Windows x64: {path}")
    sections = []
    for index in range(count):
        offset = optional + optional_size + 40 * index
        virtual_size, rva, raw_size, raw = struct.unpack_from("<IIII", data, offset + 8)
        sections.append((rva, max(virtual_size, raw_size), raw, raw_size))

    def locate(rva):
        for start, size, raw, raw_size in sections:
            if start <= rva < start + size and rva - start < raw_size:
                return raw + rva - start
        raise ValueError(f"Unmapped PE RVA {rva:x}: {path}")

    def name(rva):
        offset = locate(rva)
        end = data.index(0, offset, offset + 512)
        result = data[offset:end].decode("ascii")
        if not re.fullmatch(r"[\w.+-]+\.dll", result, re.I):
            raise ValueError(f"Invalid DLL name: {result}")
        return result.lower()

    result = set()
    rva, size = struct.unpack_from("<II", data, optional + 112 + 8)
    if rva:
        offset = locate(rva)
        for step in range(0, size, 20):
            fields = struct.unpack_from("<IIIII", data, offset + step)
            if not any(fields):
                break
            result.add(name(fields[3]))
    rva, size = struct.unpack_from("<II", data, optional + 112 + 13 * 8)
    if rva:
        offset = locate(rva)
        base, = struct.unpack_from("<Q", data, optional + 24)
        for step in range(0, size, 32):
            fields = struct.unpack_from("<8I", data, offset + step)
            if not any(fields):
                break
            result.add(name(fields[1] if fields[0] & 1 else fields[1] - base))
    return sorted(result)


def copy_binaries(executables, destination, search):
    destination.mkdir(parents=True, exist_ok=True)
    system = Path(os.environ["SystemRoot"]) / "System32"
    records = {}
    pending = [(path, destination / path.name) for path in executables]
    while pending:
        source, target = pending.pop()
        if target.name.lower() in records:
            if digest(source) != digest(target):
                raise ValueError(f"Conflicting runtime DLL: {target.name}")
            continue
        shutil.copy2(source, target)
        dependencies = imports(target)
        records[target.name.lower()] = dependencies
        for dependency in dependencies:
            if dependency.startswith(("api-ms-", "ext-ms-")) or (system / dependency).is_file():
                continue
            found = next((folder / dependency for folder in [source.parent, *search]
                          if (folder / dependency).is_file()), None)
            if found is None:
                raise FileNotFoundError(f"Missing {dependency}, required by {source.name}")
            if dependency == "msys-2.0.dll":
                raise ValueError("MSYS runtime must not enter a native portable build")
            pending.append((found, destination / dependency))
    return records


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


def stage_rust(args):
    binaries = [args.binary_dir / f"{name}.exe" for name in ("lotus-desktop", "ctr-cli", "lotus-mcp")]
    dependencies = copy_binaries(binaries, args.output / "bin", args.dll_dir)
    metadata = json.loads(args.metadata.read_text(encoding="utf-8"))
    silvestris = package_silvestris.stage(binaries[0], metadata, args.output / "licenses")
    notices = []
    for package in metadata["packages"]:
        if package["source"] is None:
            continue
        source = Path(package["manifest_path"]).parent
        target = args.output / "licenses/rust" / f"{package['name']}-{package['version']}"
        copied = copy_licenses(source, target, recursive=False)
        # Workspace packages (GPUI) inherit the repository's licenses.
        for parent in source.parents:
            if (parent / ".git").exists():
                copied += copy_licenses(parent, target / "repository", recursive=False)
                break
        if package.get("license_file"):
            path = source / package["license_file"]
            if path.is_file():
                target.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target / path.name)
                copied += 1
        if not copied:
            for filename in ("README.md", "README", "Readme.md"):
                if (source / filename).is_file():
                    target.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(source / filename, target / filename)
                    break
        notices.append({key: package.get(key) for key in ("name", "version", "license", "repository", "source")})
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    copy_licenses(sysroot / "share/doc/rust", args.output / "licenses/rust-toolchain")
    for directory in args.dll_dir:
        copy_licenses(directory.parent, args.output / "licenses/mingw")
    write_json(args.output / "licenses/rust-packages.json", notices)
    write_json(args.output / "rust-build.json", {
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
        "cargo": subprocess.check_output(["cargo", "-V"], text=True).strip(),
        "profile": "release", "target": "x86_64-pc-windows-gnu",
        "lto": "thin", "codegen_units": 1, "panic": "abort", "imports": dependencies,
        "silvestris": silvestris,
    })


def stage_runtime(args):
    dependencies = copy_binaries([args.build / "bin/Release/ctr-worker.exe"], args.output, args.dll_dir)
    manifest = json.loads((args.build / "lotus-build-manifest.json").read_text(encoding="utf-8-sig"))
    manifest.pop("source_root", None)
    manifest["imports"] = dependencies
    write_json(args.output / "build-manifest.json", manifest)
    for directory in args.dll_dir:
        copy_licenses(directory.parent, args.output / "licenses/mingw")


def assemble(args):
    pins = tomllib.loads((ROOT / "config/pins.toml").read_text(encoding="utf-8"))
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if args.version != version:
        raise ValueError(f"Requested {args.version}; Cargo version is {version}")
    name = f"LOTUS-v{version}-windows-x64"
    destination = args.output / name
    destination.mkdir(parents=True, exist_ok=False)
    shutil.copytree(ROOT / "packaging/windows", destination, dirs_exist_ok=True)
    shutil.copytree(args.rust, destination, dirs_exist_ok=True)
    silvestris = json.loads((destination / "rust-build.json").read_text(encoding="utf-8"))["silvestris"]
    if any(silvestris[key] != pins["silvestris"][pin] for key, pin in
           (("revision", "rev"), ("tree", "tree"))):
        raise ValueError("Staged desktop has a different Silvestris pin")
    for backend, source, branch in (("azahar", args.azahar, "integration"), ("citnlo", args.citnlo, "experimental")):
        manifest = json.loads((source / "build-manifest.json").read_text(encoding="utf-8"))
        if manifest["source"] != pins["azahar"][f"{branch}_commit"] or manifest["branch"] != branch:
            raise ValueError(f"Wrong source/flavor for {backend}")
        expected_name = "CITnLO" if backend == "citnlo" else "AZAHAR"
        if f"-DCTR_BACKEND_NAME={expected_name}" not in manifest["cmake_flags"]:
            raise ValueError(f"Wrong backend identity for {backend}")
        shutil.copytree(source, destination / "runtimes" / backend)
    shutil.copy2(ROOT / "LICENSE", destination / "LICENSE")
    documents = subprocess.check_output(["git", "-C", str(ROOT), "ls-files", "docs"], text=True).splitlines()
    for document in documents:
        target = destination / document
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / document, target)
    # Source archives and notices contain only tracked upstream inputs, never instance roots.
    copy_licenses(ROOT / "vendor/azahar", destination / "licenses/azahar")
    source_sha = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
    (destination / "SOURCE.md").write_text(
        f"# Corresponding source\n\nLOTUS {version}: https://github.com/estyseesghosts/lotus/tree/{source_sha}\n\n"
        f"AZAHAR / CITnLO: https://github.com/estyseesghosts/azahar/tree/{pins['azahar']['integration_commit']}\n\n"
        f"GPUI: https://github.com/zed-industries/zed/tree/{pins['gpui']['rev']}\n\n"
        f"Silvestris {silvestris['revision']}: vendor/silvestris in the LOTUS source archive.\n\n"
        "The release also includes a recursive LOTUS/native source ZIP. For a Git build, clone\n"
        "the LOTUS release tag with --recurse-submodules; see docs/26-portable-release.md.\n"
        "Rust dependency versions/sources are fixed by Cargo.lock; licenses/rust-packages.json\n"
        "records their SPDX licenses and source identifiers. Native dependencies are included\n"
        "in the recursive source ZIP. Build tools are not required to run this application.\n",
        encoding="utf-8",
    )
    files = {path.relative_to(destination).as_posix(): digest(path)
             for path in sorted(destination.rglob("*")) if path.is_file()}
    write_json(destination / "release-manifest.json", {
        "application": "LOTUS", "version": version, "source": source_sha,
        "target": "windows-x64", "native_pins": pins["azahar"], "gpui": pins["gpui"],
        "silvestris": silvestris,
        "files_sha256": files,
    })
    output = args.output / f"{name}.zip"
    with zipfile.ZipFile(output, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=6,
                         strict_timestamps=False) as archive:
        for path in sorted(destination.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(args.output))
    print(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    rust = commands.add_parser("rust")
    rust.add_argument("--binary-dir", type=Path, required=True)
    rust.add_argument("--metadata", type=Path, required=True)
    runtime = commands.add_parser("runtime")
    runtime.add_argument("--build", type=Path, required=True)
    for command in (rust, runtime):
        command.add_argument("--output", type=Path, required=True)
        command.add_argument("--dll-dir", type=Path, action="append", default=[])
    package = commands.add_parser("assemble")
    for name in ("rust", "azahar", "citnlo", "output"):
        package.add_argument(f"--{name}", type=Path, required=True)
    package.add_argument("--version", required=True)
    args = parser.parse_args()
    {"rust": stage_rust, "runtime": stage_runtime, "assemble": assemble}[args.command](args)


if __name__ == "__main__":
    main()
