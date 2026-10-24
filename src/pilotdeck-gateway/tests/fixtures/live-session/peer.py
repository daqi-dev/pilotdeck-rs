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
elif mode == "live-double-failure":
    # TurnRunner.ts:397-402 forwards the loop's turn_failed; a finalization
    # step (:408-419) may then throw and the catch (:437) yields a second
    # turn_failed before the same error terminal (:439). Two turn_failed
    # events are therefore a legal TS stream.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send_event(3, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_persist_error",
                                 message="Failed to complete the turn.")))
    send(agent_event(4, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="model_error",
        turns=1, errors=[dict(code="agent_persist_error",
                              message="Failed to complete the turn.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-max-turns":
    # AgentLoop.ts:2210-2233: max_turns yields a status event and the module
    # final only, without turn_failed.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="agent_status", sessionId=session, turnId=turn,
                      event="model", kind="status", text="max turns reached"))
    send(agent_event(3, dict(result=dict(
        type="max_turns", sessionId=session, turnId=turn, stopReason="max_turns",
        turns=3, errors=[dict(code="agent_max_turns",
                              message="Reached maximum number of turns (3).")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-cancel-pending":
    # Codex P1#2 scenario: turn_started is applied, input_accepted is left to
    # a host callback that never completes, and a valid success final has been
    # received. The host's bounded cancel exit must still surface that final
    # even though the unapplied lifecycle events are lost.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send(success_final(2))
elif mode == "live-cancel-pending-failure":
    # The same pending callback, but with turn_failed and an error final
    # queued behind it: the error terminal requires turn_failed, which the
    # consumer never sees because its application never completes.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(agent_event(3, dict(result=dict(
        type="error", sessionId=session, turnId=turn, stopReason="model_error",
        turns=1, errors=[dict(code="agent_model_error",
                              message="Model request failed.")]),
        messages=messages), final=True, outcome="failed"))
elif mode == "live-missing-accepted":
    # Negative control (a)/(c): a success final without input_accepted. No
    # amount of cancellation makes this a legal terminal; only an
    # input_accepted the sidecar actually sent would.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send(success_final(1))
elif mode == "live-failed-then-success":
    # Negative control (b): after turn_failed only the same error terminal
    # may follow (TurnRunner.ts:437-439); a success final contradicts it.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(success_final(3))
elif mode == "live-failed-then-max-turns":
    # Negative control (b), max_turns variant: the max-turns path never emits
    # turn_failed (AgentLoop.ts:2218-2233), so this ordering is contradictory.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="turn_failed", sessionId=session, turnId=turn,
                      error=dict(code="agent_model_error",
                                 message="Model request failed.")))
    send(agent_event(3, dict(result=dict(
        type="max_turns", sessionId=session, turnId=turn, stopReason="max_turns",
        turns=3, errors=[dict(code="agent_max_turns",
                              message="Reached maximum number of turns (3).")]),
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
elif mode == "live-cancel-full-aborted":
    # TS AgentLoop.ts:594-611: when the abort signal fires, the loop still
    # returns a complete result (type "aborted", stopReason
    # "aborted_streaming", the real turn count and usage) as the module final
    # — the authoritative Terminal path, unlike the incomplete compaction
    # final above, which the host surfaces as ExecuteResult::Aborted.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="input_accepted", sessionId=session, turnId=turn))
    send_event(2, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="hello")))
    while True:
        frame = read()
        if frame.get("method") == "cancel":
            assert frame["reason"] == "user-stop", frame
            break
    send(agent_event(3, dict(result=dict(
        type="aborted", sessionId=session, turnId=turn,
        stopReason="aborted_streaming", turns=1,
        usage=dict(inputTokens=12, outputTokens=34, totalTokens=46),
        permissionDenials=[], startedAt="2026-10-25T00:00:00Z",
        completedAt="2026-10-25T00:00:01Z",
        finalMessage=dict(role="assistant",
                          content=[dict(type="text", text="stopped")])),
        messages=messages), final=True, outcome="cancelled"))
elif mode == "live-duplicate-start":
    # Known-illegal lifecycle: a second turn_started must fail the stream closed.
    # A legal event follows the violating one, so a buffered-frame regression
    # would surface it after the violation error.
    send_event(0, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(1, dict(type="turn_started", sessionId=session, turnId=turn))
    send_event(2, dict(type="model_event", sessionId=session, turnId=turn,
                      event=dict(type="text_delta", text="after violation")))
    sys.stdin.read()
    sys.exit(0)
else:
    sys.exit("unknown live-session peer mode: " + mode)
assert sys.stdin.read() == ""
