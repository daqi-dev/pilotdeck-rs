use super::budget::Frame;
use super::*;
use pilotdeck_protocol::{
    module::ModuleOutcome,
    stringify_json,
    wire::{EventRef, RequestBodyRef},
};
use std::collections::{HashMap, VecDeque};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep_until},
};

struct Call {
    request: Frame,
    response: Option<Frame>,
    deliveries: usize,
}
enum Completed {
    Module(JsString, Result<Frame, HostError>),
    Event(Arc<Frame>, bool, Result<(), HostError>),
}

/// Observation and arming are one synchronous transition. Sending uses only
/// this saved reason, never a fresh cancellation read that could bypass arming.
#[derive(Default)]
struct CancelExit {
    reason: Option<JsString>,
    deadline: Option<Instant>,
    sent: bool,
}
impl CancelExit {
    fn observe(&mut self, cancellation: &Cancellation) {
        if self.reason.is_none()
            && let Some(reason) = cancellation.reason()
        {
            self.deadline = Some(Instant::now() + POST_CANCEL_EXIT_TIMEOUT);
            self.reason = Some(reason);
        }
    }
    fn take_reason_to_send(&mut self) -> Option<JsString> {
        if self.sent {
            return None;
        }
        let reason = self.reason.clone()?;
        debug_assert!(self.deadline.is_some());
        self.sent = true;
        Some(reason)
    }
}

fn post_cancel_timeout() -> HostError {
    HostError::new(
        HostErrorKind::Timeout,
        "Sidecar execute did not reach a terminal within the post-cancel exit deadline; progress is unknown.",
    )
}

fn terminal_result(event: Arc<Frame>, aborted: bool, progress: &mut Progress) -> ExecuteResult {
    // All callback/queue references have been released before transferring the
    // original lease. Sharing a validated final adds no unbudgeted payload copy.
    let event = Arc::try_unwrap(event).expect("terminal exclusively owned");
    progress.terminal_lease = Some(event.lease);
    if aborted {
        ExecuteResult::Aborted { terminal: None }
    } else {
        ExecuteResult::Terminal(event.value)
    }
}

/// Own callbacks and their cleanup together: abort alone is not a join barrier.
pub(super) struct TurnScope {
    callbacks: JoinSet<Completed>,
    handler: Option<Arc<dyn HostHandler>>,
    runtime: tokio::runtime::Handle,
    timeout: Duration,
}
impl TurnScope {
    pub(super) fn new(handler: Arc<dyn HostHandler>, timeout: Duration) -> Self {
        Self {
            callbacks: JoinSet::new(),
            handler: Some(handler),
            // Capture while execute is polled: its future may later be dropped
            // on a plain thread even though this runtime is still alive.
            runtime: tokio::runtime::Handle::current(),
            timeout,
        }
    }
    /// Test hook: an in-flight callback that never completes.
    #[cfg(test)]
    pub(super) fn spawn_pending_callback_for_test(&mut self) {
        self.callbacks.spawn(std::future::pending());
    }
    pub(super) async fn stop_callbacks(&mut self) {
        self.callbacks.shutdown().await;
    }
    fn start_dispose(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let handler = self.handler.take()?;
        let mut callbacks = std::mem::take(&mut self.callbacks);
        let duration = self.timeout;
        Some(self.runtime.spawn(async move {
            callbacks.shutdown().await;
            let _ = timeout(duration, async { handler.dispose().await }).await;
        }))
    }
    pub(super) async fn dispose(&mut self) {
        if let Some(task) = self.start_dispose() {
            // Isolate a cleanup panic as well; it cannot replace a known result.
            let _ = task.await;
        }
    }
}
impl Drop for TurnScope {
    fn drop(&mut self) {
        // Dropping the JoinHandle detaches cleanup, including if the caller is
        // itself cancelled while awaiting dispose. Ownership is taken once.
        drop(self.start_dispose());
    }
}

