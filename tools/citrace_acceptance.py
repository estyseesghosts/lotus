"""Record native GPU actions, replay with CPU held, compare actual frame bytes."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import time
from native_debug_probe import Peer

parser=argparse.ArgumentParser(__doc__)
parser.add_argument("worker",type=Path)
parser.add_argument("rom",type=Path)
parser.add_argument("output",type=Path)
args=parser.parse_args()

def hashes(peer):
    return {frame["screen"]:hashlib.sha256((peer.root/frame["path"]).read_bytes()[32:]).hexdigest()
            for frame in peer.call("capture")["frames"]}

for new in (False,True):
    for vulkan in (False,True):
        root=args.output.resolve()/f"{'new' if new else 'old'}-{'vulkan' if vulkan else 'software'}"
        peer=Peer(args.worker.resolve(),root)
        try:
            peer.call("hello",{"notifications":True})
            for name,value in {"is_new_3ds":new,"graphics_api":2 if vulkan else 0}.items():
                peer.debug("settings_set",name=name,value=value)
            peer.call("load",{"path":str(args.rom.resolve())})
            peer.call("run_until",{"frames":60})
            start=time.monotonic()
            peer.call("run_until",{"frames":2})
            baseline_seconds=time.monotonic()-start
            peer.debug("citrace_start")
            start=time.monotonic()
            peer.call("run_until",{"frames":2})
            recording_seconds=time.monotonic()-start
            original=hashes(peer)
            recorded=peer.debug("citrace_stop")
            assert recorded["frames"]==2 and not recorded["truncated"],recorded
            trace=root/recorded["artifacts"][0]["path"]
            data=trace.read_bytes()
            assert data[:4]==b'CiTr' and len(data)<64*1024*1024
            header=struct.unpack_from('<25I',data)
            assert header[1]==1 and header[2]==100
            stream_offset,stream_size=header[-2:]
            assert stream_offset+stream_size*20==len(data)
            assert stream_size==recorded["elements"]
            replay=peer.debug("citrace_replay")
            assert replay["frames"]==2 and replay["cpu_executed"] is False,replay
            actual=hashes(peer)
            assert original==actual,(original,actual)
            # Replaying the same trace again starts from its paired initial state.
            peer.debug("citrace_replay")
            assert hashes(peer)==original
            # Malformed data must be rejected before loading the initial state.
            corrupt=bytearray(data);struct.pack_into('<I',corrupt,96,0xffffffff)
            trace.write_bytes(corrupt)
            peer.call("debug",{"operation":"citrace_replay"},expected="invalid_argument")
            trace.write_bytes(data)
            peer.call("run_until",{"frames":1},expected="invalid_argument")
            peer.debug("load_state",name="lotus-citrace")
            peer.call("run_until",{"frames":1})
            report=dict(new_3ds=new,renderer="vulkan" if vulkan else "software",hashes=original,
                        recorded=recorded,replay=replay,baseline_host_seconds=baseline_seconds,
                        recording_host_seconds=recording_seconds,file_sha256=hashlib.sha256(data).hexdigest())
            (root/'acceptance.json').write_text(json.dumps(report,indent=2))
            print(json.dumps(report),flush=True)
        finally:peer.close()
