#!/usr/bin/env python3
# anchor-selfcheck: unsupported end-to-end TCP probe; no product source mutations
"""Real native sidecar TCP listener and native Gateway/control, fake loopback model."""
import argparse,hashlib,http.server,json,os,pathlib,queue,select,signal,socket,socketserver,subprocess,threading,time,traceback,re
p=argparse.ArgumentParser()
for name in ("control","sidecar","out"):p.add_argument("--"+name,type=pathlib.Path,required=True)
p.add_argument("--case",choices=["normal","same-marker","fresh-marker"],required=True)
a=p.parse_args();os.umask(0o077);out=a.out.resolve();out.mkdir(mode=0o700,parents=True,exist_ok=False)
home=out/"home";project=out/"project";home.mkdir();project.mkdir()
legacy=home/"projects/000-fixture";legacy.mkdir(parents=True)
(legacy/".cwd").write_text(str(project))
rows=[];wire=[];wire_lock=threading.Lock();errors=[];ready=queue.Queue();lines=queue.Queue();children=[];success=False
class Provider(http.server.BaseHTTPRequestHandler):
 protocol_version="HTTP/1.1"
 def log_message(self,*_):pass
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers["Content-Length"])));rows.append(body)
  assert len(rows)<=5
  self.send_response(200)
  if not body.get("stream"):
   data=b'{"id":"title","choices":[{"message":{"role":"assistant","content":"title"},"finish_reason":"stop"}]}'
   self.send_header("Content-Length",str(len(data)));self.end_headers();self.wfile.write(data);return
  self.send_header("Content-Type","text/event-stream");self.send_header("Connection","close");self.end_headers()
  for delta,finish in [({"role":"assistant","content":"TCP_READY"},None),({},"stop")]:
   msg={"id":"fixture","model":"mock","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}
   self.wfile.write(("data: "+json.dumps(msg)+"\n\n").encode());self.wfile.flush()
  self.wfile.write(b"data: [DONE]\n\n");self.wfile.flush();self.close_connection=True
provider=http.server.ThreadingHTTPServer(("127.0.0.1",0),Provider);provider.daemon_threads=True;threading.Thread(target=provider.serve_forever,daemon=True).start()
(home/"pilotdeck.yaml").write_text("schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:"+str(provider.server_port)+"/v1\n      apiKey: fixture-only\n      models: {mock: {}}\nmemory: {enabled: false}\n")
token=home/"token";token.write_text("fixture-token\n")
def fresh_port():
 s=socket.socket();s.bind(("127.0.0.1",0));port=s.getsockname()[1];s.close();assert port!=18789;return port
port=fresh_port()
env={"PATH":"/usr/bin:/bin","HOME":str(home),"PILOT_HOME":str(home),"TMPDIR":str(out),"LANG":"en_US.UTF-8"}
def log_stream(pipe,path,ready_prefix=None):
 with path.open("ab") as f:
  for line in pipe:
   f.write(line);f.flush()
   if ready_prefix and line.startswith(ready_prefix):ready.put(line.decode().strip().split()[-1])
def direct():
 s=socket.create_connection(("127.0.0.1",port),timeout=2);s.settimeout(4);return s
def exchange(sock,frame):
 sock.sendall((json.dumps(frame)+"\n").encode());data=b""
 while not data.endswith(b"\n"):
  part=sock.recv(1);assert part;data+=part
 return json.loads(data)
def hello(sock,id):
 return exchange(sock,{"kind":"request","messageId":id,"method":"hello","payload":{}})
class Relay(socketserver.ThreadingTCPServer):allow_reuse_address=True;daemon_threads=False;block_on_close=True
class Forward(socketserver.BaseRequestHandler):
 def handle(self):
  try:
   other=direct();buffers={self.request:b"",other:b""};opposite={self.request:other,other:self.request}
   while True:
    readable,_,_=select.select([self.request,other],[],[],.2)
    for source in readable:
     data=source.recv(65536)
     if not data:return
     direction="to_sidecar" if source is self.request else "to_gateway"
     buffers[source]+=data
     while b"\n" in buffers[source]:
      line,buffers[source]=buffers[source].split(b"\n",1)
      try:
       frame=json.loads(line)
       with wire_lock:wire.append({"direction":direction,"frame":frame})
      except Exception:errors.append("invalid module frame")
     if len(buffers[source])>8*1024*1024:raise ValueError("relay line too large")
     opposite[source].sendall(data)
  except (BrokenPipeError,ConnectionResetError,OSError):pass
  except Exception as e:errors.append(repr(e))
  finally:
   if "other" in locals():other.close()
relay=Relay(("127.0.0.1",0),Forward);threading.Thread(target=relay.serve_forever,daemon=True).start()
side_env=env|{"PILOTDECK_AGENT_LOOP_TCP_HOST":"127.0.0.1","PILOTDECK_AGENT_LOOP_TCP_PORT":str(port)}
(out/"storage").mkdir()
host_env=env|{"PILOTDECK_AGENT_LOOP_TRANSPORT":"tcp","PILOTDECK_AGENT_LOOP_TCP_HOST":"127.0.0.1","PILOTDECK_AGENT_LOOP_TCP_PORT":str(relay.server_address[1])}
host=client=sidecar=None;result={}
def terminate_requested(signum, _frame):
 raise SystemExit("received signal " + str(signum))
signal.signal(signal.SIGTERM, terminate_requested)
try:
 sidecar=subprocess.Popen([str(a.sidecar)],cwd=project,env=side_env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True);children.append(sidecar)
 threading.Thread(target=log_stream,args=(sidecar.stderr,out/"sidecar.stderr"),daemon=True).start()
 deadline=time.monotonic()+20
 while True:
  if sidecar.poll() is not None:raise RuntimeError("sidecar exited before listen")
  try:one=direct();break
  except OSError:
   if time.monotonic()>deadline:raise TimeoutError("sidecar listen")
   time.sleep(.03)
 h1=hello(one,"hello-one");two=direct();h2=hello(two,"hello-two")
 assert h1["moduleInstanceId"]==h2["moduleInstanceId"] and h1["connectionGeneration"]!=h2["connectionGeneration"],(h1,h2)
 one.close();two.close()
 host=subprocess.Popen([str(a.control),"serve","--home",str(home),"--project",str(project),"--listen","127.0.0.1:0","--token-file",str(token),"--sidecar",str(a.sidecar),"--drain-ms","2000","--storage-root",str(out/"storage")],cwd=out,env=host_env,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,start_new_session=True);children.append(host)
 threading.Thread(target=log_stream,args=(host.stderr,out/"gateway.stderr",b"native listening "),daemon=True).start()
 deadline=time.monotonic()+25
 while ready.empty():
  if host.poll() is not None:raise RuntimeError("gateway exited before readiness: "+str(host.returncode))
  if time.monotonic()>deadline:raise TimeoutError("gateway readiness")
  time.sleep(.03)
 url="ws://"+ready.get()+"/ws"
 client=subprocess.Popen([str(a.control),"connect","--url",url,"--token-file",str(token)],cwd=project,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True);children.append(client)
 def collect():
  with (out/"connect.ndjson").open("ab") as f:
   for line in client.stdout:
    f.write(line);f.flush()
    try:lines.put(json.loads(line))
    except Exception as e:errors.append(repr(e))
 threading.Thread(target=collect,daemon=True).start()
 def send(id,method,params):
  client.stdin.write((json.dumps({"jsonrpc":"2.0","id":id,"method":method,"params":params})+"\n").encode());client.stdin.flush()
 def until(id,timeout=35):
  end=time.monotonic()+timeout
  while time.monotonic()<end:
   try:item=lines.get(timeout=.2)
   except queue.Empty:continue
   if item.get("id")==id:return item
  raise TimeoutError("control reply "+str(id))
 send(1,"initialize",{"protocolVersion":"pilotdeck-connect/1"});assert "result" in until(1)
 marker=legacy/".cwd"
 send(2,"gateway.call",{"method":"new_session","params":{"projectKey":str(project),"channelKey":"cli"}});session=until(2)["result"]["sessionKey"]
 project_key=str(project)
 def run_turn(id,run_id):
  params={"sessionKey":session,"runId":run_id,"projectKey":project_key,"channelKey":"cli","workspaceCwd":str(project),"message":"ISOLATED_MARKER_TEST","canPrompt":False}
  send(id,"run.submit",params);answer=until(id,timeout=35)
  return answer
 first=run_turn("before","before-marker")
 assert first.get("result",{}).get("terminal",{}).get("finishReason")=="completed",first
 send(3,"gateway.call",{"method":"new_session","params":{"projectKey":str(project),"channelKey":"cli"}});second_session=until(3)["result"]["sessionKey"]
 # The slug-named catalog entry permits later project-key registration while
 # carrying its own marker so project registration survives the old marker change.
 slug=re.sub(r"[^A-Za-z0-9._-]+", "-", str(project)).strip("-") or "project"
 (home/"projects"/slug).mkdir()
 (home/"projects"/slug/".cwd").write_text(str(project))
 send("list-before","gateway.call",{"method":"snapshot_list","params":{"projectKey":str(project),"sessionKey":session}});listed=until("list-before");items=listed.get("result",{}).get("items",[]);assert any(x.get("workspaceId")=="000-fixture" for x in items),listed
 def marker_state():
  out=[]
  for path in (marker,home/"projects"/slug/".cwd"):
   try:text=path.read_text()
   except OSError as e:text="unreadable:"+str(e.errno)
   out.append({"path":str(path),"mode":oct(path.stat().st_mode & 0o777),"text":text})
  return out
 marker_before=marker_state()
 if a.case!="normal":marker.chmod(0)
 marker_after=marker_state()
 send("registered-projects","gateway.call",{"method":"list_projects","params":{}})
 registered=until("registered-projects")
 projects=registered.get("result",{}).get("projects",[])
 matches=[x["projectKey"] for x in projects if isinstance(x.get("projectKey"),str) and pathlib.Path(x["projectKey"]).resolve()==project.resolve()]
 assert matches,("actual registered projects",registered)
 project_key=matches[0]
 if a.case=="fresh-marker":session=second_session
 second=run_turn("after","after-marker")
 assert second.get("result",{}).get("terminal",{}).get("finishReason")=="completed",second
 send("health","gateway.call",{"method":"describe_server","params":{}});health=until("health",timeout=4);assert "result" in health,health
 # Close the gateway-owned connections and join every relay handler before
 # inspecting wire or errors. The finally block remains safe after this drain.
 for child in reversed(children):
  if child.poll() is None:
   try:os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=3)
   except (ProcessLookupError,subprocess.TimeoutExpired):
    try:os.killpg(child.pid,signal.SIGKILL);child.wait(timeout=3)
    except ProcessLookupError:pass
 relay.shutdown();relay.server_close()
 with wire_lock:
  executed=[x["frame"] for x in wire if x["direction"]=="to_sidecar" and x["frame"].get("method")=="execute"]
  events=[x["frame"] for x in wire if x["direction"]=="to_gateway" and x["frame"].get("kind")=="event"]
 assert not errors,errors
 assert len(executed)==2,executed
 request_ids=[x.get("requestId") for x in executed];assert all(isinstance(x,str) and x and x!="default" for x in request_ids) and len(set(request_ids))==2,request_ids
 assert [x["payload"]["execution"]["runId"] for x in executed]==["before-marker","after-marker"]
 assert [x["payload"]["execution"]["operationId"] for x in executed]==["before-marker","after-marker"]
 session_ids=[x["payload"]["sessionId"] for x in executed]
 assert (session_ids[0]!=session_ids[1])==(a.case=="fresh-marker"),session_ids
 for request_id in request_ids:
  stream=[e for e in events if e.get("requestId")==request_id]
  assert stream and [e.get("sequence") for e in stream]==list(range(len(stream))) and stream[-1].get("final") is True,(request_id,stream)
 workspaces=sorted(x.name for x in (out/"storage/workspaces").iterdir() if x.is_dir());assert "000-fixture" in workspaces,workspaces
 result={"pass":True,"case":a.case,"markerBefore":marker_before,"markerAfter":marker_after,"registeredProjects":registered,"selectedProjectKey":project_key,"first":first,"second":second,"firstSession":first.get("result",{}).get("sessionKey"),"secondSessionKey":session,"validMarkerWorkspace":"000-fixture","allWorkspaceDirectories":workspaces,"otherWorkspaceDirectories":[x for x in workspaces if x!="000-fixture"],"health":health,"control_sha256":hashlib.sha256(a.control.read_bytes()).hexdigest(),"sidecar_sha256":hashlib.sha256(a.sidecar.read_bytes()).hexdigest()};success=True

except BaseException as e:
 result={"pass":False,"error":repr(e),"traceback":traceback.format_exc(),"control_sha256":hashlib.sha256(a.control.read_bytes()).hexdigest(),"sidecar_sha256":hashlib.sha256(a.sidecar.read_bytes()).hexdigest()}
finally:
 for child in reversed(children):
  if child.poll() is None:
   try:os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=3)
   except (ProcessLookupError,subprocess.TimeoutExpired):
    try:os.killpg(child.pid,signal.SIGKILL);child.wait(timeout=3)
    except ProcessLookupError:pass
 relay.shutdown();relay.server_close();provider.shutdown();provider.server_close()
 (out/"wire.json").write_text(json.dumps(wire,indent=2)+"\n");(out/"provider.json").write_text(json.dumps(rows,indent=2)+"\n");(out/"result.json").write_text(json.dumps(result,indent=2)+"\n")
 print(json.dumps({"pass":success,"error":result.get("error")}))
 raise SystemExit(0 if success else 1)
