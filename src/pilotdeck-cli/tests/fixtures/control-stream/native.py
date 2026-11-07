#!/usr/bin/env python3
"""Real Rust Gateway/sidecar/control integration. Only owned temporary state and loopback."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import select
import signal
import subprocess
import threading
import time

parser = argparse.ArgumentParser()
for name in ("control", "gateway", "sidecar", "out"):
    parser.add_argument("--" + name, required=True, type=Path)
args = parser.parse_args()
for name in ("control", "gateway", "sidecar", "out"):
    setattr(args, name, getattr(args, name).resolve())
args.out.mkdir(parents=True, exist_ok=False)
home, project = args.out / "home", args.out / "project"
home.mkdir(); project.mkdir()
children = []
rows = []
held = threading.Event()
held_closed = threading.Event()
stop = threading.Event()
deadline = time.monotonic() + 90


def remaining():
    return max(0.01, min(20, deadline - time.monotonic()))


class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        rows.append({"provider": body})
        self.send_response(200)
        if not body.get("stream"):
            raw = json.dumps({"id": "title", "choices": [{"message": {"role": "assistant", "content": '{"title":"CLI stream test"}'}, "finish_reason": "stop"}]}).encode()
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers(); self.wfile.write(raw); return
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()

        def emit(delta, reason=None):
            frame = {"id": "stream", "model": "mock", "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}
            self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode()); self.wfile.flush()

        emit({"role": "assistant", "content": "native stream text"})
        if "NATIVE_HANG" in json.dumps(body.get("messages")):
            held.set()
            while not stop.is_set() and time.monotonic() < deadline:
                if select.select([self.connection], [], [], 0.1)[0]:
                    if not self.connection.recv(1):
                        held_closed.set(); return
            return
        emit({}, "stop")
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
        self.close_connection = True


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
config = f"""schemaVersion: 1
agent: {{model: fixture/mock}}
model:
  providers:
    fixture:
      protocol: openai
      url: http://127.0.0.1:{server.server_port}/v1
      apiKey: mock-only
      models:
        mock: {{}}
"""
(home / "pilotdeck.yaml").write_text(config)
token = home / "token"
token.write_text("stream-native-test-token\n")
env = {"PATH": os.environ["PATH"], "HOME": str(home), "PILOT_HOME": str(home), "LANG": "en_US.UTF-8"}
diagnostics = []
ready = queue.Queue()


def spawn(argv, **kwargs):
    child = subprocess.Popen([str(p) for p in argv], cwd=project, env=env, **kwargs)
    children.append(child)
    return child


def collect_stderr(child):
    for line in child.stderr:
        diagnostics.append(line.decode(errors="replace"))
        if line.startswith(b"native listening "):
            ready.put(line.decode().strip().split(" ")[-1])


def control(command, value):
    child = spawn([args.control, "--url", url, "--token-file", token, command], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    out, err = child.communicate(json.dumps(value).encode(), timeout=remaining())
    return child.returncode, out, err


def verify_stream(out, success):
    frames = [json.loads(line) for line in out.splitlines()]
    assert frames and frames[-1]["final"] is True
    assert all(frame["seq"] == i for i, frame in enumerate(frames))
    assert all(frame["final"] is False for frame in frames[:-1])
    assert frames[-1]["event"]["type"] == "turn_completed"
    assert frames[-1]["event"]["finishReason"] == ("completed" if success else "aborted_streaming")
    assert any(f["event"].get("type") == "assistant_text_delta" and f["event"].get("text") == "native stream text" for f in frames)
    rows.append({"stream": frames})
    return frames


passed = False
try:
    host = spawn([args.gateway, "--native", "--home", home, "--project", project, "--listen", "127.0.0.1:0", "--token-file", token, "--sidecar", args.sidecar, "--drain-ms", "2000"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    threading.Thread(target=collect_stderr, args=(host,), daemon=True).start()
    address = ready.get(timeout=remaining())
    assert address.startswith("127.0.0.1:") and int(address.split(":")[-1]) > 0
    url = "ws://" + address + "/ws"
    params = {"sessionKey": "native-stream", "runId": "native-run", "channelKey": "cli", "projectKey": str(project), "message": "hello", "canPrompt": False}
    code, out, err = control("stream", params)
    assert code == 0 and not err, (code, err.decode())
    verify_stream(out, True)
    code, out, err = control("call", {"method": "run_get", "params": {"projectKey": str(project), "sessionKey": params["sessionKey"], "runId": params["runId"]}})
    assert code == 0 and not err
    record = json.loads(out)
    assert record["ok"] is True and record["result"]["state"] == "completed", record
    rows.append({"persisted_run": record})
    transcripts = list(home.rglob("*.jsonl"))
    assert any("native stream text" in path.read_text() for path in transcripts), "real transcript missing text"
    params.update(sessionKey="cancel-stream", runId="cancel-run", message="NATIVE_HANG")
    stream = spawn([args.control, "--url", url, "--token-file", token, "stream"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stream.stdin.write(json.dumps(params).encode()); stream.stdin.close(); stream.stdin = None
    # Cancellation must follow actual CLI delivery, not merely the provider's
    # write: gateway projection and pipe delivery are asynchronous.
    streamed = []
    delta_delivered = threading.Event()
    def read_stream():
        for line in stream.stdout:
            streamed.append(line)
            event = json.loads(line).get("event", {})
            if event.get("type") == "assistant_text_delta" and event.get("text") == "native stream text":
                delta_delivered.set()
    reader = threading.Thread(target=read_stream, daemon=True)
    reader.start()
    assert held.wait(remaining()), "provider never entered controlled pending state"
    assert delta_delivered.wait(remaining()), "CLI did not deliver the pending turn delta"
    code, out, err = control("call", {"method": "abort_turn", "params": {"projectKey": str(project), "sessionKey": params["sessionKey"], "runId": params["runId"]}})
    assert code == 0 and not err, (code, err.decode())
    cancel = json.loads(out); assert cancel["ok"] is True
    stream.wait(timeout=remaining())
    reader.join(timeout=remaining())
    assert not reader.is_alive(), "stream reader failed to finish"
    out, err = b"".join(streamed), stream.stderr.read()
    assert stream.returncode != 0 and json.loads(err)["kind"] == "turn_incomplete", (stream.returncode, err)
    verify_stream(out, False)
    assert held_closed.wait(remaining()), "cancel did not close provider stream"
    rows.append({"cancel": cancel, "provider_closed": True})
    host.send_signal(signal.SIGTERM)
    assert host.wait(timeout=remaining()) == 0, diagnostics
    passed = True
finally:
    stop.set()
    for child in reversed(children):
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)
    server.shutdown(); server.server_close()
    (args.out / "observations.json").write_text(json.dumps(rows, ensure_ascii=False, indent=2))
    (args.out / "gateway.stderr").write_text("".join(diagnostics))
    (args.out / "result.json").write_text(json.dumps({"pass": passed, "binaries": {name: {"path": str(getattr(args,name)), "sha256": hashlib.sha256(getattr(args,name).read_bytes()).hexdigest()} for name in ("control","gateway","sidecar")}, "children_reaped": all(child.poll() is not None for child in children)}, indent=2))
print(json.dumps({"pass": passed, "evidence": str(args.out)}))
