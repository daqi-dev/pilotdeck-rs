//! Gateway async-hook ownership, expiry and result delivery (InProcessGateway).
//! Timers are injected independently of the wall clock: a delayed timer and an
//! explicit deadline check have different observable behavior in the TS host.
use super::steer::{SteerMailbox, SteerMessage};
use crate::host::{HostError, HostFuture, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

static UNDEFINED: JsValue = JsValue::Undefined;

pub const DEFAULT_TIMEOUT_MS: f64 = 60_000.0;
pub const OUTCOME_RETENTION_MS: f64 = 300_000.0;

/// Dropping a timer cancels it. Callbacks must never run inline in `schedule`.
pub trait AsyncHookTimer: Send + Sync {}
pub trait AsyncHookClock: Send + Sync {
    fn now_ms(&self) -> f64;
    fn schedule(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn AsyncHookTimer>;
}

#[derive(Default)]
pub struct TokioHookClock;
struct TokioTimer(tokio::task::AbortHandle);
impl AsyncHookTimer for TokioTimer {}
impl Drop for TokioTimer {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl AsyncHookClock for TokioHookClock {
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as f64
    }
    fn schedule(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn AsyncHookTimer> {
        // Node setTimeout clamps non-finite/overflow/sub-ms values to one ms,
        // and truncates fractional delays. The absolute expiry is not clamped.
        let delay = timer_delay_ms(delay_ms);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(delay);
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            callback();
        });
        Box::new(TokioTimer(task.abort_handle()))
    }
}
pub use pilotdeck_protocol::timer_delay_ms;

#[derive(Clone)]
pub struct AsyncHookTurn {
    pub run_id: JsString,
    pub mailbox: SteerMailbox,
}
#[derive(Clone, Debug)]
pub struct AsyncHookRegistration {
    pub session_key: JsString,
    pub invocation_id: JsString,
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub timeout_ms: Option<f64>,
    pub include_hook_events: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncHookStatus {
    Delivered,
    Duplicate,
    Expired,
    Unknown,
}
impl AsyncHookStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Duplicate => "duplicate",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
        }
    }
    fn repeated(self) -> Self {
        if self == Self::Delivered {
            Self::Duplicate
        } else {
            self
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsyncHookResult {
    pub invocation_id: JsString,
    pub status: AsyncHookStatus,
}
type Delivery = Result<AsyncHookResult, HostError>;
type DeliveryReceiver = watch::Receiver<Option<Delivery>>;
type TurnLookup = dyn Fn(&JsString) -> Option<AsyncHookTurn> + Send + Sync;
type TurnEventSink = dyn Fn(JsObject) + Send + Sync;
type TurnCapture = dyn Fn(&JsString) -> Option<(AsyncHookTurn, Arc<TurnEventSink>)> + Send + Sync;

/// Production session bindings share the existing steer mailbox and event
/// stream. A generation guard prevents a retiring turn from unbinding its
/// replacement. No second queue or session owner is introduced.
#[derive(Clone)]
pub struct LiveAsyncHookBindings {
    current: Arc<Mutex<HashMap<JsString, LiveAsyncHookBinding>>>,
    pub registry: AsyncHookRegistry,
}
struct LiveAsyncHookBinding {
    identity: Arc<()>,
    turn: AsyncHookTurn,
    emit: Arc<TurnEventSink>,
}
pub struct LiveAsyncHookGuard {
    bindings: LiveAsyncHookBindings,
    session: JsString,
    run: JsString,
    identity: Arc<()>,
}
impl Default for LiveAsyncHookBindings {
    fn default() -> Self {
        let current = Arc::new(Mutex::new(HashMap::<JsString, LiveAsyncHookBinding>::new()));
        let capture = current.clone();
        let registry = AsyncHookRegistry::with_capture(Arc::new(TokioHookClock), move |session| {
            capture
                .lock()
                .unwrap()
                .get(session)
                .map(|entry| (entry.turn.clone(), entry.emit.clone()))
        });
        Self { current, registry }
    }
}
impl LiveAsyncHookBindings {
    pub fn bind(
        &self,
        session: JsString,
        turn: AsyncHookTurn,
        emit: impl Fn(JsObject) + Send + Sync + 'static,
    ) -> LiveAsyncHookGuard {
        let identity = Arc::new(());
        let run = turn.run_id.clone();
        let mut replacement = Some(LiveAsyncHookBinding {
            identity: identity.clone(),
            turn,
            emit: Arc::new(emit),
        });
        loop {
            let old = {
                let mut current = self.current.lock().unwrap();
                match current.remove(&session) {
                    Some(old) => Some(old),
                    None => {
                        current.insert(session.clone(), replacement.take().unwrap());
                        None
                    }
                }
            };
            let Some(old) = old else { break };
            // Retire pending work before publishing its replacement. Each
            // registration retains the old sink even when another drainer or
            // an accepted steer finishes after this transition.
            self.registry
                .expire_for_binding(&session, &old.turn.run_id, &old.emit);
        }
        LiveAsyncHookGuard {
            bindings: self.clone(),
            session,
            run,
            identity,
        }
    }
}
impl Drop for LiveAsyncHookGuard {
    fn drop(&mut self) {
        let owned = {
            let mut current = self.bindings.current.lock().unwrap();
            let owns = current
                .get(&self.session)
                .is_some_and(|entry| Arc::ptr_eq(&entry.identity, &self.identity));
            if owns {
                current.remove(&self.session)
            } else {
                None
            }
        };
        if let Some(owned) = owned {
            self.bindings
                .registry
                .expire_for_binding(&self.session, &self.run, &owned.emit);
        }
    }
}
struct Pending {
    input: AsyncHookRegistration,
    run_id: JsString,
    emit: Arc<TurnEventSink>,
    expires_at: f64,
    timer: Mutex<Option<Box<dyn AsyncHookTimer>>>,
    delivery: Mutex<Option<DeliveryReceiver>>,
}
struct Outcome {
    session_key: JsString,
    status: AsyncHookStatus,
    _timer: Box<dyn AsyncHookTimer>,
}
#[derive(Default)]
struct State {
    // Vec preserves registration order for expire_for_turn's event stream.
    pending: Vec<Arc<Pending>>,
    outcomes: HashMap<JsString, Arc<Outcome>>,
    // Events are queued in settlement order under this lock and handed to the
    // sink by one drainer at a time (see `flush`).
    events: VecDeque<(Arc<TurnEventSink>, JsObject)>,
    emitting: bool,
}
impl State {
    fn pending(&self, id: &JsString) -> Option<Arc<Pending>> {
        self.pending
            .iter()
            .find(|p| &p.input.invocation_id == id)
            .cloned()
    }
    /// Status a repeated submit from `pending`'s session would see now.
    fn observed(&self, pending: &Pending) -> AsyncHookStatus {
        self.outcomes
            .get(&pending.input.invocation_id)
            .filter(|prior| prior.session_key == pending.input.session_key)
            .map_or(AsyncHookStatus::Unknown, |prior| prior.status.repeated())
    }
}
struct Inner {
    clock: Arc<dyn AsyncHookClock>,
    turn: Arc<TurnLookup>,
    capture: Arc<TurnCapture>,
    state: Mutex<State>,
}
/// Gateway-owned registry. Lookup supplies the current run and its existing
/// steer mailbox; no alternate message queue is introduced. Invoke operations
/// within a Tokio runtime. Submissions keep running if their future is dropped.
///
/// Event sink: TS settles and emits in one synchronous step, so events reach
/// the sink in settlement order and the last event for an id matches its
/// stored outcome. Here each settlement queues its event under the state lock
/// and a single drainer calls the sink outside every registry lock, one call
/// at a time and in queue order. The sink may therefore re-enter the registry
/// (no deadlock); events raised meanwhile are delivered after the current call
/// returns. When another thread is draining, an operation can return before
/// its event has reached the sink. `expire_for_turn` settles its whole burst
/// before the first event is drained, so a sink re-entering for a later entry
/// of the same burst sees it already expired, while TS's per-entry
/// settle→emit loop would still show it pending.
#[derive(Clone)]
pub struct AsyncHookRegistry(Arc<Inner>);
impl AsyncHookRegistry {
    pub fn new(
        clock: Arc<dyn AsyncHookClock>,
        turn: impl Fn(&JsString) -> Option<AsyncHookTurn> + Send + Sync + 'static,
        emit: impl Fn(&JsString, JsObject) + Send + Sync + 'static,
    ) -> Self {
        let emit = Arc::new(emit);
        Self::with_capture(clock, move |session| {
            let turn = turn(session)?;
            let session = session.clone();
            let emit = emit.clone();
            let sink: Arc<TurnEventSink> = Arc::new(move |event| emit(&session, event));
            Some((turn, sink))
        })
    }
    fn with_capture(
        clock: Arc<dyn AsyncHookClock>,
        capture: impl Fn(&JsString) -> Option<(AsyncHookTurn, Arc<TurnEventSink>)>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let capture = Arc::new(capture);
        let lookup = capture.clone();
        Self(Arc::new(Inner {
            clock,
            turn: Arc::new(move |session| lookup(session).map(|(turn, _)| turn)),
            capture,
            state: Mutex::new(State::default()),
        }))
    }
    pub fn register(&self, input: AsyncHookRegistration) -> Result<(), HostError> {
        if blank(&input.invocation_id) {
            return Ok(());
        }
        // Bind removes the retiring generation before taking this lock to
        // expire it. Capture while holding the lock so registration cannot
        // enter that generation after its expiry scan has completed.
        let mut state = self.0.state.lock().unwrap();
        let Some((turn, emit)) =
            (self.0.capture)(&input.session_key).filter(|(turn, _)| !turn.run_id.is_empty())
        else {
            return Ok(());
        };
        if let Some(existing) = state.pending(&input.invocation_id) {
            if existing.input.session_key == input.session_key && existing.run_id == turn.run_id {
                return Ok(());
            }
            return Err(error(
                "ASYNC_HOOK_INVOCATION_CONFLICT",
                "Async hook invocation id is already owned by another active turn.",
            ));
        }
        let timeout = input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout = if timeout.is_nan() {
            timeout
        } else {
            timeout.max(1.0)
        };
        let pending = Arc::new(Pending {
            input,
            run_id: turn.run_id,
            emit,
            expires_at: self.0.clock.now_ms() + timeout,
            timer: Mutex::new(None),
            delivery: Mutex::new(None),
        });
        let weak = Arc::downgrade(&self.0);
        let entry = Arc::downgrade(&pending);
        *pending.timer.lock().unwrap() = Some(self.0.clock.schedule(
            timeout,
            Box::new(move || {
                if let (Some(inner), Some(pending)) = (weak.upgrade(), entry.upgrade()) {
                    Self(inner).expire(&pending);
                }
            }),
        ));
        state.pending.push(pending);
        Ok(())
    }
    pub fn submit(
        &self,
        session_key: JsString,
        invocation_id: JsString,
        output: JsValue,
    ) -> HostFuture<AsyncHookResult> {
        let result = |status| AsyncHookResult {
            invocation_id: invocation_id.clone(),
            status,
        };
        if blank(&session_key) || blank(&invocation_id) {
            return ready(Err(error(
                "INVALID_ASYNC_HOOK_RESULT",
                "sessionKey and invocationId are required.",
            )));
        }
        let pending = {
            let state = self.0.state.lock().unwrap();
            if let Some(prior) = state.outcomes.get(&invocation_id) {
                return ready(Ok(result(if prior.session_key == session_key {
                    prior.status.repeated()
                } else {
                    AsyncHookStatus::Unknown
                })));
            }
            match state.pending(&invocation_id) {
                Some(pending) if pending.input.session_key == session_key => pending,
                _ => return ready(Ok(result(AsyncHookStatus::Unknown))),
            }
        };
        if self.0.clock.now_ms() >= pending.expires_at {
            // A concurrent settlement may win between admission and here; TS
            // would then have returned the stored outcome instead.
            return ready(Ok(result(self.expire(&pending))));
        }
        let mut in_flight = pending
            .delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A concurrent submit may have captured this entry before the first
        // delivery settled. Recheck under admission's lock before reusing it.
        if let Some(prior) = self.0.state.lock().unwrap().outcomes.get(&invocation_id) {
            return ready(Ok(result(if prior.session_key == session_key {
                prior.status.repeated()
            } else {
                AsyncHookStatus::Unknown
            })));
        }
        if let Some(receiver) = &*in_flight {
            return wait_for_delivery(receiver.clone(), true);
        }
        let context = match deferred_context(&output, &pending.input.hook_event) {
            Ok(context) => context,
            Err(error) => return ready(Err(error)),
        };
        // Empty context still has a run fence, but never enters steer.
        let turn = (self.0.turn)(&pending.input.session_key)
            .filter(|t| !t.run_id.is_empty() && t.run_id == pending.run_id);
        if context.is_empty() || turn.is_none() {
            let status = if turn.is_some() {
                // TS settles synchronously here, so no timer can run between
                // admission and settlement. With threaded timers, re-check
                // ownership under the state lock instead of overwriting an
                // expiry that won the race.
                let mut state = self.0.state.lock().unwrap();
                if !state
                    .pending(&invocation_id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry, &pending))
                {
                    return ready(Ok(result(state.observed(&pending))));
                }
                self.settle_locked(&mut state, &pending, AsyncHookStatus::Delivered);
                drop(state);
                drop(in_flight);
                self.flush();
                AsyncHookStatus::Delivered
            } else {
                // Keep admission held so no other submit settles in between;
                // still report the stored outcome like the TS prefix would.
                // Like the delivered branch, the sink only runs after every
                // registry lock (including admission) is released.
                let status = self.expire_unflushed(&pending);
                drop(in_flight);
                self.flush();
                status
            };
            return ready(Ok(result(status)));
        }
        let turn = turn.unwrap();
        let item_id = concat(&[&"async-hook:".into(), &invocation_id]);
        let message = object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    ("text", JsValue::String(context)),
                ])]),
            ),
            (
                "metadata",
                object([
                    ("purpose", "mid_turn_steer".into()),
                    ("queueItemId", JsValue::String(item_id.clone())),
                ]),
            ),
        ]);
        let delivery = turn.mailbox.enqueue(
            pending.run_id.clone(),
            SteerMessage {
                item_id,
                message,
                allowed_read_files: Some(Vec::new()),
            },
        );
        let (sender, receiver) = watch::channel(None);
        *in_flight = Some(receiver.clone());
        drop(in_flight);
        let registry = self.clone();
        spawn_delivery(pending.clone(), sender, async move {
            match delivery.await {
                Ok(result) if result.accepted => {
                    registry.settle(&pending, AsyncHookStatus::Delivered);
                    registry.flush();
                    Ok(AsyncHookResult {
                        invocation_id,
                        status: AsyncHookStatus::Delivered,
                    })
                }
                Ok(_) => {
                    // TS reports `expired` here even if a timer settled first.
                    registry.expire(&pending);
                    Ok(AsyncHookResult {
                        invocation_id,
                        status: AsyncHookStatus::Expired,
                    })
                }
                Err(error) => Err(error),
            }
        });
        wait_for_delivery(receiver, false)
    }
    pub fn expire_for_turn(&self, session_key: &JsString, run_id: &JsString) {
        self.expire_matching_turn(session_key, run_id, None);
    }
    fn expire_for_binding(
        &self,
        session_key: &JsString,
        run_id: &JsString,
        sink: &Arc<TurnEventSink>,
    ) {
        self.expire_matching_turn(session_key, run_id, Some(sink));
    }
    fn expire_matching_turn(
        &self,
        session_key: &JsString,
        run_id: &JsString,
        sink: Option<&Arc<TurnEventSink>>,
    ) {
        // TS expires the whole turn in one synchronous loop; settle every
        // entry under one lock so no other settlement interleaves the burst.
        let mut state = self.0.state.lock().unwrap();
        let entries = state
            .pending
            .iter()
            .filter(|p| {
                &p.input.session_key == session_key
                    && &p.run_id == run_id
                    && sink.is_none_or(|sink| Arc::ptr_eq(sink, &p.emit))
            })
            .cloned()
            .collect::<Vec<_>>();
        for pending in &entries {
            self.settle_locked(&mut state, pending, AsyncHookStatus::Expired);
        }
        drop(state);
        self.flush();
    }
    /// Expires `pending` if it still owns its id. Returns what a repeated
    /// submission from the same session would observe under the same lock:
    /// `expired` when this call settled it, otherwise the outcome that won.
    fn expire(&self, pending: &Arc<Pending>) -> AsyncHookStatus {
        let status = self.expire_unflushed(pending);
        self.flush();
        status
    }
    /// [`Self::expire`] without draining events; the caller must `flush`
    /// after releasing its own locks.
    fn expire_unflushed(&self, pending: &Arc<Pending>) -> AsyncHookStatus {
        let mut state = self.0.state.lock().unwrap();
        if !state
            .pending(&pending.input.invocation_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry, pending))
        {
            return state.observed(pending);
        }
        self.settle_locked(&mut state, pending, AsyncHookStatus::Expired);
        AsyncHookStatus::Expired
    }
    fn settle(&self, pending: &Arc<Pending>, status: AsyncHookStatus) {
        self.settle_locked(&mut self.0.state.lock().unwrap(), pending, status);
    }
    fn settle_locked(&self, state: &mut State, pending: &Arc<Pending>, status: AsyncHookStatus) {
        state.pending.retain(|entry| !Arc::ptr_eq(entry, pending));
        pending.timer.lock().unwrap().take();
        // An already accepted, delayed steer can settle after expiry in TS.
        // It replaces the prior outcome and starts a fresh retention timer.
        let weak = Arc::downgrade(&self.0);
        let id = pending.input.invocation_id.clone();
        let outcome = Arc::new_cyclic(|entry: &Weak<Outcome>| {
            let entry = entry.clone();
            Outcome {
                session_key: pending.input.session_key.clone(),
                status,
                _timer: self.0.clock.schedule(
                    OUTCOME_RETENTION_MS,
                    Box::new(move || {
                        if let (Some(inner), Some(entry)) = (weak.upgrade(), entry.upgrade()) {
                            let mut state = inner.state.lock().unwrap();
                            if state
                                .outcomes
                                .get(&id)
                                .is_some_and(|current| Arc::ptr_eq(current, &entry))
                            {
                                state.outcomes.remove(&id);
                            }
                        }
                    }),
                ),
            }
        });
        state
            .outcomes
            .insert(pending.input.invocation_id.clone(), outcome);
        // Every TS settlement is followed by its event in the same sync step.
        if pending.input.include_hook_events {
            let event = object([
                ("type", "hook_async_result".into()),
                (
                    "invocationId",
                    JsValue::String(pending.input.invocation_id.clone()),
                ),
                ("hookName", JsValue::String(pending.input.hook_name.clone())),
                (
                    "hookEvent",
                    JsValue::String(pending.input.hook_event.clone()),
                ),
                ("status", status.as_str().into()),
            ]);
            state.events.push_back((pending.emit.clone(), event.as_object().unwrap().clone()));
        }
    }
    /// Hands queued events to the sink in order. Only one caller drains at a
    /// time; a caller that finds a drainer active leaves its events to it.
    fn flush(&self) {
        struct Draining<'a>(&'a Mutex<State>);
        impl Drop for Draining<'_> {
            fn drop(&mut self) {
                // Also runs if the sink panics, so later events still drain.
                if let Ok(mut state) = self.0.lock() {
                    state.emitting = false;
                }
            }
        }
        {
            let mut state = self.0.state.lock().unwrap();
            if state.emitting || state.events.is_empty() {
                return;
            }
            state.emitting = true;
        }
        let draining = Draining(&self.0.state);
        loop {
            let next = {
                let mut state = self.0.state.lock().unwrap();
                let next = state.events.pop_front();
                if next.is_none() {
                    // Release in the same critical section that saw the queue
                    // empty, or a concurrent enqueue could be stranded.
                    state.emitting = false;
                }
                next
            };
            let Some((emit, event)) = next else {
                std::mem::forget(draining);
                return;
            };
            emit(event);
        }
    }
}
struct DeliveryCompletion {
    pending: Arc<Pending>,
    sender: Option<watch::Sender<Option<Delivery>>>,
}
impl DeliveryCompletion {
    fn finish(&mut self, outcome: Delivery) {
        // Clear before waking callers, including on unwind/cancellation.
        *self
            .pending
            .delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.sender.take().unwrap().send_replace(Some(outcome));
    }
}
impl Drop for DeliveryCompletion {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Err(error(
                "ASYNC_HOOK_DELIVERY_FAILED",
                "Async hook delivery stopped.",
            )));
        }
    }
}
fn spawn_delivery(
    pending: Arc<Pending>,
    sender: watch::Sender<Option<Delivery>>,
    delivery: impl std::future::Future<Output = Delivery> + Send + 'static,
) {
    let mut completion = DeliveryCompletion {
        pending,
        sender: Some(sender),
    };
    tokio::spawn(async move {
        completion.finish(delivery.await);
    });
}

