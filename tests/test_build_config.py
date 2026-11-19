"""Build-speed settings from #117 stay in place."""

import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]


def main() -> int:
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    errors = []
    if cargo.get("profile", {}).get("dev", {}).get("debug") != "line-tables-only":
        errors.append("Cargo.toml [profile.dev] debug is not line-tables-only")
    for name in ("windows.yml", "linux.yml"):
        text = (ROOT / ".github/workflows" / name).read_text(encoding="utf-8")
        if "CARGO_BUILD_JOBS" in text:
            errors.append(f"{name}: CARGO_BUILD_JOBS caps cargo parallelism")
        if "actions/cache@" in text or "Swatinem/rust-cache@" not in text:
            errors.append(f"{name}: not using Swatinem/rust-cache")
        if "\n  lint:\n" not in text:
            errors.append(f"{name}: no parallel lint job")
    if "-fuse-ld=mold" not in (ROOT / ".github/workflows/linux.yml").read_text(encoding="utf-8"):
        errors.append("linux.yml: not linking with mold")
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
