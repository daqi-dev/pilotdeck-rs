//! Concurrent Module2.0 execution service. Factory owns AgentLoop policy;
//! this owner owns immutable acceptance, streams, controls and bounded shutdown.
//! It does not provide a fake factory, a Date.parse approximation, or persistence.
use crate::{HostExchange, HostExchangeResponse, stdio::ConnectionOwner};
use pilotdeck_agent::ports::{
    CallControl, CancelCause, Cancellation, ExecutionIdentity, PortError, PortResult,
};
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_protocol::wire::{EnvelopeRef, MessageRef, RequestBodyRef, RequestRef};
use pilotdeck_protocol::{JsObject, JsString, JsValue, MODULE_PROTOCOL_VERSION, parse_json};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

/// A process-wide TCP limit: deadline timers own one OS thread each.
const TCP_TIMER_LIMIT: usize = 32;
#[derive(Default)]
struct TimerBudget(AtomicUsize);
struct TimerLease(Arc<TimerBudget>);
impl TimerBudget {
    fn acquire(self: &Arc<Self>) -> Option<TimerLease> {
        let mut current = self.0.load(Ordering::SeqCst);
        loop {
            if current >= TCP_TIMER_LIMIT {
                return None;
            }
            match self.0.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Some(TimerLease(self.clone())),
                Err(observed) => current = observed,
            }
        }
    }
}
impl Drop for TimerLease {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}
static TCP_TIMERS: std::sync::LazyLock<Arc<TimerBudget>> =
    std::sync::LazyLock::new(|| Arc::new(TimerBudget::default()));

fn spawn_deadline_timer(
    deadline: Instant,
    cancellation: Cancellation,
    lease: Option<TimerLease>,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<thread::JoinHandle<()>>,
) -> io::Result<thread::JoinHandle<()>> {
    spawn(Box::new(move || {
        let _lease = lease;
        if matches!(
            cancellation.wait_until(deadline),
            Some(CancelCause::Deadline)
        ) {
            cancellation.cancel(CancelCause::Deadline);
        }
    }))
}

fn reserve_bytes(counter: &AtomicUsize, bytes: usize, limit: usize) -> Result<(), ()> {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        let next = current
            .checked_add(bytes)
            .filter(|next| *next <= limit)
            .ok_or(())?;
        match counter.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

const TCP_INPUT_BYTES: usize = 8 * 1024 * 1024;
const TCP_LEDGER_BYTES: usize = 64 * 1024 * 1024;
struct IncomingQuota {
    used: Arc<AtomicUsize>,
    held: usize,
}
impl IncomingQuota {
    fn new(used: Arc<AtomicUsize>) -> Self {
        Self { used, held: 0 }
    }
    fn acquire(&mut self, bytes: usize) -> io::Result<()> {
        reserve_bytes(&self.used, bytes, TCP_INPUT_BYTES)
            .map_err(|_| io::Error::other("TCP input budget exceeded."))?;
        self.held += bytes;
        Ok(())
    }
}
impl Drop for IncomingQuota {
    fn drop(&mut self) {
        self.used.fetch_sub(self.held, Ordering::SeqCst);
    }
}
fn read_tcp_line(
    input: &mut impl BufRead,
    bytes: &mut Vec<u8>,
    quota: &mut IncomingQuota,
) -> io::Result<usize> {
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(bytes.len());
        }
        let end = buffer.iter().position(|byte| *byte == b'\n').map(|n| n + 1);
        let length = end.unwrap_or(buffer.len());
        quota.acquire(length)?;
        bytes.extend_from_slice(&buffer[..length]);
        input.consume(length);
        if end.is_some() {
            return Ok(bytes.len());
        }
    }
}
/// Conservative retained representation charge; the multiplier at admission
/// also covers the request's queue/context clones. Traversal is iterative.
pub(crate) fn retained_cost(value: &JsValue) -> Option<usize> {
    let mut total = 0usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        total = total.checked_add(128)?;
        match value {
            JsValue::String(text) => {
                total = total.checked_add(text.units().len().checked_mul(2)?)?
            }
            JsValue::Array(items) => {
                if items.len() > TCP_LEDGER_BYTES / 128 {
                    return None;
                }
                pending.extend(items.iter());
            }
            JsValue::Object(fields) => {
                for (key, value) in fields.own_entries() {
                    total = total
                        .checked_add(128)?
                        .checked_add(key.units().len().checked_mul(2)?)?;
                    if total > TCP_LEDGER_BYTES {
                        return None;
                    }
                    pending.push(value);
                }
            }
            _ => {}
        }
        if total > TCP_LEDGER_BYTES {
            return None;
        }
    }
    Some(total)
}

