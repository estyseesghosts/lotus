"""MCP movie determinism, scaled captures and repeated fresh-root matrices.
Args: server host-config approved-rom output-directory
"""
import json
from pathlib import Path
import sys
from mcp_client import Client

server,config,rom,output=sys.argv[1:]
output=Path(output);output.mkdir(parents=True,exist_ok=False)
client=Client(server,config,output/'transcript.jsonl')
try:
    for new in (False,True):
        card='movie-new' if new else 'movie-old'
        expected=None
        for repeat in range(4):
            session=client.tool('lotus_create_session',{'kind':'primary','card':card})['session']
            def debug(op,**params):
                return client.tool('lotus_debug',{'session':session,'command':{'operation':op,**params}})
            debug('settings_set',name='is_new_3ds',value=new)
            debug('settings_set',name='deterministic_async_operations',value=True)
            if repeat==0:debug('movie_record',name='owned')
            else:debug('movie_play',name='owned',read_only=True)
            client.tool('lotus_load',{'session':session,'asset':rom})
            captures=[]
            for frames in (20,20,20):
                if repeat==0:
                    client.tool('lotus_input',{'session':session,'owner':'scenario',
                        'buttons':1 if len(captures)==1 else 0,'x':120,'y':80,'touch':len(captures)==1})
                client.tool('lotus_run_until',{'session':session,'frames':frames})
                result=debug('capture_scaled',scale=1)
                captures.append([a['sha256'] for a in result['artifacts']])
                if repeat>0:
                    if len(captures)==1:
                        debug('save_state',name='movie-read-only')
                        client.tool('lotus_run_until',{'session':session,'frames':1})
                        debug('load_state',name='movie-read-only')
                    denied=client.tool('lotus_debug',{'session':session,'command':{
                        'operation':'input_state','owner':'live','state':{'buttons':0,'x':0,'y':0,'touch':False,
                            'circle_pad':[0,0],'c_stick':[0,0],'acceleration':[0,0,0],
                            'gyroscope':[0,0,0],'slider_3d':0,'volume':1}}},expected_error=True)
                    assert 'movie' in str(denied),denied
            status=debug('movie_status')
            assert status['dropped_inputs']==0,status
            if repeat==0:expected=captures
            else:assert captures==expected,(new,repeat,captures,expected)
            scaled=debug('capture_scaled',scale=2)['artifacts']
            assert [(a['width'],a['height']) for a in scaled]==[(800,480),(640,480)]
            stopped=debug('movie_stop')
            assert stopped['artifacts'],stopped
            client.tool('lotus_control',{'session':session,'operation':'stop'})
        (output/f'{card}.json').write_text(json.dumps({'new_3ds':new,'checkpoints':expected,'replays':3},indent=2))
        print(f'Movie Old/New={new}: recording + three equal replays and scaled capture passed',flush=True)

    session=client.tool('lotus_create_session',{'kind':'secondary'})['session']
    client.tool('lotus_load',{'session':session,'asset':rom})
    matrix=client.tool('lotus_debug',{'session':session,'command':{'operation':'matrix',
        'profiles':[{'name':name,'backend':None,'settings':{'deterministic_async_operations':True}} for name in ('a','b')],
        'frames':6,'checkpoints':[3,6],'repeats':3}})
    assert all(p['repeatable_at_checkpoints'] for p in matrix['report']['profiles']),matrix
    assert matrix['report']['comparisons'][0]['earliest_sampled_divergence'] is None,matrix
    (output/'matrix.json').write_text(json.dumps(matrix,indent=2))
    client.tool('lotus_control',{'session':session,'operation':'stop'})
    print('Two identical fresh-root matrix profiles, three repeats each, passed',flush=True)
finally:client.close()
