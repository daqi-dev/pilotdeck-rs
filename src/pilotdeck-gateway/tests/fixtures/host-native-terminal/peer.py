#!/usr/bin/env python3
# anchor-selfcheck: unsupported protocol peer fixture, no mutation definitions
"""Self-written Module2 failure peer; no provider, filesystem or TS business code."""
import json
import signal
import sys

signal.alarm(20)


def read():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def reply(request, **fields):
    send(dict(kind="response", messageId="reply-" + request["messageId"],
              inReplyTo=request["messageId"], ok=True, **fields))


binding = dict(protocolVersion="2.0", moduleId="fixture", moduleInstanceId="native-failure",
               connectionGeneration="generation", capabilitiesVersion="v1")
hello = read()
assert hello["method"] == "hello"
reply(hello, **binding)
capabilities = read()
assert capabilities["method"] == "capabilities"
reply(capabilities, **binding, payload=dict(capabilitiesVersion="v1", methods=[
    dict(name="execute", profiles=["streaming"]), dict(name="ack")]))
request = read()
assert request["method"] == "execute"
reply(request, requestId=request["requestId"], streamId="stream")
identity = {key: request[key] for key in ("runId", "operationId", "requestId")}
for sequence in range(2):
    send(dict(kind="event", messageId="event-" + str(sequence), eventType="agent_event",
              streamId="stream", sequence=sequence, final=False, **identity,
              payload=dict(type="text_delta", sessionId=request["sessionId"],
                           turnId=request["turnId"], text=str(sequence))))
detail = dict(code="NATIVE_FACTORY_FAILURE", message="failure \ud800", fixtureDetail=7)
send(dict(kind="event", messageId="native-failure", eventType="agent.execute.failed",
          streamId="stream", sequence=2, final=True, outcome="failed", **identity,
          code=detail["code"], error=detail, payload=dict(error=detail)))
for line in sys.stdin:
    assert json.loads(line).get("method") == "ack"