fn ready(result: Delivery) -> HostFuture<AsyncHookResult> {
    Box::pin(async { result })
}
fn wait_for_delivery(
    mut receiver: DeliveryReceiver,
    repeated: bool,
) -> HostFuture<AsyncHookResult> {
    Box::pin(async move {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result.map(|mut result| {
                    if repeated {
                        result.status = result.status.repeated();
                    }
                    result
                });
            }
            receiver
                .changed()
                .await
                .map_err(|_| error("ASYNC_HOOK_DELIVERY_FAILED", "Async hook delivery stopped."))?;
        }
    })
}
fn error(code: &str, message: impl Into<JsString>) -> HostError {
    let mut error = ModuleError::new(message);
    error.code = Some(code.into());
    error.into()
}
pub(crate) fn blank(value: &JsString) -> bool {
    value
        .units()
        .iter()
        .all(|&c| crate::runtime::memory::js_is_space_unit(c))
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|s| s.units().iter().copied())
            .collect(),
    )
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn deferred_context(output: &JsValue, event: &JsString) -> Result<JsString, HostError> {
    let invalid = |message| error("INVALID_ASYNC_HOOK_RESULT", message);
    let Some(record) = output.as_object() else {
        return Err(invalid("Deferred hook output must be an object."));
    };
    if record
        .entries()
        .iter()
        .any(|(key, _)| !key.equals_str("hookSpecificOutput"))
    {
        return Err(error(
            "UNSUPPORTED_ASYNC_HOOK_EFFECT",
            "Deferred hook results may only supply hookSpecificOutput.additionalContext.",
        ));
    }
    let specific = record.get("hookSpecificOutput").unwrap_or(&UNDEFINED);
    if matches!(specific, JsValue::Undefined) {
        return Ok("".into());
    }
    let Some(specific) = specific.as_object() else {
        return Err(invalid("hookSpecificOutput must be an object."));
    };
    if specific
        .entries()
        .iter()
        .any(|(key, _)| !key.equals_str("hookEventName") && !key.equals_str("additionalContext"))
    {
        return Err(error(
            "UNSUPPORTED_ASYNC_HOOK_EFFECT",
            "Deferred hook results may only supply hookEventName and additionalContext.",
        ));
    }
    let actual = specific.get("hookEventName").unwrap_or(&UNDEFINED);
    if actual.as_string() != Some(event) {
        return Err(error(
            "INVALID_ASYNC_HOOK_RESULT",
            concat(&[
                &"Deferred hook result belongs to ".into(),
                &js_string(actual)?,
                &", expected ".into(),
                event,
                &".".into(),
            ]),
        ));
    }
    match specific.get("additionalContext") {
        None | Some(JsValue::Undefined) => Ok("".into()),
        Some(JsValue::String(context)) => Ok(concat(&[
            &"<async_hook_context event=\"".into(),
            event,
            &"\">\n".into(),
            context,
            &"\n</async_hook_context>".into(),
        ])),
        _ => Err(invalid("Deferred hook additionalContext must be a string.")),
    }
}
fn js_string(value: &JsValue) -> Result<JsString, HostError> {
    // Wire objects cannot contain callable conversion methods. An own
    // `toString` shadows Object.prototype.toString and makes String() throw.
    // Bound this traversal like the shared iterative array conversion.
    let overflow = || HostError::from(ModuleError::new(crate::js_value::StackOverflow::MESSAGE));
    let mut pending = vec![(value, 0)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            JsValue::Array(values) => {
                if depth >= crate::js_value::MAX_ARRAY_DEPTH {
                    return Err(overflow());
                }
                pending.extend(values.iter().rev().map(|value| (value, depth + 1)));
            }
            JsValue::Object(object) if object.get("toString").is_some() => {
                return Err(ModuleError::new("Cannot convert object to primitive value").into());
            }
            _ => {}
        }
    }
    crate::js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| overflow())
}

