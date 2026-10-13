//! Concurrent connection owner for Module2.0 NDJSON. No provider or Session IO.
//! The input reader resolves calls/controls; an independent writer performs IO.
//! EOF retires this connection. This owner never advertises process reconnect.
use crate::{HostExchange, HostExchangeResponse};
use pilotdeck_agent::ports::{CallControl, ExecutionIdentity, PortError, PortResult, WakeSignal};
use pilotdeck_protocol::admission::{CallEffect, SidecarBinding};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::wire::{EnvelopeRef, MessageRef};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchPhase {
    Queued,
    Dispatching,
    Sent,
    Replied,
    InvalidReply,
    /// A dispatched call exhausted its owning reconciliation bound. A later
    /// correlated ACK can still settle the retained record; no resend follows.
    Unresolved,
}
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub identity: ExecutionIdentity,
    pub effect: CallEffect,
    /// Exact bytes retained before dispatch; never reconstructed for retry.
    pub request_line: String,
    pub phase: DispatchPhase,
    /// Cached before notifying a waiting worker. Duplicate input cannot replace it.
    pub response: Option<BoundarySnapshot>,
    pub cancelled_before_response: bool,
    pub invalid_response: Option<BoundarySnapshot>,
    admission_sequence: u64,
    module: &'static str,
    operation: &'static str,
    preparation_id: Option<JsString>,
    /// This request never reached Dispatching; its waiter receives the prior
    /// accepted uncertainty instead of inventing acceptance for this request.
    admission_rejected: Option<PortError>,
    response_control: CallControl,
    abort_capture_payload: bool,
}
#[derive(Debug)]
struct Packet {
    pending: Option<JsString>,
    line: String,
}
#[derive(Debug, Default)]
struct State {
    closed: bool,
    control: VecDeque<Packet>,
    data: VecDeque<Packet>,
    calls: HashMap<JsString, CallRecord>,
    next_call_sequence: u64,
}
#[derive(Debug)]
struct Inner {
    binding: SidecarBinding,
    state: Mutex<State>,
    wake: Arc<WakeSignal>,
    next_id: AtomicU64,
    queue_capacity: usize,
}
/// Clone is a handle to this exact connection, not a new generation.
#[derive(Debug, Clone)]
pub struct ConnectionOwner(Arc<Inner>);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDisposition {
    Completed,
    Duplicate,
    Uncorrelated,
    InvalidCorrelation,
}
impl ConnectionOwner {
    pub fn new(binding: SidecarBinding, queue_capacity: usize) -> Self {
        Self(Arc::new(Inner {
            binding,
            state: Mutex::new(State::default()),
            wake: Arc::new(WakeSignal::default()),
            next_id: AtomicU64::new(1),
            queue_capacity: queue_capacity.max(1),
        }))
    }
    pub fn binding(&self) -> &SidecarBinding {
        &self.0.binding
    }
    pub fn next_id(&self, prefix: &str) -> JsString {
        let sequence = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        // Instance/generation are already owner-issued strings. They stay exact.
        let mut units: Vec<u16> = prefix.encode_utf16().collect();
        units.push(45);
        units.extend_from_slice(self.0.binding.connection_generation.units());
        units.push(45);
        units.extend(sequence.to_string().encode_utf16());
        JsString::from_utf16(units)
    }
    pub fn is_closed(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed
    }
    pub fn close(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        state.control.clear();
        state.data.clear();
        drop(state);
        self.0.wake.notify();
    }
    pub fn call_record(&self, message_id: &JsString) -> Option<CallRecord> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .calls
            .get(message_id)
            .cloned()
    }
    pub(crate) fn check_known_uncertainty(&self, identity: &ExecutionIdentity) -> PortResult<()> {
        known_uncertainty(
            &self.0.state.lock().unwrap_or_else(|p| p.into_inner()),
            identity,
        )
        .map_or(Ok(()), Err)
    }
    /// Release known completed replies after their execution has settled. Keep
    /// every unresolved/contradictory/explicit-unknown request until this owner
    /// retires: none of those can safely become a fresh dispatch.
    pub fn release_known_calls(&self, execution: &ExecutionIdentity) -> usize {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        let before = state.calls.len();
        let has_unknown = known_uncertainty(&state, execution).is_some();
        state.calls.retain(|_, call| {
            let same = call.identity.run_id == execution.run_id
                && call.identity.operation_id == execution.operation_id
                && call.identity.session_id == execution.session_id
                && call.identity.turn_id == execution.turn_id;
            let known = call.phase == DispatchPhase::Replied
                && call.response.as_ref().is_some_and(|response| {
                    !response
                        .value()
                        .get("outcome")
                        .and_then(JsValue::as_string)
                        .is_some_and(|outcome| outcome.equals_str("result_unknown"))
                });
            !(same
                && known
                && !(has_unknown && call.module == "context" && call.operation == "capture_turn"))
        });
        before - state.calls.len()
    }
    pub fn bound_exchange(&self, identity: ExecutionIdentity) -> PortResult<BoundExchange> {
        if identity.module_instance_id != self.0.binding.module_instance_id
            || identity.connection_generation != self.0.binding.connection_generation
        {
            return Err(PortError::InvalidResponse {
                message: "Exchange binding does not belong to this connection.".into(),
            });
        }
        Ok(BoundExchange {
            owner: self.clone(),
            identity,
        })
    }
    /// Reader never waits for output capacity. Controls change cancellation/state
    /// first; output overload retires the connection instead of freezing input.
    pub fn send_control(&self, message: &JsValue) -> io::Result<()> {
        let line = line(message)?;
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(closed());
        }
        if state.control.len() >= self.0.queue_capacity {
            drop(state);
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Module control output queue is full.",
            ));
        }
        state.control.push_back(Packet {
            pending: None,
            line,
        });
        drop(state);
        self.0.wake.notify();
        Ok(())
    }
    /// Reader-safe replay stays behind already queued stream frames. A cached
    /// final must never enter the priority control queue and overtake its prefix.
    pub fn send_cached_stream(&self, message: &JsValue) -> io::Result<()> {
        let line = line(message)?;
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(closed());
        }
        if state.data.len() >= self.0.queue_capacity {
            drop(state);
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Module stream output queue is full.",
            ));
        }
        state.data.push_back(Packet {
            pending: None,
            line,
        });
        drop(state);
        self.0.wake.notify();
        Ok(())
    }
    /// Owning worker may wait for bounded output capacity without blocking the
    /// independent reader. Cancellation/EOF/output progress all wake that wait.
    pub fn publish(&self, control: &CallControl, message: &JsValue) -> PortResult<()> {
        let line = line(message).map_err(before)?;
        self.0.wake_registered(control);
        loop {
            let generation = self.0.wake.generation();
            control.check()?;
            let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return Err(before(closed()));
            }
            if state.data.len() < self.0.queue_capacity {
                state.data.push_back(Packet {
                    pending: None,
                    line,
                });
                drop(state);
                self.0.wake.notify();
                return Ok(());
            }
            drop(state);
            self.0.wake.wait(generation, control.deadline);
        }
    }
    /// Called only by the reader of this connection. Optional identity fields
    /// are checked when present; no new mandatory fields are imposed on original
    /// host responses. Handshake binding is out of band, never payload authority.
    pub fn receive_response(&self, value: &JsValue) -> ResponseDisposition {
        let Ok(envelope) = EnvelopeRef::decode(value) else {
            return ResponseDisposition::Uncorrelated;
        };
        let MessageRef::Response(response) = envelope.message else {
            return ResponseDisposition::Uncorrelated;
        };
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(record) = state.calls.get_mut(response.in_reply_to) else {
            return ResponseDisposition::Uncorrelated;
        };
        if matches!(
            record.phase,
            DispatchPhase::Replied | DispatchPhase::InvalidReply
        ) {
            return ResponseDisposition::Duplicate;
        }
        // A peer cannot satisfy a queued request it has not yet received.
        if record.phase == DispatchPhase::Queued {
            return ResponseDisposition::Uncorrelated;
        }
        let checks = [
            ("requestId", &record.identity.request_id),
            ("runId", &record.identity.run_id),
            ("operationId", &record.identity.operation_id),
            ("moduleInstanceId", &record.identity.module_instance_id),
            (
                "connectionGeneration",
                &record.identity.connection_generation,
            ),
        ];
        let mismatched = checks.into_iter().any(|(name, expected)| {
            response
                .fields
                .get(name)
                .is_some_and(|value| !value.is_undefined() && value.as_string() != Some(expected))
        });
        let disposition = if mismatched {
            record.invalid_response = Some(BoundarySnapshot::capture(value));
            record.phase = DispatchPhase::InvalidReply;
            ResponseDisposition::InvalidCorrelation
        } else {
            let received_at = std::time::Instant::now();
            record.cancelled_before_response = record
                .response_control
                .cancellation
                .was_cancelled_by(received_at)
                || record
                    .response_control
                    .deadline
                    .is_some_and(|deadline| deadline <= received_at);
            record.response = Some(BoundarySnapshot::capture(value));
            record.phase = DispatchPhase::Replied;
            ResponseDisposition::Completed
        };
        drop(state);
        self.0.wake.notify();
        disposition
    }
    /// Independent single writer. No state lock spans write/flush. Dispatching is
    /// recorded before attempting bytes; partial output is accepted/unknown.
    pub fn drive_writer(&self, mut output: impl Write) -> io::Result<()> {
        loop {
            let generation = self.0.wake.generation();
            let packet = {
                let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
                if state.closed {
                    return Ok(());
                }
                let packet = state.control.pop_front().or_else(|| state.data.pop_front());
                if let Some(packet) = &packet
                    && let Some(id) = &packet.pending
                {
                    let Some(call) = state.calls.get(id) else {
                        continue;
                    };
                    if call.phase != DispatchPhase::Queued {
                        continue;
                    }
                    // Receipt and Queued -> Dispatching linearize under the
                    // same lock. Already Dispatching bytes remain uncertain.
                    let rejection = if owned_containment_cleanup(
                        &state,
                        &call.identity,
                        call.module,
                        call.operation,
                        call.preparation_id.as_ref(),
                    ) || owned_abort_capture(
                        &state,
                        &call.identity,
                        call.module,
                        call.operation,
                        &call.response_control,
                        call.abort_capture_payload,
                    ) {
                        None
                    } else {
                        known_uncertainty(&state, &call.identity)
                    };
                    let call = state.calls.get_mut(id).expect("queued call retained");
                    if let Some(error) = rejection {
                        call.admission_rejected = Some(error);
                        self.0.wake.notify();
                        continue;
                    }
                    call.phase = DispatchPhase::Dispatching;
                }
                packet
            };
            let Some(packet) = packet else {
                self.0.wake.wait(generation, None);
                continue;
            };
            self.0.wake.notify();
            if let Err(error) = output
                .write_all(packet.line.as_bytes())
                .and_then(|()| output.flush())
            {
                self.close();
                return Err(error);
            }
            if let Some(id) = &packet.pending {
                let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(record) = state.calls.get_mut(id)
                    && record.phase == DispatchPhase::Dispatching
                {
                    record.phase = DispatchPhase::Sent;
                }
            }
            self.0.wake.notify();
        }
    }
}
impl Inner {
    fn wake_registered(&self, control: &CallControl) {
        control.cancellation.register_wake(&self.wake);
    }
}
fn line(value: &JsValue) -> io::Result<String> {
    stringify_json(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
        .map(|mut line| {
            line.push('\n');
            line
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Module output is undefined."))
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "Module connection is closed.")
}
fn before(error: io::Error) -> PortError {
    PortError::TransportBeforeDispatch {
        message: error.to_string().into(),
    }
}
fn unknown(identity: &ExecutionIdentity) -> PortError {
    PortError::AcceptedUnknown {
        identity: Box::new(identity.clone()),
        last_applied_sequence: None,
    }
}
pub struct BoundExchange {
    owner: ConnectionOwner,
    identity: ExecutionIdentity,
}
impl BoundExchange {
    pub(crate) fn start(
        &mut self,
        control: &CallControl,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<PendingExchange> {
        let abort_capture_payload = payload.get("input").is_some_and(|input| {
            input.get("sessionId").and_then(JsValue::as_string) == Some(&self.identity.session_id)
                && input.get("turnId").and_then(JsValue::as_string) == Some(&self.identity.turn_id)
                && input.get("errored") == Some(&JsValue::Bool(false))
        });
        let preparation_id = payload
            .get("preparationId")
            .and_then(JsValue::as_string)
            .cloned();
        {
            let state = self.owner.0.state.lock().unwrap_or_else(|p| p.into_inner());
            if !owned_containment_cleanup(
                &state,
                &self.identity,
                module,
                operation,
                preparation_id.as_ref(),
            ) && !owned_abort_capture(
                &state,
                &self.identity,
                module,
                operation,
                control,
                abort_capture_payload,
            ) && let Some(error) = known_uncertainty(&state, &self.identity)
            {
                return Err(error);
            }
            control.check()?;
        }
        let message_id = self.owner.next_id("module-call");
        let mut identity = self.identity.clone();
        identity.request_id = self.owner.next_id("host-request");
        let mut request = JsObject::new();
        for (key, value) in [
            ("kind", JsValue::from("request")),
            ("messageId", JsValue::String(message_id.clone())),
            ("method", JsValue::from("module_call")),
            ("module", JsValue::from(module)),
            ("operation", JsValue::from(operation)),
            ("runId", JsValue::String(identity.run_id.clone())),
            (
                "operationId",
                JsValue::String(identity.operation_id.clone()),
            ),
            ("requestId", JsValue::String(identity.request_id.clone())),
            ("payload", JsValue::Object(payload)),
        ] {
            request.insert(key.into(), value);
        }
        if let Some(key) = &identity.idempotency_key {
            request.insert("idempotencyKey".into(), JsValue::String(key.clone()));
        }
        let request = JsValue::Object(request);
        // Stage-one validation is still required at our own outbound boundary.
        EnvelopeRef::decode(&request).map_err(|error| PortError::InvalidResponse {
            message: error.message.into(),
        })?;
        let request_line = line(&request).map_err(before)?;
        self.owner.0.wake_registered(control);
        loop {
            let generation = self.owner.0.wake.generation();
            let mut state = self.owner.0.state.lock().unwrap_or_else(|p| p.into_inner());
            // Reader receipt and new admission share this lock. A cached
            // Unknown cannot be overlooked while another worker wakes late.
            if !owned_containment_cleanup(
                &state,
                &self.identity,
                module,
                operation,
                preparation_id.as_ref(),
            ) && !owned_abort_capture(
                &state,
                &self.identity,
                module,
                operation,
                control,
                abort_capture_payload,
            ) && let Some(error) = known_uncertainty(&state, &self.identity)
            {
                return Err(error);
            }
            control.check()?;
            if state.closed {
                return Err(before(closed()));
            }
            if state.data.len() < self.owner.0.queue_capacity {
                let admission_sequence = state.next_call_sequence;
                state.next_call_sequence =
                    state.next_call_sequence.checked_add(1).ok_or_else(|| {
                        PortError::InvalidResponse {
                            message: "Connection call admission sequence exhausted.".into(),
                        }
                    })?;
                state.calls.insert(
                    message_id.clone(),
                    CallRecord {
                        identity: identity.clone(),
                        effect,
                        request_line: request_line.clone(),
                        phase: DispatchPhase::Queued,
                        response: None,
                        cancelled_before_response: false,
                        invalid_response: None,
                        admission_sequence,
                        module,
                        operation,
                        preparation_id: preparation_id.clone(),
                        admission_rejected: None,
                        response_control: control.clone(),
                        abort_capture_payload,
                    },
                );
                state.data.push_back(Packet {
                    pending: Some(message_id.clone()),
                    line: request_line,
                });
                drop(state);
                self.owner.0.wake.notify();
                break;
            }
            drop(state);
            self.owner.0.wake.wait(generation, control.deadline);
        }
        Ok(PendingExchange {
            owner: self.owner.clone(),
            identity,
            message_id,
        })
    }
}
/// A single-use pending call already owned by the connection. Dropping the
/// waiter never replays a dispatched effect or erases its responsibility record.
pub(crate) struct PendingExchange {
    owner: ConnectionOwner,
    identity: ExecutionIdentity,
    message_id: JsString,
}
impl PendingExchange {
    pub(crate) fn is_ready(&self) -> bool {
        let state = self.owner.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed
            || state.calls.get(&self.message_id).is_some_and(|record| {
                record.admission_rejected.is_some()
                    || record.response.is_some()
                    || record.phase == DispatchPhase::InvalidReply
            })
    }
    pub(crate) fn wait(self, control: &CallControl) -> PortResult<HostExchangeResponse> {
        let Self {
            owner,
            identity,
            message_id,
        } = self;
        owner.0.wake_registered(control);
        let mut reconciliation_deadline = None;
        loop {
            let generation = owner.0.wake.generation();
            let mut state = owner.0.state.lock().unwrap_or_else(|p| p.into_inner());
            let record = state
                .calls
                .get(&message_id)
                .expect("pending call owned until reply or retirement");
            if let Some(error) = &record.admission_rejected {
                let error = error.clone();
                state.calls.remove(&message_id);
                drop(state);
                owner.0.wake.notify();
                return Err(error);
            }
            if let Some(response) = &record.response {
                let boundary = response.clone();
                let cancelled_before_response = record.cancelled_before_response;
                drop(state);
                // Explicit unknown never becomes an ordinary retryable host error.
                if boundary
                    .value()
                    .get("outcome")
                    .and_then(JsValue::as_string)
                    .is_some_and(|outcome| outcome.equals_str("result_unknown"))
                {
                    return Err(PortError::AcceptedUnknown {
                        identity: Box::new(identity),
                        last_applied_sequence: boundary
                            .value()
                            .get("lastAppliedSequence")
                            .and_then(JsValue::as_number)
                            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0),
                    });
                }
                return Ok(HostExchangeResponse {
                    identity,
                    boundary,
                    cancelled_before_response,
                });
            }
            let dispatched = record.phase != DispatchPhase::Queued;
            let invalid = record.phase == DispatchPhase::InvalidReply;
            let cancellation = control.check().err();
            let hard_stopped = matches!(
                cancellation,
                Some(PortError::Cancelled {
                    cause: pilotdeck_agent::ports::CancelCause::OwnerRetired
                        | pilotdeck_agent::ports::CancelCause::TransportClosed,
                })
            );
            if dispatched && cancellation.is_some() {
                // Reconcile only this already-dispatched identity. Cancellation
                // cannot prove its outcome; a later valid ACK can settle it.
                // The bound is monotonic and never extended by duplicate wakes.
                reconciliation_deadline.get_or_insert_with(|| {
                    let grace = std::time::Duration::from_secs(5);
                    // An expired execution deadline stops new dispatch, but
                    // cannot also expire reconciliation of an already sent
                    // call. Let its original ACK settle within the same fixed
                    // cancellation budget, as for an explicit user abort.
                    control
                        .cancellation
                        .reconciliation_deadline(grace)
                        .unwrap_or_else(|| std::time::Instant::now() + grace)
                });
            }
            let reconciliation_expired = reconciliation_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline);
            if invalid
                || state.closed
                || hard_stopped
                || reconciliation_expired
                || (cancellation.is_some() && !dispatched)
            {
                if !dispatched {
                    // Removing an undispatched record prevents the writer sending
                    // a packet it already dequeued but has not marked dispatched.
                    state.calls.remove(&message_id);
                    state
                        .data
                        .retain(|packet| packet.pending.as_ref() != Some(&message_id));
                }
                if dispatched && !invalid {
                    state
                        .calls
                        .get_mut(&message_id)
                        .expect("accepted responsibility retained")
                        .phase = DispatchPhase::Unresolved;
                }
                drop(state);
                owner.0.wake.notify();
                if dispatched {
                    return Err(unknown(&identity));
                }
                return Err(cancellation.unwrap_or_else(|| before(closed())));
            }
            drop(state);
            owner
                .0
                .wake
                .wait(generation, reconciliation_deadline.or(control.deadline));
        }
    }
}
fn same_execution(a: &ExecutionIdentity, b: &ExecutionIdentity) -> bool {
    a.run_id == b.run_id
        && a.operation_id == b.operation_id
        && a.session_id == b.session_id
        && a.turn_id == b.turn_id
        && a.module_instance_id == b.module_instance_id
        && a.connection_generation == b.connection_generation
}
/// Only the close of a handle already carried by an accepted pull or a known
/// successful prepare in this exact execution may contain an uncertain stream.
/// No method-name-only exemption, arbitrary handle, or repeated close is admitted.
fn owned_containment_cleanup(
    state: &State,
    identity: &ExecutionIdentity,
    module: &str,
    operation: &str,
    preparation_id: Option<&JsString>,
) -> bool {
    if module != "model" || operation != "close_stream" {
        return false;
    }
    let Some(preparation_id) = preparation_id else {
        return false;
    };
    let matching = |call: &&CallRecord| {
        same_execution(&call.identity, identity)
            && call.module == "model"
            && call.preparation_id.as_ref() == Some(preparation_id)
    };
    let mut owned = false;
    for call in state.calls.values().filter(matching) {
        if call.operation == "close_stream"
            && call.identity.request_id != identity.request_id
            && call.admission_rejected.is_none()
        {
            return false;
        }
        if call.operation == "stream_next" && call.phase != DispatchPhase::Queued {
            owned = true;
        }
        if call.operation == "prepare"
            && call.phase == DispatchPhase::Replied
            && call.response.as_ref().is_some_and(|r| {
                r.value().get("ok") == Some(&JsValue::Bool(true))
                    && !r
                        .value()
                        .get("outcome")
                        .and_then(JsValue::as_string)
                        .is_some_and(|v| v.equals_str("result_unknown"))
            })
        {
            owned = true;
        }
    }
    owned
}
// A user-aborted model/tool call permits exactly one fresh bounded memory
// capture. The original uncertainty and any new capture receipt remain owned.
fn owned_abort_capture(
    state: &State,
    identity: &ExecutionIdentity,
    module: &str,
    operation: &str,
    control: &CallControl,
    matching_payload: bool,
) -> bool {
    use pilotdeck_agent::ports::CancelCause;
    if module != "context"
        || operation != "capture_turn"
        || !matching_payload
        || control.check().is_err()
        || !control.deadline.is_some_and(|deadline| {
            deadline <= std::time::Instant::now() + std::time::Duration::from_secs(5)
        })
    {
        return false;
    }
    let Some(PortError::AcceptedUnknown {
        identity: original, ..
    }) = known_uncertainty(state, identity)
    else {
        return false;
    };
    let mut eligible = false;
    for call in state
        .calls
        .values()
        .filter(|call| same_execution(&call.identity, identity))
    {
        if call.module == "context"
            && call.operation == "capture_turn"
            && call.identity.request_id != identity.request_id
            && call.admission_rejected.is_none()
        {
            return false;
        }
        if call.identity == *original {
            eligible = call
                .response_control
                .cancellation
                .hard_stop_cause()
                .is_none()
                && matches!(
                    (call.module, call.operation),
                    ("model", "stream_next" | "stream")
                        | ("capability", "execute_batch" | "plan_todo")
                )
                && matches!(
                    call.response_control.check(),
                    Err(PortError::Cancelled {
                        cause: CancelCause::Requested(_) | CancelCause::Deadline,
                    })
                );
        }
    }
    eligible
}
fn known_uncertainty(state: &State, identity: &ExecutionIdentity) -> Option<PortError> {
    state
        .calls
        .values()
        .filter(|call| {
            let same = same_execution(&call.identity, identity);
            same && (matches!(
                call.phase,
                DispatchPhase::Unresolved | DispatchPhase::InvalidReply
            ) || call.response.as_ref().is_some_and(|response| {
                response
                    .value()
                    .get("outcome")
                    .and_then(JsValue::as_string)
                    .is_some_and(|outcome| outcome.equals_str("result_unknown"))
            }))
        })
        .min_by_key(|call| call.admission_sequence)
        .map(|call| PortError::AcceptedUnknown {
            identity: Box::new(call.identity.clone()),
            last_applied_sequence: call
                .response
                .as_ref()
                .and_then(|response| response.value().get("lastAppliedSequence"))
                .and_then(JsValue::as_number)
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0),
        })
}
impl HostExchange for BoundExchange {
    fn exchange(
        &mut self,
        control: &CallControl,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        self.start(control, module, operation, effect, payload)?
            .wait(control)
    }
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use pilotdeck_agent::ports::{CancelCause, Cancellation};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Recorder {
        bytes: Vec<u8>,
        sent: mpsc::Sender<Vec<u8>>,
    }
    impl Write for Recorder {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.sent.send(std::mem::take(&mut self.bytes)).unwrap();
            Ok(())
        }
    }
    fn check_expired_wait(acknowledged: bool) {
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "pilotdeck-agent-loop".into(),
                module_instance_id: "instance".into(),
                connection_generation: "generation".into(),
                capabilities_version: "1".into(),
            },
            4,
        );
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let identity = ExecutionIdentity {
            session_id: "s".into(),
            turn_id: "t".into(),
            run_id: "r".into(),
            operation_id: "o".into(),
            request_id: "q".into(),
            idempotency_key: Some("key".into()),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
        };
        let mut exchange = owner.bound_exchange(identity).unwrap();
        let pending = exchange
            .start(
                &control,
                "event",
                "dispatch",
                CallEffect::ReadOnly,
                JsObject::new(),
            )
            .unwrap();
        let (sent, received) = mpsc::channel();
        let writer_owner = owner.clone();
        let writer = std::thread::spawn(move || {
            writer_owner.drive_writer(Recorder {
                bytes: Vec::new(),
                sent,
            })
        });
        let wire = received.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = pilotdeck_protocol::parse_json(std::str::from_utf8(&wire).unwrap()).unwrap();
        let id = request
            .get("messageId")
            .unwrap()
            .as_string()
            .unwrap()
            .clone();
        let retained = owner.call_record(&id).unwrap();
        assert_ne!(retained.phase, DispatchPhase::Queued);
        let expired = Instant::now() - Duration::from_secs(1);
        assert!(
            control.cancellation.cancel_with_reconciliation_deadline(
                CancelCause::Requested("finish".into()),
                expired
            )
        );
        if acknowledged {
            let mut reply = JsObject::new();
            reply.insert("kind".into(), "response".into());
            reply.insert("messageId".into(), "ack".into());
            reply.insert("inReplyTo".into(), JsValue::String(id.clone()));
            reply.insert("ok".into(), JsValue::Bool(true));
            reply.insert("payload".into(), JsValue::Object(JsObject::new()));
            assert_eq!(
                owner.receive_response(&JsValue::Object(reply)),
                ResponseDisposition::Completed
            );
        }
        // The already-expired shared bound cannot become a fresh five seconds
        // when this waiter first begins observing the accepted call.
        let started = Instant::now();
        let result = pending.wait(&control);
        let elapsed = started.elapsed();
        if !acknowledged {
            // Even a new healthy control cannot resume the same execution
            // after a previous waiter exhausted its accepted-call bound.
            let next = exchange.start(
                &CallControl {
                    cancellation: Cancellation::default(),
                    deadline: None,
                },
                "model",
                "prepare",
                CallEffect::SideEffect,
                JsObject::new(),
            );
            assert!(
                matches!(next, Err(PortError::AcceptedUnknown { identity, .. }) if *identity == retained.identity)
            );
            assert_eq!(
                owner.call_record(&id).unwrap().phase,
                DispatchPhase::Unresolved
            );
        }
        owner.close();
        assert!(writer.join().unwrap().is_ok());
        assert!(
            elapsed < Duration::from_secs(1),
            "late waiter extended bound: {elapsed:?}"
        );
        if acknowledged {
            assert_eq!(result.unwrap().identity, retained.identity);
        } else {
            let Err(PortError::AcceptedUnknown {
                identity,
                last_applied_sequence,
            }) = result
            else {
                panic!("accepted call lost its unknown classification")
            };
            assert_eq!(*identity, retained.identity);
            assert_eq!(last_applied_sequence, None);
        }
        assert_eq!(owner.call_record(&id).unwrap().identity, retained.identity);
        assert!(received.try_recv().is_err(), "call was replayed");
    }
    #[test]
    fn delayed_waiter_uses_expired_owner_bound_and_retains_accepted_identity() {
        check_expired_wait(false);
    }
    #[test]
    fn already_known_ack_wins_over_expired_reconciliation_bound() {
        check_expired_wait(true);
    }

    #[test]
    fn reply_reader_freezes_cancel_ack_order_before_a_late_waiter() {
        for cancel_first in [false, true] {
            for ok in [false, true] {
                let owner = ConnectionOwner::new(
                    SidecarBinding {
                        module_id: "pilotdeck-agent-loop".into(),
                        module_instance_id: "instance".into(),
                        connection_generation: "generation".into(),
                        capabilities_version: "1".into(),
                    },
                    4,
                );
                let control = CallControl {
                    cancellation: Cancellation::default(),
                    deadline: None,
                };
                let identity = ExecutionIdentity {
                    session_id: "s".into(),
                    turn_id: "t".into(),
                    run_id: "r".into(),
                    operation_id: "o".into(),
                    request_id: "q".into(),
                    idempotency_key: None,
                    module_instance_id: "instance".into(),
                    connection_generation: "generation".into(),
                };
                let pending = owner
                    .bound_exchange(identity)
                    .unwrap()
                    .start(
                        &control,
                        "capability",
                        "execute_batch",
                        CallEffect::SideEffect,
                        JsObject::new(),
                    )
                    .unwrap();
                let (sent, received) = mpsc::channel();
                let writer_owner = owner.clone();
                let writer = std::thread::spawn(move || {
                    writer_owner.drive_writer(Recorder {
                        bytes: Vec::new(),
                        sent,
                    })
                });
                let wire = received.recv_timeout(Duration::from_secs(2)).unwrap();
                let request =
                    pilotdeck_protocol::parse_json(std::str::from_utf8(&wire).unwrap()).unwrap();
                let id = request.get("messageId").unwrap().as_string().unwrap();
                let cancel = || {
                    assert!(
                        control
                            .cancellation
                            .cancel(CancelCause::Requested("stop".into()))
                    );
                };
                if cancel_first {
                    cancel();
                }
                let mut reply = JsObject::new();
                reply.insert("kind".into(), "response".into());
                reply.insert("messageId".into(), "ack".into());
                reply.insert("inReplyTo".into(), JsValue::String(id.clone()));
                reply.insert("ok".into(), JsValue::Bool(ok));
                let reply = JsValue::Object(reply);
                assert_eq!(
                    owner.receive_response(&reply),
                    ResponseDisposition::Completed
                );
                if !cancel_first {
                    cancel();
                }
                // A later duplicate and an already-cancelled worker cannot
                // change the frozen winner for either success or failure ACK.
                assert_eq!(
                    owner.receive_response(&reply),
                    ResponseDisposition::Duplicate
                );
                assert!(control.check().is_err());
                let response = pending.wait(&control).unwrap();
                assert_eq!(response.cancelled_before_response, cancel_first);
                assert_eq!(response.boundary.value(), &reply);
                assert_eq!(response.identity, owner.call_record(id).unwrap().identity);
                owner.close();
                assert!(writer.join().unwrap().is_ok());
            }
        }
    }

    #[test]
    fn reader_cached_unknown_fences_new_effect_before_notification_waiter_observes_it() {
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "pilotdeck-agent-loop".into(),
                module_instance_id: "instance".into(),
                connection_generation: "generation".into(),
                capabilities_version: "1".into(),
            },
            4,
        );
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let identity = ExecutionIdentity {
            session_id: "s".into(),
            turn_id: "t".into(),
            run_id: "r".into(),
            operation_id: "o".into(),
            request_id: "q".into(),
            idempotency_key: Some("key".into()),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
        };
        let mut exchange = owner.bound_exchange(identity).unwrap();
        let pending = exchange
            .start(
                &control,
                "event",
                "emit",
                CallEffect::SideEffect,
                JsObject::new(),
            )
            .unwrap();
        let (sent, received) = mpsc::channel();
        let writer_owner = owner.clone();
        let writer = std::thread::spawn(move || {
            writer_owner.drive_writer(Recorder {
                bytes: Vec::new(),
                sent,
            })
        });
        let wire = received.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = pilotdeck_protocol::parse_json(std::str::from_utf8(&wire).unwrap()).unwrap();
        let id = request
            .get("messageId")
            .unwrap()
            .as_string()
            .unwrap()
            .clone();
        let retained = owner.call_record(&id).unwrap();
        let mut reply = JsObject::new();
        reply.insert("kind".into(), "response".into());
        reply.insert("messageId".into(), "ack".into());
        reply.insert("inReplyTo".into(), JsValue::String(id.clone()));
        reply.insert("ok".into(), JsValue::Bool(false));
        reply.insert("outcome".into(), "result_unknown".into());
        reply.insert("lastAppliedSequence".into(), JsValue::Number(19.0));
        assert_eq!(
            owner.receive_response(&JsValue::Object(reply)),
            ResponseDisposition::Completed
        );
        // Do not call pending.wait yet: the independent reader receipt itself
        // must fence model preparation and terminal capture admission.
        for (module, operation) in [("model", "prepare"), ("context", "capture_turn")] {
            assert!(
                matches!(exchange.start(&control, module, operation, CallEffect::SideEffect, JsObject::new()), Err(PortError::AcceptedUnknown { identity, last_applied_sequence: Some(19.0) }) if *identity == retained.identity)
            );
        }
        assert_eq!(owner.release_known_calls(&retained.identity), 0);
        assert!(
            received.try_recv().is_err(),
            "new effect reached writer after cached Unknown"
        );
        assert!(
            matches!(pending.wait(&control), Err(PortError::AcceptedUnknown { identity, last_applied_sequence: Some(19.0) }) if *identity == retained.identity)
        );
        // A method name or arbitrary handle is not cleanup authority.
        for preparation in [None, Some("foreign-handle")] {
            let mut payload = JsObject::new();
            if let Some(id) = preparation {
                payload.insert("preparationId".into(), id.into());
            }
            assert!(
                matches!(exchange.start(&control, "model", "close_stream", CallEffect::Advance, payload),
                Err(PortError::AcceptedUnknown { identity, .. }) if *identity == retained.identity)
            );
        }
        owner.close();
        assert!(writer.join().unwrap().is_ok());
        assert!(received.try_recv().is_err(), "accepted call was replayed");
    }
    fn fixture() -> (ConnectionOwner, CallControl, BoundExchange) {
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "pilotdeck-agent-loop".into(),
                module_instance_id: "instance".into(),
                connection_generation: "generation".into(),
                capabilities_version: "1".into(),
            },
            8,
        );
        let identity = ExecutionIdentity {
            session_id: "s".into(),
            turn_id: "t".into(),
            run_id: "r".into(),
            operation_id: "o".into(),
            request_id: "q".into(),
            idempotency_key: Some("key".into()),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
        };
        let exchange = owner.bound_exchange(identity).unwrap();
        (
            owner,
            CallControl {
                cancellation: Cancellation::default(),
                deadline: None,
            },
            exchange,
        )
    }
    fn wire_id(wire: &[u8]) -> JsString {
        pilotdeck_protocol::parse_json(std::str::from_utf8(wire).unwrap())
            .unwrap()
            .get("messageId")
            .unwrap()
            .as_string()
            .unwrap()
            .clone()
    }
    fn reply(owner: &ConnectionOwner, id: &JsString, uncertain: bool) {
        let mut value = JsObject::new();
        value.insert("kind".into(), "response".into());
        value.insert("messageId".into(), "ack".into());
        value.insert("inReplyTo".into(), JsValue::String(id.clone()));
        value.insert("ok".into(), JsValue::Bool(!uncertain));
        if uncertain {
            value.insert("outcome".into(), "result_unknown".into());
            value.insert("lastAppliedSequence".into(), JsValue::Number(19.0));
        }
        assert_eq!(
            owner.receive_response(&JsValue::Object(value)),
            ResponseDisposition::Completed
        );
    }
    struct GatedRecorder {
        recorder: Recorder,
        advance: mpsc::Receiver<()>,
    }
    impl Write for GatedRecorder {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.recorder.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.recorder.flush()?;
            self.advance
                .recv_timeout(Duration::from_secs(2))
                .map_err(io::Error::other)
        }
    }
    #[test]
    fn writer_rejects_already_queued_effect_with_prior_unknown_identity() {
        let (owner, control, mut exchange) = fixture();
        let prior = exchange
            .start(
                &control,
                "event",
                "emit",
                CallEffect::SideEffect,
                JsObject::new(),
            )
            .unwrap();
        let later = exchange
            .start(
                &control,
                "model",
                "prepare",
                CallEffect::SideEffect,
                JsObject::new(),
            )
            .unwrap();
        let later_id = later.message_id.clone();
        let (sent, received) = mpsc::channel();
        let (advance, paused) = mpsc::channel();
        let writer_owner = owner.clone();
        let writer = std::thread::spawn(move || {
            writer_owner.drive_writer(GatedRecorder {
                recorder: Recorder {
                    bytes: Vec::new(),
                    sent,
                },
                advance: paused,
            })
        });
        let id = wire_id(&received.recv_timeout(Duration::from_secs(2)).unwrap());
        let accepted = owner.call_record(&id).unwrap().identity;
        reply(&owner, &id, true);
        advance.send(()).unwrap();
        assert!(
            matches!(later.wait(&control), Err(PortError::AcceptedUnknown { identity, last_applied_sequence: Some(19.0) }) if *identity == accepted)
        );
        assert!(
            owner.call_record(&later_id).is_none(),
            "undispatched rejection was retained as accepted"
        );
        assert!(
            matches!(prior.wait(&control), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == accepted)
        );
        assert!(
            received.try_recv().is_err(),
            "queued model bytes advanced after cached Unknown"
        );
        owner.close();
        assert!(writer.join().unwrap().is_ok());
    }
    #[test]
    fn cleanup_requires_owned_handle_and_exact_execution_and_never_replays_close() {
        let (owner, control, mut exchange) = fixture();
        let mut payload = JsObject::new();
        payload.insert("preparationId".into(), "owned-handle".into());
        let prepared = exchange
            .start(
                &control,
                "model",
                "prepare",
                CallEffect::SideEffect,
                payload.clone(),
            )
            .unwrap();
        let (sent, received) = mpsc::channel();
        let writer_owner = owner.clone();
        let writer = std::thread::spawn(move || {
            writer_owner.drive_writer(Recorder {
                bytes: Vec::new(),
                sent,
            })
        });
        let prepare_id = wire_id(&received.recv_timeout(Duration::from_secs(2)).unwrap());
        reply(&owner, &prepare_id, false);
        prepared.wait(&control).unwrap();
        let notice = exchange
            .start(
                &control,
                "event",
                "emit",
                CallEffect::SideEffect,
                JsObject::new(),
            )
            .unwrap();
        let notice_id = wire_id(&received.recv_timeout(Duration::from_secs(2)).unwrap());
        let original = owner.call_record(&notice_id).unwrap().identity;
        reply(&owner, &notice_id, true);
        let mut foreign_identity = exchange.identity.clone();
        foreign_identity.turn_id = "foreign-turn".into();
        let mut foreign = owner.bound_exchange(foreign_identity).unwrap();
        // A different turn's Unknown must not block this turn's ordinary call.
        let unrelated = foreign
            .start(
                &control,
                "context",
                "capture_turn",
                CallEffect::SideEffect,
                JsObject::new(),
            )
            .unwrap();
        let unrelated_id = wire_id(&received.recv_timeout(Duration::from_secs(2)).unwrap());
        reply(&owner, &unrelated_id, true);
        assert!(
            matches!(foreign.start(&control, "model", "close_stream", CallEffect::Advance, payload.clone()),
            Err(PortError::AcceptedUnknown { identity, .. }) if identity.turn_id.equals_str("foreign-turn"))
        );
        let mut wrong = JsObject::new();
        wrong.insert("preparationId".into(), "foreign-handle".into());
        assert!(
            matches!(exchange.start(&control, "model", "close_stream", CallEffect::Advance, wrong),
            Err(PortError::AcceptedUnknown { identity, .. }) if *identity == original)
        );
        let close = exchange
            .start(
                &control,
                "model",
                "close_stream",
                CallEffect::Advance,
                payload.clone(),
            )
            .unwrap();
        let close_id = wire_id(&received.recv_timeout(Duration::from_secs(2)).unwrap());
        reply(&owner, &close_id, false);
        close.wait(&control).unwrap();
        assert!(
            matches!(exchange.start(&control, "model", "close_stream", CallEffect::Advance, payload),
            Err(PortError::AcceptedUnknown { identity, .. }) if *identity == original)
        );
        assert!(matches!(
            notice.wait(&control),
            Err(PortError::AcceptedUnknown { .. })
        ));
        assert!(matches!(
            unrelated.wait(&control),
            Err(PortError::AcceptedUnknown { .. })
        ));
        owner.close();
        assert!(writer.join().unwrap().is_ok());
        assert!(
            received.try_recv().is_err(),
            "cleanup replay or foreign close escaped"
        );
    }
}
