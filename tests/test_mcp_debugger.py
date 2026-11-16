"""Diagnose owned guest bugs using only MCP tool/resource calls.
python tests/test_mcp_debugger.py <server> <host.json> <rom-handle> <elf-handle> <output>
"""
import json
from pathlib import Path
import socket
import sys
import time
from mcp_client import Client

server,config,rom,elf,output=sys.argv[1:]
output=Path(output);output.mkdir(parents=True,exist_ok=False)
client=Client(server,config,output/"transcript.jsonl")
try:
    for new in (False,True):
        card="debug-new" if new else "debug-old"
        info=client.tool("lotus_create_session",{"kind":"primary","card":card})
        session=info["session"]
        def debug(operation,**params):
            return client.tool("lotus_debug",{"session":session,"command":{"operation":operation,**params}})
        debug("settings_set",name="is_new_3ds",value=new)
        client.tool("lotus_load",{"session":session,"asset":rom})
        debug("load_symbols",asset=elf)
        def address(query):
            found=debug("resolve_symbol",query=query)["addresses"]
            assert len(found)==1,(query,found)
            return found[0]
        main=address("main");mode=address("mode");canary=address("canary")
        location=debug("symbolize",addresses=[main])["locations"][0]
        assert location["line"] and debug("resolve_symbol",query=f"fixture.c:{location['line']}")["addresses"]
        bp=debug("breakpoint_set",address=main,kind="execute",length=4)
        stopped=client.tool("lotus_run_until",{"session":session,"frames":1})
        assert stopped["stop"]["reason"]=="breakpoint",stopped
        debug("breakpoint_remove",id=bp["id"])
        # A second controller can inspect but cannot mutate while GDB owns the lease.
        listener=client.tool("lotus_gdb",{"session":session,"operation":"start"})
        host,port=listener["address"].rsplit(':',1)
        sock=socket.create_connection((host,int(port)),timeout=3)
        payload=b"qSupported";sock.sendall(b'$'+payload+b'#'+f'{sum(payload)%256:02x}'.encode())
        data=b''
        while b'#' not in data:data+=sock.recv(8192)
        assert b'PacketSize=' in data,data
        debug("registers",core=0)
        denied=client.tool("lotus_debug",{"session":session,"command":{"operation":"pause"}},expected_error=True)
        assert "lease" in str(denied),denied
        sock.close()
        for attempt in range(50):
            result=client.send("tools/call",{"name":"lotus_debug","arguments":{"session":session,"command":{"operation":"pause"}}})
            if not result.get("isError"):break
            time.sleep(.02)
        else:raise AssertionError("GDB disconnect leaked the debug lease")
        client.tool("lotus_gdb",{"session":session,"operation":"stop"})
        for bug,reason in ((1,"exception"),(2,"watchpoint"),(3,"svc_break"),(4,"exception"),(5,"svc_break")):
            client.tool("lotus_control",{"session":session,"operation":"reset"})
            if bug==2:debug("breakpoint_set",address=canary+4,kind="write",length=4)
            debug("write_memory",address=mode,bytes=[bug,0,0,0])
            stopped=client.tool("lotus_run_until",{"session":session,"frames":1})
            assert stopped["stop"]["reason"]==reason,stopped
            report=debug("crash_report",core=0)
            assert report["failure_domain"]=="guest"
            if bug==1:assert stopped["stop"]["address"]==0,stopped
            if bug==2:
                assert stopped["stop"]["address"]==canary+4,stopped
                assert debug("read_memory",address=canary+4,length=4)["bytes"]==[0xad,0xb,0,0]
            if bug==5:assert "lotus-hio:hello" in json.dumps(debug("logs",after=0,limit=128))
            (output/f"{'new' if new else 'old'}-bug-{bug}.json").write_text(json.dumps(report,indent=2))
        client.tool("lotus_control",{"session":session,"operation":"reset"})
        debug("write_memory",address=mode,bytes=[6,0,0,0])
        client.tool("lotus_run_until",{"session":session,"frames":1})
        deadlock=debug("deadlocks")
        assert any(len(cycle)==2 for cycle in deadlock["mutex_cycles"]),deadlock
        (output/f"{'new' if new else 'old'}-deadlock.json").write_text(json.dumps(deadlock,indent=2))
        client.tool("lotus_control",{"session":session,"operation":"reset"})
        debug("write_memory",address=mode,bytes=[7,0,0,0])
        client.tool("lotus_run_until",{"session":session,"frames":2})
        assert debug("read_memory",address=address("race_ready"),length=4)["bytes"]==[1,0,0,0]
        raced=debug("read_memory",address=address("raced_counter"),length=4)["bytes"]
        assert raced==([1,0,0,0] if new else [2,0,0,0]),raced
        race={"new_3ds":new,"expected":2,"actual":raced[0],"threads":debug("threads")}
        (output/f"{'new' if new else 'old'}-race.json").write_text(json.dumps(race,indent=2))
        client.tool("lotus_control",{"session":session,"operation":"stop"})
        # Relaunch the same primary card: stale sentinels/manifests must not lock it out.
        second=client.tool("lotus_create_session",{"kind":"primary","card":card,"asset":rom})
        client.tool("lotus_control",{"session":second["session"],"operation":"stop"})
        print(f"MCP-only {'New' if new else 'Old'} 3DS: symbols, lease, crash/overwrite/HIO/deadlock/model-specific race, relaunch passed",flush=True)
    # EOF must close active workers and mark their retained primary manifests closed.
    client.tool("lotus_create_session",{"kind":"primary","card":"debug-shutdown","asset":rom})
finally:
    client.close()
storage=Path(json.loads(Path(config).read_text())["storage_root"])
for card in ("debug-old","debug-new","debug-shutdown"):
    manifest=json.loads((storage/"cards"/card/"native-manifest.json").read_text())
    assert manifest["lifecycle"]=="closed" and manifest["pid"] is None,manifest
print("MCP EOF retained truthful closed manifests for all primary cards",flush=True)
