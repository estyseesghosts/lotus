"""Two homebrew ROMs with title ID zero must never share incompatible states."""
import argparse
from pathlib import Path
from native_debug_probe import Peer

parser=argparse.ArgumentParser(__doc__)
parser.add_argument('worker',type=Path)
parser.add_argument('first',type=Path)
parser.add_argument('second',type=Path)
parser.add_argument('output',type=Path)
args=parser.parse_args()
for new in (False,True):
    peer=Peer(args.worker.resolve(),args.output.resolve()/('new' if new else 'old'))
    try:
        peer.call('hello')
        peer.debug('settings_set',name='is_new_3ds',value=new)
        peer.call('load',{'path':str(args.first.resolve())})
        peer.call('run_until',{'frames':1})
        saved=peer.debug('registers',core=0)['registers']
        peer.debug('save_state',name='owned')
        peer.call('load',{'path':str(args.second.resolve())})
        before=peer.debug('registers',core=0)['registers']
        peer.call('debug',{'operation':'load_state','name':'owned'},expected='invalid_argument')
        assert peer.debug('registers',core=0)['registers']==before
        peer.call('load',{'path':str(args.first.resolve())})
        peer.debug('load_state',name='owned')
        assert peer.debug('registers',core=0)['registers']==saved
        path=peer.root/'checkpoints/owned.bin'
        data=bytearray(path.read_bytes());data[-1]^=1;path.write_bytes(data)
        peer.call('debug',{'operation':'load_state','name':'owned'},expected='invalid_argument')
        assert peer.debug('registers',core=0)['registers']==saved
        print(f'State ROM identity and integrity New3DS={new}: passed',flush=True)
    finally:peer.close()
