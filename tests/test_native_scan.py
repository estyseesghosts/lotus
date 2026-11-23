"""CITnLO scan correctness and pause-time benchmark using the owned scanner fixture."""
import argparse
import json
from pathlib import Path
import statistics
import sys
import time

from mcp_client import Client

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from native_debug_probe import Peer


REGION_BYTES = 1 << 20
PAGE_BYTES = 4096
MMIO_STATE = 2  # Kernel::MemoryState::IO in vm_manager.h.


def pairs(result):
    return [(row["address"], row["value"]) for row in result["matches"]]


def pipelined_pause(worker, fixture, output, zeroes, samples):
    peer = Peer(worker, output)
    try:
        peer.call("hello")
        peer.debug("settings_set", name="graphics_api", value=0)
        peer.call("load", {"path": str(fixture)})
        peer.call("run_until", {"frames": 1})
        assert peer.debug("scan", address=zeroes, length=REGION_BYTES,
                          width=1, value=0)["total"] == REGION_BYTES
        times = []
        for _ in range(samples):
            request = peer.sequence + 1
            batch = peer.frame("debug", {"operation": "scan_narrow", "mode": "unchanged"})
            batch += peer.frame("pause")
            start = time.perf_counter()
            peer.process.stdin.write(batch)
            peer.process.stdin.flush()
            assert peer.receive(request)["total"] == REGION_BYTES
            peer.receive(request + 1)
            assert peer.state == "paused"
            times.append((time.perf_counter() - start) * 1000)
        return times
    finally:
        peer.close()


