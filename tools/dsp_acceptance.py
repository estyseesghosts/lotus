"""Owned DSP handshake and PCM captures. Optional firmware stays in private roots."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import wave
from native_debug_probe import Peer
from debugger_acceptance import symbols

parser=argparse.ArgumentParser(__doc__)
parser.add_argument('worker',type=Path)
parser.add_argument('rom',type=Path)
parser.add_argument('output',type=Path)
parser.add_argument('--firmware',type=Path)
parser.add_argument('--mode',choices=['hle','lle','lle-mt'],default='hle')
args=parser.parse_args()
if args.mode!='hle' and not args.firmware:parser.error('LLE requires --firmware <private dspfirm.cdc>')
addresses=symbols(args.rom.with_suffix('.elf'))
for new in (False,True):
    hashes=[]
    for repeat in range(3):
        peer=Peer(args.worker.resolve(),args.output.resolve()/f"{'new' if new else 'old'}-{repeat}")
        try:
            peer.call('hello')
            peer.debug('settings_set',name='is_new_3ds',value=new)
            peer.debug('settings_set',name='audio_emulation',value={'hle':0,'lle':1,'lle-mt':2}[args.mode])
            if args.firmware:
                destination=peer.root/'sdmc/3ds/dspfirm.cdc';destination.parent.mkdir(parents=True)
                shutil.copyfile(args.firmware,destination)
            peer.call('load',{'path':str(args.rom.resolve())})
            if args.mode!='hle':peer.debug('dsp_load_firmware')
            peer.debug('audio_capture',enabled=True)
            status=peer.call('run_until',{'frames':60})
            stage=peer.debug('read_memory',address=addresses['stage'],length=4)['bytes']
            failure=peer.debug('read_memory',address=addresses['failure'],length=4)['bytes']
            assert stage==[5,0,0,0] and not any(failure),(stage,failure,status)
            capture=peer.debug('audio_capture',enabled=False)
            with wave.open(str(peer.root/capture['artifacts'][0]['path']),'rb') as file:
                assert (file.getnchannels(),file.getsampwidth(),file.getframerate())==(2,2,32728)
                pcm=file.readframes(file.getnframes())
            assert len(pcm)>1000 and any(pcm),'DSP produced no nonzero PCM'
            digest=hashlib.sha256(pcm).hexdigest();hashes.append(digest)
            report=dict(new_3ds=new,mode=args.mode,repeat=repeat,pcm_bytes=len(pcm),pcm_sha256=digest,
                        source=status['source'],stage=stage[0])
            (peer.root/'acceptance.json').write_text(json.dumps(report,indent=2))
            print(json.dumps(report),flush=True)
        finally:peer.close()
    assert len(set(hashes))==1,('DSP captures were not repeatable',new,hashes)
