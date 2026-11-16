"""Fetch verified, private build tools; never install into Windows directories."""
import argparse
import hashlib
from pathlib import Path
import urllib.request
import zipfile

WINLIBS = (
    "https://github.com/brechtsanders/winlibs_mingw/releases/download/"
    "16.2.0posix-14.0.0-ucrt-r2/winlibs-x86_64-posix-seh-gcc-16.2.0-mingw-w64ucrt-14.0.0-r2.zip",
    "d5dbafc4a170e762ca6143151ec918fb9e2c72736fb14cd704abebc6bdd5276a",
)
SDK = (
    "https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.cpp/10.0.28000.2705/"
    "microsoft.windows.sdk.cpp.10.0.28000.2705.nupkg",
    "a74ca8f9af98bd61925d9e2932ad98f746306123167b8f0bba99cd9ea9f03807",
)


def fetch(root, name, source):
    path = root / name
    if not path.exists():
        urllib.request.urlretrieve(source[0], path)
    with path.open("rb") as source_file:
        if hashlib.file_digest(source_file, "sha256").hexdigest() != source[1]:
            raise ValueError(f"Build-tool checksum mismatch: {path}")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--sdk-only", action="store_true")
    args = parser.parse_args()
    args.root.mkdir(parents=True, exist_ok=True)
    if not args.sdk_only:
        archive = fetch(args.root, "winlibs.zip", WINLIBS)
        if not (args.root / "winlibs/mingw64/bin/g++.exe").exists():
            with zipfile.ZipFile(archive) as source:
                source.extractall(args.root / "winlibs")
    archive = fetch(args.root, "windows-sdk.nupkg", SDK)
    with zipfile.ZipFile(archive) as source:
        for name in ("fxc.exe", "d3dcompiler_47.dll"):
            source.extract(f"c/bin/10.0.28000.0/x64/{name}", args.root / "sdk")
    print(args.root.resolve())


if __name__ == "__main__":
    main()
