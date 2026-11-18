"""lotus#50: generate the Azahar settings inventory from the pinned workers.

Starts each worker with an empty root and pages `settings_list` (ctr_worker/debug.h). It
records every binding's name, JSON type, stock default, range, choices, apply class and
consumer citations, as the worker reports them. The output is the generated half of the
settings registry (crates/settings), which adds grouping, scopes and availability. Settings
present on only one branch are listed with that branch only.

Usage: settings_inventory.py --integration <ctr-worker> --experimental <ctr-worker> <out.json>
"""
import argparse
import hashlib
import json
import tempfile
import tomllib
from pathlib import Path
from native_debug_probe import Peer

ROOT = Path(__file__).resolve().parents[1]


def kind(value):
    # nlohmann reports "number" for both; Python's json keeps 1 and 1.0 apart, as the worker
    # serialises integers and floating-point settings differently.
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "float"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    return "object"


def dump(worker, root):
    peer = Peer(worker, root)
    try:
        hello = peer.call("hello")
        rows = []
        while True:
            page = peer.debug("settings_list", offset=len(rows), limit=32)
            rows += page["settings"]
            if page["next"] is None:
                break
        assert len(rows) == page["total"], (len(rows), page["total"])
        return hello, rows
    finally:
        peer.close()


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--integration", type=Path, required=True)
parser.add_argument("--experimental", type=Path, required=True)
parser.add_argument("output", type=Path)
args = parser.parse_args()
pins = tomllib.loads((ROOT / "config/pins.toml").read_text())
settings = {}
sources = {}
with tempfile.TemporaryDirectory() as scratch:
    for branch, worker in (("integration", args.integration), ("experimental", args.experimental)):
        hello, rows = dump(worker.resolve(), Path(scratch) / branch)
        sources[branch] = dict(
            flavor=hello.get("flavor"),
            worker_sha256=hashlib.sha256(worker.read_bytes()).hexdigest(),
            source=hello.get("source"),
            total=len(rows),
        )
        names = [row["name"] for row in rows]
        assert len(names) == len(set(names)), "duplicate setting names"
        for row in rows:
            entry = settings.setdefault(row["name"], dict(
                name=row["name"], type=kind(row["default"]), default=row["default"],
                range=row["range"], choices=row["choices"], apply={}, consumer_sources=row["consumer_sources"]))
            for field in ("default", "range", "choices"):
                assert entry[field] == row[field], (row["name"], field, entry[field], row[field])
            entry["apply"][branch] = row["apply"]
inventory = dict(
    generated_by="tools/settings_inventory.py",
    pins={branch: pins.get(branch, {}) for branch in sources},
    sources=sources,
    settings=sorted(settings.values(), key=lambda entry: entry["name"]),
)
args.output.write_text(json.dumps(inventory, indent=1, sort_keys=False) + "\n")
print(len(settings), "settings;", {b: s["total"] for b, s in sources.items()})
