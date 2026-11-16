"""MCP-only audio focus switching across two sessions (T46).

Two sessions run the owned audio fixture while focus moves A, B, A. Focus must
be exclusive after every switch, and each session's raw DSP capture must equal
a reference session that was never focused: switching host output does not
pause, perturb or drop guest audio. Host-device glitches are not measured;
the capture is taken before host volume and stretching.
"""
import hashlib
import json
from pathlib import Path
import sys
import wave
from mcp_client import Client

server, config, rom, output = sys.argv[1:]
output = Path(output)
output.mkdir(parents=True, exist_ok=False)
storage = Path(json.loads(Path(config).read_text())["storage_root"])
client = Client(server, config, output / "transcript.jsonl")


def debug(session, operation, **params):
    return client.tool("lotus_debug", {"session": session, "command": {"operation": operation, **params}})


def focused(session):
    return client.tool("lotus_session_status", {"session": session})["status"]["audio_focused"]


def pcm(card):
    with wave.open(str(storage / "cards" / card / "audio" / "capture.wav"), "rb") as wav:
        return wav.readframes(wav.getnframes())


try:
    sessions = {}
    for card in ("focus-ref", "focus-a", "focus-b"):
        session = client.tool("lotus_create_session", {"kind": "primary", "card": card})["session"]
        client.tool("lotus_load", {"session": session, "asset": rom})
        debug(session, "audio_capture", enabled=True)
        sessions[card] = session
    ref, a, b = sessions["focus-ref"], sessions["focus-a"], sessions["focus-b"]
    client.tool("lotus_run_until", {"session": ref, "frames": 60})
    steps = []
    for target in (a, b, a):
        debug(target, "audio_focus", enabled=True)
        state = {name: focused(s) for name, s in sessions.items()}
        steps.append(state)
        assert sum(state.values()) == 1 and focused(target), state
        for session in (a, b):
            client.tool("lotus_run_until", {"session": session, "frames": 20})
    debug(a, "audio_focus", enabled=False)
    assert not any(focused(s) for s in sessions.values())
    hashes = {}
    for card, session in sessions.items():
        debug(session, "audio_capture", enabled=False)
        samples = pcm(card)
        assert len(samples) > 32728 * 2, (card, len(samples))
        hashes[card] = hashlib.sha256(samples).hexdigest()
    report = dict(focus_steps=steps, sha256=hashes)
    (output / "report.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report), flush=True)
    assert len(set(hashes.values())) == 1, hashes
    for session in sessions.values():
        client.tool("lotus_control", {"session": session, "operation": "stop"})
finally:
    client.close()
