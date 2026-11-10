#!/usr/bin/env python3
import pathlib,sys,json,subprocess,os,threading,signal,hashlib,http.server,argparse
extra_parser=argparse.ArgumentParser(add_help=False)
for field in ("candidate-manifest","source-manifest"):extra_parser.add_argument("--"+field,type=pathlib.Path,required=True)
extra_parser.add_argument("--expected-source-sha",required=True)
binding,remaining=extra_parser.parse_known_args();sys.argv=[sys.argv[0],*remaining]
TASK_HERE=pathlib.Path(__file__).resolve().parent
shared=TASK_HERE.parent/"native-business-loop/probe.py"
raw=shared.read_bytes();assert hashlib.sha256(raw).hexdigest()=="ee43dbb95e8be6dce2b21663e4564ced000e75aca18eeeb64b68ab56a32c8bde"
__file__=str(shared);exec(compile(raw.decode().split("lease=None;lease_acquired=False;alignment=None")[0],str(shared),"exec"),globals())
versions=json.loads(subprocess.check_output(["node","-p","JSON.stringify(process.versions)"],text=True));assert versions["node"]=="22.22.0" and versions["icu"]=="78.2"
candidate=json.loads(binding.candidate_manifest.read_text());expected={row["path"]:row["sha256"]for row in candidate}
assert set(binary_before.values())=={expected["pilotdeck-gateway"],expected["pilotdeck-agent-loop-sidecar"]}
source=binding.source_manifest
assert hashlib.sha256(source.read_bytes()).hexdigest()==binding.expected_source_sha
success=False;cleanup=False;seed_data=None
class MemoryProvider(http.server.BaseHTTPRequestHandler):
 protocol_version="HTTP/1.1"
 def log_message(self,*args):pass
 def do_POST(self):
  try:
   self.connection.settimeout(5);n=int(self.headers.get("Content-Length","0"));assert 0<n<=MAX_BODY
   body=json.loads(self.rfile.read(n));assert len(wire)<32
   text=json.dumps(body.get("messages",[]));kind="agent"
   if body.get("stream"):
    response={"role":"assistant","content":"MEMORY_RETRIEVAL_ACK"}
   else:
    if "single field route" in text:payload={"route":"project"};kind="memory-gate"
    elif "selected_ids" in text:payload={"selected_ids":[seed_data["record"]["relativePath"]]};kind="memory-selection"
    elif "should_store" in text:payload={"should_store":False,"labels":[]};kind="memory-indexing"
    else:payload={"title":"Memory fixture"};kind="auxiliary"
    response={"role":"assistant","content":json.dumps(payload)}
   wire.append({"phase":phase,"kind":kind,"request":body,"response":response})
   self.send_response(200)
   if body.get("stream"):
    self.send_header("Content-Type","text/event-stream");self.send_header("Connection","close");self.end_headers()
    for row in [{"choices":[{"index":0,"delta":response,"finish_reason":None}]},{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105}}]:self.wfile.write(("data: "+json.dumps(row)+"\n\n").encode())
    self.wfile.write(b"data: [DONE]\n\n");self.close_connection=True
   else:
    data=json.dumps({"choices":[{"index":0,"message":response,"finish_reason":"stop"}]}).encode();self.send_header("Content-Type","application/json");self.send_header("Content-Length",str(len(data)));self.end_headers();self.wfile.write(data)
   self.wfile.flush()
  except Exception as e:errors.append(str(e));self.close_connection=True
with common.parity_lock(timeout=1):
 try:
  provider=http.server.ThreadingHTTPServer(("127.0.0.1",0),MemoryProvider);provider.daemon_threads=True
  threading.Thread(target=provider.serve_forever,daemon=True).start()
  monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
  config(provider.server_port);memory_root=a.out/"memory-root";memory_root.mkdir()
  with (home/"pilotdeck.yaml").open("a") as f:f.write("\nmemory:\n  enabled: true\n  provider: edgeclaw\n  model: fixture/mock\n  rootDir: "+json.dumps(str(memory_root))+"\n")
  seed_env=env_for(1)
  seeded=subprocess.run(["node","--import",str(a.baseline/"node_modules/tsx/dist/loader.mjs"),str(TASK_HERE/"seed.mts"),str(a.baseline),str(project),str(memory_root),str(a.out/"seed.json")],env=seed_env,cwd=project,capture_output=True,timeout=20)
  (a.out/"seed.stdout").write_bytes(seeded.stdout);(a.out/"seed.stderr").write_bytes(seeded.stderr);assert seeded.returncode==0,seeded.returncode
  seed_data=json.loads((a.out/"seed.json").read_text())
  for phase in ["retrieve","restart-wipe","wiped-restart"]:
   out=a.out/phase;out.mkdir()
   with socket.socket() as sock:sock.bind(("127.0.0.1",0));port=sock.getsockname()[1]
   env=env_for(port)
   host=spawn([a.gateway,"--native","--home",home,"--project",project,"--listen",f"127.0.0.1:{port}","--token-file",home/"token","--sidecar",a.sidecar,"--drain-ms","2000"],env,out)
   wait_ready(host,port)
   run=subprocess.run(["node","--import",str(a.baseline/"node_modules/tsx/dist/loader.mjs"),str(TASK_HERE/"driver.mts"),f"ws://127.0.0.1:{port}/ws",str(home/"token"),str(project),str(out),str(a.out/"seed.json"),phase],env=env,cwd=project,capture_output=True,timeout=50)
   (out/"driver.stdout").write_bytes(run.stdout);(out/"driver.stderr").write_bytes(run.stderr);assert run.returncode==0,(phase,run.returncode)
   halt(host);host=None
  streams=[row for row in wire if row["request"].get("stream")]
  assert len(streams)==2,len(streams)
  for row in streams:
   system=json.dumps([m for m in row["request"]["messages"]if m.get("role")=="system"])
   assert (seed_data["marker"]in system)==(row["phase"]=="retrieve"),(row["phase"],"memory marker injection mismatch")
  assert any(row["kind"]=="memory-gate"for row in wire)
  assert any(row["kind"]=="memory-selection"for row in wire)
  assert not errors,errors
  success=True
 finally:
  if host is not None:halt(host);host=None
  cleanup=True;stop.set()
  if monitor_thread:monitor_thread.join(timeout=2)
  if provider:provider.shutdown();provider.server_close()
  save(a.out/"provider.json",wire)
  save(a.out/"result.json",{"pass":success,"cleanupVerified":cleanup,"errors":errors,"node":versions,"sourceManifestSha256":binding.expected_source_sha,"binarySha256":binary_before,"notFormalAcceptance":True})