#[derive(Debug, Default)]
pub(crate) struct TransportBudget(AtomicUsize);
#[derive(Debug)]
pub(crate) struct TransportLease {
    budget: Arc<TransportBudget>,
    bytes: usize,
}
impl TransportBudget {
    pub(crate) fn acquire(self: &Arc<Self>, value: &JsValue) -> io::Result<Arc<TransportLease>> {
        let bytes = retained_cost(value)
            .and_then(|cost| cost.checked_mul(4))
            .ok_or_else(|| io::Error::other("TCP transport memory budget exceeded."))?;
        reserve_bytes(&self.0, bytes, TCP_LEDGER_BYTES)
            .map_err(|_| io::Error::other("TCP transport memory budget exceeded."))?;
        Ok(Arc::new(TransportLease {
            budget: self.clone(),
            bytes,
        }))
    }
}
impl Drop for TransportLease {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// Implementations must declare their actual original-engine date profile.
/// Strings with an unsupported engine-valid form must not silently mean no deadline.
pub trait ExecutionClock: Send + Sync + 'static {
    fn now_epoch_ms(&self) -> f64;
    fn now_iso(&self) -> JsString;
    fn parse_date(&self, input: &JsString) -> Result<Option<f64>, JsString>;
}
#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    pub message_id: JsString,
    pub run_id: JsString,
    pub operation_id: JsString,
    pub request_id: JsString,
    /// Full validated wire request, including payload and deadline presence.
    pub boundary: JsValue,
}
#[derive(Debug, Clone)]
pub struct ExecutionTerminal {
    pub outcome: ModuleOutcome,
    /// Original terminal payload (result/messages/optional checkpoint projections).
    pub payload: JsObject,
    pub error: Option<JsObject>,
}
/// A production implementation must run the real Rust AgentLoop. This component
/// only establishes ownership; implementing this trait is not an acceptance gate.
pub trait ExecutionFactory: Send + Sync + 'static {
    fn execute(
        &self,
        request: &ExecuteRequest,
        context: &mut ExecutionContext,
    ) -> PortResult<ExecutionTerminal>;
}
pub struct ExecutionContext {
    owner: ConnectionOwner,
    request: ExecuteRequest,
    stream_id: JsString,
    sequence: Arc<Mutex<f64>>,
    identities: Mutex<Vec<ExecutionIdentity>>,
    pub control: CallControl,
}
impl ExecutionContext {
    pub fn binding(&self) -> &pilotdeck_protocol::admission::SidecarBinding {
        self.owner.binding()
    }
    /// Factory must derive session/turn from the original execute payload rules.
    /// Binding/run/operation/request identities are issued by this owner.
    pub fn identity(&self, session_id: JsString, turn_id: JsString) -> ExecutionIdentity {
        ExecutionIdentity {
            session_id,
            turn_id,
            run_id: self.request.run_id.clone(),
            operation_id: self.request.operation_id.clone(),
            request_id: self.request.request_id.clone(),
            idempotency_key: self
                .request
                .boundary
                .get("idempotencyKey")
                .and_then(JsValue::as_string)
                .cloned(),
            module_instance_id: self.owner.binding().module_instance_id.clone(),
            connection_generation: self.owner.binding().connection_generation.clone(),
        }
    }
    pub fn exchange(&self, identity: ExecutionIdentity) -> PortResult<crate::stdio::BoundExchange> {
        if identity.run_id != self.request.run_id
            || identity.operation_id != self.request.operation_id
            || identity.request_id != self.request.request_id
        {
            return Err(PortError::InvalidResponse {
                message: "Execution identity belongs to another operation.".into(),
            });
        }
        let exchange = self.owner.bound_exchange(identity.clone())?;
        let mut identities = self.identities.lock().unwrap_or_else(|p| p.into_inner());
        if !identities.contains(&identity) {
            identities.push(identity);
        }
        Ok(exchange)
    }
    pub fn call(
        &self,
        identity: ExecutionIdentity,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        self.exchange(identity)?
            .exchange(&self.control, module, operation, effect, payload)
    }
    /// Cached accepted-call uncertainty belongs to this execution, even if a
    /// collaborator ignored a cleanup error or has not consumed its reply yet.
    pub(crate) fn check_accepted_uncertainty(&self) -> PortResult<()> {
        let identities = self
            .identities
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for identity in identities {
            self.owner.check_known_uncertainty(&identity)?;
        }
        Ok(())
    }
    pub fn emit(&mut self, event_type: JsString, payload: JsObject) -> PortResult<()> {
        self.control.check()?;
        // Commit sequence together with successful queue admission. Holding this
        // per-stream lock prevents retirement allocating a final in that gap;
        // no connection lock spans this bounded/cancellation-aware capacity wait.
        let mut sequence = self.sequence.lock().unwrap_or_else(|p| p.into_inner());
        let event = self.build_event(event_type, payload, None, None, *sequence);
        self.owner.publish(&self.control, &event)?;
        *sequence += 1.0;
        Ok(())
    }
    /// Publication of already accepted provider data and its aborted partial
    /// result has a separate bounded delivery budget. It cannot authorize a new
    /// host effect and still observes connection retirement/output failure.
    pub(crate) fn emit_accepted(
        &mut self,
        event_type: JsString,
        payload: JsObject,
    ) -> PortResult<()> {
        if matches!(
            self.control.cancellation.cause(),
            Some(CancelCause::OwnerRetired | CancelCause::TransportClosed)
        ) || self.control.cancellation.hard_stop_cause().is_some()
        {
            return self.control.check();
        }
        let delivery = CallControl {
            cancellation: Cancellation::default(),
            deadline: Instant::now().checked_add(Duration::from_secs(5)),
        };
        let mut sequence = self.sequence.lock().unwrap_or_else(|p| p.into_inner());
        let event = self.build_event(event_type, payload, None, None, *sequence);
        self.owner.publish(&delivery, &event)?;
        *sequence += 1.0;
        Ok(())
    }
    fn event(
        &self,
        event_type: JsString,
        payload: JsObject,
        outcome: Option<ModuleOutcome>,
        error: Option<JsObject>,
    ) -> JsValue {
        let mut sequence = self.sequence.lock().unwrap_or_else(|p| p.into_inner());
        let event = self.build_event(event_type, payload, outcome, error, *sequence);
        *sequence += 1.0;
        event
    }
    fn build_event(
        &self,
        event_type: JsString,
        payload: JsObject,
        outcome: Option<ModuleOutcome>,
        error: Option<JsObject>,
        sequence: f64,
    ) -> JsValue {
        let mut event = object([
            ("kind", JsValue::from("event")),
            ("messageId", JsValue::String(self.owner.next_id("event"))),
            ("eventType", JsValue::String(event_type)),
            ("streamId", JsValue::String(self.stream_id.clone())),
            ("sequence", JsValue::Number(sequence)),
            ("runId", JsValue::String(self.request.run_id.clone())),
            (
                "operationId",
                JsValue::String(self.request.operation_id.clone()),
            ),
            (
                "requestId",
                JsValue::String(self.request.request_id.clone()),
            ),
            ("final", JsValue::Bool(outcome.is_some())),
            ("payload", JsValue::Object(payload)),
        ]);
        if let Some(outcome) = outcome {
            event.insert("outcome".into(), JsValue::from(outcome_name(outcome)));
        }
        if let Some(error) = error {
            if let Some(JsValue::String(code)) = error.get("code") {
                event.insert("code".into(), JsValue::String(code.clone()));
            }
            event.insert("error".into(), JsValue::Object(error));
        }
        JsValue::Object(event)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationPhase {
    Running,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
    Resolving,
}
#[derive(Debug)]
struct Operation {
    connection_generation: JsString,
    request: ExecuteRequest,
    stream_id: JsString,
    sequence: Arc<Mutex<f64>>,
    cancellation: Cancellation,
    cancel_requested: bool,
    deadline: Option<Instant>,
    timer: Option<thread::JoinHandle<()>>,
    phase: OperationPhase,
    accepted: JsValue,
    terminal: Option<JsValue>,
    outcome: Option<ModuleOutcome>,
    updated_at: JsString,
}
#[derive(Debug)]
struct Work {
    request: ExecuteRequest,
    stream_id: JsString,
    sequence: Arc<Mutex<f64>>,
    control: CallControl,
}
#[derive(Debug, Default)]
struct State {
    retained_bytes: usize,
    operations: HashMap<JsString, Operation>,
    requests: HashMap<JsString, JsString>,
    queues: HashMap<JsString, VecDeque<Work>>,
}
/// Volatile operation state shared by the connections of one TCP sidecar.
/// It confers no permission to replay an accepted execute or move host calls.
#[derive(Clone, Default)]
pub struct SharedExecutionLedger {
    state: Arc<Mutex<State>>,
    decoding: Arc<Mutex<()>>,
}
pub struct ExecutionServer {
    owner: ConnectionOwner,
    state: Arc<Mutex<State>>,
    operation_limit: Option<usize>,
    incoming: Option<Arc<AtomicUsize>>,
    decoding: Option<Arc<Mutex<()>>>,
    ready: Arc<pilotdeck_agent::ports::WakeSignal>,
    factory: Arc<dyn ExecutionFactory>,
    clock: Arc<dyn ExecutionClock>,
    queue_capacity: usize,
}
/// Every serving thread retires its owner and publishes completion even if
/// its body panics. The report still records the panic as an incomplete join.
struct ServeThreadGuard {
    server: Arc<ExecutionServer>,
    done: mpsc::Sender<()>,
}
impl Drop for ServeThreadGuard {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.server.retire()));
        let _ = self.done.send(());
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    pub reader_joined: bool,
    pub writer_joined: bool,
    pub worker_joined: bool,
}
impl ShutdownReport {
    pub fn complete(self) -> bool {
        self.reader_joined && self.writer_joined && self.worker_joined
    }
}
impl ExecutionServer {
    pub fn new(
        owner: ConnectionOwner,
        factory: Arc<dyn ExecutionFactory>,
        clock: Arc<dyn ExecutionClock>,
        queue_capacity: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            state: Arc::new(Mutex::new(State::default())),
            operation_limit: None,
            incoming: None,
            decoding: None,
            ready: Arc::default(),
            factory,
            clock,
            queue_capacity: queue_capacity.max(1),
        })
    }
    /// A TCP connection shares only the ledger. Queues, streams, host calls,
    /// transport cancellation and binding generation remain connection-owned.
    pub fn with_shared_ledger(
        owner: ConnectionOwner,
        factory: Arc<dyn ExecutionFactory>,
        clock: Arc<dyn ExecutionClock>,
        queue_capacity: usize,
        ledger: SharedExecutionLedger,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            state: ledger.state,
            operation_limit: Some(4096),
            incoming: Some(Arc::default()),
            decoding: Some(ledger.decoding),
            ready: Arc::default(),
            factory,
            clock,
            queue_capacity: queue_capacity.max(1),
        })
    }
    /// Called by one reader. No factory work or blocking output occurs here.
    pub fn receive(&self, value: &JsValue) -> io::Result<()> {
        let envelope = match EnvelopeRef::decode(value) {
            Ok(envelope) => envelope,
            Err(error) => return self.send_error(error.code, error.message.into()),
        };
        let MessageRef::Request(request) = envelope.message else {
            if matches!(envelope.message, MessageRef::Response(_)) {
                self.owner.receive_response(value);
            }
            return Ok(());
        };
        match request.body {
            RequestBodyRef::Hello { .. } | RequestBodyRef::Capabilities { .. } => {
                let mut response = self.response(request, true);
                let binding = self.owner.binding();
                for (key, value) in [
                    ("protocolVersion", JsValue::from(MODULE_PROTOCOL_VERSION)),
                    ("moduleId", JsValue::String(binding.module_id.clone())),
                    (
                        "moduleInstanceId",
                        JsValue::String(binding.module_instance_id.clone()),
                    ),
                    (
                        "connectionGeneration",
                        JsValue::String(binding.connection_generation.clone()),
                    ),
                    (
                        "capabilitiesVersion",
                        JsValue::String(binding.capabilities_version.clone()),
                    ),
                ] {
                    response.insert(key.into(), value);
                }
                response.insert(
                    "payload".into(),
                    JsValue::Object(
                        if matches!(request.body, RequestBodyRef::Capabilities { .. }) {
                            self.capabilities()
                        } else {
                            JsObject::new()
                        },
                    ),
                );
                self.owner.send_control(&JsValue::Object(response))
            }
            RequestBodyRef::Execute { identity, .. } => self.accept(
                request,
                ExecuteRequest {
                    message_id: request.message_id.clone(),
                    run_id: identity.run_id.clone(),
                    operation_id: identity.operation_id.clone(),
                    request_id: identity.request_id.clone(),
                    boundary: value.clone(),
                },
            ),
            RequestBodyRef::Cancel {
                run_id,
                operation_id,
                reason,
            } => {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let active = state
                    .operations
                    .get_mut(operation_id)
                    .filter(|op| op.terminal.is_none());
                let matches = active.as_ref().is_some_and(|op| {
                    &op.request.run_id == run_id
                        && request
                            .fields
                            .get("requestId")
                            .filter(|v| !v.is_undefined())
                            .is_none_or(|v| v.as_string() == Some(&op.request.request_id))
                });
                let code = if active.is_some() {
                    "OPERATION_IDENTITY_MISMATCH"
                } else {
                    "UNKNOWN_OPERATION"
                };
                if matches {
                    let op = active.expect("matched active operation");
                    if op.deadline.is_some_and(|d| Instant::now() >= d) {
                        op.cancellation.cancel(CancelCause::Deadline);
                    }
                    op.phase = OperationPhase::CancelRequested;
                    op.cancel_requested = true;
                    op.updated_at = self.clock.now_iso();
                    let mut timeout: Vec<u16> = "timeout:".encode_utf16().collect();
                    timeout.extend_from_slice(run_id.units());
                    let cause = if reason.units() == timeout {
                        CancelCause::Deadline
                    } else {
                        CancelCause::Requested(reason.clone())
                    };
                    op.cancellation.cancel(cause);
                }
                drop(state);
                self.ready.notify();
                let mut response = self.response(request, matches);
                if matches {
                    response.insert(
                        "payload".into(),
                        JsValue::Object(object([
                            ("operationId", JsValue::String(operation_id.clone())),
                            ("cancelled", JsValue::Bool(true)),
                        ])),
                    );
                } else {
                    response.insert("code".into(), JsValue::from(code));
                }
                self.owner.send_control(&JsValue::Object(response))
            }
            RequestBodyRef::Status { request_id } => {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let op = state
                    .requests
                    .get(request_id)
                    .and_then(|id| state.operations.get(id));
                let mut response = self.response(request, op.is_some());
                if let Some(op) = op {
                    let mut snapshot = object([
                        ("runId", JsValue::String(op.request.run_id.clone())),
                        (
                            "operationId",
                            JsValue::String(op.request.operation_id.clone()),
                        ),
                        ("state", JsValue::from(phase_name(op.phase))),
                        (
                            "requestIds",
                            JsValue::Array(vec![JsValue::String(op.request.request_id.clone())]),
                        ),
                        ("cancelRequested", JsValue::Bool(op.cancel_requested)),
                        ("updatedAt", JsValue::String(op.updated_at.clone())),
                    ]);
                    if let Some(outcome) = op.outcome {
                        snapshot.insert("outcome".into(), JsValue::from(outcome_name(outcome)));
                    }
                    response.insert("payload".into(), JsValue::Object(snapshot));
                } else {
                    response.insert("code".into(), JsValue::from("UNKNOWN_REQUEST"));
                }
                drop(state);
                self.owner.send_control(&JsValue::Object(response))
            }
            RequestBodyRef::ModuleCall { .. } => {
                self.reject(request, "UNEXPECTED_MODULE_CALL", true)
            }
            RequestBodyRef::Resume { .. } => self.reject(request, "RESUME_UNSUPPORTED", false),
            RequestBodyRef::Ack { .. } => self.reject(request, "ACK_UNSUPPORTED", false),
        }
    }
    fn accept(&self, request: RequestRef<'_>, input: ExecuteRequest) -> io::Result<()> {
        // Parse before accepting. A restricted parser fails visibly, never drops a deadline.
        let mut deadline_ms: Option<f64> = None;
        for name in ["operationDeadline", "attemptDeadline"] {
            if let Some(JsValue::String(value)) = request.fields.get(name) {
                if value.is_empty() {
                    continue;
                }
                match self.clock.parse_date(value) {
                    Ok(Some(ms)) if ms.is_finite() => {
                        deadline_ms = Some(deadline_ms.map_or(ms, |old| old.min(ms)))
                    }
                    Ok(_) => {}
                    Err(_) => return self.reject(request, "UNSUPPORTED_DEADLINE_PROFILE", true),
                }
            }
        }
        let now = self.clock.now_epoch_ms();
        if !now.is_finite() {
            return self.reject(request, "INVALID_OWNER_CLOCK", true);
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(prior_id) = state.requests.get(&input.request_id) {
            let prior = state
                .operations
                .get(prior_id)
                .expect("registered request operation");
            if self.operation_limit.is_some() && prior.terminal.is_none() {
                drop(state);
                return self.reject(request, "OPERATION_ALREADY_ACTIVE", false);
            }
            if prior.connection_generation != self.owner.binding().connection_generation {
                let code = if prior.terminal.is_none() {
                    "OPERATION_ALREADY_ACTIVE"
                } else {
                    "OPERATION_REQUIRES_RECONCILIATION"
                };
                drop(state);
                return self.reject(request, code, false);
            }
            if prior.request.boundary == input.boundary {
                let cached = prior.terminal.as_ref().unwrap_or(&prior.accepted).clone();
                drop(state);
                return if cached
                    .get("kind")
                    .and_then(JsValue::as_string)
                    .is_some_and(|k| k.equals_str("event"))
                {
                    self.owner.send_cached_stream(&cached)
                } else {
                    self.owner.send_control(&cached)
                };
            }
            drop(state);
            return self.reject(request, "REQUEST_IDENTITY_CONFLICT", false);
        }
        if let Some(prior) = state.operations.get(&input.operation_id) {
            let code = if prior.request.run_id != input.run_id {
                "OPERATION_IDENTITY_MISMATCH"
            } else if prior.terminal.is_none() {
                "OPERATION_ALREADY_ACTIVE"
            } else {
                "OPERATION_REQUIRES_RECONCILIATION"
            };
            drop(state);
            return self.reject(request, code, false);
        }
        if self
            .operation_limit
            .is_some_and(|limit| state.operations.len() >= limit)
        {
            drop(state);
            return self.reject(request, "EXECUTION_LEDGER_FULL", false);
        }
        if state
            .queues
            .get(&self.owner.binding().connection_generation)
            .is_some_and(|queue| queue.len() >= self.queue_capacity)
        {
            drop(state);
            return self.reject(request, "EXECUTION_QUEUE_FULL", false);
        }
        let stream_id = self.owner.next_id("stream");
        let sequence = Arc::new(Mutex::new(0.0));
        let cancellation = Cancellation::default();
        let mut accepted = self.response(request, true);
        accepted.insert("streamId".into(), JsValue::String(stream_id.clone()));
        accepted.insert("cursor".into(), JsValue::Number(0.0));
        let expired = deadline_ms.is_some_and(|ms| ms <= now);
        if expired {
            accepted = self.response(request, false);
            accepted.insert("final".into(), JsValue::Bool(true));
            accepted.insert("outcome".into(), JsValue::from("failed"));
            accepted.insert("code".into(), JsValue::from("DEADLINE_EXCEEDED"));
        }
        // Freeze the earliest deadline once, never from a later host response.
        let deadline = deadline_ms
            .and_then(|ms| Instant::now().checked_add(execution_deadline_delay(ms - now)));
        let timer = if expired {
            None
        } else if let Some(deadline) = deadline {
            let timer_lease = if self.operation_limit.is_some() {
                match TCP_TIMERS.acquire() {
                    Some(lease) => Some(lease),
                    None => {
                        drop(state);
                        return self.reject(request, "EXECUTION_TIMER_LIMIT", false);
                    }
                }
            } else {
                None
            };
            let cancellation = cancellation.clone();
            match spawn_deadline_timer(deadline, cancellation, timer_lease, |job| {
                thread::Builder::new().spawn(job)
            }) {
                Ok(timer) => Some(timer),
                Err(_) => {
                    drop(state);
                    return self.reject(request, "EXECUTION_TIMER_UNAVAILABLE", false);
                }
            }
        } else {
            None
        };
        let accepted = JsValue::Object(accepted);
        if self.operation_limit.is_some() {
            // Account for the retained request plus worker/context clones, and
            // reserve room for a small truthful terminal on budget exhaustion.
            let cost = retained_cost(&input.boundary)
                .and_then(|n| n.checked_mul(8))
                .and_then(|n| n.checked_add(4096));
            let total = cost.and_then(|n| state.retained_bytes.checked_add(n));
            let Some(total) = total.filter(|n| *n <= TCP_LEDGER_BYTES) else {
                // No operation was accepted, and no timer may be left detached.
                cancellation.cancel(CancelCause::OwnerRetired);
                drop(state);
                if let Some(timer) = timer {
                    let _ = timer.join();
                }
                return self.reject(request, "EXECUTION_LEDGER_FULL", false);
            };
            state.retained_bytes = total;
        }
        // Acceptance/final cached before publication, including immediate expiration.
        state
            .requests
            .insert(input.request_id.clone(), input.operation_id.clone());
        state.operations.insert(
            input.operation_id.clone(),
            Operation {
                connection_generation: self.owner.binding().connection_generation.clone(),
                request: input.clone(),
                stream_id: stream_id.clone(),
                sequence: sequence.clone(),
                cancellation: cancellation.clone(),
                cancel_requested: false,
                deadline,
                timer,
                phase: if expired {
                    OperationPhase::Failed
                } else {
                    OperationPhase::Running
                },
                accepted: accepted.clone(),
                terminal: expired.then_some(accepted.clone()),
                outcome: expired.then_some(ModuleOutcome::Failed),
                updated_at: self.clock.now_iso(),
            },
        );
        drop(state);
        if let Err(error) = self.owner.send_control(&accepted) {
            self.retire();
            return Err(error);
        }
        if !expired {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if self.owner.is_closed() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Execution connection closed.",
                ));
            }
            state
                .queues
                .entry(self.owner.binding().connection_generation.clone())
                .or_default()
                .push_back(Work {
                    request: input,
                    stream_id,
                    sequence,
                    control: CallControl {
                        cancellation,
                        deadline,
                    },
                });
            drop(state);
            self.ready.notify();
        }
        Ok(())
    }
    fn worker(&self) {
        loop {
            let generation = self.ready.generation();
            if self.owner.is_closed() {
                return;
            }
            let work = self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .queues
                .get_mut(&self.owner.binding().connection_generation)
                .and_then(VecDeque::pop_front);
            let Some(work) = work else {
                self.ready.wait(generation, None);
                continue;
            };
            let mut context = ExecutionContext {
                owner: self.owner.clone(),
                request: work.request.clone(),
                stream_id: work.stream_id,
                sequence: work.sequence,
                identities: Mutex::new(Vec::new()),
                control: work.control,
            };
            // Catching a worker panic prevents a permanently running operation.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                context.control.check()?;
                self.factory.execute(&work.request, &mut context)
            }));
            let terminal = match result {
                Ok(Ok(terminal)) => terminal,
                Ok(Err(error)) => failure_terminal(error),
                Err(_) => failure_terminal(PortError::InvalidResponse {
                    message: "Execution worker panicked.".into(),
                }),
            };
            if context
                .control
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                context.control.cancellation.cancel(CancelCause::Deadline);
            }
            let mut terminal = if matches!(
                context.control.cancellation.cause(),
                Some(CancelCause::Deadline)
            ) {
                ExecutionTerminal {
                    outcome: ModuleOutcome::ResultUnknown,
                    payload: JsObject::new(),
                    error: Some(object([
                        ("code", JsValue::from("DEADLINE_EXCEEDED")),
                        (
                            "message",
                            JsValue::from("Module execution deadline exceeded."),
                        ),
                    ])),
                }
            } else {
                terminal
            };
            if self.operation_limit.is_some() {
                let mut payload = JsValue::Object(std::mem::take(&mut terminal.payload));
                let mut error = terminal.error.take().map(JsValue::Object);
                let cost = retained_cost(&payload)
                    .and_then(|n| {
                        retained_cost(error.as_ref().unwrap_or(&JsValue::Undefined))
                            .and_then(|e| n.checked_add(e))
                    })
                    .and_then(|n| n.checked_mul(4));
                let admitted = {
                    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(total) = cost
                        .and_then(|n| state.retained_bytes.checked_add(n))
                        .filter(|n| *n <= TCP_LEDGER_BYTES)
                    {
                        state.retained_bytes = total;
                        true
                    } else {
                        false
                    }
                };
                if admitted {
                    if let JsValue::Object(fields) = &mut payload {
                        terminal.payload = std::mem::take(fields);
                    }
                    if let Some(JsValue::Object(fields)) = &mut error {
                        terminal.error = Some(std::mem::take(fields));
                    }
                } else {
                    terminal.outcome = ModuleOutcome::ResultUnknown;
                    terminal.error = Some(object([
                        ("code", "EXECUTION_LEDGER_FULL".into()),
                        (
                            "message",
                            "Execution result exceeded the TCP ledger budget.".into(),
                        ),
                    ]));
                }
            }
            let event_type = format!("agent.execute.{}", outcome_name(terminal.outcome));
            let final_event = context.event(
                event_type.into(),
                terminal.payload,
                Some(terminal.outcome),
                terminal.error,
            );
            // Wake and reap the exact owned timer; completion is already classified.
            context
                .control
                .cancellation
                .cancel(CancelCause::OwnerRetired);
            let timer = self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .operations
                .get_mut(&work.request.operation_id)
                .and_then(|op| op.timer.take());
            if let Some(timer) = timer {
                let _ = timer.join();
            }
            {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let op = state
                    .operations
                    .get_mut(&work.request.operation_id)
                    .expect("owned active operation");
                if op.terminal.is_some() {
                    continue;
                }
                op.outcome = Some(terminal.outcome);
                op.phase = outcome_phase(terminal.outcome);
                op.terminal = Some(final_event.clone());
                op.updated_at = self.clock.now_iso();
            }
            // Main cancellation must not suppress its own terminal. Cleanup has a
            // separately bounded publication budget, never a permission to retry effects.
            let cleanup = CallControl {
                cancellation: Cancellation::default(),
                deadline: Instant::now().checked_add(Duration::from_secs(5)),
            };
            if self.owner.publish(&cleanup, &final_event).is_err() {
                self.retire();
            }
            if terminal.outcome != ModuleOutcome::ResultUnknown {
                for identity in context
                    .identities
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .iter()
                {
                    self.owner.release_known_calls(identity);
                }
            }
        }
    }
    pub fn retire(&self) {
        self.owner.close();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for op in state.operations.values_mut().filter(|op| {
            op.terminal.is_none()
                && op.connection_generation == self.owner.binding().connection_generation
        }) {
            op.cancellation.cancel(CancelCause::TransportClosed);
            op.phase = OperationPhase::Resolving;
            op.outcome = Some(ModuleOutcome::ResultUnknown);
            op.updated_at = self.clock.now_iso();
            let context = ExecutionContext {
                owner: self.owner.clone(),
                request: op.request.clone(),
                stream_id: op.stream_id.clone(),
                sequence: op.sequence.clone(),
                identities: Mutex::new(Vec::new()),
                control: CallControl {
                    cancellation: op.cancellation.clone(),
                    deadline: None,
                },
            };
            op.terminal = Some(context.event(
                "agent.execute.result_unknown".into(),
                JsObject::new(),
                Some(ModuleOutcome::ResultUnknown),
                Some(object([
                    ("code", JsValue::from("RESULT_UNKNOWN")),
                    (
                        "message",
                        JsValue::from("Execution connection retired before a known terminal."),
                    ),
                ])),
            ));
        }
        state
            .queues
            .remove(&self.owner.binding().connection_generation);
        drop(state);
        self.ready.notify();
        let timers: Vec<_> = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state
                .operations
                .values_mut()
                .filter(|op| op.connection_generation == self.owner.binding().connection_generation)
                .filter_map(|op| op.timer.take())
                .collect()
        };
        for timer in timers {
            let _ = timer.join();
        }
    }
    pub fn is_retired(&self) -> bool {
        self.owner.is_closed()
    }
    /// Blocking reader/writer handles are never joined indefinitely. An incomplete
    /// report requires the owning production process to terminate, not reuse this
    /// server as a cleanly retired connection. GUI/embedding owners need an actual
    /// interruptible IO implementation before treating incomplete shutdown as safe.
    pub fn serve(
        self: &Arc<Self>,
        input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
        grace: Duration,
    ) -> ShutdownReport {
        // OS thread creation can fail. No partially spawned owner may be
        // reported as reusable even if Rust's thread::spawn panics.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.serve_inner(input, output, grace)
        })) {
            Ok(report) => report,
            Err(_) => {
                self.retire();
                ShutdownReport {
                    reader_joined: false,
                    writer_joined: false,
                    worker_joined: false,
                }
            }
        }
    }
    fn serve_inner(
        self: &Arc<Self>,
        mut input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
        grace: Duration,
    ) -> ShutdownReport {
        let (done_tx, done_rx) = mpsc::channel();
        let reader_server = self.clone();
        let reader_done = done_tx.clone();
        let reader = thread::spawn(move || {
            let _completion = ServeThreadGuard {
                server: reader_server.clone(),
                done: reader_done,
            };
            while !reader_server.owner.is_closed() {
                // Drop the allocation and its quota together after each frame.
                let mut quota = reader_server.incoming.clone().map(IncomingQuota::new);
                let mut bytes = Vec::new();
                let read = if let Some(quota) = &mut quota {
                    read_tcp_line(&mut input, &mut bytes, quota)
                } else {
                    input.read_until(b'\n', &mut bytes)
                };
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                let line = String::from_utf8_lossy(&bytes);
                if line.chars().all(js_whitespace) {
                    continue;
                }
                // A stalled partial frame spends only its own connection's
                // allowance. Decode/dispatch one bounded frame at a time so
                // 32 simultaneous raw buffers cannot multiply JSON expansion.
                let _decode = reader_server
                    .decoding
                    .as_ref()
                    .map(|gate| gate.lock().unwrap_or_else(|poison| poison.into_inner()));
                let result = match parse_json(&line) {
                    Ok(value) => reader_server.receive(&value),
                    Err(_) => reader_server
                        .send_error("INVALID_JSON", "Module message is not valid JSON.".into()),
                };
                if result.is_err() {
                    break;
                }
            }
        });
        let writer_server = self.clone();
        let writer_done = done_tx.clone();
        let writer = thread::spawn(move || {
            let _completion = ServeThreadGuard {
                server: writer_server.clone(),
                done: writer_done,
            };
            let _ = writer_server.owner.drive_writer(output);
        });
        let worker_server = self.clone();
        let worker = thread::spawn(move || {
            let _completion = ServeThreadGuard {
                server: worker_server.clone(),
                done: done_tx,
            };
            worker_server.worker();
        });
        let _ = done_rx.recv();
        self.retire();
        let until = Instant::now()
            .checked_add(grace)
            .unwrap_or_else(Instant::now);
        while Instant::now() < until
            && !(reader.is_finished() && writer.is_finished() && worker.is_finished())
        {
            thread::sleep(Duration::from_millis(5));
        }
        ShutdownReport {
            reader_joined: reader.is_finished() && reader.join().is_ok(),
            writer_joined: writer.is_finished() && writer.join().is_ok(),
            worker_joined: worker.is_finished() && worker.join().is_ok(),
        }
    }
    fn response(&self, request: RequestRef<'_>, ok: bool) -> JsObject {
        let mut response = object([
            ("kind", JsValue::from("response")),
            ("messageId", JsValue::String(self.owner.next_id("response"))),
            ("inReplyTo", JsValue::String(request.message_id.clone())),
            ("ok", JsValue::Bool(ok)),
        ]);
        if let Some(JsValue::String(id)) = request.fields.get("requestId") {
            response.insert("requestId".into(), JsValue::String(id.clone()));
        }
        response
    }
    fn reject(
        &self,
        request: RequestRef<'_>,
        code: &'static str,
        final_failure: bool,
    ) -> io::Result<()> {
        let mut response = self.response(request, false);
        response.insert("code".into(), JsValue::from(code));
        if final_failure {
            response.insert("final".into(), JsValue::Bool(true));
            response.insert("outcome".into(), JsValue::from("failed"));
        }
        self.owner.send_control(&JsValue::Object(response))
    }
    fn send_error(&self, code: &'static str, message: JsString) -> io::Result<()> {
        self.owner.send_control(&JsValue::Object(object([
            ("kind", JsValue::from("error")),
            ("messageId", JsValue::String(self.owner.next_id("error"))),
            ("code", JsValue::from(code)),
            ("message", JsValue::String(message)),
            ("retryability", JsValue::from("unsafe")),
        ])))
    }
    fn capabilities(&self) -> JsObject {
        let methods = [
            ("execute", true),
            ("cancel", true),
            ("status", true),
            ("resume", false),
            ("ack", false),
        ]
        .map(|(name, enabled)| {
            let mut method = object([
                ("name", JsValue::from(name)),
                ("enabled", JsValue::Bool(enabled)),
            ]);
            if name == "execute" {
                for (key, value) in [
                    ("profiles", JsValue::Array(vec![JsValue::from("streaming")])),
                    ("cancel", JsValue::Bool(true)),
                    ("resumeSupport", JsValue::from("none")),
                    ("retry", JsValue::from("retry_after_status")),
                ] {
                    method.insert(key.into(), value);
                }
            }
            JsValue::Object(method)
        });
        object([
            (
                "capabilitiesVersion",
                JsValue::String(self.owner.binding().capabilities_version.clone()),
            ),
            ("methods", JsValue::Array(methods.into())),
        ])
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    object
}
fn outcome_name(outcome: ModuleOutcome) -> &'static str {
    match outcome {
        ModuleOutcome::Completed => "completed",
        ModuleOutcome::Failed => "failed",
        ModuleOutcome::Cancelled => "cancelled",
        ModuleOutcome::ResultUnknown => "result_unknown",
    }
}
fn outcome_phase(outcome: ModuleOutcome) -> OperationPhase {
    match outcome {
        ModuleOutcome::Completed => OperationPhase::Completed,
        ModuleOutcome::Failed => OperationPhase::Failed,
        ModuleOutcome::Cancelled => OperationPhase::Cancelled,
        ModuleOutcome::ResultUnknown => OperationPhase::Resolving,
    }
}
fn phase_name(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Running => "running",
        OperationPhase::CancelRequested => "cancel_requested",
        OperationPhase::Completed => "completed",
        OperationPhase::Failed => "failed",
        OperationPhase::Cancelled => "cancelled",
        OperationPhase::Resolving => "resolving",
    }
}
fn failure_terminal(error: PortError) -> ExecutionTerminal {
    let accepted_call = if let PortError::AcceptedUnknown {
        identity,
        last_applied_sequence,
    } = &error
    {
        let mut call = object([
            ("sessionId", JsValue::String(identity.session_id.clone())),
            ("turnId", JsValue::String(identity.turn_id.clone())),
            ("runId", JsValue::String(identity.run_id.clone())),
            (
                "operationId",
                JsValue::String(identity.operation_id.clone()),
            ),
            ("requestId", JsValue::String(identity.request_id.clone())),
            (
                "moduleInstanceId",
                JsValue::String(identity.module_instance_id.clone()),
            ),
            (
                "connectionGeneration",
                JsValue::String(identity.connection_generation.clone()),
            ),
        ]);
        if let Some(key) = &identity.idempotency_key {
            call.insert("idempotencyKey".into(), JsValue::String(key.clone()));
        }
        if let Some(sequence) = last_applied_sequence {
            call.insert("lastAppliedSequence".into(), JsValue::Number(*sequence));
        }
        Some(JsValue::Object(call))
    } else {
        None
    };
    let (outcome, code, message) = match error {
        PortError::AcceptedUnknown { .. } => (
            ModuleOutcome::ResultUnknown,
            Some("RESULT_UNKNOWN".into()),
            "Host capability execution outcome is unknown.".into(),
        ),
        PortError::Cancelled {
            cause: CancelCause::Deadline,
        } => (
            ModuleOutcome::ResultUnknown,
            Some("DEADLINE_EXCEEDED".into()),
            "Module execution deadline exceeded.".into(),
        ),
        PortError::Cancelled { .. } => (
            ModuleOutcome::Cancelled,
            None,
            "Sidecar module call aborted.".into(),
        ),
        PortError::HostFailure { code, message, .. }
        | PortError::ProviderBoundaryOnly { code, message, .. } => {
            (ModuleOutcome::Failed, Some(code), message)
        }
        PortError::KnownTerminalException { code, message, .. } => {
            // This is the proven thrown Error's code, not a raw diagnostic
            // moduleCode. Source's outer catch classifies it before cancellation.
            let outcome = if code
                .as_ref()
                .is_some_and(|code| code.equals_str("RESULT_UNKNOWN"))
            {
                ModuleOutcome::ResultUnknown
            } else {
                ModuleOutcome::Failed
            };
            (outcome, code, message)
        }
        PortError::InvalidResponse { message }
        | PortError::SourceException { message, .. }
        | PortError::KnownConsumerException { message, .. }
        | PortError::TransportBeforeDispatch { message } => (ModuleOutcome::Failed, None, message),
        PortError::NotAdvertised { module, operation } => (
            ModuleOutcome::Failed,
            None,
            format!("Host method {module}.{operation} is not advertised.").into(),
        ),
        PortError::Provider(error) => (
            ModuleOutcome::Failed,
            Some(error.view.code),
            error.view.message,
        ),
    };
    let mut error = object([("message", JsValue::String(message))]);
    if let Some(code) = code {
        error.insert("code".into(), JsValue::String(code));
    }
    if let Some(call) = accepted_call {
        error.insert("acceptedCall".into(), call);
    }
    ExecutionTerminal {
        outcome,
        payload: object([("error", JsValue::Object(error.clone()))]),
        error: Some(error),
    }
}
/// TS agentLoopSidecarServer.ts:252-253 expires nonpositive delays immediately;
/// positive delays pass through setTimeout after capping at 2^31-1 ms. The real
/// Node timer-list probe for #401 confirmed 0.5 -> 1, 1.9 -> 1, 2.9 -> 2 ms.
fn execution_deadline_delay(remaining_ms: f64) -> Duration {
    if remaining_ms <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_millis(pilotdeck_protocol::timer_delay_ms(
            remaining_ms.min(2_147_483_647.0),
        ))
    }
}

