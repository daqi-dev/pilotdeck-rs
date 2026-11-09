//! Gateway async-hook ownership, expiry and result delivery (InProcessGateway).
//! Timers are injected independently of the wall clock: a delayed timer and an
//! explicit deadline check have different observable behavior in the TS host.
use super::steer::{EnqueueResult, SteerMessage};
use crate::host::{HostError, HostFuture, ModuleError};
use crate::runtime::project::interaction_steer::ActiveSteerInfo;
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

/// One live turn as captured when an async hook registers.
///
/// Delivery must re-check run identity and enqueue through the same
/// admission-locked path as `steer_turn`. A bare `SteerMailbox` is keyed by
/// the textual turn ID, so a restarted run that reuses that ID would claim the
/// previous run's pending result.
pub trait AsyncHookTurnHandle: Send + Sync {
    /// External run ID; empty when the turn carries none.
    fn run_id(&self) -> &JsString;
    /// Opaque run identity. Handles captured from the same run share the same
    /// `Arc`; a restarted run with equal textual IDs gets a fresh one.
    fn identity(&self) -> &Arc<()>;
    /// True while the captured run is still the session's current, open run.
    fn is_live(&self) -> bool;
    /// Enqueue as a mid-turn steer, refusing a captured run that has ended.
    fn enqueue(&self, message: SteerMessage) -> HostFuture<EnqueueResult>;
}

#[derive(Clone)]
pub struct AsyncHookTurn {
    handle: Arc<dyn AsyncHookTurnHandle>,
    run_id: JsString,
}
impl AsyncHookTurn {
    pub fn new(handle: impl AsyncHookTurnHandle + 'static) -> Self {
        let run_id = handle.run_id().clone();
        Self {
            handle: Arc::new(handle),
            run_id,
        }
    }
    pub fn run_id(&self) -> &JsString {
        &self.run_id
    }
    fn identity(&self) -> &Arc<()> {
        self.handle.identity()
    }
    fn is_live(&self) -> bool {
        self.handle.is_live()
    }
    fn enqueue(&self, message: SteerMessage) -> HostFuture<EnqueueResult> {
        self.handle.enqueue(message)
    }
}

