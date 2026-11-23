"""Check native WAV lifecycle with an owned guest and two independent Windows workers."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
from pathlib import Path
import sys
import wave

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from native_debug_probe import Peer


BASELINE_PCM = "7c8a00e8f5ed50b0862cc7cc473ca6b2a84a3ff247a92ee39d5a0206c46fda09"
BASELINE_FNV = 4857884725183162789


def pcm(path):
    with wave.open(str(path), "rb") as wav:
        assert (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) == (32728, 2, 2)
        data = wav.readframes(wav.getnframes())
    assert path.stat().st_size == len(data) + 44
    return data


def capture(worker, guest, root, new, renderer):
    peer = Peer(worker, root)
    final = root / "audio/capture.wav"
    pending = final.with_suffix(".wav.part")
    try:
        hello = peer.call("hello", {"notifications": True})
        peer.debug("settings_set", name="is_new_3ds", value=new)
        peer.debug("settings_set", name="graphics_api", value=renderer)
        peer.call("load", {"path": str(guest)})
        initial = peer.debug("audio_capture", enabled=False)
        assert initial["state"] == "idle" and "artifacts" not in initial
        if new:
            # The New 3DS worker exercises orderly process shutdown while recording.
            assert peer.debug("audio_capture", enabled=True)["state"] == "recording"
            assert not final.exists()
            status = peer.call("run_until", {"frames": 60})
            peer.close()
            assert peer.process.returncode == 0
        else:
            # The Old 3DS worker exercises a normal stop and then an interrupted replacement.
            assert peer.debug("audio_capture", enabled=True)["state"] == "recording"
            assert not final.exists()
            status = peer.call("run_until", {"frames": 60})
            stopped = peer.debug("audio_capture", enabled=False)
            assert stopped["state"] == "complete" and stopped["pcm_bytes"] == 131200
            assert stopped["queued_pcm_bytes"] == 0 and stopped["dropped_frames"] == 0
            assert 0 < stopped["queue_peak_pcm_bytes"] <= stopped["queue_capacity_pcm_bytes"]
            assert stopped["artifacts"] == [{"path": "audio/capture.wav", "format": "wav"}]
        assert status["audio_samples"] == 32800
        assert status["audio_hash_fnv1a64"] == BASELINE_FNV
        assert status["audio_focused"] is False
        assert (status["gpu_memory"] is not None) == (renderer == 2)
        assert not pending.exists()
        data = pcm(final)
        assert len(data) == 131200 and hashlib.sha256(data).hexdigest() == BASELINE_PCM
        if not new:
            assert peer.call("run_until", {"frames": 5})["audio_samples"] > 32800
            assert pcm(final) == data
            assert peer.debug("audio_capture", enabled=True)["state"] == "recording"
            peer.call("run_until", {"frames": 8})
            peer.process.kill()
            peer.process.wait(timeout=5)
            assert pending.exists()
            assert pcm(final) == data
        return (hello["backend_name"], hello["source"])
    finally:
        peer.close()


def main():
    worker, guest, output = (Path(arg).resolve() for arg in sys.argv[1:4])
    renderer = {"software": 0, "vulkan": 2}[sys.argv[4] if len(sys.argv) > 4 else "software"]
    output.mkdir(parents=True, exist_ok=False)
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = [
            pool.submit(capture, worker, guest, output / model, new, renderer)
            for model, new in (("old", False), ("new", True))
        ]
        old, new = (result.result() for result in results)
    assert old == new
    print(f"{old[0]} {old[1]} preserved PCM on both models and passed WAV shutdown checks.")


if __name__ == "__main__":
    main()