#[cfg(test)]
#[path = "../../tests/common/spawn_completion.rs"]
pub(super) mod spawn_test;

#[cfg(test)]
mod spawn_tests {
    use super::spawn_test::{Port, Wait, Work, bounded};
    use super::*;
    use std::future::Future;
    use tokio::sync::oneshot;

    fn pending_entry() -> Arc<Pending> {
        Arc::new(Pending {
            input: AsyncHookRegistration {
                session_key: "s".into(),
                invocation_id: "i".into(),
                hook_name: "h".into(),
                hook_event: "e".into(),
                timeout_ms: None,
                include_hook_events: false,
            },
            run_id: "r".into(),
            emit: Arc::new(|_| {}),
            expires_at: f64::INFINITY,
            timer: Mutex::new(None),
            delivery: Mutex::new(None),
        })
    }
    fn start(
        pending: &Arc<Pending>,
        future: impl Future<Output = Delivery> + Send + 'static,
    ) -> DeliveryReceiver {
        let (sender, receiver) = watch::channel(None);
        *pending
            .delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(receiver.clone());
        spawn_delivery(pending.clone(), sender, future);
        receiver
    }
    fn success() -> Delivery {
        Ok(AsyncHookResult {
            invocation_id: "i".into(),
            status: AsyncHookStatus::Delivered,
        })
    }
    async fn observe(receiver: DeliveryReceiver, repeated: bool) -> Delivery {
        bounded(wait_for_delivery(receiver, repeated)).await
    }

