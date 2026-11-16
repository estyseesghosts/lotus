"""T29: scenario-driven movie record/playback with the determinism profile.

Per model: record a movie three times from the scenario input schedule
(repeat mode rejects differing frame hashes), play the first recording three
times read-only with no scenario input, and require the playback checkpoints
to equal the recording's. A run with neither input nor movie shows whether the
recorded input changes the content's screens (reported, not asserted: content
may ignore some buttons).
Usage: test_scenario_movie.py <ctr-cli> <experimental-worker> <3dsx> <output>
"""
import json
from pathlib import Path
import subprocess
import sys

cli, worker, rom, output = (Path(a).resolve() for a in sys.argv[1:5])
output.mkdir(parents=True, exist_ok=False)
INPUT = """  schedule:
    - { at_frame: 20, button: right, action: down }
    - { at_frame: 30, button: A, action: press }
    - { at_frame: 40, button: touch, action: down, x: 160, y: 120 }
    - { at_frame: 42, button: touch, action: up, x: 160, y: 120 }
    - { at_frame: 50, button: right, action: up }"""


def scenario(name, model, movie, schedule):
    return f"""version: 1
name: {name}
content: {{ kind: homebrew_3dsx, path_handle: {json.dumps(str(rom))} }}
profile: {{ model: {model}, renderer: software, worker_flavor: experimental, audio_output: discard, determinism: true }}
storage: {{ mode: isolated_copy }}
input:
  owner: scenario
{schedule if schedule else "  schedule: []"}
stop: {{ max_frames: 60, wall_timeout_seconds: 90 }}
{movie}captures:
  - {{ when: {{ frame: 35 }}, include: [top_raw_frame, bottom_raw_frame] }}
  - {{ when: {{ frame: 45 }}, include: [top_raw_frame, bottom_raw_frame] }}
assertions:
  - {{ type: frame_count, minimum: 60 }}
"""


def run(path, count):
    root = path.with_suffix("")
    subprocess.run([str(cli), "scenario", "run", str(path), str(worker), str(root), str(count)],
                   check=True, timeout=900)
    reports = [json.loads(p.read_text()) for p in root.glob("*/report.json")]
    assert len(reports) == count, reports
    return root, reports


def points(report):
    return [(c["frame"], c["screen"], c["sha256"]) for c in report["checkpoints"]]


for model in ("old3ds", "new3ds"):
    record = output / f"{model}-record.yaml"
    record.write_text(scenario(f"{model}-movie-record", model, "movie: { mode: record, name: take1 }\n", INPUT))
    root, recorded = run(record, 3)
    take = next(root.glob("*/take1.ctm"))
    play = output / f"{model}-play.yaml"
    play.write_text(scenario(f"{model}-movie-play", model,
                             f"movie: {{ mode: play, name: take1, path_handle: {json.dumps(str(take))} }}\n", ""))
    _, played = run(play, 3)
    idle = output / f"{model}-idle.yaml"
    idle.write_text(scenario(f"{model}-idle", model, "", ""))
    _, (idle_report,) = run(idle, 1)
    reference = points(recorded[0])
    assert played[0]["movie"]["status"]["input_index"] == played[0]["movie"]["status"]["total_inputs"] > 0
    assert all(points(r) == reference for r in recorded + played), "playback diverged from recording"
    movie = played[0]["movie"]["status"]
    summary = dict(model=model, checkpoints=len(reference), recorded_inputs=recorded[0]["movie"]["status"]["input_index"],
                   playback_consumed=movie["input_index"], input_changes_output=points(idle_report) != reference,
                   recorded_source=recorded[0]["source"])
    (output / f"{model}-summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary), flush=True)