def run(args):
    output = args.output.resolve()
    output.mkdir()
    config = output / "host.json"
    fixture = args.fixture.resolve()
    config.write_text(json.dumps({
        "worker": str(args.worker.resolve()), "storage_root": str(output / "storage"),
        "allow_control": True, "max_sessions": 1, "prefer_vulkan": False,
        "assets": {"scanner": str(fixture), "symbols": str(fixture.with_suffix(".elf"))},
    }), encoding="utf-8")

    client = Client(args.server.resolve(), config, output / "mcp.jsonl",
                    stderr_path=output / "stderr.txt")
    try:
        session = client.tool("lotus_create_session", {
            "kind": "secondary", "asset": "scanner",
        })["session"]

        def debug(operation, *, error=False, **fields):
            return client.tool("lotus_debug", {
                "session": session, "command": {"operation": operation, **fields},
            }, expected_error=error)

        client.tool("lotus_run_until", {"session": session, "frames": 1})
        debug("load_symbols", asset="symbols")
        base, = debug("resolve_symbol", query="scan_area")["addresses"]
        assert base % PAGE_BYTES == 0
        zeroes = base + 2 * PAGE_BYTES
        samples = []
        for _ in range(args.samples):
            iteration = {}
            for operation, fields in [
                ("scan", {"address": zeroes, "length": REGION_BYTES, "width": 1, "value": 0}),
                ("scan_narrow", {"mode": "unchanged"}),
            ]:
                start = time.perf_counter()
                result = debug(operation, **fields)
                elapsed = (time.perf_counter() - start) * 1000
                assert result["width"] == 1 and result["total"] == REGION_BYTES, result
                assert pairs(result) == [(zeroes + index, 0) for index in range(256)]
                stats = result.get("scan_stats")
                if not args.baseline:
                    assert stats and stats["candidates_checked"] == REGION_BYTES, stats
                    assert stats["page_checks"] == stats["bulk_reads"] == 256, stats
                    assert stats["bytes_read"] == REGION_BYTES, stats
                    assert stats["candidate_capacity_bytes"] <= 32 * REGION_BYTES, stats
                    assert stats["worker_us"] < 250_000, stats
                iteration[operation] = {"rpc_ms": elapsed, "native": stats}
            for offset in (255, REGION_BYTES - 2, REGION_BYTES):
                page = debug("scan_results", offset=offset, limit=2)
                assert page["total"] == REGION_BYTES
                assert pairs(page) == [(zeroes + index, 0)
                                       for index in range(offset, min(offset + 2, REGION_BYTES))]
            start = time.perf_counter()
            client.tool("lotus_control", {"session": session, "operation": "pause"})
            iteration["pause_rpc_ms"] = (time.perf_counter() - start) * 1000
            samples.append(iteration)
            print(json.dumps(iteration), flush=True)

        for width in (1, 2, 4, 8):
            # The first value straddles the page for widths >= 4. Matches are sparse,
            # and their values are deliberately unsorted after the edit.
            start = base + PAGE_BYTES - 3
            prefix = 1 << (width * 8 - 2)
            value = prefix + 9
            original = [value, 3, value, 2, value, 5]

            def write(values):
                data = b"".join(item.to_bytes(width, "little") for item in values)
                debug("write_memory", address=start, bytes=list(data))
                assert debug("read_memory", address=start, length=len(data))["bytes"] == list(data)

            write(original)
            scan = {"address": start, "length": len(original) * width, "width": width, "value": value}
            addresses = [start + width * index for index in (0, 2, 4)]
            assert pairs(debug("scan", **scan)) == [(address, value) for address in addresses]
            assert pairs(debug("scan_narrow", mode="unchanged")) == [
                (address, value) for address in addresses
            ]
            write([prefix + 12, 3, prefix + 10, 2, prefix + 11, 5])
            expected = list(zip(addresses, [prefix + 12, prefix + 10, prefix + 11], strict=True))
            changed = debug("scan_narrow", mode="changed")
            assert pairs(changed) == expected, changed
            assert pairs(debug("scan_narrow", mode="range", value=prefix + 10, upper=prefix + 11)
                         ) == expected[1:]
            assert pairs(debug("scan_narrow", mode="exact", value=prefix + 11)) == expected[-1:]
            assert debug("scan_narrow", mode="exact", value=prefix + 13)["total"] == 0

        regions = debug("memory_map")["regions"]
        fixture_process, = {r["process"] for r in regions
                            if r["address"] <= base < r["address"] + r["length"]}
        regions = sorted((r for r in regions if r["process"] == fixture_process),
                         key=lambda r: r["address"])
        # The code and data VMAs of the fixture are adjacent. A candidate may
        # cross this boundary if both sides are readable.
        across = base - 2
        data = debug("read_memory", address=across, length=4)["bytes"]
        value = int.from_bytes(bytes(data), "little")
        assert pairs(debug("scan", address=across, length=4, width=4,
                           value=value)) == [(across, value)]
        assert pairs(debug("scan_narrow", mode="unchanged")) == [(across, value)]

        # Exercise dense, unaligned groups on multiple pages, including their tails.
        address = zeroes + PAGE_BYTES - 3
        for width in (2, 4, 8):
            total = 2 * PAGE_BYTES // width
            assert debug("scan", address=address, length=2 * PAGE_BYTES,
                         width=width, value=0)["total"] == total
            assert debug("scan_narrow", mode="unchanged")["total"] == total
            assert pairs(debug("scan_results", offset=total - 1, limit=1)) == [
                (address + (total - 1) * width, 0),
            ]

        boundary = next(left["address"] + left["length"]
                        for left, right in zip(regions, regions[1:])
                        if left["address"] <= base < left["address"] + left["length"]
                        < right["address"])
        last_byte, = debug("read_memory", address=boundary - 1, length=1)["bytes"]
        assert pairs(debug("scan", address=boundary - 1, length=1, width=1,
                           value=last_byte)) == [(boundary - 1, last_byte)]
        result = debug("scan", address=zeroes, length=8, width=1, value=0)
        assert result["total"] == 8
        debug("scan", error=True, address=boundary - 1, length=2, width=1, value=last_byte)
        mmio = next(region["address"] for region in regions if region["state"] == MMIO_STATE)
        debug("scan", error=True, address=mmio, length=4, width=4, value=0)
        for address, length in ((0, 4), (0xFFFF_FFFF, 8), (zeroes, 0), (zeroes, REGION_BYTES + 1)):
            debug("scan", error=True, address=address, length=length, width=1, value=0)
        if not args.baseline:
            # Failed scans leave the committed result and its width intact.
            page = debug("scan_results", offset=0, limit=8)
            assert pairs(page) == pairs(result) and page["width"] == 1
            assert page["scan_generation"] == result["scan_generation"]

        pointer_address = base + PAGE_BYTES - 2
        debug("write_memory", address=pointer_address, bytes=list(zeroes.to_bytes(4, "little")))
        chain = debug("pointer_chain", base=pointer_address, offsets=[3])
        assert chain["address"] == zeroes + 3
        assert chain["hops"] == [{"address": pointer_address, "pointer": zeroes, "offset": 3}]
        client.tool("lotus_control", {"session": session, "operation": "stop"})
    finally:
        client.close()
    summary = {
        "samples": samples,
        "median_rpc_ms": {name: statistics.median(sample[name]["rpc_ms"] for sample in samples)
                          for name in ("scan", "scan_narrow")},
        "pause_behind_narrow_ms": pipelined_pause(args.worker.resolve(), fixture, output / "wire",
                                                  zeroes, args.samples),
    }
    (output / "samples.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({"median_rpc_ms": summary["median_rpc_ms"],
                      "pause_behind_narrow_ms": summary["pause_behind_narrow_ms"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "worker", "fixture", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--samples", type=int, choices=range(1, 11), default=5)
    run(parser.parse_args())