    struct DeliveryPort(Arc<Pending>, Option<DeliveryReceiver>);
    impl Port for DeliveryPort {
        const SCOPE: &'static str = "delivery";
        const PANIC: &'static str = "Async hook delivery stopped.";
        fn new() -> Self {
            Self(pending_entry(), None)
        }
        fn begin(&self, work: Work) -> Wait {
            Self::wait(
                start(&self.0, async move {
                    work.await.map_err(|_| error("TEST", "ordinary failure"))?;
                    success()
                }),
                false,
            )
        }
        fn snapshot(&mut self) {
            self.1 = self.0.delivery.lock().unwrap().clone();
        }
        fn subscribe(&self, repeated: bool) -> Wait {
            Self::wait(self.1.clone().unwrap(), repeated)
        }
        fn cached(&self) -> bool {
            self.0.delivery.lock().unwrap().is_some()
        }
        fn published(&self) -> bool {
            self.1.as_ref().unwrap().borrow().is_some()
        }
    }
    impl DeliveryPort {
        fn wait(receiver: DeliveryReceiver, repeated: bool) -> Wait {
            Box::pin(async move {
                wait_for_delivery(receiver, repeated)
                    .await
                    .map(|value| value.status.as_str().into())
                    .map_err(|e| e.module_error.unwrap().message.to_utf8().unwrap())
            })
        }
    }
    #[test]
    fn delivery_spawn_failure_matrix() {
        spawn_test::matrix::<DeliveryPort>(false);
    }
    #[test]
    fn delivery_spawn_drop_matrix() {
        spawn_test::matrix::<DeliveryPort>(true);
    }

