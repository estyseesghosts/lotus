"""Native settings acceptance for one worker branch via MCP.

python tests/test_mcp_settings_apply.py <lotus-mcp> <worker> <homebrew> <fresh output>
"""
import json
from pathlib import Path
import sys
import time

from mcp_client import Client


def run(client):
    def create(card):
        return client.tool("lotus_create_session", {
            "kind": "primary", "card": card, "asset": "demo",
        })["session"]

    def card_target(card):
        return {"target": "card", "card": card}

    def change(target, key, value, **kwargs):
        return client.tool("lotus_settings_change", {
            "target": target, "operation": "set", "key": key, "value": value,
        }, **kwargs)

    def settings(target):
        return client.tool("lotus_settings", {"target": target})

    def control(session, operation):
        return client.tool("lotus_control", {"session": session, "operation": operation})

    def status(session):
        return client.tool("lotus_session_status", {"session": session})

    def get(session, name):
        return client.tool("lotus_debug", {
            "session": session, "command": {"operation": "settings_get", "name": name},
        })["value"]

    a, b = create("a"), create("b")
    target = card_target("a")
    control(a, "run")
    control(b, "run")
    before = status(a)
    sibling_volume = get(b, "volume")
    edit = change(target, "volume", 0.25)
    assert edit["changes"] == [
        {"key": "volume", "session": a, "status": "applied", "reason": None}
    ], edit["changes"]
    assert get(a, "volume") == 0.25
    assert get(b, "volume") == sibling_volume
    deadline = time.monotonic() + 2
    after = status(a)
    while after["status"]["frames"] <= before["status"]["frames"] and time.monotonic() < deadline:
        time.sleep(0.05)
        after = status(a)
    assert after["session"]["telemetry"]["epoch"] == before["session"]["telemetry"]["epoch"]
    assert after["status"]["frames"] > before["status"]["frames"]

    changed = {
        "resolution_factor": ("renderer", 2),
        "is_new_3ds": ("boot", True),
        "cfg.000a0002": ("cfg-savedata", "02"),
    }
    for key, (apply, value) in changed.items():
        actual = get(a, key)
        assert actual != value, (key, actual)
        edit = change(target, key, value)
        assert edit["changes"][0]["status"] == "pending", edit["changes"]
        pending = edit["pending_details"][key]
        assert pending["apply"] == apply and pending["next_action"] == "start_card", pending
        assert pending["desired"] == value and pending["effective"] == actual, pending
        assert get(a, key) == actual
        scoped = settings({"target": "session", "session": a})
        assert scoped["settings"][key]["value"] == actual
        assert scoped["desired_settings"][key]["value"] == value

    change(target, "current_input_profile_index", 1, expected_error=True)
    change(target, "graphics_api", 1, expected_error=True)
    assert "current_input_profile_index" not in settings(target)["settings"]

    control(a, "stop")
    a = create("a")
    for key, (_, value) in changed.items():
        assert get(a, key) == value
    assert settings(target)["pending"] == []
    assert get(a, "volume") == 0.25
    control(a, "stop")
    control(b, "stop")


def main():
    server, worker, fixture, output = (Path(p).resolve() for p in sys.argv[1:])
    output.mkdir(parents=True)
    config = output / "host.json"
    config.write_text(json.dumps({
        "worker": str(worker), "storage_root": str(output / "storage"),
        "assets": {"demo": str(fixture)}, "allow_control": True, "max_sessions": 2,
    }))
    client = Client(server, config, output / "transcript.jsonl")
    try:
        run(client)
    finally:
        client.close()
    print("Live, renderer, boot, CFG savedata and inert settings verified.")


if __name__ == "__main__":
    main()
