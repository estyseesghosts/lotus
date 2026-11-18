"""Local stdio MCP test client, with bounded waits and a retained JSONL transcript."""
import json
from pathlib import Path
import queue
import subprocess
import threading
import time

class Client:
    def __init__(self, executable, config, transcript):
        self.process = subprocess.Popen([str(executable),str(config)],stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding="utf-8")
        self.messages=queue.Queue(64);self.sequence=0
        self.transcript=Path(transcript).open("w",encoding="utf-8")
        def read():
            try:
                for line in self.process.stdout:self.messages.put(json.loads(line))
            finally:self.messages.put(None)
        threading.Thread(target=read,daemon=True).start()
        self.send("initialize",{"protocolVersion":"2025-03-26","capabilities":{},
            "clientInfo":{"name":"lotus-debugger-acceptance","version":"1"}})
        self.send("notifications/initialized",notification=True)

    def record(self,direction,data):
        self.transcript.write(json.dumps({"direction":direction,"message":data})+"\n")
        self.transcript.flush()

    def send(self,method,params=None,notification=False):
        self.sequence+=1
        message={"jsonrpc":"2.0","method":method}
        if not notification:message["id"]=self.sequence
        if params is not None:message["params"]=params
        self.record("request",message)
        self.process.stdin.write(json.dumps(message)+"\n");self.process.stdin.flush()
        if notification:return
        deadline=time.monotonic()+40
        while True:
            reply=self.messages.get(timeout=max(.001,deadline-time.monotonic()))
            assert reply is not None,"MCP server exited early"
            self.record("response",reply)
            if reply.get("id")==self.sequence:
                assert "error" not in reply,reply
                return reply["result"]

    def send_concurrent(self,method,params_list):
        """Write every request before reading any reply; return results in request order."""
        ids=[]
        for params in params_list:
            self.sequence+=1;ids.append(self.sequence)
            message={"jsonrpc":"2.0","method":method,"id":self.sequence,"params":params}
            self.record("request",message)
            self.process.stdin.write(json.dumps(message)+"\n")
        self.process.stdin.flush()
        results={};deadline=time.monotonic()+40
        while len(results)<len(ids):
            reply=self.messages.get(timeout=max(.001,deadline-time.monotonic()))
            assert reply is not None,"MCP server exited early"
            self.record("response",reply)
            if reply.get("id") in ids:
                assert "error" not in reply,reply
                results[reply["id"]]=reply["result"]
        return [results[i] for i in ids]

    def tool(self,name,arguments,expected_error=False):
        result=self.send("tools/call",{"name":name,"arguments":arguments})
        assert bool(result.get("isError"))==expected_error,result
        if result.get("content") and result["content"][0].get("type")=="text":
            text=result["content"][0]["text"]
            try:return json.loads(text)
            except json.JSONDecodeError:return text
        return result

    def close(self):
        self.process.stdin.close()
        try:self.process.wait(timeout=12)
        except subprocess.TimeoutExpired:self.process.kill();self.process.wait();raise
        finally:
            self.transcript.close()
        assert self.process.returncode==0,self.process.stderr.read()