    #[tokio::test]
    async fn delivery_poison_failure() {
        let entry = pending_entry();
        let (release, gate) = oneshot::channel();
        let receiver = start(&entry, async move {
            gate.await.unwrap();
            panic!("delivery panic");
        });
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _locked = entry.delivery.lock().unwrap();
            panic!("poison delivery lock");
        }));
        release.send(()).unwrap();
        assert!(observe(receiver.clone(), false).await.is_err());
        assert!(
            entry
                .delivery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "poisoned delivery cleared"
        );
        assert!(receiver.borrow().is_some(), "poisoned delivery published");
    }

    #[tokio::test]
    async fn submit_sink_panic_clears_delivery() {
        let mailbox = SteerMailbox::new(None);
        mailbox.start("r".into());
        let registry = AsyncHookRegistry::new(
            Arc::new(TokioHookClock),
            move |_| {
                Some(AsyncHookTurn {
                    run_id: "r".into(),
                    mailbox: mailbox.clone(),
                })
            },
            |_, _| panic!("injected sink panic"),
        );
        let mut input = pending_entry().input.clone();
        input.include_hook_events = true;
        registry.register(input).unwrap();
        let entry = registry
            .0
            .state
            .lock()
            .unwrap()
            .pending(&"i".into())
            .unwrap();
        let output = object([(
            "hookSpecificOutput",
            object([
                ("hookEventName", "e".into()),
                ("additionalContext", "context".into()),
            ]),
        )]);
        let first = registry.submit("s".into(), "i".into(), output.clone());
        let second = registry.submit("s".into(), "i".into(), output.clone());
        for waiter in [first, second] {
            let failure = bounded(waiter).await.unwrap_err();
            assert_eq!(
                failure.module_error.unwrap().code,
                Some("ASYNC_HOOK_DELIVERY_FAILED".into())
            );
        }
        assert!(
            entry.delivery.lock().unwrap().is_none(),
            "submit panic clears delivery"
        );
        assert_eq!(
            registry
                .submit("s".into(), "i".into(), output)
                .await
                .unwrap()
                .status,
            AsyncHookStatus::Duplicate
        );
    }
}

