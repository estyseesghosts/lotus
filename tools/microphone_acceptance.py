"""Owned guest reads injected PCM; state restore must retain the source cursor."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import wave
from native_debug_probe import Peer
from debugger_acceptance import symbols

parser=argparse.ArgumentParser(__doc__)
parser.add_argument('worker',type=Path)
parser.add_argument('rom',type=Path)
parser.add_argument('output',type=Path)
args=parser.parse_args()
addresses=symbols(args.rom.with_suffix('.elf'))
for new in (False,True):
    peer=Peer(args.worker.resolve(),args.output.resolve()/('new' if new else 'old'))
    try:
        peer.call('hello')
        directory=peer.root/'media';directory.mkdir()
        pcm=b''.join(struct.pack('<H',(i*73)&65535) for i in range(100000))
        with wave.open(str(directory/'mic.wav'),'wb') as file:
            file.setnchannels(1);file.setsampwidth(2);file.setframerate(32728);file.writeframes(pcm)
        peer.debug('settings_set',name='is_new_3ds',value=new)
        peer.debug('asset_mount',kind='mic_wav',asset='owned-generated-PCM')
        peer.call('load',{'path':str(args.rom.resolve())})
        peer.call('run_until',{'frames':2})
        stage=peer.debug('read_memory',address=addresses['stage'],length=4)['bytes']
        failure=peer.debug('read_memory',address=addresses['failure'],length=4)['bytes']
        assert stage==[5,0,0,0] and not any(failure),(stage,failure)
        def buffer():return bytes(peer.debug('read_memory',address=addresses['mic_buffer'],length=4096)['bytes'])
        initial=buffer()
        assert initial[:32]==pcm[:32] and struct.unpack_from('<I',initial,4092)[0]>0
        peer.debug('save_state',name='mic')
        peer.call('run_until',{'frames':2})
        expected=buffer()
        peer.debug('load_state',name='mic')
        peer.call('run_until',{'frames':2})
        actual=buffer()
        assert actual==expected,'mic source cursor changed after checkpoint restore'
        report=dict(new_3ds=new,guest_buffer_sha256=hashlib.sha256(actual).hexdigest(),
                    injected_pcm_sha256=hashlib.sha256(pcm).hexdigest(),stage=stage[0])
        (peer.root/'acceptance.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report),flush=True)
    finally:peer.close()
