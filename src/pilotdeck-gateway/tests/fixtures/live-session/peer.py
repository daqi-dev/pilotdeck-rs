#!/usr/bin/env python3
"""One-turn stdio peer that echoes the execute payload received on the wire."""
import json
import sys


def read():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def response(request, **fields):
    return dict(kind="response", messageId="response-" + request["messageId"],
                inReplyTo=request["messageId"], ok=True, **fields)


binding = dict(protocolVersion="2.0", moduleId="fixture", moduleInstanceId="instance",
               connectionGeneration="current", capabilitiesVersion="v1")
hello = read()
assert hello["method"] == "hello"
send(response(hello, **binding))
capabilities = read()
assert capabilities["method"] == "capabilities"
send(response(capabilities, **binding, payload=dict(
    capabilitiesVersion="v1", methods=[dict(name="execute", profiles=["streaming"])])))
execute = read()
assert execute["method"] == "execute"
send(response(execute, requestId=execute["requestId"], streamId="stream"))
identity = {key: execute[key] for key in ("runId", "operationId", "requestId")}
send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=0,
          final=False, **identity, payload=dict(
              type="input_accepted", sessionId=execute["sessionId"],
              turnId=execute["turnId"], receivedPayload=execute["payload"])))
send(dict(kind="event", eventType="agent_event", streamId="stream", sequence=1,
          final=True, outcome="completed", **identity, payload=dict(
              messages=execute["payload"].get("messages", []), result=dict(
                  type="success", sessionId=execute["sessionId"],
                  turnId=execute["turnId"], stopReason="stop", turns=1))))
assert sys.stdin.read() == ""