#[cfg(test)]
mod live_binding_tests {
    use super::*;
    #[tokio::test]
    async fn production_binding_delivers_to_real_mailbox_and_expires_once_before_release() {
        let bindings = LiveAsyncHookBindings::default();
        let mailbox = SteerMailbox::default();
        mailbox.start("run".into());
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let guard = bindings.bind(
            "s".into(),
            AsyncHookTurn {
                run_id: "run".into(),
                mailbox: mailbox.clone(),
            },
            move |event| output.lock().unwrap().push(event),
        );
        let register = |id: &str| {
            bindings
                .registry
                .register(AsyncHookRegistration {
                    session_key: "s".into(),
                    invocation_id: id.into(),
                    hook_name: "http".into(),
                    hook_event: "PreModelRequest".into(),
                    timeout_ms: Some(60_000.0),
                    include_hook_events: true,
                })
                .unwrap()
        };
        register("delivered");
        let result = bindings.registry.submit("s".into(), "delivered".into(), pilotdeck_protocol::parse_json(r#"{"hookSpecificOutput":{"hookEventName":"PreModelRequest","additionalContext":"later"}}"#).unwrap()).await.unwrap();
        assert_eq!(result.status, AsyncHookStatus::Delivered);
        let steers = mailbox.drain(&"run".into());
        assert_eq!(steers.len(), 1);
        assert_eq!(steers[0].item_id, "async-hook:delivered".into());
        assert_eq!(
            bindings
                .registry
                .submit("s".into(), "delivered".into(), JsValue::Null)
                .await
                .unwrap()
                .status,
            AsyncHookStatus::Duplicate
        );
        register("expired");
        drop(guard);
        assert_eq!(
            bindings
                .registry
                .submit("s".into(), "expired".into(), JsValue::Null)
                .await
                .unwrap()
                .status,
            AsyncHookStatus::Expired
        );
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].get("status"), Some(&"delivered".into()));
        assert_eq!(events[1].get("status"), Some(&"expired".into()));
    }

