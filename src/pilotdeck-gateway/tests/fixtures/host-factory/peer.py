#!/usr/bin/env python3
# anchor-selfcheck: unsupported protocol peer fixture, no mutation definitions
"""Deterministic stdio peer for the turn host factory tests.

Modes:
  success          full handshake, then one successful turn
  hold             full handshake, model_request_started, then wait forever
  handshake-reject hello answered with an error, then exit
  handshake-hang   record the pid and never answer hello
  exit             exit before the handshake
The pid file is published atomically before any protocol work, so a test that
sees it either sees the complete pid or no file at all.
"""
import json
import os
import sys
import time

mode = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("PDRS_PEER_MODE", "success")
pid_file = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("PDRS_PEER_PID")
if pid_file:
    temporary = f"{pid_file}.tmp"
    with open(temporary, "w", encoding="utf-8") as out:
        out.write(str(os.getpid()))
        out.flush()
        os.fsync(out.fileno())
    os.replace(temporary, pid_file)
binding = dict(protocolVersion="2.0", moduleId="fixture", moduleInstanceId="instance",
               connectionGeneration="current", capabilitiesVersion="v1")

if mode == "exit":
    sys.exit(0)
if mode == "handshake-hang":
    time.sleep(60)
    sys.exit(0)


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def response(request, **fields):
    return dict(kind="response", messageId="response-" + request["messageId"],
                inReplyTo=request["messageId"], ok=True, **fields)


hello = read()
assert hello["method"] == "hello"
if mode == "handshake-reject":
    send(dict(kind="response", messageId="response-" + hello["messageId"],
              inReplyTo=hello["messageId"], ok=False,
              error=dict(code="HANDSHAKE_REJECTED", message="fixture rejects handshake")))
    sys.exit(0)
send(response(hello, **binding))
capabilities = read()
assert capabilities["method"] == "capabilities"
send(response(capabilities, **binding, payload=dict(
    capabilitiesVersion="v1", methods=[dict(name="execute", profiles=["streaming"])])))
execute = read()
assert execute["method"] == "execute"
if pid_file:
    with open(f"{pid_file}.{os.getpid()}.execute", "x", encoding="utf-8") as out:
        out.write("execute\n")
send(response(execute, requestId=execute["requestId"], streamId="stream"))
identity = {key: execute[key] for key in ("runId", "operationId", "requestId")}
session, turn = execute["sessionId"], execute["turnId"]
messages = execute["payload"].get("messages", [])
if mode == "hold":
    send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=0, final=False,
              **identity, payload=dict(type="model_request_started", sessionId=session, turnId=turn)))
    while True:
        time.sleep(1)
send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=0, final=False,
          **identity, payload=dict(type="model_request_started", sessionId=session, turnId=turn)))
send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=1, final=False,
          **identity, payload=dict(type="turn_completed", sessionId=session, turnId=turn,
                                  result=dict(type="success", sessionId=session, turnId=turn))))
send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=2, final=True,
          outcome="completed", **identity,
          payload=dict(result=dict(type="success", sessionId=session, turnId=turn,
                                  stopReason="completed", turns=1), messages=messages)))
