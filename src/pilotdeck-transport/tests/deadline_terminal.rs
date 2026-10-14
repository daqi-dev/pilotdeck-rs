//! Production factory/turn/EventSink/server over a socket pair, with host replies
//! as the only fixture. No TCP listener, provider service, or parity ruler edits.
use pilotdeck_agent::ports::{CallControl, Cancellation, PortError};
use pilotdeck_protocol::admission::{CallEffect, SidecarBinding};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::HostExchange;
use pilotdeck_transport::default_factory::OriginalAgentFactory;
use pilotdeck_transport::server::{ExecutionClock, ExecutionServer};
use pilotdeck_transport::stdio::{ConnectionOwner, DispatchPhase, ResponseDisposition};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Clock;
impl ExecutionClock for Clock {
    fn now_epoch_ms(&self) -> f64 {
        1000.0
    }
    fn now_iso(&self) -> JsString {
        "1970-01-01T00:00:01.000Z".into()
    }
    fn parse_date(&self, value: &JsString) -> Result<Option<f64>, JsString> {
        assert!(value.equals_str("future"));
        Ok(Some(2000.0))
    }
}
fn read(io: &UnixStream) -> JsValue {
    let mut line = String::new();
    BufReader::with_capacity(1, io)
        .read_line(&mut line)
        .unwrap();
    parse_json(&line).unwrap()
}
fn send(io: &mut UnixStream, value: &JsValue) {
    writeln!(
        io,
        "{}",
        pilotdeck_protocol::stringify_json(value).unwrap().unwrap()
    )
    .unwrap();
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing {key}: {value:?}"))
}
fn label(value: &JsValue, key: &str) -> String {
    field(value, key).as_string().unwrap().to_utf8().unwrap()
}
fn reply(io: &mut UnixStream, request: &JsValue, payload: &str) {
    let mut response = parse_json(payload).unwrap().as_object().unwrap().clone();
    response.insert("kind".into(), "response".into());
    response.insert("messageId".into(), "ack".into());
    response.insert("inReplyTo".into(), field(request, "messageId").clone());
    send(io, &JsValue::Object(response));
}
fn run(tool: bool, requested: bool) {
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
            capabilities_version: "1".into(),
        },
        32,
    );
    let clock = Arc::new(Clock);
    let server = ExecutionServer::new(
        owner.clone(),
        Arc::new(OriginalAgentFactory {
            clock: clock.clone(),
            process_cwd: "/controlled/project".into(),
        }),
        clock,
        4,
    );
    let (mut io, remote) = UnixStream::pair().unwrap();
    io.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = BufReader::new(remote.try_clone().unwrap());
    let worker =
        std::thread::spawn(move || server.serve(input, remote, Duration::from_millis(500)));
    let mut execute = parse_json(r#"{"kind":"request","messageId":"execute","method":"execute","runId":"run","operationId":"op","requestId":"initial","payload":{"provider":"test","model":"model","task":{"prompt":"test"},"tools":[{"name":"owned_tool"}],"hostModules":{"model":{"methods":["stream_next","close_stream"]},"context":{"methods":["try_auto_compact","capture_turn"]},"capability":{"methods":["execute_batch"]}}}}"#).unwrap();
    if !requested {
        let JsValue::Object(ref mut value) = execute else {
            unreachable!()
        };
        value.insert("operationDeadline".into(), "future".into());
    }
    send(&mut io, &execute);
    let accepted = read(&io);
    assert_eq!(field(&accepted, "ok"), &JsValue::Bool(true));
    let start = Instant::now();
    let mut events = Vec::new();
    let mut calls = Vec::new();
    let mut pending = None;
    let mut capture_count = 0;
    let terminal = loop {
        let message = read(&io);
        if label(&message, "kind") == "event" {
            if field(&message, "final") == &JsValue::Bool(true) {
                break message;
            }
            events.push(message);
            continue;
        }
        let operation = label(&message, "operation");
        calls.push(operation.clone());
        match operation.as_str() {
            "try_auto_compact" => reply(
                &mut io,
                &message,
                r#"{"ok":true,"payload":{"result":{"type":"skipped","snapshot":{"tokens":1,"maxContextTokens":100000,"warningRatio":0.8,"blockingRatio":1,"ratio":0.00001,"state":"ok"}}}}"#,
            ),
            "stream_next" if tool => {
                reply(
                    &mut io,
                    &message,
                    r#"{"ok":true,"payload":{"events":[{"type":"request_started","provider":"test","model":"model"},{"type":"message_start","role":"assistant"},{"type":"tool_call_end","toolCall":{"id":"call","name":"owned_tool","input":{},"raw":{}}},{"type":"message_end","finishReason":"tool_call"}],"done":true}}"#,
                );
            }
            "stream_next" | "execute_batch" => {
                assert!(pending.replace(message.clone()).is_none(), "no replay");
                if requested {
                    send(&mut io, &parse_json(r#"{"kind":"request","messageId":"cancel","method":"cancel","runId":"run","operationId":"op","requestId":"initial","reason":"user cancel"}"#).unwrap());
                    let ack = read(&io);
                    assert_eq!(field(&ack, "inReplyTo"), &JsValue::from("cancel"));
                    // Requested cancellation retains ACK reconciliation, unlike deadline.
                    reply(
                        &mut io,
                        &message,
                        if tool {
                            r#"{"ok":true,"payload":{"results":[]}}"#
                        } else {
                            r#"{"ok":true,"payload":{"events":[],"done":true}}"#
                        },
                    );
                }
            }
            "close_stream" => {
                assert!(!tool);
                // Deadline must not wait for even a containment-cleanup ACK.
                if requested {
                    reply(
                        &mut io,
                        &message,
                        r#"{"ok":true,"payload":{"closed":true}}"#,
                    );
                }
            }
            "capture_turn" => {
                capture_count += 1;
                let input = field(field(&message, "payload"), "input");
                assert_eq!(field(input, "errored"), &JsValue::Bool(false));
                let JsValue::Array(messages) = field(input, "messages") else {
                    panic!("messages")
                };
                assert_eq!(messages.len(), if tool { 2 } else { 1 });
                if requested {
                    reply(
                        &mut io,
                        &message,
                        r#"{"ok":true,"payload":{"result":null}}"#,
                    );
                }
            }
            _ => panic!("unexpected host effect {operation}"),
        }
    };
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "deadline cleanup waited for ACK"
    );
    let mut expected = if tool {
        vec![
            "agent.model_request_started",
            "agent.context_budget",
            "agent.model_event",
            "agent.model_event",
            "agent.model_event",
            "agent.model_event",
            "agent.assistant_message",
            "agent.tool_calls_detected",
            "agent.turn_completed",
        ]
    } else {
        vec![
            "agent.model_request_started",
            "agent.context_budget",
            "agent.turn_completed",
        ]
    };
    if requested && !tool {
        expected.insert(expected.len() - 1, "agent.agent_status");
        let status = field(&events[events.len() - 2], "payload");
        assert_eq!(field(status, "event"), &JsValue::from("turn_aborted"));
    }
    assert_eq!(
        events
            .iter()
            .map(|event| label(event, "eventType"))
            .collect::<Vec<_>>(),
        expected
    );
    for (sequence, event) in events.iter().chain(std::iter::once(&terminal)).enumerate() {
        assert_eq!(field(event, "sequence"), &JsValue::Number(sequence as f64));
        assert_eq!(field(event, "streamId"), field(&accepted, "streamId"));
    }
    let result = field(field(events.last().unwrap(), "payload"), "result");
    assert_eq!(field(result, "type"), &JsValue::from("aborted"));
    assert_eq!(
        field(result, "stopReason"),
        &JsValue::from("aborted_streaming")
    );
    assert_eq!(capture_count, 1);
    assert_eq!(calls.iter().filter(|op| *op == "stream_next").count(), 1);
    assert_eq!(
        calls.iter().filter(|op| *op == "execute_batch").count(),
        usize::from(tool)
    );
    if requested {
        assert_eq!(
            field(&terminal, "eventType"),
            &JsValue::from("agent.execute.cancelled")
        );
        assert_eq!(field(&terminal, "outcome"), &JsValue::from("cancelled"));
        assert_eq!(field(field(&terminal, "payload"), "result"), result);
    } else {
        assert_eq!(
            field(&terminal, "eventType"),
            &JsValue::from("agent.execute.result_unknown")
        );
        assert_eq!(
            field(&terminal, "outcome"),
            &JsValue::from("result_unknown")
        );
        assert_eq!(
            field(&terminal, "code"),
            &JsValue::from("DEADLINE_EXCEEDED")
        );
        assert_eq!(
            field(&terminal, "payload"),
            &JsValue::Object(JsObject::new())
        );
        assert_eq!(
            field(&terminal, "sequence"),
            &JsValue::Number(if tool { 9.0 } else { 3.0 })
        );
        let pending = pending.unwrap();
        let id = field(&pending, "messageId").as_string().unwrap();
        let retained = owner.call_record(id).unwrap();
        assert_eq!(retained.phase, DispatchPhase::Unresolved);
        let mut ack = JsObject::new();
        ack.insert("kind".into(), "response".into());
        ack.insert("messageId".into(), "late".into());
        ack.insert("inReplyTo".into(), JsValue::String(id.clone()));
        ack.insert("ok".into(), JsValue::Bool(true));
        let ack = JsValue::Object(ack);
        assert_eq!(owner.receive_response(&ack), ResponseDisposition::Completed);
        assert_eq!(owner.receive_response(&ack), ResponseDisposition::Duplicate);
        assert_eq!(
            owner.call_record(id).unwrap().phase,
            DispatchPhase::Unresolved
        );
        let mut exchange = owner.bound_exchange(retained.identity.clone()).unwrap();
        let fresh = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        assert!(
            matches!(exchange.exchange(&fresh, "capability", "execute_batch", CallEffect::SideEffect, JsObject::new()), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == retained.identity)
        );
        owner.release_known_calls(&retained.identity);
        assert_eq!(
            owner.call_record(id).unwrap().phase,
            DispatchPhase::Unresolved
        );
    }
    // An operation-status request observes the cached terminal without replay.
    send(
        &mut io,
        &parse_json(
            r#"{"kind":"request","messageId":"status","method":"status","requestId":"initial"}"#,
        )
        .unwrap(),
    );
    let status = read(&io);
    assert_eq!(field(&status, "inReplyTo"), &JsValue::from("status"));
    io.shutdown(std::net::Shutdown::Both).unwrap();
    drop(io);
    assert!(worker.join().unwrap().complete());
}
#[test]
fn deadline_model_has_ts_sequence_three_and_aborted_completed_without_cleanup_acks() {
    run(false, false);
}
#[test]
fn deadline_tool_has_ts_sequence_nine_and_aborted_completed_without_capture_ack() {
    run(true, false);
}
#[test]
fn user_cancel_pending_model_keeps_known_aborted_turn_and_cancelled_terminal() {
    run(false, true);
}
#[test]
fn user_cancel_during_tool_keeps_known_aborted_turn_and_cancelled_terminal() {
    run(true, true);
}
