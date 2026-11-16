"""Archive committed LOTUS and recursive native submodule sources, without user data."""
import argparse
from pathlib import Path
import shutil
import subprocess
import tarfile
import zipfile


def archive(destination, repository, revision, prefix):
    process = subprocess.Popen(["git", "-C", str(repository), "archive", "--format=tar", revision],
                               stdout=subprocess.PIPE)
    with tarfile.open(fileobj=process.stdout, mode="r|") as source:
        for member in source:
            if member.isfile():
                with source.extractfile(member) as content, destination.open(prefix + member.name, "w") as output:
                    shutil.copyfileobj(content, output)
    if process.wait() != 0:
        raise RuntimeError(f"Source archive failed: {repository}")
    tree = subprocess.check_output(["git", "-C", str(repository), "ls-tree", "-r", "-z", revision])
    for entry in tree.split(b"\0"):
        if entry.startswith(b"160000 "):
            metadata, name = entry.split(b"\t", 1)
            path = name.decode("utf-8")
            archive(destination, repository / path, metadata.split()[2].decode(), prefix + path + "/")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(args.output)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parents[1]
    with zipfile.ZipFile(args.output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as destination:
        archive(destination, root, "HEAD", "LOTUS-source/")
    print(args.output)


if __name__ == "__main__":
    main()
