use super::*;
use pilotdeck_protocol::{
    module::ModuleOutcome,
    stringify_json,
    wire::{EventRef, RequestBodyRef},
};
use std::collections::{HashMap, VecDeque};
use tokio::task::JoinSet;

struct Call {
    request: JsValue,
    response: Option<JsValue>,
    deliveries: usize,
}
enum Completed {
    Module(JsString, Result<JsObject, HostError>),
    Event(JsValue, Result<(), HostError>),
}

pub(super) async fn run(
    connection: &mut Connection,
    request: ExecuteRequest,
    handler: Arc<dyn HostHandler>,
    ack_enabled: bool,
    limits: Limits,
    accepted: &mut bool,
) -> Result<ExecuteResult, HostError> {
    let execute_id = JsString::from("execute");
    let identity = request.identity;
    let mut outgoing = request_frame("execute", &execute_id, request.payload)
        .as_object()
        .unwrap()
        .clone();
    for (name, value) in [
        ("runId", &identity.run_id),
        ("operationId", &identity.operation_id),
        ("requestId", &identity.request_id),
        ("sessionId", &identity.session_id),
        ("turnId", &identity.turn_id),
    ] {
        outgoing.insert(name.into(), JsValue::String(value.clone()));
    }
    if let Some(key) = request.idempotency_key {
        outgoing.insert("idempotencyKey".into(), JsValue::String(key));
    }
    if let Some(deadline) = request.operation_deadline {
        outgoing.insert("operationDeadline".into(), JsValue::String(deadline));
    }
    let outgoing = JsValue::Object(outgoing);
    decode(&outgoing)?;
    connection.send(&outgoing)?;
    let mut stream_id: Option<JsString> = None;
    let mut next_sequence = 0.0;
    let mut calls: HashMap<JsString, Call> = HashMap::new();
    let mut events: VecDeque<JsValue> = VecDeque::new();
    let mut event_running = false;
    let mut terminal_received = false;
    let mut terminal_applied = None;
    let mut callbacks = JoinSet::new();
    loop {
        if callbacks.is_empty()
            && let Some(terminal) = terminal_applied.take()
        {
            return Ok(ExecuteResult::Terminal(terminal));
        }
        if !event_running && let Some(event) = events.pop_front() {
            let handler = handler.clone();
            callbacks.spawn(async move {
                let result = handler.event(event.clone()).await;
                Completed::Event(event, result)
            });
            event_running = true;
        }
        tokio::select! {
            completion = callbacks.join_next(), if !callbacks.is_empty() => {
                match completion.expect("nonempty callback set").map_err(|error|
                    HostError::new(HostErrorKind::Callback, format!("Host callback task failed: {error}")))? {
                    Completed::Module(id, payload) => {
                        let call = calls.get_mut(&id).expect("registered callback");
                        let response = module_response(&call.request, payload);
                        // TS client:1129-1133: cache before write; no second effect.
                        call.response = Some(response.clone());
                        for _ in 0..call.deliveries { connection.send(&response)?; }
                    }
                    Completed::Event(event, result) => {
                        result?;
                        event_running = false;
                        let MessageRef::Event(view) = decode(&event)? else { unreachable!() };
                        if view.final_event { terminal_applied = Some(event); continue; }
                        // TS client:680-684, 961-970: ACK after application, never final.
                        if ack_enabled {
                            let mut ack = request_frame("ack", &format!("ack-{}", view.sequence.value()).into(), JsObject::new())
                                .as_object().unwrap().clone();
                            ack.insert("streamId".into(), JsValue::String(view.stream_id.clone()));
                            ack.insert("lastAppliedSequence".into(), JsValue::Number(view.sequence.value()));
                            connection.send_ack(&JsValue::Object(ack));
                        }
                    }
                }
            }
            input = connection.receive(), if !terminal_received => {
                let raw = input?;
                match decode(&raw)? {
                    MessageRef::Response(response) => {
                        // TS client:780-781 ignores non-execute replies, including
                        // failed ACKs and unknown inReplyTo values, before checking ok.
                        if response.in_reply_to != &execute_id { continue; }
                        if response.fields.get("requestId").and_then(JsValue::as_string) != Some(&identity.request_id) {
                            return Err(HostError::protocol("Sidecar execute response does not match the active request."));
                        }
                        if stream_id.is_some() {
                            return Err(HostError::protocol("Sidecar sent a duplicate execute acceptance."));
                        }
                        if !response.ok {
                            if response.fields.get("final").and_then(JsValue::as_bool) == Some(true)
                                && response.outcome == Some(ModuleOutcome::Failed) {
                                return Ok(ExecuteResult::Rejected(raw));
                            }
                            return Err(HostError::protocol("Sidecar rejected execute request."));
                        }
                        if response.fields.get("final").and_then(JsValue::as_bool) == Some(true) {
                            return Err(HostError::protocol("Sidecar streaming execute response cannot be final when accepted."));
                        }
                        stream_id = Some(response.fields.get("streamId").and_then(JsValue::as_string)
                            .filter(|id| !id.is_empty()).ok_or_else(|| HostError::protocol("Sidecar execute response did not provide streamId."))?.clone());
                        *accepted = true;
                    }
                    MessageRef::Event(event) => {
                        // TS client:879-893: all four IDs, strictly 0,1,...; any duplicate fails.
                        if stream_id.as_ref() != Some(event.stream_id) || event.identity.run_id != &identity.run_id
                            || event.identity.operation_id != &identity.operation_id || event.identity.request_id != &identity.request_id {
                            return Err(HostError::protocol("Sidecar event identity does not match the active execution (or event before acceptance)."));
                        }
                        if event.sequence.value() != next_sequence {
                            return Err(HostError::protocol(format!("Sidecar event sequence mismatch: expected {next_sequence}, got {}.", event.sequence.value())));
                        }
                        next_sequence += 1.0;
                        validate_event(event, &identity)?;
                        // TS:680 suppresses the non-authoritative turn_completed projection.
                        if !event.final_event && text_is(event.payload.get("type"), "turn_completed") { continue; }
                        terminal_received = event.final_event;
                        if events.len() >= limits.queue_capacity {
                            return Err(HostError::new(HostErrorKind::Limit, "Host event application queue full."));
                        }
                        events.push_back(raw);
                    }
                    MessageRef::Request(call) => {
                        if !matches!(call.body, RequestBodyRef::ModuleCall { .. }) { continue; }
                        let RequestBodyRef::ModuleCall { identity: call_identity, .. } = call.body else { unreachable!() };
                        let id = call.message_id.clone();
                        // TS:1120-1121 revalidates identity before using the cache, so a
                        // replayed wrong-identity call fails closed instead of replaying.
                        if calls.contains_key(&id)
                            && (call_identity.run_id != &identity.run_id || call_identity.operation_id != &identity.operation_id) {
                            return Err(HostError::protocol("Sidecar replayed a module_call with a conflicting immutable identity."));
                        }
                        // TS:1149-1152: callback requestId is independent of execute.requestId.
                        if call_identity.run_id != &identity.run_id || call_identity.operation_id != &identity.operation_id {
                            if calls.len() >= limits.max_module_calls {
                                return Err(HostError::new(HostErrorKind::Limit, "Host module callback limit reached."));
                            }
                            let response = module_response(&raw, Err(HostError::protocol("Sidecar module call identity does not match the active execution.")));
                            // TS:1119-1133 caches failure replies as well. A later
                            // corrected identity must not recycle this messageId.
                            calls.insert(id, Call { request: raw.clone(), response: Some(response.clone()), deliveries: 1 });
                            connection.send(&response)?;
                            continue;
                        }
                        if let Some(previous) = calls.get_mut(&id) {
                            // TS:1167-1176. Compare JSON payload with protocol JS semantics,
                            // not serde maps (which would discard property order/lone surrogates).
                            if !same_call(&previous.request, &raw)? {
                                return Err(HostError::protocol("Sidecar replayed a module_call with a conflicting immutable identity."));
                            }
                            if let Some(response) = &previous.response { connection.send(response)?; }
                            else {
                                previous.deliveries += 1;
                                if previous.deliveries > limits.queue_capacity {
                                    return Err(HostError::new(HostErrorKind::Limit, "Too many pending duplicate module calls."));
                                }
                            }
                            continue;
                        }
                        if calls.len() >= limits.max_module_calls || callbacks.len() >= limits.queue_capacity {
                            return Err(HostError::new(HostErrorKind::Limit, "Host module callback limit reached."));
                        }
                        calls.insert(id.clone(), Call { request: raw.clone(), response: None, deliveries: 1 });
                        let handler = handler.clone();
                        callbacks.spawn(async move { Completed::Module(id, handler.module_call(raw).await) });
                    }
                    MessageRef::Error(error) => return Err(HostError::protocol(format!("Sidecar transport error {:?}: {:?}", error.code, error.message))),
                }
            }
        }
    }
    // JoinSet aborts unfinished callbacks on any return/drop, including timeouts.
}
pub(super) fn same_call(left: &JsValue, right: &JsValue) -> Result<bool, HostError> {
    let fields = [
        "messageId",
        "runId",
        "operationId",
        "requestId",
        "idempotencyKey",
        "module",
    ];
    if fields.iter().any(|key| left.get(key) != right.get(key)) {
        return Ok(false);
    }
    let encode = |value: &JsValue| {
        stringify_json(value.get("payload").expect("validated payload"))
            .map_err(|error| HostError::protocol(error.to_string()))
    };
    Ok(encode(left)? == encode(right)?)
}
fn module_response(call: &JsValue, result: Result<JsObject, HostError>) -> JsValue {
    let mut response = JsObject::new();
    response.insert("kind".into(), "response".into());
    let id = call.get("messageId").unwrap().as_string().unwrap();
    let mut message_id: Vec<u16> = "host-module-".encode_utf16().collect();
    message_id.extend_from_slice(id.units());
    response.insert(
        "messageId".into(),
        JsValue::String(JsString::from_utf16(message_id)),
    );
    response.insert("inReplyTo".into(), JsValue::String(id.clone()));
    response.insert("requestId".into(), call.get("requestId").unwrap().clone());
    response.insert("ok".into(), JsValue::Bool(result.is_ok()));
    match result {
        Ok(payload) => {
            response.insert("payload".into(), JsValue::Object(payload));
        }
        Err(error) => {
            response.insert("code".into(), "HOST_MODULE_FAILED".into());
            let mut detail = JsObject::new();
            detail.insert("message".into(), error.message.as_str().into());
            response.insert("error".into(), JsValue::Object(detail));
        }
    }
    JsValue::Object(response)
}
fn validate_event(event: EventRef<'_>, identity: &ExecutionIdentity) -> Result<(), HostError> {
    let matches_identity = |object: &JsObject, required_turn: bool| {
        object.get("sessionId").and_then(JsValue::as_string) == Some(&identity.session_id)
            && ((!required_turn && !object.contains_key("turnId"))
                || object.get("turnId").and_then(JsValue::as_string) == Some(&identity.turn_id))
    };
    // TS client:1310-1335, 1406-1425. No reconciliation authority in this PR.
    if !event.final_event {
        if event
            .payload
            .get("type")
            .and_then(JsValue::as_string)
            .is_none()
            || !matches_identity(event.payload, false)
        {
            return Err(HostError::protocol(
                "Sidecar AgentEvent does not match the active session or turn.",
            ));
        }
    } else {
        if event.outcome == Some(ModuleOutcome::ResultUnknown) {
            return Err(HostError::new(
                HostErrorKind::ResultUnknown,
                "Sidecar terminal outcome is result_unknown; host reconciliation is required.",
            ));
        }
        let result = event
            .payload
            .get("result")
            .and_then(JsValue::as_object)
            .ok_or_else(|| {
                HostError::protocol(
                    "Sidecar terminal payload does not contain an AgentLoop result.",
                )
            })?;
        if !matches!(event.payload.get("messages"), Some(JsValue::Array(_)))
            || !matches_identity(result, true)
            || result
                .get("stopReason")
                .and_then(JsValue::as_string)
                .is_none()
            || result.get("turns").and_then(JsValue::as_number).is_none()
        {
            return Err(HostError::protocol(
                "Sidecar terminal result does not match the active session or turn, or is missing messages.",
            ));
        }
        let valid = match event.outcome {
            Some(ModuleOutcome::Completed) => text_is(result.get("type"), "success"),
            Some(ModuleOutcome::Cancelled) => text_is(result.get("type"), "aborted"),
            Some(ModuleOutcome::Failed) => {
                text_is(result.get("type"), "error") || text_is(result.get("type"), "max_turns")
            }
            _ => false,
        };
        if !valid {
            return Err(HostError::protocol(
                "Sidecar terminal outcome conflicts with result type.",
            ));
        }
    }
    Ok(())
}
