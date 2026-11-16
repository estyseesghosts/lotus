"""Guest DSP/PCM acceptance. No synthetic host samples or bundled firmware."""
import argparse
import hashlib
import json
from pathlib import Path
import wave
from native_debug_probe import Peer
from debugger_acceptance import symbols

parser=argparse.ArgumentParser(__doc__)
parser.add_argument("worker",type=Path)
parser.add_argument("rom",type=Path)
parser.add_argument("output",type=Path)
parser.add_argument("--audible",action="store_true")
args=parser.parse_args()
addresses=symbols(args.rom.with_suffix('.elf'))
reports=[]
for new in (False,True):
    hashes=[]
    for repeat in range(3):
        root=args.output.resolve()/f"{'new' if new else 'old'}-{repeat}"
        peer=Peer(args.worker.resolve(),root)
        try:
            peer.call("hello",{"notifications":True})
            peer.debug("settings_set",name="is_new_3ds",value=new)
            peer.call("load",{"path":str(args.rom.resolve())})
            peer.debug("audio_capture",enabled=True)
            if args.audible and repeat==0:peer.debug("audio_focus",enabled=True)
            status=peer.call("run_until",{"frames":60})
            stage=peer.debug("read_memory",address=addresses["stage"],length=4)["bytes"]
            failure=peer.debug("read_memory",address=addresses["failure"],length=4)["bytes"]
            assert stage==[5,0,0,0] and failure==[0,0,0,0],(stage,failure,status)
            capture=peer.debug("audio_capture",enabled=False)
            with wave.open(str(root/"audio/capture.wav"),'rb') as wav:
                assert wav.getframerate()==32728 and wav.getnchannels()==2 and wav.getsampwidth()==2
                samples=wav.readframes(wav.getnframes())
            assert any(samples) and len(samples)>32728*2, (status,capture,len(samples))
            digest=hashlib.sha256(samples).hexdigest();hashes.append(digest)
            dsp=peer.debug("dsp_inspect")
            assert any(region["voices"][0]["playing"] for region in dsp["regions"]),dsp
            report={"new_3ds":new,"repeat":repeat,"pcm_bytes":len(samples),"sha256":digest,
                "audio_samples":status["audio_samples"],"voice":dsp["regions"][0]["voices"][0],
                "sample_rate":32728,"stage":stage}
            (root/"acceptance.json").write_text(json.dumps(report,indent=2));reports.append(report)
            print(json.dumps(report),flush=True)
        finally:peer.close()
    assert len(set(hashes))==1,hashes
(args.output/"report.json").write_text(json.dumps(reports,indent=2))