#[cfg(test)]
mod tcp_liveness_tests {
    use super::*;
    use pilotdeck_protocol::admission::SidecarBinding;

    #[test]
    fn tcp_timer_limit_is_shared_and_releases_on_drop() {
        let budget = Arc::new(TimerBudget::default());
        let mut leases: Vec<_> = (0..TCP_TIMER_LIMIT)
            .map(|_| budget.acquire().unwrap())
            .collect();
        assert!(budget.acquire().is_none());
        drop(leases.pop());
        assert!(budget.acquire().is_some());
        drop(leases);
        assert_eq!(budget.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn timer_spawn_failure_releases_slot() {
        let budget = Arc::new(TimerBudget::default());
        let lease = budget.acquire().unwrap();
        let result =
            spawn_deadline_timer(Instant::now(), Cancellation::default(), Some(lease), |_| {
                Err(io::Error::other("injected thread spawn failure"))
            });
        assert!(result.is_err());
        assert_eq!(budget.0.load(Ordering::SeqCst), 0);
    }

    struct BrokenInput;
    impl std::io::Read for BrokenInput {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("injected reader panic")
        }
    }
    impl BufRead for BrokenInput {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            panic!("injected reader panic")
        }
        fn consume(&mut self, _: usize) {}
    }
    struct UnusedFactory;
    impl ExecutionFactory for UnusedFactory {
        fn execute(
            &self,
            _: &ExecuteRequest,
            _: &mut ExecutionContext,
        ) -> PortResult<ExecutionTerminal> {
            unreachable!("reader panics before execute")
        }
    }
    struct Clock;
    impl ExecutionClock for Clock {
        fn now_epoch_ms(&self) -> f64 {
            0.0
        }
        fn now_iso(&self) -> JsString {
            "1970-01-01T00:00:00.000Z".into()
        }
        fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> {
            Ok(None)
        }
    }
    #[cfg(unix)]
    #[test]
    fn poisoned_shared_decode_gate_releases_between_real_connections() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        use std::net::Shutdown;
        let ledger = SharedExecutionLedger::default();
        let gate = ledger.decoding.clone();
        let poison = gate.clone();
        assert!(thread::spawn(move || {
            let _guard = poison.lock().unwrap();
            panic!("injected decode panic");
        }).join().is_err());
        assert!(gate.is_poisoned());
        let (tx, rx) = mpsc::channel();
        for index in 0..2 {
            let owner = ConnectionOwner::new(
                SidecarBinding {
                    module_id: "m".into(), module_instance_id: "i".into(),
                    connection_generation: format!("g-{index}").as_str().into(),
                    capabilities_version: "1".into(),
                }, 8,
            );
            let server = ExecutionServer::with_shared_ledger(
                owner, Arc::new(UnusedFactory), Arc::new(Clock), 8, ledger.clone(),
            );
            assert!(Arc::ptr_eq(server.decoding.as_ref().unwrap(), &gate));
            let (stream, mut client) = UnixStream::pair().unwrap();
            client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let input = stream.try_clone().unwrap();
            let done = tx.clone();
            thread::spawn(move || {
                let _ = done.send(server.serve(BufReader::new(input), stream, Duration::from_secs(2)));
            });
            writeln!(client, "{{\"kind\":\"request\",\"messageId\":\"h\",\"method\":\"hello\",\"payload\":{{}}}}").unwrap();
            let mut response = String::new();
            BufReader::new(&client).read_line(&mut response).unwrap();
            let response = pilotdeck_protocol::parse_json(&response).unwrap();
            assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
            client.shutdown(Shutdown::Both).unwrap();
            let report = rx.recv_timeout(Duration::from_secs(4)).expect("connection stuck");
            assert!(report.complete(), "{report:?}");
        }
    }

    #[test]
    fn reader_panic_retires_and_reports_incomplete_without_hanging() {
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "m".into(),
                module_instance_id: "i".into(),
                connection_generation: "g".into(),
                capabilities_version: "1".into(),
            },
            8,
        );
        let server = ExecutionServer::new(owner, Arc::new(UnusedFactory), Arc::new(Clock), 8);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(server.serve(BrokenInput, io::sink(), Duration::from_secs(2)));
        });
        let report = rx
            .recv_timeout(Duration::from_secs(4))
            .expect("reader panic stranded server");
        assert!(!report.reader_joined);
        assert!(report.writer_joined && report.worker_joined);
    }
}

