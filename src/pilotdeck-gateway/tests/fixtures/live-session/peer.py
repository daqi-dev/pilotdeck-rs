#!/usr/bin/env python3
# anchor-selfcheck: unsupported protocol peer fixture, no mutation definitions
"""One-turn stdio peer for the gateway LiveSession adapter.

Mirrors agentLoopSidecarServer.ts:270-340: every AgentLoop AgentEvent is
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
if mode in ("live-cancel-compaction", "live-cancel-full-aborted"):
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


if mode in ("live-payload", "live-manifest"):
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn,
                      receivedPayload=execute["payload"]))
    if mode == "live-manifest":
        send_event(1, dict(type="agent_status", sessionId=session, turnId=turn,
                          event="manifest", kind="status", text="payload observed",
                          receivedPayload=execute["payload"]))
    send(success_final(2 if mode == "live-manifest" else 1))
elif mode == "live-tail":
    from pathlib import Path
    Path(sys.argv[2], "report.txt").write_text("hello matrix\n")
    send_event(0, dict(type="tool_result", sessionId=session, turnId=turn,
        result=dict(type="success", toolCallId="write-1", toolName="write_file", content=[], data=dict(filePath="report.txt"))))
    send(agent_event(1, dict(result=dict(type="success", sessionId=session, turnId=turn,
        stopReason="completed", turns=1, finalMessage=dict(role="assistant", content=[dict(type="text",text="done")])),
        messages=messages), final=True, outcome="completed"))
elif mode == "live-interaction":

    def call(module, payload):
        send(dict(kind="request", messageId="call-" + module, method="module_call",
                  module=module, runId=execute["runId"], operationId=execute["operationId"],
                  requestId="callback", payload=payload))
        reply = read()
        assert reply["ok"], reply
        return reply["payload"]

    call("event", dict(operation="emit", event=dict(type="agent_status", sessionId=session,
         turnId=turn, event="host-probe", kind="status", text="host callback")))
    decision = call("permission", dict(operation="decide", tool=dict(name="bash"),
                    input=dict(command="rm -rf /"), toolCallId="call", context=dict(mode="bypassPermissions")))
    assert decision["decision"]["type"] == "deny", decision
    call("lifecycle", dict(operation="dispatch", event="PreToolUse", input={}))
    drained = call("turn", dict(operation="drain_or_close_steer"))
    send_event(0, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="ports-tested", kind="status", text="all four", observed=drained))
    send(success_final(1))
elif mode == "live-fresh-router":
    call_index = 0

    def probe_call(module, payload, allow_error=False):
        global call_index
        call_index += 1
        message_id = "fresh-call-" + str(call_index)
        send(dict(kind="request", messageId=message_id, method="module_call",
                  module=module, **identity, payload=payload))
        while True:
            reply = read()
            if reply.get("inReplyTo") == message_id:
                break
            assert reply.get("method") == "ack", reply
        if allow_error:
            return reply
        assert reply["ok"], reply
        return reply["payload"]

    request = dict(provider="provider", model="model", messages=[])
    old = probe_call("model", dict(operation="materialize_prepared_request",
                     preparationId="old-only", request=request), allow_error=True)
    prepared = probe_call("model", dict(operation="prepare", preparationId="current",
                          request=request))["prepared"]["request"]
    streamed = probe_call("model", dict(operation="stream", preparationId="current"))["events"][0]
    budget = probe_call("budget", dict(operation="evaluate_request_budget", request=request,
                        options=dict(maxContextTokens=100)))["snapshot"]
    context = probe_call("context", dict(operation="prepare_for_model", input={}))["result"]
    capability = probe_call("capability", dict(operation="execute", name="probe", toolCallId="tool"))
    plan = probe_call("capability", dict(operation="plan_todo"))
    send_event(0, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="fresh-router", kind="status", text="observed actual callbacks",
                      observed=dict(oldPreparationRejected=not old["ok"],
                                    oldPreparationError=old.get("error", {}).get("message"), model=prepared,
                                    stream=streamed, budget=budget, context=context,
                                    capability=capability, plan=plan)))
    send(success_final(1))
elif mode == "live-normal":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    send_event(1, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="model", kind="status", text="thinking"))
    send_event(2, dict(type="tool_progress", sessionId=session, turnId=turn,
                      toolCallId="call-1", toolName="read", message="reading",
                      createdAt="2026-10-25T00:00:00Z"))
    send_event(3, dict(type="turn_completed", sessionId=session, turnId=turn,
                      result=dict(type="success")))
    send(success_final(4))
elif mode == "live-early-failure":
    send_event(0, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="transcript", kind="error", text="record failed"))
    send_event(1, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_transcript_error",
                                 message="Failed to record accepted input.")))
    send(agent_event(2, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="error",
        turns=0, errors=[dict(code="agent_transcript_error",
                              message="Failed to record accepted input.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-double-failure":
    send_event(0, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send_event(1, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_persist_error",
                                 message="Failed to complete the turn.")))
    send(agent_event(2, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="model_error",
        turns=1, errors=[dict(code="agent_persist_error",
                              message="Failed to complete the turn.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-max-turns":
    send_event(0, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="model", kind="status", text="max turns reached"))
    send(agent_event(1, dict(result=dict(
        type="max_turns", sessionId=session, turnId=turn, stopReason="max_turns",
        turns=3, errors=[dict(code="agent_max_turns",
                              message="Reached maximum number of turns (3).")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-cancel-pending":
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn))
    send(success_final(1))
elif mode == "live-cancel-pending-failure":
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(agent_event(2, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="model_error",
        turns=1, errors=[dict(code="agent_model_error",
                              message="Model request failed.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-cancel-blocked-model-event":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="blocked")))
    send(success_final(1))
elif mode == "live-cancel-queued-turn-failed":
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(success_final(2))
elif mode == "live-cancel-slow-consumer":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="queued")))
    send_event(1, dict(type="model_request_started", sessionId=session, turnId=turn))
    send(success_final(2))
elif mode == "live-cancel-slow-consumer-failure":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="queued")))
    send_event(1, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(agent_event(2, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="model_error",
        turns=1, errors=[dict(code="agent_model_error",
                              message="Model request failed.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-missing-loop-prefix":
    send(success_final(0))
elif mode == "live-failed-then-success":
    send_event(0, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(success_final(1))
elif mode == "live-failed-then-max-turns":
    send_event(0, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(agent_event(1, dict(result=dict(
        type="max_turns", sessionId=session, turnId=turn, stopReason="max_turns",
        turns=3, errors=[dict(code="agent_max_turns",
                              message="Reached maximum number of turns (3).")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-cancel-compaction":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    while True:
        frame = read()
        if frame.get("method") == "cancel":
            assert frame["reason"] == "compaction_persistence_failed", frame
            break
    send(agent_event(1, dict(compaction="incomplete"), final=True,
                     outcome="cancelled"))
elif mode == "live-cancel-full-aborted":
    send_event(0, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    while True:
        frame = read()
        if frame.get("method") == "cancel":
            assert frame["reason"] == "user-stop", frame
            break
    send(agent_event(1, dict(result=dict(
        type="aborted", sessionId=session, turnId=turn,
        stopReason="aborted_streaming", turns=1,
        usage=dict(inputTokens=12, outputTokens=34, totalTokens=46),
        permissionDenials=[], startedAt="2026-10-25T00:00:00Z",
        completedAt="2026-10-25T00:00:01Z",
        finalMessage=dict(role="assistant",
                          content=[dict(type="text", text="stopped")])),
        messages=messages), final=True, outcome="cancelled"))
elif mode == "live-duplicate-sequence":
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn))
    send_event(0, dict(type="model_request_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="model_event", event=dict(type="text_delta", text="after violation")))
    sys.stdin.read()
    sys.exit(0)
else:
    sys.exit("unknown live-session peer mode: " + mode)
assert sys.stdin.read() == ""
