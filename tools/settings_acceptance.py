"""T24 apply classes and T37 GPU timing/shader setting observables.

Apply classes: every binding has a known class and a consumer citation;
non-live classes are rejected on a running session with the class named;
live settings are accepted. GPU observables use a homebrew GPU ROM and
measure guest ticks (never host time) at successive PICA draw stops:
  simulate_3ds_gpu_timings     delays GSP interrupts (gsp_gpu.cpp:675)
  delay_game_render_thread_us  delays SubmitCmdList replies (gsp_gpu.cpp:886)
  use_shader_jit               JIT and interpreter both match the reference
                               shader trace of the draw (pica_core.cpp:73)
  shaders_accurate_mul         hardware shaders only (gpu.cpp:448); with
                               use_hw_shader off it is recorded, not claimed.
Software must reproduce baseline ticks exactly. Vulkan was observed to drift
(96 ticks at early draws, then reconverge), so on Vulkan a shift is claimed
only when it exceeds the measured baseline jitter.
Vertex inputs are not compared across Vulkan sessions: they were observed to
differ at identical guest ticks.
"""
import argparse
import json
from pathlib import Path
from native_debug_probe import Peer

CLASSES = {"live", "renderer", "inert", "boot", "cfg-savedata"}
CASES = {
    "baseline": {},
    "baseline_repeat": {},
    "simulate_3ds_gpu_timings": {"simulate_3ds_gpu_timings": True},
    "delay_game_render_thread_us": {"delay_game_render_thread_us": 8000},
    "use_shader_jit_off": {"use_shader_jit": False},
    "shaders_accurate_mul_off": {"shaders_accurate_mul": False},
}


def classes(worker, rom, root):
    peer = Peer(worker, root)
    try:
        peer.call("hello")
        rows = []
        while True:
            page = peer.debug("settings_list", offset=len(rows), limit=32)["settings"]
            rows += page
            if len(page) < 32:
                break
        bad = [r["name"] for r in rows if r["apply"] not in CLASSES or not r["consumer_sources"]]
        assert not bad, bad
        counts = {c: sum(r["apply"] == c for r in rows) for c in CLASSES}
        peer.call("load", {"path": str(rom)})
        refused = {}
        for row in rows:
            if row["apply"] in ("renderer", "inert", "boot") and row["apply"] not in refused:
                reply = peer.call("debug", dict(operation="settings_set", name=row["name"], value=row["value"]),
                                  expected="invalid_argument")
                assert reply["reason"].startswith(row["apply"]), reply
                refused[row["apply"]] = row["name"]
        for name, value in (("battery_level", 1), ("steps_per_hour", 7), ("audio_muted", True)):
            assert peer.debug("settings_set", name=name, value=value)["value"] == value
        return dict(counts=counts, refused_while_running=refused)
    finally:
        peer.close()


def draws(worker, rom, root, new, settings, vulkan):
    peer = Peer(worker, root)
    try:
        peer.call("hello", {"notifications": True})
        base = {"is_new_3ds": new, "graphics_api": 2 if vulkan else 0, "use_hw_shader": False}
        for name, value in {**base, **settings}.items():
            peer.debug("settings_set", name=name, value=value)
        peer.call("load", {"path": str(rom)})
        peer.call("run_until", {"frames": 30})
        peer.debug("gpu_configure", enabled=True, stop_events=[2])
        ticks, outputs = [], None
        for _ in range(6):
            stop = peer.call("run_until", {"frames": 2})["stop"]
            assert stop["reason"] == "pica", stop
            ticks.append(peer.debug("registers", core=0)["guest_ticks"])
            if outputs is None:
                trace = peer.debug("shader_trace", draw=stop["draw"], vertex=0)
                rows = json.loads((root / trace["artifacts"][0]["path"]).read_text())
                outputs = [r.get("dest_f32_bits") for r in rows["instructions"]]
                matches = rows["matches_frozen_output"]
        return dict(guest_ticks=ticks, deltas=[b - a for a, b in zip(ticks, ticks[1:])],
                    shader_outputs=outputs, engine_matches_reference_trace=matches)
    finally:
        peer.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("debug_rom", type=Path, help="owned debugger fixture")
    parser.add_argument("gpu_rom", type=Path, help="homebrew GPU ROM (user asset)")
    parser.add_argument("output", type=Path)
    parser.add_argument("--classes-only", action="store_true",
                        help="integration (AZAHAR) workers lack the PICA draw stops")
    args = parser.parse_args()
    out = args.output.resolve()
    report = {"classes": classes(args.worker.resolve(), args.debug_rom.resolve(), out / "classes")}
    print(json.dumps(report["classes"]), flush=True)
    for new in () if args.classes_only else (False, True):
        for vulkan in (False, True):
            label = f"{'new' if new else 'old'}-{'vulkan' if vulkan else 'software'}"
            runs = {case: draws(args.worker.resolve(), args.gpu_rom.resolve(), out / label / case, new, s, vulkan)
                    for case, s in CASES.items()}
            base = runs["baseline"]
            def shift(case):
                return max(abs(a - b) for a, b in zip(runs[case]["guest_ticks"], base["guest_ticks"]))
            jitter = shift("baseline_repeat")
            checks = {
                "baseline_ticks_repeatable": jitter == 0 or vulkan,
                "gpu_timings_shift_guest_ticks": shift("simulate_3ds_gpu_timings") > 10 * jitter,
                "render_delay_shifts_guest_ticks": shift("delay_game_render_thread_us") > 10 * jitter,
                "shader_jit_matches_trace": base["engine_matches_reference_trace"],
                "shader_interpreter_matches_trace": runs["use_shader_jit_off"]["engine_matches_reference_trace"],
            }
            report[label + "_observations"] = dict(
                baseline_jitter_ticks=jitter,
                vertex_outputs_repeat=runs["baseline_repeat"]["shader_outputs"] == base["shader_outputs"],
                accurate_mul_off_outputs_equal=runs["shaders_accurate_mul_off"]["shader_outputs"] == base["shader_outputs"])
            report[label] = dict(checks=checks, runs=runs)
            print(json.dumps({"profile": label, "checks": checks, "observations": report[label + "_observations"],
                              "first_ticks": {k: v["guest_ticks"][0] for k, v in runs.items()}}), flush=True)
            (out / "acceptance.json").write_text(json.dumps(report, indent=2))
            failed = [k for k, ok in checks.items() if not ok]
            assert not failed, failed
