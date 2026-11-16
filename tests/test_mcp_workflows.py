"""MCP-only predicate, reverse and isolated-fork checks using the owned fixture."""
import json
from pathlib import Path
import sys
import time
from mcp_client import Client

server,config,rom,elf,output=sys.argv[1:]
output=Path(output);output.mkdir(parents=True,exist_ok=False)
client=Client(server,config,output/"transcript.jsonl")
try:
    for new in (False,True):
        session=client.tool("lotus_create_session",{"kind":"primary","card":"flow-new" if new else "flow-old"})["session"]
        def debug(operation,session=session,**params):
            return client.tool("lotus_debug",{"session":session,"command":{"operation":operation,**params}})
        def wait(session=session):
            deadline=time.monotonic()+22
            while time.monotonic()<deadline:
                value=client.tool("lotus_session_status",{"session":session})["status"]
                if value.get("stop",{}).get("reason")=="predicate":return value
                time.sleep(.02)
            raise TimeoutError(value)
        debug("settings_set",name="is_new_3ds",value=new)
        client.tool("lotus_load",{"session":session,"asset":rom})
        debug("load_symbols",asset=elf)
        def address(name):return debug("resolve_symbol",query=name)["addresses"][0]
        body=address("lotus_tick_body");watched=address("watched");counter=address("counter")
        debug("run_until",predicate={"kind":"pc","core":0,"address":body},frames=2,timeout_ms=5000)
        stopped=wait();assert stopped["stop"]["pc"]==body,stopped
        debug("rewind_configure",enabled=True,capacity=32,budget_mib=64)
        bp=debug("breakpoint_set",address=watched,kind="write",length=4)
        for attempt in range(5):
            first=debug("step",core=0,count=100)
            if first["stop"]["reason"]=="watchpoint":break
        assert first["stop"]["reason"]=="watchpoint",first
        assert debug("read_memory",address=watched,length=4)["bytes"]==[2,0,0,0]
        reverse=debug("reverse_continue")
        assert reverse["stop"]["reverse"] and reverse["stop"]["reason"]=="watchpoint",reverse
        assert debug("read_memory",address=watched,length=4)["bytes"]==[0x44,0x33,0x22,0x11]
        replay=debug("step",core=0,count=1)
        assert replay["stop"]["address"]==first["stop"]["address"],replay
        debug("reverse_step",core=0)
        debug("breakpoint_remove",id=bp["id"])
        forks=debug("fork",count=2)["forks"]
        before=debug("read_memory",address=watched,length=4)["bytes"]
        for child in forks:
            assert debug("read_memory",session=child["id"],address=watched,length=4)["bytes"]==before
        debug("write_memory",session=forks[0]["id"],address=watched,bytes=[9,0,0,0])
        assert debug("read_memory",address=watched,length=4)["bytes"]==before
        assert debug("read_memory",session=forks[1]["id"],address=watched,length=4)["bytes"]==before
        child=forks[0]["id"]
        debug("run_until",session=child,predicate={"kind":"memory","core":0,"address":counter,"width":4,"value":6},frames=2,timeout_ms=5000)
        result=wait(child)
        assert result["stop"]["predicate"]["value"]==6,result
        assert debug("read_memory",session=child,address=counter,length=4)["bytes"]==[6,0,0,0]
        for child in forks:client.tool("lotus_control",{"session":child["id"],"operation":"stop"})
        debug("rewind_configure",enabled=False,capacity=32,budget_mib=64)
        for predicate in ({"kind":"log","regex":"lotus-fixture:ready"},{"kind":"svc","id":0x3d,"enter":True},
                          {"kind":"frame","frame":2}):
            client.tool("lotus_control",{"session":session,"operation":"reset"})
            debug("run_until",predicate=predicate,frames=3,timeout_ms=5000)
            result=wait();assert result["stop"]["predicate"]["kind"]==predicate["kind"],result
        client.tool("lotus_control",{"session":session,"operation":"stop"})
        print(f"{'New' if new else 'Old'} 3DS MCP predicates/reverse-watchpoint/fork isolation passed",flush=True)
finally:client.close()