pub(super) async fn run(
    client: &mut HostClient,
    request: ExecuteRequest,
    handler: Arc<dyn HostHandler>,
    progress: &mut Progress,
    cancellation: Cancellation,
    scope: &mut TurnScope,
) -> Result<ExecuteResult, HostError> {
    // TS client:593: an already-aborted input never sends execute.
    if cancellation.is_cancelled() {
        return Ok(ExecuteResult::Aborted { terminal: None });
    }
    let limits = client.limits;
    let execution_deadline = limits
        .execution_timeout
        .map(|timeout| {
            Instant::now().checked_add(timeout).ok_or_else(|| {
                HostError::protocol(
                    "Sidecar execution_timeout is not representable by the monotonic clock.",
                )
            })
        })
        .transpose()?;
    let mut cancel_changes = cancellation.0.subscribe();
    let mut cancel = CancelExit::default();
    #[cfg(test)]
    let signals = client.test_signals.clone();
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
    client.connection.send(&outgoing)?;
    drop(outgoing);
    let mut next_sequence = 0.0;
    let mut calls: HashMap<JsString, Call> = HashMap::new();
    let mut events: VecDeque<(Arc<Frame>, bool)> = VecDeque::new();
    let mut event_running = false;
    let mut terminal_received = false;
    let mut known_terminal = None;
    let mut terminal_applied = None;
    let mut drain_deadline = None;
    let callbacks = &mut scope.callbacks;
    let mut interruption = None;
    // True once the cancel deadline is seen already elapsed at a loop top,
    // i.e. this future resumed after being unpolled (a slow consumer). One
    // scheduling turn is then granted so an application spawned on this poll
    // can reach the session before the exit drops it.
    let mut cancel_catch_up = false;
    loop {
        // The deadline survives acceptance, best-effort cancel, reconnect and
        // terminal application. Repeated observations never move it forward.
        cancel.observe(&cancellation);
        // This future is only polled from the session's next(), so a slow
        // consumer can resume the loop with the deadline already elapsed while
        // lifecycle work is still queued. Grant the applications waiting on
        // this poll one scheduling turn before the deadline is reconsidered;
        // a genuinely blocked callback makes no progress and the next expiry
        // still exits.
        if cancel
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            && (!events.is_empty() || !callbacks.is_empty())
        {
            cancel_catch_up = true;
        }
        #[cfg(test)]
        signals.cancel_deadline.send_replace(cancel.deadline);
        if let Some(error) = interruption.take() {
            // A known final never opens another connection. Preserve pr2's
            // required-write failure semantics and bounded terminal drain.
            if terminal_received {
                return Err(error);
            }
            // Old response deliveries belong to the lost connection. Keep the
            // cached response/future, and wait for the peer's immutable replay.
            // Accepted but not yet applied events stay queued and are applied
            // in order; the cursor below tells the peer not to replay them.
            for call in calls.values_mut() {
                call.deliveries = 0;
            }
            let reconnect = client.reconnect(progress, &handler, error, next_sequence - 1.0);
            tokio::pin!(reconnect);
            loop {
                cancel.observe(&cancellation);
                #[cfg(test)]
                signals.cancel_deadline.send_replace(cancel.deadline);
                tokio::select! {
                    result = &mut reconnect => {
                        result?;
                        #[cfg(test)]
                        signals.reconnect_completed.send_replace(true);
                        break;
                    }
                    _ = wait_for_deadline(execution_deadline) => {
                        return Err(HostError::new(HostErrorKind::Timeout, "Sidecar execute timed out during reconnect."));
                    }
                    _ = wait_for_deadline(cancel.deadline) => return Err(post_cancel_timeout()),
                    _ = cancel_changes.changed(), if cancel.deadline.is_none() => {}
                }
            }
            // Reobserve before sending: cancellation may have raced with the
            // successful reconnect poll. Do not restart the reconnect future.
            continue;
        }
        if callbacks.is_empty()
            && let Some(terminal) = terminal_applied.take()
        {
            return Ok(terminal);
        }
        if client.cancel_enabled
            && progress.stream_id.is_some()
            && let Some(reason) = cancel.take_reason_to_send()
        {
            // TS client:943-958: once, only after acceptance, no streamId/payload.
            // Set before attempting the best-effort write; failure never retries.
            let mut cancel = JsObject::new();
            for (key, value) in [
                ("kind", "request"),
                ("messageId", "cancel-execute"),
                ("method", "cancel"),
            ] {
                cancel.insert(key.into(), value.into());
            }
            for (key, value) in [
                ("runId", &identity.run_id),
                ("operationId", &identity.operation_id),
                ("requestId", &identity.request_id),
            ] {
                cancel.insert(key.into(), JsValue::String(value.clone()));
            }
            cancel.insert(
                "reason".into(),
                JsValue::String(if reason.is_empty() {
                    "host_abort".into()
                } else {
                    reason
                }),
            );
            client.connection.send_cancel(&JsValue::Object(cancel));
        }
        if !event_running && let Some((event, aborted)) = events.pop_front() {
            let handler = handler.clone();
            let lease = event.lease.duplicate()?;
            let sequence = event
                .get("sequence")
                .and_then(JsValue::as_number)
                .expect("validated event");
            let last_applied = progress.last_applied_sequence.clone();
            callbacks.spawn(async move {
                // TS readFinalTerminal replaces an incomplete compaction final
                // with abortedResult(input). Do not expose that raw payload to
                // a handler; the session layer projects the original input.
                let result = if aborted {
                    Ok(())
                } else {
                    handler.event(event.value.clone()).await
                };
                // Publish at callback success, not when the parent polls JoinSet:
                // a simultaneously-ready EOF must retain the applied sequence.
                if result.is_ok() && !aborted {
                    last_applied.store(sequence.to_bits(), Ordering::SeqCst);
                }
                drop(lease);
                Completed::Event(event, aborted, result)
            });
            event_running = true;
        }
        if cancel_catch_up {
            // Let a task spawned by the block above (or a completed callback)
            // run before the expired deadline is polled again.
            tokio::task::yield_now().await;
        }
        // Prefer work that is already available over the cancel deadline: this
        // future is only polled from the session's next(), so a slow consumer
        // can leave an application finished but not yet collected, and its
        // bounded channel send may be waiting for the consumer. Collect and
        // deliver those before the exit drops the queue. The deadline stays
        // ahead of `receive` so a still-streaming peer cannot starve it.
        tokio::select! {
            biased;
            _ = wait_for_deadline(execution_deadline), if terminal_applied.is_none() => {
                return Err(HostError::new(HostErrorKind::Timeout, "Sidecar execute timed out."));
            }
            _ = wait_for_deadline(drain_deadline), if terminal_applied.is_some() => {
                // TS processes callbacks serially (570-579); this host runs them
                // concurrently. Once final is applied, bounded drain cannot turn
                // a known terminal into an execution timeout (#191/2).
                callbacks.abort_all();
                while callbacks.join_next().await.is_some() {}
                return Ok(terminal_applied.take().expect("applied terminal"));
            }
            _ = cancel_changes.changed(), if cancel.deadline.is_none() => {}

            completion = callbacks.join_next(), if !callbacks.is_empty() => {
                match completion.expect("nonempty callback set").map_err(|error|
                    HostError::new(HostErrorKind::Callback, format!("Host callback task failed: {error}")))? {
                    Completed::Module(id, response) => {
                        let call = calls.get_mut(&id).expect("registered callback");
                        // TS client:1129-1133: cache before write; no second effect.
                        call.response = Some(response?);
                        for _ in 0..call.deliveries {
                            if let Err(error) = client.connection.send(call.response.as_ref().unwrap()) {
                                interruption = Some(error);
                                break;
                            }
                        }
                    }
                    Completed::Event(event, aborted, result) => {
                        result?;
                        event_running = false;
                        let MessageRef::Event(view) = decode(&event)? else { unreachable!() };
                        if view.final_event {
                            known_terminal.take();
                            terminal_applied = Some(terminal_result(event, aborted, progress));
                            // Pre-existing drain policy: an unrepresentable
                            // terminal_drain_timeout is effectively unbounded.
                            // `unbounded_execution_and_maximum_terminal_drain_do_not_panic`
                            // pins that behavior; unlike execution_timeout, this
                            // accepted-diff field is not validated for
                            // representability in this fix.
                            drain_deadline = Instant::now().checked_add(limits.terminal_drain_timeout);
                            continue;
                        }
                        // TS client:680-684, 961-970: ACK after application, never final.
                        if client.ack_enabled {
                            let mut ack = request_frame("ack", &format!("ack-{}", view.sequence.value()).into(), JsObject::new())
                                .as_object().unwrap().clone();
                            ack.insert("streamId".into(), JsValue::String(view.stream_id.clone()));
                            ack.insert("lastAppliedSequence".into(), JsValue::Number(view.sequence.value()));
                            client.connection.send_ack(&JsValue::Object(ack));
                        }
                    }
                }
            }
            // Receiving final does not mean its event callback has completed.
            // At the same absolute bound, stop cooperative application and keep
            // the validated terminal authoritative without claiming it applied.
            _ = wait_for_deadline(cancel.deadline) => {
                callbacks.shutdown().await;
                events.clear();
                if let Some(terminal) = terminal_applied.take() { return Ok(terminal); }
                if let Some((event, aborted)) = known_terminal.take() {
                    return Ok(terminal_result(event, aborted, progress));
                }
                return Err(post_cancel_timeout());
            }
            input = client.connection.receive(), if !terminal_received => {
                let raw = match input {
                    Ok(raw) => raw,
                    Err(error) => { interruption = Some(error); continue; }
                };
                match decode(&raw)? {
                    MessageRef::Response(response) => {
                        // TS client:780-781 ignores non-execute replies, including
                        // failed ACKs and unknown inReplyTo values, before checking ok.
                        if response.in_reply_to != &execute_id { continue; }
                        if response.fields.get("requestId").and_then(JsValue::as_string) != Some(&identity.request_id) {
                            return Err(HostError::protocol("Sidecar execute response does not match the active request."));
                        }
                        if !response.ok {
                            if response.fields.get("final").and_then(JsValue::as_bool) == Some(true)
                                && response.outcome == Some(ModuleOutcome::Failed) {
                                progress.terminal_lease = Some(raw.lease);
                                return Ok(ExecuteResult::Rejected(raw.value));
                            }
                            let reason = response.fields.get("code").and_then(JsValue::as_string)
                                .or_else(|| response.fields.get("error").and_then(|e| e.get("message")).and_then(JsValue::as_string))
                                .map_or_else(|| "unknown error".to_owned(), |s| String::from_utf16_lossy(s.units()));
                            return Err(HostError::protocol(format!("Sidecar rejected execute request: {reason}")));
                        }
                        if response.fields.get("final").and_then(JsValue::as_bool) == Some(true) {
                            return Err(HostError::protocol("Sidecar streaming execute response cannot be final when accepted."));
                        }
                        // TS client:780-798 accepts repeated acceptance, updates
                        // streamId without resetting sequence or re-sending cancel.
                        progress.stream_id = Some(response.fields.get("streamId").and_then(JsValue::as_string)
                            .filter(|id| !id.is_empty()).ok_or_else(|| HostError::protocol("Sidecar execute response did not provide streamId."))?.clone());
                    }
                    MessageRef::Event(event) => {
                        // TS client:879-893: all four IDs, strictly 0,1,..., replay
                        // included. Any duplicate/backward/gap fails closed.
                        if progress.stream_id.as_ref() != Some(event.stream_id) || event.identity.run_id != &identity.run_id
                            || event.identity.operation_id != &identity.operation_id || event.identity.request_id != &identity.request_id {
                            return Err(HostError::protocol("Sidecar event identity does not match the active execution (or event before acceptance)."));
                        }
                        let aborted = validate_event(event, &identity, &cancellation)?;
                        if event.sequence.value() != next_sequence {
                            return Err(HostError::protocol(format!("Sidecar event sequence mismatch: expected {next_sequence}, got {}.", event.sequence.value())));
                        }
                        next_sequence += 1.0;
                        // TS:680 suppresses the non-authoritative turn_completed projection.
                        if !event.final_event && text_is(event.payload.get("type"), "turn_completed") { continue; }
                        // Lifecycle validation after the wire checks and the
                        // suppression above, before the application queue: a
                        // violating event is never applied and never ACKed. The
                        // authoritative final is skipped; the caller projects
                        // it from ExecuteResult.
                        if !event.final_event
                            && let Err(error) = handler.validate_event(
                                raw.get("payload").expect("validated event payload"),
                            )
                        {
                            return Err(error);
                        }
                        terminal_received = event.final_event;
                        if events.len() >= limits.queue_capacity {
                            return Err(HostError::new(HostErrorKind::Limit, "Host event application queue full."));
                        }
                        let final_event = event.final_event;
                        let raw = Arc::new(raw);
                        if final_event {
                            known_terminal = Some((raw.clone(), aborted));
                            #[cfg(test)]
                            client.test_signals.final_received.store(true, Ordering::SeqCst);
                        }
                        events.push_back((raw, aborted));
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
                            let delivery = client.connection.send(&response);
                            let response = client.connection.budget.retain(response)?;
                            calls.insert(id, Call { request: raw, response: Some(response), deliveries: 1 });
                            if let Err(error) = delivery { interruption = Some(error); }
                            continue;
                        }
                        if let Some(previous) = calls.get_mut(&id) {
                            // TS:1167-1176. Compare JSON payload with protocol JS semantics,
                            // not serde maps (which would discard property order/lone surrogates).
                            if !same_call(&previous.request, &raw)? {
                                return Err(HostError::protocol("Sidecar replayed a module_call with a conflicting immutable identity."));
                            }
                            if let Some(response) = &previous.response {
                                if let Err(error) = client.connection.send(response) { interruption = Some(error); }
                            }
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
                        let lease = raw.lease.duplicate()?;
                        let callback_request = raw.value.clone();
                        // Only correlation IDs must survive the handler call. Charge
                        // them too (IDs can be large), without another payload copy.
                        let mut correlation = JsObject::new();
                        for key in ["messageId", "requestId"] {
                            correlation.insert(key.into(), raw.get(key).unwrap().clone());
                        }
                        let correlation = client.connection.budget.retain(JsValue::Object(correlation))?;
                        calls.insert(id.clone(), Call { request: raw, response: None, deliveries: 1 });
                        // Construct the callback in arrival order on this task:
                        // a handler's synchronous prefix (e.g. `event.emit`)
                        // then runs in sidecar order, like TS's async handlers
                        // before their first await. Only the rest runs
                        // concurrently.
                        let callback = handler.module_call(callback_request);
                        let budget = client.connection.budget.clone();
                        callbacks.spawn(async move {
                            let payload = callback.await;
                            drop(lease);
                            let response = module_response(&correlation, payload);
                            // Acquire before JoinSet stores a completed result: many
                            // finished callbacks cannot accumulate unbudgeted payloads.
                            Completed::Module(id, budget.retain(response))
                        });
                    }
                    MessageRef::Error(error) => return Err(HostError::protocol(format!("Sidecar transport error {:?}: {:?}", error.code, error.message))),
                }
            }
        }
    }
    // TurnScope joins unfinished callbacks before disposing turn-owned state.
}
// Execution and terminal drain are independently optional; None parks that
// branch forever, so "no deadline" is modeled instead of using a sentinel.
async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
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
pub(super) fn module_response(call: &JsValue, result: Result<JsObject, HostError>) -> JsValue {
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
            let module = error.module_error.as_deref();
            response.insert(
                "code".into(),
                JsValue::String(
                    module
                        .and_then(|error| error.code.clone())
                        .unwrap_or_else(|| "HOST_MODULE_FAILED".into()),
                ),
            );
            let mut detail = JsObject::new();
            detail.insert(
                "message".into(),
                JsValue::String(
                    module
                        .map(|error| error.message.clone())
                        .unwrap_or_else(|| error.message.into()),
                ),
            );
            if let Some(module) = module {
                if let Some(canonical) = &module.canonical
                    && canonical.get("code").and_then(JsValue::as_string).is_some()
                    && canonical
                        .get("message")
                        .and_then(JsValue::as_string)
                        .is_some()
                {
                    detail.insert("canonical".into(), JsValue::Object(canonical.clone()));
                }
                if let Some(retryable) = module.retryable {
                    detail.insert("retryable".into(), JsValue::Bool(retryable));
                }
                if let Some(delay) = module.retry_after_ms.filter(|value| value.is_finite()) {
                    detail.insert("retryAfterMs".into(), JsValue::Number(delay));
                }
            }
            response.insert("error".into(), JsValue::Object(detail));
        }
    }
    JsValue::Object(response)
}
fn validate_event(
    event: EventRef<'_>,
    identity: &ExecutionIdentity,
    cancellation: &Cancellation,
) -> Result<bool, HostError> {
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
        // TS client:1338-1350: only this local abort reason permits a missing
        // result/messages fallback. A well-shaped contradictory result still fails.
        let complete = event
            .payload
            .get("result")
            .and_then(JsValue::as_object)
            .is_some_and(|r| {
                ["success", "error", "aborted", "max_turns"]
                    .iter()
                    .any(|t| text_is(r.get("type"), t))
                    && ["sessionId", "turnId", "stopReason"]
                        .iter()
                        .all(|k| r.get(k).and_then(JsValue::as_string).is_some())
                    && r.get("turns").and_then(JsValue::as_number).is_some()
            })
            && matches!(event.payload.get("messages"), Some(JsValue::Array(_)));
        if !complete
            && event.outcome == Some(ModuleOutcome::Cancelled)
            && cancellation
                .reason()
                .is_some_and(|reason| reason.equals_str("compaction_persistence_failed"))
        {
            return Ok(true);
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
    Ok(false)
}

#[cfg(test)]
mod cancel_state_tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_between_observation_and_send_cannot_send_without_deadline() {
        let cancellation = Cancellation::default();
        let mut state = CancelExit::default();
        state.observe(&cancellation);
        // A different OS thread cancels after the top-of-loop observation.
        std::thread::spawn({
            let cancellation = cancellation.clone();
            move || cancellation.cancel(Some("concurrent-cancel".into()))
        })
        .join()
        .unwrap();
        assert_eq!(state.take_reason_to_send(), None);
        assert!(!state.sent && state.deadline.is_none());
        state.observe(&cancellation);
        let deadline = state.deadline.expect("observation must arm before send");
        assert_eq!(
            state.take_reason_to_send(),
            Some("concurrent-cancel".into())
        );
        assert!(state.sent && state.deadline.is_some());
        state.observe(&cancellation);
        assert_eq!(state.deadline, Some(deadline));
        assert_eq!(state.take_reason_to_send(), None);
    }
}