/// Production handle: the gateway's admission-locked steer path. `enqueue`
/// re-checks run identity under the registry lock and takes the mailbox tail
/// placeholder in the same critical section, so delivery cannot slip into a
/// restarted run.
impl AsyncHookTurnHandle for ActiveSteerInfo {
    fn run_id(&self) -> &JsString {
        &self.run_id
    }
    fn identity(&self) -> &Arc<()> {
        ActiveSteerInfo::identity(self)
    }
    fn is_live(&self) -> bool {
        ActiveSteerInfo::is_live(self)
    }
    fn enqueue(&self, message: SteerMessage) -> HostFuture<EnqueueResult> {
        ActiveSteerInfo::enqueue(self, message)
    }
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
type EventSink = dyn Fn(&JsString, JsObject) + Send + Sync;
struct Pending {
    input: AsyncHookRegistration,
    turn: AsyncHookTurn,
    pinned_guard: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
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
    events: VecDeque<(JsString, JsObject)>,
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
    emit: Arc<EventSink>,
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
        Self(Arc::new(Inner {
            clock,
            turn: Arc::new(turn),
            emit: Arc::new(emit),
            state: Mutex::new(State::default()),
        }))
    }
    pub fn register(&self, input: AsyncHookRegistration) -> Result<(), HostError> {
        let Some(turn) = (self.0.turn)(&input.session_key).filter(|t| !t.run_id().is_empty())
        else {
            return Ok(());
        };
        self.register_with_turn(input, turn, None)
    }
    /// Register with the admission-owned turn from its pinned project generation.
    pub(crate) fn register_pinned(
        &self,
        input: AsyncHookRegistration,
        turn: AsyncHookTurn,
        guard: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), HostError> {
        if !guard() || !turn.is_live() {
            return Err(error(
                "ASYNC_HOOK_STALE_TURN",
                "The async hook turn is no longer active.",
            ));
        }
        self.register_with_turn(input, turn, Some(guard))
    }
    fn register_with_turn(
        &self,
        input: AsyncHookRegistration,
        turn: AsyncHookTurn,
        pinned_guard: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<(), HostError> {
        if blank(&input.invocation_id) {
            return Ok(());
        }
        let mut state = self.0.state.lock().unwrap();
        if let Some(existing) = state.pending(&input.invocation_id) {
            // Same run identity, not equal text: a retried run may reuse the
            // invocation and turn IDs.
            if existing.input.session_key == input.session_key
                && Arc::ptr_eq(existing.turn.identity(), turn.identity())
            {
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
            turn,
            pinned_guard,
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
        // Empty context still has a run fence, but never enters steer. The
        // fence is run identity, never the textual turn/run ID: a restarted
        // run may reuse both.
        let turn = if let Some(guard) = &pending.pinned_guard {
            (guard() && pending.turn.is_live()).then(|| pending.turn.clone())
        } else {
            (self.0.turn)(&pending.input.session_key).filter(|t| {
                !t.run_id().is_empty() && Arc::ptr_eq(t.identity(), pending.turn.identity())
            })
        };
        if context.is_empty() || turn.is_none() {
            let status = if turn.is_some() && pending.turn.is_live() {
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
        // Admission-locked enqueue on the captured run: identity is re-checked
        // under the registry lock and the tail placeholder is taken in the
        // same critical section, so a restarted run cannot claim this result.
        let delivery = pending.turn.enqueue(SteerMessage {
            item_id,
            message,
            allowed_read_files: Some(Vec::new()),
        });
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
        // TS expires the whole turn in one synchronous loop; settle every
        // entry under one lock so no other settlement interleaves the burst.
        let mut state = self.0.state.lock().unwrap();
        let entries = state
            .pending
            .iter()
            .filter(|p| &p.input.session_key == session_key && p.turn.run_id() == run_id)
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
            state.events.push_back((
                pending.input.session_key.clone(),
                event.as_object().unwrap().clone(),
            ));
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
            let Some((session, event)) = next else {
                std::mem::forget(draining);
                return;
            };
            (self.0.emit)(&session, event);
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
    use crate::runtime::steer::SteerMailbox;
    use std::future::Future;
    use tokio::sync::oneshot;

    /// Delivery handle over a bare mailbox for the registry mechanics tests.
    /// Production uses `ActiveSteerInfo`; this one keeps the mailbox observable
    /// without an admission registry.
    struct BareHandle {
        run_id: JsString,
        identity: Arc<()>,
        mailbox: SteerMailbox,
        turn_id: JsString,
    }
    impl AsyncHookTurnHandle for BareHandle {
        fn run_id(&self) -> &JsString {
            &self.run_id
        }
        fn identity(&self) -> &Arc<()> {
            &self.identity
        }
        fn is_live(&self) -> bool {
            true
        }
        fn enqueue(&self, message: SteerMessage) -> HostFuture<EnqueueResult> {
            self.mailbox.enqueue(self.turn_id.clone(), message)
        }
    }
    fn bare_turn() -> AsyncHookTurn {
        let mailbox = SteerMailbox::new(None);
        mailbox.start("r".into());
        AsyncHookTurn::new(BareHandle {
            run_id: "r".into(),
            identity: Arc::new(()),
            mailbox,
            turn_id: "r".into(),
        })
    }

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
            turn: bare_turn(),
            pinned_guard: None,
            expires_at: f64::INFINITY,
            timer: Mutex::new(None),
            delivery: Mutex::new(None),
        })
    }
    #[tokio::test]
    async fn pinned_async_survives_generation_change_but_not_turn_replacement() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let registry = AsyncHookRegistry::new(Arc::new(TokioHookClock), |_| None, |_, _| {});
        let current = Arc::new(AtomicBool::new(true));
        let guard = {
            let current = current.clone();
            Arc::new(move || current.load(Ordering::SeqCst))
        };
        let registration = |id: &str| AsyncHookRegistration {
            session_key: "s".into(),
            invocation_id: id.into(),
            hook_name: "h".into(),
            hook_event: "PreToolUse".into(),
            timeout_ms: None,
            include_hook_events: false,
        };
        registry
            .register_pinned(registration("first"), bare_turn(), guard.clone())
            .unwrap();
        let result = registry
            .submit("s".into(), "first".into(), JsValue::Object(JsObject::new()))
            .await
            .unwrap();
        assert_eq!(result.status, AsyncHookStatus::Delivered);
        registry
            .register_pinned(registration("stale"), bare_turn(), guard.clone())
            .unwrap();
        current.store(false, Ordering::SeqCst);
        let result = registry
            .submit("s".into(), "stale".into(), JsValue::Object(JsObject::new()))
            .await
            .unwrap();
        assert_eq!(result.status, AsyncHookStatus::Expired);
        assert!(
            registry
                .register_pinned(registration("rejected"), bare_turn(), guard)
                .is_err()
        );
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
        let identity = Arc::new(());
        let registry = AsyncHookRegistry::new(
            Arc::new(TokioHookClock),
            move |_| {
                Some(AsyncHookTurn::new(BareHandle {
                    run_id: "r".into(),
                    identity: identity.clone(),
                    mailbox: mailbox.clone(),
                    turn_id: "r".into(),
                }))
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
