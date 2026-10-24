#!/usr/bin/env python3
"""One-turn stdio peer for the gateway LiveSession adapter.

Mirrors agentLoopSidecarServer.ts:270-340: every TurnRunner AgentEvent is
published as a non-final agent_event frame (including the sidecar's own
turn_completed, which the host suppresses at agentLoopSidecarClient.ts:680-684),
and the module final carries {result, messages}. The mode selects the turn
lifecycle under test.
"""
import json
import sys


def read():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def response(request, **fields):
    return dict(kind="response", messageId="response-" + request["messageId"],
                inReplyTo=request["messageId"], ok=True, **fields)


mode = sys.argv[1] if len(sys.argv) > 1 else "live-payload"
binding = dict(protocolVersion="2.0", moduleId="fixture", moduleInstanceId="instance",
               connectionGeneration="current", capabilitiesVersion="v1")
hello = read()
assert hello["method"] == "hello"
send(response(hello, **binding))
capabilities = read()
assert capabilities["method"] == "capabilities"
methods = [dict(name="execute", profiles=["streaming"])]
if mode == "live-cancel-compaction":
    methods.append(dict(name="cancel"))
send(response(capabilities, **binding, payload=dict(
    capabilitiesVersion="v1", methods=methods)))
execute = read()
assert execute["method"] == "execute"
send(response(execute, requestId=execute["requestId"], streamId="stream"))
identity = {key: execute[key] for key in ("runId", "operationId", "requestId")}
session, turn = execute["sessionId"], execute["turnId"]
messages = execute["payload"].get("messages", [])


def agent_event(sequence, payload, final=False, outcome=None):
    value = dict(kind="event", eventType="agent_event", streamId="stream",
                sequence=sequence, final=final, **identity, payload=payload)
    if final:
        value["outcome"] = outcome
    return value


def send_event(sequence, payload):
    send(agent_event(sequence, payload))


def success_final(sequence):
    return agent_event(sequence, dict(result=dict(
        type="success", sessionId=session, turnId=turn,
        stopReason="completed", turns=1), messages=messages),
        final=True, outcome="completed")


if mode == "live-payload":
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn,
                      receivedPayload=execute["payload"]))
    send(success_final(2))
elif mode == "live-normal":
    # Full legal lifecycle, including the sidecar's own turn_completed
    # (suppressed by the host; the module final is the authoritative terminal).
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    send_event(3, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="model", kind="status", text="thinking"))
    send_event(4, dict(type="tool_progress", sessionId=session, turnId=turn,
                      toolCallId="call-1", toolName="read", message="reading",
                      createdAt="2026-10-25T00:00:00Z"))
    send_event(5, dict(type="turn_completed", sessionId=session, turnId=turn,
                      result=dict(type="success")))
    send(success_final(6))
elif mode == "live-early-failure":
    # TurnRunner.ts:208-210: a transcript write failure ends the turn before
    # input_accepted: status, turn_failed, then the module final.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="transcript", kind="error", text="record failed"))
    send_event(2, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_transcript_error",
                                 message="Failed to record accepted input.")))
    send(agent_event(3, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="error",
        turns=0, errors=[dict(code="agent_transcript_error",
                              message="Failed to record accepted input.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-cancel-compaction":
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    # TS client:931-958: cancel is best-effort; this peer answers with the
    # incomplete compaction final (client:1338-1350 allows the missing result
    # only for the compaction_persistence_failed reason).
    while True:
        frame = read()
        if frame.get("method") == "cancel":
            assert frame["reason"] == "compaction_persistence_failed", frame
            break
    send(agent_event(3, dict(compaction="incomplete"), final=True,
                     outcome="cancelled"))
elif mode == "live-duplicate-start":
    # Known-illegal lifecycle: a second turn_started must fail the stream closed.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="turn_started", sessionId=session, turnId=turn))
    sys.stdin.read()
    sys.exit(0)
else:
    sys.exit("unknown live-session peer mode: " + mode)
assert sys.stdin.read() == ""
