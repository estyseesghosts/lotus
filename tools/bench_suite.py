"""Run the Windows stock, native-worker, and frontend performance comparisons (#10)."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

from bench import CONFIGS, manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("stock-oracle", "integration", "citnlo", "mcp", "desktop", "rom", "output"):
        parser.add_argument(f"--{binary}", required=True, type=Path)
    parser.add_argument("--config", action="append", choices=sorted(CONFIGS.keys() - {"off"}),
                        help="CITnLO instrumentation profile to test separately; off always runs")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=120)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--warmup-seconds", type=float, default=5)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--new-3ds", action="store_true")
    parser.add_argument("--require-idle", action="store_true")
    args = parser.parse_args()
    if not (0 < args.frames <= 6000 and 0 <= args.warmup <= 6000 and args.repeats > 0
            and args.seconds > 0 and args.warmup_seconds >= 0):
        parser.error("use positive repeats, frames and seconds, and nonnegative warmup (at most 6000 frames)")
    for name in ("stock_oracle", "integration", "citnlo", "mcp", "desktop", "rom"):
        path = getattr(args, name)
        if not path.is_file():
            parser.error(f"{name} file does not exist: {path}")
    for name in ("stock_oracle", "integration", "citnlo"):
        build = manifest(getattr(args, name).resolve()) or {}
        if not build.get("source") or "-DCMAKE_BUILD_TYPE=Release" not in build.get("cmake_flags", []):
            parser.error(f"{name} requires a Release build manifest with a source SHA and CMake flags")

    configs = args.config or ["svc_ipc_trace", "gpu_counters", "gpu_events", "rewind_interval_10_fast"]
    common = ["--rom", str(args.rom.resolve()), "--frame-limit", "0", "--repeats", str(args.repeats)]
    if args.new_3ds:
        common.append("--new-3ds")
    if args.require_idle:
        common.append("--require-idle")
    native = ["--worker", f"integration={args.integration.resolve()}",
              "--worker", f"citnlo={args.citnlo.resolve()}",
              "--setting", "deterministic_async_operations=true",
              "--warmup", str(args.warmup), "--frames", str(args.frames), "--config", "off"]
    for config in dict.fromkeys(configs):
        native += ["--config", config]
    frontend = ["--mcp", str(args.mcp.resolve()), "--desktop", str(args.desktop.resolve()),
                "--worker", str(args.citnlo.resolve()), "--deterministic-async",
                "--warmup-seconds", str(args.warmup_seconds), "--seconds", str(args.seconds)]
    phases = (
        ("native-cold", "bench.py", native + ["--stock-oracle", str(args.stock_oracle.resolve())]),
        ("native-warm", "bench.py", native + ["--renderer", "vulkan", "--shader-cache", "warm"]),
        ("frontends", "bench_frontends.py", frontend),
    )
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    report = {"tool": "tools/bench_suite.py", "reports": {}}
    summary = []
    failures = []
    for label, script, flags in phases:
        directory = out / label
        command = [sys.executable, str(Path(__file__).with_name(script)), *common, *flags,
                   "--output", str(directory)]
        code = subprocess.run(command, check=False).returncode
        report_path = directory / "report.json"
        phase = json.loads(report_path.read_text(encoding="utf-8")) if report_path.exists() else None
        report["reports"][label] = {"exit_code": code, "command": command, "report": phase}
        if code != 0 or phase is None:
            failures.append(f"{label} failed (exit {code})")
        elif args.require_idle and phase["host_busy"]:
            failures.append(f"{label} ran on a busy host")
        if phase is not None and label.startswith("native-"):
            if phase.get("build_manifest_stale"):
                failures.append(f"{label} native executable and manifest source SHAs differ")
            for renderer, by_worker in phase["results"].items():
                baseline = by_worker["citnlo"]["off"]
                if baseline.get("ratio_vs_integration") is None or not baseline.get("guest_identical_to_integration"):
                    failures.append(f"{label}/{renderer} native baselines are incomparable")
                for config, result in by_worker["citnlo"].items():
                    if result.get("completed") != args.repeats or not result.get("guest_repeatable"):
                        failures.append(f"{label}/{renderer}/{config} was skipped, incomplete or nonrepeatable")
            if label == "native-cold":
                stock = phase["stock_oracle"]
                if stock["completed"] != args.repeats or not stock["guest_repeatable"]:
                    failures.append("stock oracle was incomplete or nonrepeatable")
                if any(keys.keys() - {"output_type"} for keys in stock["profile_differences"].values()):
                    failures.append("stock and workers used different core profiles")
        elif phase is not None and any(not entry["profiles_match"] for entry in phase["results"].values()):
            failures.append("frontend effective profiles differ")
        phase_summary = directory / "summary.md"
        summary += [f"## {label}", "",
                    phase_summary.read_text(encoding="utf-8").strip() if phase_summary.exists()
                    else "No phase summary was produced.", ""]
        report["failures"] = failures
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        lines = ["# Lotus performance suite", "", *[f"- {item}" for item in failures], "", *summary]
        (out / "summary.md").write_text("\n".join(lines), encoding="utf-8")
    if failures:
        raise SystemExit(f"Performance suite has unqualified results. See {out / 'summary.md'}")


if __name__ == "__main__":
    main()
