"""package-linux.py finds ctr-worker in every layout it is given (#108).

The CI native job archives the worker at the archive root; local CMake builds
leave it in bin/<config>/ or bin/.
"""

import importlib.util
import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package_linux", ROOT / "scripts/package-linux.py")
package_linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_linux)


def main() -> int:
    with tempfile.TemporaryDirectory() as temp:
        base = pathlib.Path(temp)
        for layout in ("bin/Release/ctr-worker", "bin/ctr-worker", "ctr-worker"):
            build = base / layout.replace("/", "_")
            worker = build / layout
            worker.parent.mkdir(parents=True)
            worker.write_text("")
            found = package_linux.find_worker(build)
            if found != worker:
                raise AssertionError(f"{layout}: found {found}")
        empty = base / "empty"
        empty.mkdir()
        try:
            package_linux.find_worker(empty)
        except FileNotFoundError:
            pass
        else:
            raise AssertionError("missing worker not reported")
    print("package-linux worker lookup: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
