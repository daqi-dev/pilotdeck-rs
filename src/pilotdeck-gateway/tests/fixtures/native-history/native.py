#!/usr/bin/env python3
# anchor-selfcheck: unsupported native history end-to-end probe; no product source mutations
"""Real Rust Gateway/sidecar/control integration. Only owned temporary state and loopback."""
import argparse
import fcntl
import pty
import struct
import termios
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import select
import signal
import shutil
import subprocess
import threading
import time

parser = argparse.ArgumentParser()
for name in ("pilotdeck", "control", "gateway", "sidecar", "out"):
    parser.add_argument("--" + name, required=True, type=Path)
parser.add_argument("--host-mode", choices=("gateway", "control-serve"), default="control-serve")
parser.add_argument("--signal", choices=("TERM", "INT"), default="TERM")
args = parser.parse_args()
for name in ("pilotdeck", "control", "gateway", "sidecar", "out"):
    setattr(args, name, getattr(args, name).resolve())
args.out.mkdir(parents=True, exist_ok=False)
# TERM/HUP must unwind the owned-process cleanup even though the host has its own SID.
def interrupted(signum, _frame):
    for watched in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(watched, signal.SIG_IGN)
    raise InterruptedError("probe interrupted by signal " + str(signum))
for watched in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
    signal.signal(watched, interrupted)