#[cfg(test)]
mod deadline_delay_tests {
    use super::*;

    #[test]
    fn execution_deadline_matches_ts_timer_list_and_immediate_expiry() {
        // Real TS server + Node 22 timer-list values (not just _idleTimeout).
        for (remaining_ms, expected_ms) in [
            (-1.0, 0),
            (0.0, 0),
            (0.5, 1),
            (1.0, 1),
            (1.9, 1),
            (2.9, 2),
            // 封顶在截断之前：2^31-1 本身与 2^31-1+0.5 都落在 2_147_483_647
            (2_147_483_647.0, 2_147_483_647),
            (2_147_483_647.5, 2_147_483_647),
            (2_147_483_648.0, 2_147_483_647),
        ] {
            assert_eq!(
                execution_deadline_delay(remaining_ms),
                Duration::from_millis(expected_ms),
                "remaining_ms={remaining_ms}",
            );
        }
    }
}

pub(crate) fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000D}' | '\u{0020}' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

#[cfg(test)]
mod known_terminal_error_tests {
    use super::*;
    use pilotdeck_protocol::model::BoundarySnapshot;

    #[test]
    fn constructed_terminal_error_code_sets_outer_unknown_without_fabricating_acceptance() {
        let terminal = failure_terminal(PortError::KnownTerminalException {
            message: "original terminal message".into(),
            code: Some("RESULT_UNKNOWN".into()),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        });
        assert_eq!(terminal.outcome, ModuleOutcome::ResultUnknown);
        let error = terminal.error.unwrap();
        assert_eq!(
            error.get("message"),
            Some(&JsValue::String("original terminal message".into()))
        );
        assert_eq!(
            error.get("code"),
            Some(&JsValue::String("RESULT_UNKNOWN".into()))
        );
        assert!(error.get("acceptedCall").is_none());
    }

    #[test]
    fn diagnostic_code_does_not_supply_a_thrown_error_code_or_terminal_property() {
        let boundary = BoundarySnapshot::capture(&JsValue::Object(object([
            ("code", "RESULT_UNKNOWN".into()),
            ("terminal", JsValue::Bool(true)),
        ])));
        for error in [
            PortError::KnownConsumerException {
                message: "ordinary".into(),
                boundary: boundary.clone(),
            },
            PortError::KnownTerminalException {
                message: "terminal".into(),
                code: None,
                boundary: boundary.clone(),
            },
            PortError::KnownTerminalException {
                message: "terminal".into(),
                code: Some("X".into()),
                boundary,
            },
        ] {
            let terminal = failure_terminal(error);
            assert_eq!(terminal.outcome, ModuleOutcome::Failed);
            assert!(terminal.error.unwrap().get("acceptedCall").is_none());
        }
    }
}