    #[tokio::test]
    async fn stale_turn_guard_cannot_unbind_replacement() {
        let bindings = LiveAsyncHookBindings::default();
        let old_events = Arc::new(Mutex::new(Vec::new()));
        let old_sink = old_events.clone();
        let old = bindings.bind(
            "s".into(),
            AsyncHookTurn {
                run_id: "old".into(),
                mailbox: SteerMailbox::default(),
            },
            move |event| old_sink.lock().unwrap().push(event),
        );
        let register = |id: &str| {
            bindings
                .registry
                .register(AsyncHookRegistration {
                    session_key: "s".into(),
                    invocation_id: id.into(),
                    hook_name: "http".into(),
                    hook_event: "PreModelRequest".into(),
                    timeout_ms: Some(60_000.0),
                    include_hook_events: true,
                })
                .unwrap()
        };
        register("old-pending");
        let new_events = Arc::new(Mutex::new(Vec::new()));
        let new_sink = new_events.clone();
        let replacement = bindings.bind(
            "s".into(),
            AsyncHookTurn {
                run_id: "new".into(),
                mailbox: SteerMailbox::default(),
            },
            move |event| new_sink.lock().unwrap().push(event),
        );
        assert_eq!(old_events.lock().unwrap().len(), 1);
        assert_eq!(
            old_events.lock().unwrap()[0].get("status"),
            Some(&"expired".into())
        );
        assert!(new_events.lock().unwrap().is_empty());
        register("new-pending");
        drop(old);
        assert!(
            new_events.lock().unwrap().is_empty(),
            "retired guard cannot emit into replacement"
        );
        assert_eq!(
            (bindings.registry.0.turn)(&"s".into()).unwrap().run_id,
            "new".into()
        );
        drop(replacement);
        assert_eq!(new_events.lock().unwrap().len(), 1);
        assert!((bindings.registry.0.turn)(&"s".into()).is_none());
    }

    #[tokio::test]
    async fn stale_guard_cannot_expire_replacement_with_reused_run_id() {
        let bindings = LiveAsyncHookBindings::default();
        let turn = AsyncHookTurn {
            run_id: "same-run".into(),
            mailbox: SteerMailbox::default(),
        };
        let old = bindings.bind("s".into(), turn.clone(), |_| {});
        let replacement = bindings.bind("s".into(), turn, |_| {});
        bindings
            .registry
            .register(AsyncHookRegistration {
                session_key: "s".into(),
                invocation_id: "new".into(),
                hook_name: "http".into(),
                hook_event: "PreModelRequest".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .unwrap();
        drop(old);
        assert_eq!(
            bindings
                .registry
                .submit("s".into(), "new".into(), JsValue::Object(JsObject::new()))
                .await
                .unwrap()
                .status,
            AsyncHookStatus::Delivered
        );
        drop(replacement);
    }
}