# Execute an exclusive binary snapshot, never a target path that another build can replace.
def sha_file(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

binary_records = {}
(args.out / "binaries").mkdir(mode=0o700)
for name in ("pilotdeck", "control", "gateway", "sidecar"):
    original = getattr(args, name)
    before = sha_file(original)
    frozen = args.out / "binaries" / name
    with original.open("rb") as src, frozen.open("xb") as dst:
        shutil.copyfileobj(src, dst, 1024 * 1024)
    copied = sha_file(frozen)
    assert before == copied == sha_file(original), ("binary changed while snapshotting", name)
    frozen.chmod(0o700)
    binary_records[name] = {"source": str(original), "path": str(frozen), "sha256": copied}
    setattr(args, name, frozen)
(args.out / "binaries.json").write_text(json.dumps(binary_records, indent=2) + "\n")
home, project = args.out / "home", args.out / "project"
home.mkdir(); project.mkdir()
children = []
rows = []
owned_sidecars = {}
owned_descendants = {}
owned_sessions = set()
process_lock = threading.Lock()
process_scan_errors = []
monitor_stop = threading.Event()
host = None
monitor = None
held = threading.Event()
held_closed = threading.Event()
stop = threading.Event()
deadline = time.monotonic() + 150


def remaining():
    return max(0.01, min(20, deadline - time.monotonic()))


def processes():
    result = {}
    # Darwin ps has no POSIX sid field; sess is not getsid(2).
    raw = subprocess.check_output(["/bin/ps", "-axo", "pid=,ppid=,uid=,lstart=,comm="], text=True, timeout=2)
    for line in raw.splitlines():
        fields = line.split(None, 8)
        if len(fields) == 9 and int(fields[2]) == os.getuid():
            result[int(fields[0])] = (int(fields[1]), tuple(fields[3:8]), fields[8])
    return result


def capture_sidecars():
    table = processes()
    with process_lock:
        sessions = set(owned_sessions)
    found = []
    for pid, record in table.items():
        if pid in sessions:
            continue  # Session leaders are direct Popen children.
        try:
            sid = os.getsid(pid)
        except ProcessLookupError:
            continue
        if sid in sessions:
            found.append((pid, record))
    with process_lock:
        for pid, record in found:
            owned_descendants[pid] = record[1:]
            if Path(record[2]).name == args.sidecar.name:
                owned_sidecars[pid] = record[1:]
    return True


def live_owned(mapping):
    table = processes()
    with process_lock:
        identities = list(mapping.items())
    return [pid for pid, identity in identities
            if pid in table and table[pid][1:] == identity]


def observe_descendants():
    while not monitor_stop.is_set():
        try:
            capture_sidecars()
        except Exception as error:
            process_scan_errors.append(str(error))
        monitor_stop.wait(0.01)



def cleanup_owned_processes():
    # Never let one failed scan/wait prevent later owned-process cleanup.
    def attempt(label, action):
        try:
            return action()
        except Exception as error:
            process_scan_errors.append(label + ": " + repr(error))
            return None

    attempt("initial SID scan", capture_sidecars)
    monitor_stop.set()
    if monitor is not None:
        attempt("monitor join", lambda: monitor.join(timeout=3))
        if monitor.is_alive():
            process_scan_errors.append("descendant monitor did not stop")
    for child in reversed(children):
        if child.poll() is None:
            attempt("kill direct child " + str(child.pid), child.kill)
        attempt("wait direct child " + str(child.pid), lambda child=child: child.wait(timeout=5))

    # The leader may already have exited; its owned SID still identifies a
    # reparented sidecar even if that sidecar created its own process group.
    cleanup_deadline = time.monotonic() + 3
    while time.monotonic() < cleanup_deadline:
        scan_ok = attempt("post-leader SID scan", capture_sidecars)
        alive = attempt("owned identity scan", lambda: live_owned(owned_descendants))
        if scan_ok is True and alive == []:
            break
        for pid in alive or []:
            # Recheck immediately before signalling; never kill by executable
            # name or an unverified stale PID.
            def kill_verified(pid=pid):
                table = processes()
                with process_lock:
                    identity = owned_descendants.get(pid)
                if pid in table and identity is not None and table[pid][1:] == identity:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            attempt("kill verified descendant " + str(pid), kill_verified)
        time.sleep(0.01)
    attempt("final SID scan", capture_sidecars)
    descendants = attempt("final descendant verification", lambda: live_owned(owned_descendants))
    sidecars = attempt("final sidecar verification", lambda: live_owned(owned_sidecars))
    return sidecars, descendants


class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        capture_sidecars()
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
env = {"PATH": os.environ["PATH"], "HOME": str(home), "PILOT_HOME": str(home), "LANG": "en_US.UTF-8", "TMPDIR": str(args.out), "TERM": "xterm-256color"}
diagnostics = []
ready = queue.Queue()


def spawn(argv, owned_host=False, **kwargs):
    if owned_host:
        kwargs["start_new_session"] = True
    child = subprocess.Popen([str(p) for p in argv], cwd=project, env=env, **kwargs)
    children.append(child)
    if owned_host:
        # Popen waits for setsid before returning, so SID is this PID even
        # when the leader exits before the first ps snapshot.
        with process_lock:
            owned_sessions.add(child.pid)
    return child


def collect_stderr(child):
    for line in child.stderr:
        diagnostics.append(line.decode(errors="replace"))
        if line.startswith(b"native listening "):
            ready.put(line.decode().strip().split(" ")[-1])


def control(command, value):
    child = spawn([args.control, "--url", url, "--token-file", token, command], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    out, err = child.communicate(json.dumps(value).encode(), timeout=remaining())
    rows.append({"control_command": command, "input": value, "exit_code": child.returncode,
                 "stdout": out.decode(), "stderr": err.decode()})
    return child.returncode, out, err


def rejected_control(code, out, err, expected_code="gateway_request_failed"):
    # pilotdeck-control writes successful envelopes to stdout, structured failures to stderr.
    assert code == 1 and out == b"", (code, out, err)
    value = json.loads(err)
    assert value["kind"] == "gateway_error" and value["message"] == "Gateway request failed.", value
    assert value["error"]["code"] == expected_code, value
    return value["error"]


def history(session="native-stream", **extra):
    params = {"sessionKey": session, "projectKey": str(project), **extra}
    code, out, err = control("call", {"method": "read_session_messages", "params": params})
    assert code == 0 and not err, (code, out, err)
    value = json.loads(out)
    rows.append({"history_request": params, "history_response": value})
    assert value["ok"] is True, value
    result = value["result"]
    assert isinstance(result["messages"], list) and result["total"] >= len(result["messages"])
    assert result["stream"]["sessionKey"] == session
    return result


def switch_tui():
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    before = termios.tcgetattr(slave)
    raw = bytearray()
    replies = 0
    child = spawn([args.pilotdeck, "tui"], stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    def drain():
        nonlocal replies
        if select.select([master], [], [], .03)[0]:
            try: raw.extend(os.read(master, 65536))
            except OSError: pass
        count = raw.count(b"\x1b[6n")
        while replies < count:
            os.write(master, b"\x1b[1;1R"); replies += 1
    def wait(predicate, label):
        until = min(deadline, time.monotonic() + 15)
        while time.monotonic() < until:
            drain()
            if predicate(): return
            assert child.poll() is None, (label, child.returncode)
        raise AssertionError(label)
    try:
        wait(lambda: b"Ask" in raw and b"PilotDeck..." in raw, "TUI prompt")
        os.write(master, b"/switch 1")
        until = time.monotonic() + .4
        while time.monotonic() < until: drain()
        os.write(master, b"\r")
        # Ratatui reuses blank cells and positions each word with CSI H; the
        # fixed 100x30 screen must contain both labelled history lines.
        rendered_history = [b"\x1b[4;1Huser:", b"\x1b[4;7Hhello",
                            b"\x1b[5;1Hassistant:", b"\x1b[5;12Hnative",
                            b"\x1b[5;19Hstream", b"\x1b[5;26Htext"]
        wait(lambda: all(fragment in raw for fragment in rendered_history),
             "TUI restored durable history at expected screen coordinates")
        assert b"History load failed" not in raw
        os.write(master, b"\x03")
        wait(lambda: child.poll() is not None, "TUI clean exit")
        assert child.returncode == 0 and before == termios.tcgetattr(slave)
        rows.append({"tui_history": True, "tui_termios_restored": True})
    finally:
        if child.poll() is None: child.kill()
        child.wait(timeout=5)
        (args.out / "history-terminal.bin").write_bytes(raw)
        os.close(master); os.close(slave)


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
    host_prefix = [args.gateway, "--native"] if args.host_mode == "gateway" else [args.control, "serve"]
    host = spawn([*host_prefix, "--home", home, "--project", project, "--listen", "127.0.0.1:0", "--token-file", token, "--sidecar", args.sidecar, "--drain-ms", "2000"], owned_host=True, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    monitor = threading.Thread(target=observe_descendants, daemon=True)
    monitor.start()
    stderr_reader = threading.Thread(target=collect_stderr, args=(host,), daemon=True)
    stderr_reader.start()
    address = ready.get(timeout=remaining())
    assert address.startswith("127.0.0.1:") and int(address.split(":")[-1]) > 0
    url = "ws://" + address + "/ws"
    code, out, err = control("status", {})
    assert code == 0 and not err
    status = json.loads(out)
    assert status["source"] == "gateway.describe_server"
    rows.append({"status": status})
    code, out, err = control("capabilities", {})
    assert code == 0 and not err
    assert json.loads(out) == status["server"]["capabilities"]
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
    saved = history()
    assert any(m.get("role") == "user" and m.get("text") == "hello" for m in saved["messages"]), saved
    assert any(m.get("role") == "assistant" and m.get("text") == "native stream text" for m in saved["messages"]), saved
    assert saved["session"]["sessionKey"] == "native-stream"
    first = history(limit=1)
    assert first["messages"] == saved["messages"][:1] and first["nextCursor"] == "1"
    rest = history(cursor="1", limit=0)
    assert rest["messages"] == saved["messages"][1:]
    assert history(cursor="0x10")["messages"] == saved["messages"]
    sdk = spawn([args.pilotdeck, "sessions", "messages", "native-stream", "--gateway-url", url,
                 "--auth-token", "stream-native-test-token", "--project", project],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    sdk_out, sdk_err = sdk.communicate(timeout=remaining())
    assert sdk.returncode == 0 and not sdk_err, (sdk.returncode, sdk_out, sdk_err)
    assert json.loads(sdk_out) == saved["messages"]
    rows.append({"sdk_messages": json.loads(sdk_out)})
    code, out, err = control("call", {"method": "read_session_messages", "params": {
        "sessionKey": "native-stream", "projectKey": str(args.out / "another-project")}})
    denied = rejected_control(code, out, err, "PROJECT_SESSION_MISMATCH")
    assert denied["code"] == "PROJECT_SESSION_MISMATCH", denied
    assert denied["message"].startswith("PROJECT_SESSION_MISMATCH:"), denied
    rows.append({"unserved_project": denied})
    (home / "server-token").write_text("stream-native-test-token\n")
    (home / "pilotdeck.yaml").write_text(config + f"gateway:\n  port: {address.split(':')[-1]}\n")
    switch_tui()
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
    active = history("cancel-stream")
    assert active["stream"]["active"] is True and active["stream"]["runId"] == "cancel-run", active
    assert any(e.get("type") == "assistant_text_delta" and e.get("text") == "native stream text"
               for e in active["stream"]["events"]), active
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
    host.send_signal(signal.SIGTERM if args.signal == "TERM" else signal.SIGINT)
    assert host.wait(timeout=remaining()) == 0, diagnostics
    stderr_reader.join(timeout=remaining())
    assert not stderr_reader.is_alive(), "host diagnostics reader did not finish"
    assert host.stdout.read() == b"", "foreground host emitted unexpected stdout"
    assert "native stopping\n" in diagnostics, diagnostics
    assert owned_sidecars, "real Rust sidecar process was not observed"
    assert not live_owned(owned_sidecars), "owned sidecar remained after graceful shutdown"
    rows.append({"observed_sidecar_pids": sorted(owned_sidecars), "sidecars_reaped": True})
    # Restart the same owned home and verify durable reads without a live session writer.
    host = spawn([*host_prefix, "--home", home, "--project", project, "--listen", "127.0.0.1:0",
                  "--token-file", token, "--sidecar", args.sidecar, "--drain-ms", "2000"], owned_host=True,
                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stderr_reader = threading.Thread(target=collect_stderr, args=(host,), daemon=True)
    stderr_reader.start()
    address = ready.get(timeout=remaining()); assert address.startswith("127.0.0.1:")
    assert int(address.split(":")[-1]) not in (0, 18789)
    url = "ws://" + address + "/ws"
    before_files = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in home.rglob("*.jsonl")}
    restored = history()
    assert restored["messages"] == saved["messages"], (restored, saved)
    assert restored["stream"]["active"] is False
    assert before_files == {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in home.rglob("*.jsonl")}
    missing = history("absent-session")
    assert missing["messages"] == [] and missing["total"] == 0
    transcript = next(p for p in home.rglob("native-stream.jsonl"))
    # A background file must be selected by its safe relative path, without
    # adopting normal-session catalog/fallback metadata.
    background = transcript.parent / "background" / "child.jsonl"
    background.parent.mkdir()
    entries = [json.loads(line) for line in transcript.read_text().splitlines() if line.strip()]
    def background_content(value):
        if isinstance(value, str):
            return value.replace("native stream text", "BACKGROUND ANSWER").replace("hello", "BACKGROUND INPUT")
        if isinstance(value, list): return [background_content(v) for v in value]
        if isinstance(value, dict): return {k: background_content(v) for k, v in value.items()}
        return value
    for entry in entries: entry["sessionId"] = "background-session"
    # Same-id normal transcript is also distinct: wrong normal/parent selection must fail.
    transcript.with_name("background-session.jsonl").write_text("".join(json.dumps(entry) + "\n" for entry in entries))
    background.write_text("".join(json.dumps(background_content(entry)) + "\n" for entry in entries))
    back = history("background-session", sessionKind="background_task",
                   relativeTranscriptPath="background/child.jsonl", parentSessionId="native-stream")
    # The real writer also persists status records (for example context_budget).
    # Preserve that complete sequence, while requiring the two text messages
    # to be unique to the selected background file.
    assert [m.get("text") for m in back["messages"]] == [background_content(m).get("text") for m in saved["messages"]], back
    assert [(m.get("role"), m.get("text")) for m in back["messages"] if m.get("kind") == "text"] == [("user", "BACKGROUND INPUT"), ("assistant", "BACKGROUND ANSWER")], back
    assert back["session"]["sessionKind"] == "background_task", back
    assert back["session"]["summary"] == "background-session" and back["session"]["lastModified"] == 0
    assert "firstPrompt" not in back["session"] and "cwd" not in back["session"], back
    assert back["session"]["parentSessionId"] == "native-stream"
    assert back["session"]["relativeTranscriptPath"] == "background/child.jsonl"
    empty = transcript.parent / "background" / "empty.jsonl"; empty.write_text("")
    assert history("empty-background", sessionKind="background_task", relativeTranscriptPath="background/empty.jsonl")["messages"] == []
    # Symlink target belongs to this test, but is outside the transcript root.
    outside = transcript.parent.parent / "outside.jsonl"; outside.write_text(background.read_text())
    link = transcript.parent / "background-link.jsonl"; link.symlink_to(outside)
    for relative in ["../outside.jsonl", "background-link.jsonl"]:
        code, out, err = control("call", {"method": "read_session_messages", "params": {
            "sessionKey": "background-session", "projectKey": str(project),
            "sessionKind": "background_task", "relativeTranscriptPath": relative}})
        response = rejected_control(code, out, err)
        rows.append({"background_rejected": relative, "response": response})
    # One accepted message is below the input-file cap, but its fallback
    # summary/firstPrompt plus the message exceed the complete response cap.
    large = transcript.with_name("large-history.jsonl")
    big_text = "L" * (35 * 1024 * 1024)
    large.write_text(json.dumps({"type": "accepted_input", "sessionId": "large-history",
        "turnId": "large-turn", "entryId": "large-entry", "sequence": 1,
        "createdAt": "2026-01-01T00:00:00.000Z", "messages": [{"role": "user",
        "content": [{"type": "text", "text": big_text}]}]}) + "\n")
    del big_text
    assert large.stat().st_size < 50 * 1024 * 1024
    code, out, err = control("call", {"method": "read_session_messages", "params": {
        "sessionKey": "large-history", "projectKey": str(project)}})
    response = rejected_control(code, out, err)
    assert response["message"] == "History output exceeds Gateway frame limit", (code, out, err)
    rows.append({"full_response_budget": True, "input_bytes": large.stat().st_size, "response": response})
    for session, kind in [("fifo-history", "fifo"), ("oversize-history", "oversize")]:
        path = transcript.with_name(session + ".jsonl")
        assert not path.exists()
        if kind == "fifo": os.mkfifo(path, 0o600)
        else:
            with path.open("xb") as f: f.truncate(50 * 1024 * 1024 + 1)
        started = time.monotonic()
        code, out, err = control("call", {"method": "read_session_messages", "params": {
            "sessionKey": session, "projectKey": str(project)}})
        response = rejected_control(code, out, err)
        assert time.monotonic() - started < 5, (kind, code, out, err)
        rows.append({"bounded_history": kind, "response": response})
    marker = transcript.parent.parent / ".cwd"
    original_marker = marker.read_bytes()
    for kind in ("fifo", "oversize"):
        marker.unlink()
        try:
            if kind == "fifo": os.mkfifo(marker)
            else: marker.write_bytes(b"x" * (64 * 1024 + 1))
            started = time.monotonic()
            code, out, err = control("call", {"method": "read_session_messages", "params": {
                "sessionKey": "native-stream", "projectKey": str(project)}})
            response = rejected_control(code, out, err)
            assert response["message"] == "Unsafe history project marker"
            assert time.monotonic() - started < 5, (kind, code, out, err)
            rows.append({"bounded_marker": kind, "response": response})
        finally:
            marker.unlink(missing_ok=True)
            marker.write_bytes(original_marker)
    assert marker.read_bytes() == original_marker
    assert history()["messages"] == saved["messages"]
    code, out, err = control("status", {})
    assert code == 0 and not err
    host.send_signal(signal.SIGTERM)
    assert host.wait(timeout=remaining()) == 0
    stderr_reader.join(timeout=remaining()); assert not stderr_reader.is_alive()
    rows.append({"restart_history_equal": True, "history_read_only": True, "host_responsive_after_rejections": True})
    passed = True
finally:
    for watched in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(watched, signal.SIG_IGN)
    stop.set()
    remaining_sidecars, remaining_descendants = cleanup_owned_processes()
    try:
        binary_unchanged = all(sha_file(getattr(args, name)) == record["sha256"] for name, record in binary_records.items())
    except Exception as error:
        binary_unchanged = False
        process_scan_errors.append("binary verification: " + repr(error))
    for label, action in [("server shutdown", server.shutdown), ("server close", server.server_close)]:
        try:
            action()
        except Exception as error:
            process_scan_errors.append(label + ": " + repr(error))
    for name, content in [("observations.json", json.dumps(rows, ensure_ascii=False, indent=2)),
                          ("gateway.stderr", "".join(diagnostics))]:
        try:
            (args.out / name).write_text(content)
        except Exception as error:
            process_scan_errors.append("write " + name + ": " + repr(error))
    cleanup_verified = (remaining_descendants == [] and remaining_sidecars == []
                        and all(child.poll() is not None for child in children)
                        and not process_scan_errors and binary_unchanged)
    (args.out / "result.json").write_text(json.dumps({"pass": passed and cleanup_verified, "binaries": binary_records, "binary_snapshots_unchanged": binary_unchanged, "sidecars_reaped": remaining_sidecars == [], "owned_descendants_reaped": remaining_descendants == [], "cleanup_verified": cleanup_verified, "process_scan_errors": process_scan_errors, "host_mode": args.host_mode, "shutdown_signal": args.signal, "children_reaped": all(child.poll() is not None for child in children)}, indent=2))
assert cleanup_verified, "owned descendant cleanup could not be verified"
print(json.dumps({"pass": passed, "evidence": str(args.out)}))
