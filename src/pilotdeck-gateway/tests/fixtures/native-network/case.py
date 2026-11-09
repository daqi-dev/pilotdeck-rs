# Product-side stimulus extends the existing real process/provider harness.
# anchor-selfcheck: unsupported Native business integration probe; no source mutation table.
import base64, socket, concurrent.futures
network_observations = []
def network_call(method, params):
    code, output, error = control("call", {"method":method,"params":params})
    if code == 0:
        value = json.loads(output)
        assert value["ok"] is True, value
        return value["result"]
    raise AssertionError((method,code,output,error))
def raw_http(raw):
    address_tuple=("127.0.0.1",int(address.rsplit(":",1)[1]))
    chunks=[];size=0
    with socket.create_connection(address_tuple,timeout=3) as connection:
        connection.settimeout(3);connection.sendall(raw)
        while True:
            part=connection.recv(65536)
            if not part:break
            size+=len(part);assert size<=1024*1024,"HTTP response budget";chunks.append(part)
    response=b"".join(chunks)
    network_observations.append({"request":raw.decode("latin1"),"responseHex":response.hex(),"eof":True})
    (args.out/"network-http.json").write_text(json.dumps(network_observations,indent=2))
    return response
first=raw_http(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
assert first.startswith(b"HTTP/1.1 200 ") and b'{"ok":true}' in first
for keep in (b"",b"Connection: keep-alive\r\n"):
    response=raw_http(b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n"+keep+b"\r\n")
    head,body=response.split(b"\r\n\r\n",1);lower=head.lower()
    assert head.startswith(b"HTTP/1.0 200 "),head  # existing registered status-line diff
    assert b"connection: close" in lower and b"keep-alive" not in lower
    assert b"content-length:" not in lower and b"transfer-encoding:" not in lower and body==b'{"ok":true}'
response=raw_http(b"HEAD /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
assert response.startswith(b"HTTP/1.1 200 ") and response.split(b"\r\n\r\n",1)[1]==b""
response=raw_http(b"POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: nope\r\nConnection: close\r\n\r\n")
assert response.startswith(b"HTTP/1.1 400 "),response
wrong=home/"wrong-token";wrong.write_text("owned-wrong-token\n");wrong.chmod(0o600)
unauth=spawn([args.control,"--url",url,"--token-file",wrong,"status"],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
auth_out,auth_err=unauth.communicate(timeout=remaining())
assert unauth.returncode!=0 and auth_out==b"" and json.loads(auth_err)=={"kind":"transport_error","message":"Gateway authentication or handshake failed."},(unauth.returncode,auth_out,auth_err)
auth_driver=spawn(["node",_network_here/"auth.mjs",url,args.out],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
auth_driver_out,auth_driver_err=auth_driver.communicate(timeout=remaining())
assert auth_driver.returncode==0,(auth_driver.returncode,auth_driver_out,auth_driver_err)
rows.append({"network_auth":{"exit":unauth.returncode,"error":json.loads(auth_err)}})
image=base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jS1cAAAAASUVORK5CYII=")
image64=base64.b64encode(image).decode()
def create_upload(name,body):
    record=network_call("upload_create",{"projectKey":str(project),"files":[{"clientFileId":"f1","name":name,"relativePath":name,"size":len(body),"mimeType":"image/png" if name.endswith(".png") else "text/plain"}]})
    part=network_call("upload_part",{"uploadId":record["uploadId"],"clientFileId":"f1","contentBase64":base64.b64encode(body).decode()})
    assert part["bytes"]==len(body)
    return record["uploadId"]
upload=create_upload("network.png",image)
completed=network_call("upload_complete",{"uploadId":upload})
assert completed["status"]=="completed" and completed["attachments"][0]["sha256"]==hashlib.sha256(image).hexdigest()
lease_root=project/".tmp/chat-upload-leases"
lease_before={str(p.relative_to(lease_root)) for p in lease_root.glob("*/*") if p.is_dir()} if lease_root.exists() else set()
before_requests=len(rows)
upload_input={"sessionKey":"web:network-upload","runId":"network-upload-run","channelKey":"web","projectKey":str(project),"message":"NETWORK_UPLOAD_MODEL","uploadedAttachments":[{"uploadId":upload}],"canPrompt":False,"maxTurns":2}
(args.out/"upload-input.json").write_text(json.dumps(upload_input))
upload_driver=spawn(["node","--import",_network_baseline/"node_modules/tsx/dist/loader.mjs",_network_here/"upload.mts",url,token,args.out/"upload-input.json",args.out],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
upload_out,upload_err=upload_driver.communicate(timeout=remaining())
assert upload_driver.returncode==0,(upload_driver.returncode,upload_out,upload_err)
assert json.loads((args.out/"upload-result.json").read_text())["pass"] is True
provider_messages=[r["provider"]["messages"] for r in rows[before_requests:] if "provider"in r and r["provider"].get("stream")]
assert provider_messages and any(image64 in json.dumps(messages) and "image_url" in json.dumps(messages) for messages in provider_messages),provider_messages
lease_deadline=time.monotonic()+3
while time.monotonic()<lease_deadline:
    lease_after={str(p.relative_to(lease_root)) for p in lease_root.glob("*/*") if p.is_dir()} if lease_root.exists() else set()
    if lease_after==lease_before:break
    time.sleep(.02)
assert lease_after==lease_before,(lease_before,lease_after)
rows.append({"network_upload_model":{"uploadId":upload,"sha256":hashlib.sha256(image).hexdigest(),"modelReceivedImage":True,"leaseReleased":True,"providerMessages":provider_messages}})
# Public WS + actual CLI connections race the same keyed storage transition.
for round_number in range(3):
    upload=create_upload("race.txt",b"race")
    barrier=threading.Barrier(2)
    def race(method):
        barrier.wait(timeout=3)
        code,output,error=control("call",{"method":method,"params":{"uploadId":upload}})
        return {"method":method,"exit":code,"stdout":output.decode(),"stderr":error.decode()}
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as workers:
        complete=workers.submit(race,"upload_complete");cancel=workers.submit(race,"upload_cancel");outcomes=[complete.result(timeout=remaining()),cancel.result(timeout=remaining())]
    winners=[o for o in outcomes if o["exit"]==0]
    assert len(winners)==1,outcomes
    state=network_call("upload_get",{"uploadId":upload})["status"]
    directory=project/".tmp/chat-uploads"/upload
    assert json.loads((directory/"metadata.json").read_text())["status"]==state
    if winners[0]["method"]=="upload_complete":
        assert state=="completed" and (directory/"files/f1").read_bytes()==b"race"
    else:
        assert state=="cancelled" and not (directory/"files").exists()
    rows.append({"network_upload_race":{"round":round_number,"outcomes":outcomes,"diskStatus":state}})
# No explicit abort_turn is sent: the peer closes its actual WS after the delta.
held.clear();held_closed.clear()
driver=spawn(["node","--import",_network_baseline/"node_modules/tsx/dist/loader.mjs",_network_here/"disconnect.mts",url,token,project,args.out],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
driver_out,driver_err=driver.communicate(timeout=remaining())
assert driver.returncode==0,(driver.returncode,driver_out,driver_err)
assert held_closed.wait(remaining()),"WS disconnect did not close actual held provider stream"
sent=[json.loads(r["raw"]) for r in [json.loads(line)for line in(args.out/"disconnect-wire.jsonl").read_text().splitlines()] if r["direction"]=="send"]
assert not any(r.get("method")=="abort_turn" for r in sent),sent
run_samples=[];run_deadline=time.monotonic()+min(10,remaining())
while True:
    run=network_call("run_get",{"projectKey":str(project),"sessionKey":"web:network-disconnect","runId":"network-disconnect-run"});run_samples.append(run)
    (args.out/"disconnect-run-samples.json").write_text(json.dumps(run_samples,indent=2))
    if run["state"] not in {"queued","running","waiting","pending"}:break
    assert time.monotonic()<run_deadline,run_samples
    time.sleep(.05)
assert run["state"] in {"failed","cancelled","aborted","interrupted"},run
rows.append({"network_ws_disconnect":{"providerClosed":True,"run":run,"client":json.loads((args.out/"disconnect-result.json").read_text())}})
held.clear();held_closed.clear()
(args.out/"network-http.json").write_text(json.dumps(network_observations,indent=2))
