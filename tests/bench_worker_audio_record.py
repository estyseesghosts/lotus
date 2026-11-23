"""Measure exact worker Record revisions with test-only lock and write probes."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import wave


def replace_once(code, old, new):
    if code.count(old) != 1:
        raise ValueError(f"Expected one source match for {old!r}")
    return code.replace(old, new, 1)


def measure(args, revision, name):
    run = args.output / name
    headers = run / "audio_core"
    headers.mkdir(parents=True)
    sha = subprocess.check_output(["git", "-C", args.source, "rev-parse", revision], text=True).strip()

    def header(file):
        return subprocess.check_output(
            ["git", "-C", args.source, "show", f"{sha}:src/audio_core/{file}"], text=True
        )

    sink = header("worker_sink.h")
    sink = replace_once(
        sink,
        "std::scoped_lock lock{mutex};\n        for (size_t i",
        "const auto requested = Bench::Clock::now();\n"
        "        std::scoped_lock lock{mutex};\n"
        "        Bench::Hold hold{Bench::Clock::now()};\n"
        "        Bench::waits.push_back(hold.acquired - requested);\n"
        "        for (size_t i",
    )
    queued = '#include "audio_core/worker_wav_capture.h"' in sink
    if queued:
        writer = replace_once(header("worker_wav_capture.h"), "file.write(block.data(), count);",
                              "Bench::MeasureWrite([&] { file.write(block.data(), count); });")
        (headers / "worker_wav_capture.h").write_text(writer, encoding="utf-8")
    else:
        write_call = "capture.write(static_cast<const char*>(data), bytes);"
        sink = replace_once(sink, write_call, f"Bench::MeasureWrite([&] {{ {write_call} }});")
    (headers / "worker_sink.h").write_text(sink, encoding="utf-8")

    executable = run / "record-bench.exe"
    bench = Path(__file__).with_name("worker_audio_record_bench.cpp").resolve()
    subprocess.run([
        args.compiler, "-O3", "-std=c++20", "-DENABLE_SOFTWARE_RENDERER", "-DENABLE_VULKAN",
        *(["-DBENCH_QUEUED_CAPTURE"] if queued else []),
        f"-I{run}", f"-I{args.build / 'src'}", f"-I{args.source / 'src'}",
        f"-I{args.source / 'externals/fmt/include'}", f"-I{args.source / 'externals/boost'}",
        bench, "-o", executable,
    ], check=True)
    def sample(mode, index):
        wav = run / f"{mode}-{index}.wav"
        data = json.loads(subprocess.check_output([executable, mode, wav], text=True))
        if mode == "off":
            assert not wav.exists() and data["pcm_write"]["count"] == 0
        else:
            with wave.open(str(wav), "rb") as capture:
                assert (capture.getnchannels(), capture.getsampwidth(), capture.getframerate()) == (2, 2, 32728)
                pcm = capture.readframes(capture.getnframes())
            dropped = data.get("dropped_frames", 0)
            assert dropped == 0 or (queued and mode == "slow"), data
            assert len(pcm) == 4 * (data["samples"] - dropped)
            assert wav.stat().st_size == len(pcm) + 44
            frame = b"".join(struct.pack("<hh", i, -i) for i in range(160))
            assert pcm == (frame * (data["samples"] // 160))[:len(pcm)]
            assert data["pcm_write"]["count"] > 0
            data["pcm_sha256"] = hashlib.sha256(pcm).hexdigest()
        return data

    modes = {}
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        for mode in ("off", "on", "slow"):
            runs = [pool.submit(sample, mode, index) for index in range(args.workers)]
            modes[mode] = [run.result() for run in runs]
    return {"source": sha, "modes": modes}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="native worker source checkout and dependencies")
    parser.add_argument("build", type=Path, help="configured native build tree with generated headers")
    parser.add_argument("compiler", type=Path, help="Windows GNU C++ compiler on the DLL search path")
    parser.add_argument("baseline", help="pre-change native Git revision")
    parser.add_argument("updated", help="native Git revision with this capture change")
    parser.add_argument("output", type=Path, help="new output directory")
    parser.add_argument("--workers", type=int, choices=range(1, 5), default=1,
                        help="run up to four benchmark processes simultaneously")
    args = parser.parse_args()
    args.output.mkdir(parents=True)
    report = {name: measure(args, revision, name) for name, revision in (
        ("baseline", args.baseline), ("updated", args.updated),
    )}
    modes = [data for variant in report.values() for runs in variant["modes"].values() for data in runs]
    assert len({(data["samples"], data["hash"]) for data in modes}) == 1
    assert len({data["pcm_sha256"] for data in modes
                if "pcm_sha256" in data and data.get("dropped_frames", 0) == 0}) == 1
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(args.output / "report.json")


if __name__ == "__main__":
    main()
