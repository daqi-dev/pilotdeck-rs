//! Component ownership probes. Test factories do not validate production AgentLoop.
use pilotdeck_agent::ports::PortResult;
use pilotdeck_protocol::admission::{CallEffect, SidecarBinding};
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::server::{
    ExecuteRequest, ExecutionClock, ExecutionContext, ExecutionFactory, ExecutionServer,
    ExecutionTerminal,
};
use pilotdeck_transport::stdio::ConnectionOwner;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
struct Clock;
impl ExecutionClock for Clock {
    fn now_epoch_ms(&self) -> f64 {
        1000.0
    }
    fn now_iso(&self) -> JsString {
        "1970-01-01T00:00:01.000Z".into()
    }
    fn parse_date(&self, input: &JsString) -> Result<Option<f64>, JsString> {
        match input.to_utf8().as_deref() {
            Ok("expired") => Ok(Some(1000.0)),
            Ok("future") => Ok(Some(1100.0)),
            Ok("invalid") => Ok(None),
            _ => Err("unsupported fixture".into()),
        }
    }
}
struct Factory {
    calls: Arc<AtomicUsize>,
    wait: bool,
}
impl ExecutionFactory for Factory {
    fn execute(
        &self,
        _: &ExecuteRequest,
        ctx: &mut ExecutionContext,
    ) -> PortResult<ExecutionTerminal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.wait {
            let identity = ctx.identity("s".into(), "t".into());
            ctx.call(
                identity,
                "model",
                "get_metadata",
                CallEffect::ReadOnly,
                JsObject::new(),
            )?;
        }
        Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Completed,
            payload: JsObject::new(),
            error: None,
        })
    }
}
fn setup(
    wait: bool,
) -> (
    UnixStream,
    thread::JoinHandle<pilotdeck_transport::server::ShutdownReport>,
    Arc<AtomicUsize>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "test-instance".into(),
            connection_generation: "test-generation".into(),
            capabilities_version: "1".into(),
        },
        32,
    );
    let server = ExecutionServer::new(
        owner,
        Arc::new(Factory {
            calls: calls.clone(),
            wait,
        }),
        Arc::new(Clock),
        8,
    );
    let (client, io) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let input = BufReader::new(io.try_clone().unwrap());
    let handle = thread::spawn(move || server.serve(input, io, Duration::from_millis(500)));
    (client, handle, calls)
}
fn send(io: &mut UnixStream, value: &str) {
    writeln!(io, "{value}").unwrap();
}
fn read(io: &UnixStream) -> JsValue {
    // Read exactly one line without buffering past it across successive wrappers.
    let mut reader = BufReader::with_capacity(1, io);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    parse_json(&line).unwrap()
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap()
}
fn execute(deadline: Option<&str>) -> String {
    let suffix = deadline
        .map(|d| format!(",\"operationDeadline\":\"{d}\""))
        .unwrap_or_default();
    format!(
        "{{\"kind\":\"request\",\"messageId\":\"x\",\"method\":\"execute\",\"runId\":\"r\",\"operationId\":\"o\",\"requestId\":\"q\",\"payload\":{{}}{suffix}}}"
    )
}
fn close(io: UnixStream, handle: thread::JoinHandle<pilotdeck_transport::server::ShutdownReport>) {
    io.shutdown(std::net::Shutdown::Both).unwrap();
    drop(io);
    assert!(handle.join().unwrap().complete());
}
#[test]
fn actual_wire_handshake_has_exact_binding_and_separate_capabilities() {
    let (mut io, handle, calls) = setup(false);
    send(
        &mut io,
        r#"{"kind":"request","messageId":"h","method":"hello","payload":{}}"#,
    );
    let hello = read(&io);
    assert_eq!(field(&hello, "protocolVersion"), &JsValue::from("2.0"));
    assert_eq!(
        field(&hello, "connectionGeneration"),
        &JsValue::from("test-generation")
    );
    send(
        &mut io,
        r#"{"kind":"request","messageId":"c","method":"capabilities","payload":{}}"#,
    );
    let capabilities = read(&io);
    let JsValue::Array(methods) = field(field(&capabilities, "payload"), "methods") else {
        panic!("capabilities methods must be an array");
    };
    assert_eq!(field(&methods[0], "resumeSupport"), &JsValue::from("none"));
    assert_eq!(field(&methods[3], "enabled"), &JsValue::Bool(false));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    close(io, handle);
}
#[test]
fn expired_inclusive_deadline_never_invokes_factory_or_accepts_stream() {
    let (mut io, handle, calls) = setup(false);
    send(&mut io, &execute(Some("expired")));
    let result = read(&io);
    assert_eq!(field(&result, "code"), &JsValue::from("DEADLINE_EXCEEDED"));
    assert!(result.get("streamId").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    close(io, handle);
}
#[test]
fn unsupported_date_profile_fails_visibly_and_is_not_ignored() {
    let (mut io, handle, calls) = setup(false);
    send(&mut io, &execute(Some("unimplemented")));
    assert_eq!(
        field(&read(&io), "code"),
        &JsValue::from("UNSUPPORTED_DEADLINE_PROFILE")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    close(io, handle);
}
#[test]
fn status_and_cancel_reader_remain_live_during_real_pending_host_exchange() {
    let (mut io, handle, calls) = setup(true);
    // Five seconds are reserved for reconciliation of this accepted identity.
    io.set_read_timeout(Some(Duration::from_secs(7))).unwrap();
    send(&mut io, &execute(None));
    let accepted = read(&io);
    assert_eq!(field(&accepted, "ok"), &JsValue::Bool(true));
    let call = read(&io);
    assert_eq!(field(&call, "method"), &JsValue::from("module_call"));
    send(
        &mut io,
        r#"{"kind":"request","messageId":"wrong","method":"cancel","runId":"other","operationId":"o","reason":"stop"}"#,
    );
    assert_eq!(
        field(&read(&io), "code"),
        &JsValue::from("OPERATION_IDENTITY_MISMATCH")
    );
    send(
        &mut io,
        r#"{"kind":"request","messageId":"s","method":"status","requestId":"q"}"#,
    );
    assert_eq!(
        field(field(&read(&io), "payload"), "state"),
        &JsValue::from("running")
    );
    send(
        &mut io,
        r#"{"kind":"request","messageId":"stop","method":"cancel","runId":"r","operationId":"o","requestId":"q","reason":"stop"}"#,
    );
    let cancel = read(&io);
    assert_eq!(field(&cancel, "inReplyTo"), &JsValue::from("stop"));
    assert_eq!(field(&cancel, "ok"), &JsValue::Bool(true));
    // The host call was dispatched. Cancellation cannot claim it never happened.
    let terminal = read(&io);
    assert_eq!(
        field(&terminal, "outcome"),
        &JsValue::from("result_unknown")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn same_call_ack_after_cancel_settles_without_another_host_dispatch() {
    let (mut io, handle, calls) = setup(true);
    send(&mut io, &execute(None));
    let _accepted = read(&io);
    let call = read(&io);
    send(
        &mut io,
        r#"{"kind":"request","messageId":"stop","method":"cancel","runId":"r","operationId":"o","requestId":"q","reason":"stop"}"#,
    );
    let cancel = read(&io);
    assert_eq!(field(&cancel, "inReplyTo"), &JsValue::from("stop"));
    let mut reply = JsObject::new();
    reply.insert("kind".into(), "response".into());
    reply.insert("messageId".into(), "late-ack".into());
    reply.insert("inReplyTo".into(), field(&call, "messageId").clone());
    reply.insert("ok".into(), JsValue::Bool(true));
    reply.insert("payload".into(), JsValue::Object(JsObject::new()));
    send(
        &mut io,
        &pilotdeck_protocol::stringify_json(&JsValue::Object(reply))
            .unwrap()
            .unwrap(),
    );
    // This component factory has no AgentLoop abort policy. It tests only that
    // the call is known; production post-tool abort is tested against TS.
    assert_eq!(field(&read(&io), "outcome"), &JsValue::from("completed"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn explicit_unknown_retains_accepted_call_sequence_in_terminal() {
    let (mut io, handle, calls) = setup(true);
    send(&mut io, &execute(None));
    let _accepted = read(&io);
    let call = read(&io);
    let mut reply = JsObject::new();
    reply.insert("kind".into(), "response".into());
    reply.insert("messageId".into(), "unknown-ack".into());
    reply.insert("inReplyTo".into(), field(&call, "messageId").clone());
    reply.insert("ok".into(), JsValue::Bool(false));
    reply.insert("outcome".into(), "result_unknown".into());
    reply.insert("lastAppliedSequence".into(), JsValue::Number(19.0));
    send(
        &mut io,
        &pilotdeck_protocol::stringify_json(&JsValue::Object(reply))
            .unwrap()
            .unwrap(),
    );
    let terminal = read(&io);
    assert_eq!(
        field(&terminal, "outcome"),
        &JsValue::from("result_unknown")
    );
    let retained = field(field(&terminal, "error"), "acceptedCall");
    assert_eq!(
        field(retained, "lastAppliedSequence"),
        &JsValue::Number(19.0)
    );
    assert_eq!(field(retained, "requestId"), field(&call, "requestId"));
    assert_eq!(
        field(retained, "moduleInstanceId"),
        &JsValue::from("test-instance")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn deadline_prioritizes_public_terminal_over_accepted_call_diagnostic() {
    let (mut io, handle, calls) = setup(true);
    send(&mut io, &execute(Some("future")));
    let _accepted = read(&io);
    let call = read(&io);
    // No ACK and no explicit user cancel. The owning operation deadline expires.
    let terminal = read(&io);
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
    assert_eq!(field(&call, "operationId"), &JsValue::from("o"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn duplicate_terminal_is_cached_without_reexecuting_factory() {
    let (mut io, handle, calls) = setup(false);
    let request = execute(None);
    send(&mut io, &request);
    let _ = read(&io);
    let terminal = read(&io);
    send(&mut io, &request);
    assert_eq!(read(&io), terminal);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let conflicting = request.replace("\"payload\":{}", "\"payload\":{\"changed\":true}");
    send(&mut io, &conflicting);
    assert_eq!(
        field(&read(&io), "code"),
        &JsValue::from("REQUEST_IDENTITY_CONFLICT")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn accepted_deadline_returns_unknown_and_freezes_pending_effect() {
    let (mut io, handle, calls) = setup(true);
    send(&mut io, &execute(Some("future")));
    let _ = read(&io);
    let _ = read(&io);
    let terminal = read(&io);
    assert_eq!(
        field(&terminal, "outcome"),
        &JsValue::from("result_unknown")
    );
    // Deadline owns the public terminal; the connection keeps the unresolved
    // host receipt independently of the intentionally empty terminal payload.
    assert_eq!(
        field(&terminal, "code"),
        &JsValue::from("DEADLINE_EXCEEDED")
    );
    assert_eq!(
        field(&terminal, "payload"),
        &JsValue::Object(JsObject::new())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(io, handle);
}
#[test]
fn malformed_input_has_original_error_and_does_not_poison_next_hello() {
    let (mut io, handle, _) = setup(false);
    send(&mut io, "{invalid}");
    assert_eq!(field(&read(&io), "code"), &JsValue::from("INVALID_JSON"));
    send(
        &mut io,
        r#"{"kind":"request","messageId":"h","method":"hello","payload":{}}"#,
    );
    assert_eq!(field(&read(&io), "ok"), &JsValue::Bool(true));
    close(io, handle);
}
#[test]
fn eof_retires_pending_worker_without_waiting_forever() {
    let (mut io, handle, _) = setup(true);
    send(&mut io, &execute(None));
    let _ = read(&io);
    let _ = read(&io);
    let before = Instant::now();
    close(io, handle);
    assert!(before.elapsed() < Duration::from_secs(2));
}
// Silence no product declarations: every test above uses a deliberately named
// component factory. The production factory and Node Date.parse profile are pending.
