//! Live Gateway sink plus the bounded replay buffer.
//!
//! Mirrors `GatewayTurnEventCoordinator.ts:1-60`. The coordinator does not
//! decide which events exist, when a turn is admitted, or when a session
//! closes; the host publishes already-derived events through it.
//!
//! TS runs these methods on one event-loop thread, so `start` (replay + sink),
//! `emit` (sink lookup + runId fill + record + delivery) and `retainTerminal`
//! (sink removal + replay transition) are indivisible. The Rust port keeps that
//! contract by holding one composite-state lock for every such state change.
//! Sink callbacks still run outside every lock: a sink may re-enter the
//! coordinator (a reentrant `emit` from inside the session's own sink is
//! delivered synchronously inside that call, like TS, after the events that
//! concurrent emitters already recorded, and returns once its own event's
//! delivery completes), and a panicking sink must not poison the state. The
//! same holds for the replay store's injected timer (#805): a panic inside
//! `start`/`retain_terminal`/`dispose` surfaces at the call site like a
//! throwing TS call — after the same partial state, since the methods apply
//! their mutations in the TS order — but the guard is released before the
//! unwind resumes, and every state entry (including the drain cleanup
//! guard) recovers a poisoned lock instead of panicking, so no session and
//! no queued observer is wedged by the failure.
//! Delivery order per session is preserved by a FIFO queue drained by one
//! thread at a time, identified by a token that `dispose` invalidates so a
//! stale drainer — or a sink left over from before a restart — cannot touch
//! a restarted session's queue.
//!
//! Port-only edges are documented here rather than fixed: a sink may still
//! be called after `retain_terminal` or a replacing `start` returned (events
//! accepted before it are delivered by the in-flight drainer, while on TS
//! the last sink call always precedes the transition); a sink panic or a
//! `dispose` silently drops events that were accepted but not yet delivered
//! (`dispose` still lets the in-flight drainer deliver the one entry it
//! already popped, because the sink handle travelled with the entry); and a
//! reentrant `emit` delivers — and may have to catch — events other threads
//! recorded ahead of the reentrant event, so their sink panics surface at
//! the reentrant call site rather than at their own emitters'. These
//! port-only delivery edges — including the unbounded queue, disclosed at
//! `emit` — are registered as A7.2-COORDINATOR-DELIVERY-001 in
//! `specs/gateway-parity/accepted-diffs.md`. The production `TurnGateway`
//! host (`gateway.rs`) now drives every mutating coordinator call on one
//! current-thread runtime, making these edges unreachable through the
//! supported host API; the elimination is recorded as
//! A7.2-COORDINATOR-DELIVERY-001-R1 in `specs/gateway-parity/diffs/`. The
//! edges remain the documented behavior for callers that use this type
//! directly (or leak it out of a host builder) from several threads.
//!
//! Sink ownership is another port-only edge: TS keeps the sink in a plain
//! `Map` and its GC collects the whole object graph, cycles included; Rust
//! `Arc` does not. What a library can prevent, this port does: the
//! coordinator holds each registration only weakly ([`TurnSinkRegistration`]
//! is the strong owner, and `emit` checks the *registration's* liveness, not
//! the sink's — in-flight deliveries and queued events hold the sink
//! strongly and must not keep a dropped registration deliverable), so a sink
//! that strongly captures the coordinator can no longer keep the
//! coordinator — or its replay store's timer thread — alive. What no library
//! API can prevent is a cycle on the host's side of the contract: a sink
//! that strongly captures its own host forms `Host → registration → sink →
//! Host`, which keeps the host — and through it the coordinator and its
//! timer thread — alive after the last external handle drops, like a leaking
//! TS closure except that TS GC would still collect it. That half is the
//! host's responsibility, documented on [`TurnEventCoordinator::start`]: a
//! sink that needs to call back into its host must capture a `Weak` of it,
//! and the host ends a session by dropping the last registration or calling
//! [`TurnEventCoordinator::unregister`], never by relying on drop order. The
//! residual ownership semantics — including this unenforceable contract —
//! are registered as A7.2-COORDINATOR-SINK-OWNERSHIP-001 in
//! `specs/gateway-parity/accepted-diffs.md`.
use pilotdeck_protocol::{JsString, JsValue};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use super::replay::{ActiveTurnSnapshot, ActiveTurnSnapshotInput, TurnReplayStore};

type Sink = Arc<dyn Fn(&JsValue) + Send + Sync>;

/// Recovers the composite-state guard from a poisoned lock instead of
/// panicking. TS keeps no lock, so a throwing host timer (or a panicking
/// replay store) only fails its own call; a coordinator whose lock a panic
/// elsewhere poisoned must keep serving every session and release every
/// drain queue (#805, the same tolerant-lock shape as `mcp/wire.rs` and
/// `runtime/refresh.rs`). Every state entry uses it, including the drain
/// cleanup guard.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The registration record `emit`'s liveness check looks up. Separating it
/// from the sink it owns is what keeps that check honest (codex-r8 P1 #1):
/// the delivery queue and an in-flight drainer hold the *sink* strongly, so
/// a check that asked "is the sink alive" would accept new events after the
/// last registration dropped — asking "is the registration alive" does not,
/// while events accepted before the drop still deliver through the strong
/// sink handle their queue entry owns.
struct SinkRegistration {
    sink: Sink,
}

// Sessions whose sink is currently running on this thread, with the drain
// token of the loop running it. A reentrant `emit` from inside such a sink
// reuses that token, so its delivery runs synchronously inside that call,
// like the single-threaded TS host — behind the events that were already
// recorded. The token is re-validated under the state lock, so a marker left
// stale by `dispose` is only a hint.
thread_local! {
    static DRAIN_STACK: RefCell<Vec<(u64, JsString, u64)>> = const { RefCell::new(Vec::new()) };
}

static NEXT_COORDINATOR_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct CoordinatorState {
    /// One weak reference to the registration record per registered sink;
    /// the strong owner is the [`TurnSinkRegistration`] the registering
    /// `start` returned. The delivery queue holds only the inner sink, so an
    /// in-flight delivery never keeps a dropped registration alive for the
    /// `emit`-side check (codex-r8 P1 #1). See the module note for why a
    /// strong edge here would leak the coordinator's timer thread.
    sinks: HashMap<JsString, Weak<SinkRegistration>>,
    /// Events already recorded but not yet handed to their sink, in record
    /// order, tagged with the sequence number their `emit` assigned. The
    /// session's current drainer delivers them; a nested drain stops at its
    /// own event's sequence.
    deliveries: HashMap<JsString, VecDeque<(Sink, JsValue, u64)>>,
    next_delivery_seq: u64,
    /// session -> token of the drainer that owns the queue. `dispose`
    /// invalidates every token, so an in-flight drainer stops before it can
    /// touch a restarted session's queue and its cleanup is a no-op.
    draining: HashMap<JsString, u64>,
    next_drain_token: u64,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct CoordinatorHooks {
    after_start_replay: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    after_emit_run_id: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    after_retain_remove: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    after_dispose_clear: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub struct TurnEventCoordinator {
    /// Stable identity for the thread-local drain stack; two coordinators may
    /// reuse the same session key.
    id: u64,
    replay: TurnReplayStore,
    /// Serializes the composite operations that TS performs in one synchronous
    /// method body. Never held while a sink runs.
    state: Mutex<CoordinatorState>,
    #[cfg(test)]
    hooks: CoordinatorHooks,
}
/// The strong owner of a sink registered through
/// [`TurnEventCoordinator::start`]. The coordinator itself holds only a
/// weak reference, so a sink that strongly captures the coordinator — the
/// natural reentrant style — cannot keep the coordinator, or its replay
/// store's timer thread, alive after the last host handle drops. The half no
/// library API can enforce is the host's own edge: a sink that strongly
/// captures its host forms `Host → registration → sink → Host`, which stays
/// alive after the last external handle drops — a sink that needs to call
/// back into its host must capture a `Weak` of it instead (see
/// [`TurnEventCoordinator::start`] for the full contract). Hold this handle
/// for as long as the session's events must be delivered; dropping the last
/// clone unregisters the sink, so later `emit` calls return `false`, exactly
/// like `retain_terminal`, and [`TurnEventCoordinator::unregister`] detaches
/// it without waiting for that drop.
#[derive(Clone)]
pub struct TurnSinkRegistration {
    /// Held for ownership only: nothing reads it, but this clone's drop is
    /// what keeps the registration — and only the registration — alive.
    #[allow(dead_code)]
    registration: Arc<SinkRegistration>,
}

impl TurnEventCoordinator {
    pub fn new() -> Self {
        Self::with_replay_store(TurnReplayStore::new())
    }
    pub fn with_replay_store(replay: TurnReplayStore) -> Self {
        Self {
            id: NEXT_COORDINATOR_ID.fetch_add(1, Ordering::Relaxed),
            replay,
            state: Mutex::new(CoordinatorState::default()),
            #[cfg(test)]
            hooks: CoordinatorHooks::default(),
        }
    }
    /// Registers `sink` for `session_key` and returns the registration that
    /// owns it. The return value must outlive the registration: dropping the
    /// last clone unregisters the sink (see [`TurnSinkRegistration`]). In TS
    /// the sink lives in the coordinator's `Map` until `retainTerminal`,
    /// `dispose` or a replacing `start` removes it — a host that holds the
    /// registration for the same span is byte-for-byte identical.
    ///
    /// Ownership contract (the port-only half, A7.2-COORDINATOR-SINK-
    /// OWNERSHIP-001): Rust has no cycle collector, so a sink that needs to
    /// call back into its host must capture a `Weak` of it. A strong capture
    /// forms `Host → registration → sink → Host`, which keeps the host — and
    /// through it the coordinator and its replay store's timer thread —
    /// alive after the last external handle drops; no coordinator-side API
    /// can break that cycle from the outside. End a session by dropping the
    /// last registration or calling [`TurnEventCoordinator::unregister`],
    /// not by relying on drop order.
    pub fn start<S>(
        &self,
        session_key: &JsString,
        run_id: &JsString,
        sink: S,
    ) -> TurnSinkRegistration
    where
        S: Fn(&JsValue) + Send + Sync + 'static,
    {
        let registration = Arc::new(SinkRegistration {
            sink: Arc::new(sink),
        });
        let mut state = lock(&self.state);
        // The replay store may panic through its injected timer (#805): the
        // panic surfaces at the call site like a throwing TS call, but the
        // guard is dropped first so it cannot poison the composite-state
        // lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.start(session_key, run_id);
        })) {
            drop(state);
            resume_unwind(panic);
        }
        #[cfg(test)]
        self.fire_start();
        state
            .sinks
            .insert(session_key.clone(), Arc::downgrade(&registration));
        TurnSinkRegistration { registration }
    }
    pub fn record(&self, session_key: &JsString, event: &JsValue) {
        // Held across the replay mutation so `record` cannot land between the
        // replay and sink halves of a concurrent `start`. TS throws at the
        // call site for an event JSON cannot express; the panic is resumed
        // after the guard is dropped, so it cannot poison the lock for every
        // other session.
        let state = lock(&self.state);
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.record(session_key, event);
        })) {
            drop(state);
            resume_unwind(panic);
        }
    }
    /// Publishes only when a sink is registered. Replay is updated before the
    /// sink observes the event, matching TS delivery order.
    ///
    /// Return value: `true` means the event was accepted. On the TS host every
    /// `emit` runs the sink before returning; here delivery is FIFO per
    /// session, driven by whichever emitter first becomes the session's
    /// drainer — a concurrent emitter enqueues and returns immediately, and
    /// the queue is unbounded (slow sinks do not apply backpressure to
    /// emitters). A reentrant `emit` from inside the session's own sink is
    /// delivered synchronously inside this call, like TS, after the events
    /// that were already recorded, so the sink order always equals the
    /// record order; it returns once its own event's delivery completed,
    /// leaving events recorded after that to the outer drainer (a sink
    /// failure in an event another thread recorded ahead of the reentrant
    /// event surfaces at this call site — a port-only attribution). Events
    /// accepted before a sink panic or a `dispose` are dropped without any
    /// signal to their emitters.
    pub fn emit(&self, session_key: &JsString, event: &JsValue) -> bool {
        let marker_token = self.drain_token_on_this_thread(session_key);
        let (token, boundary) = {
            let mut state = lock(&self.state);
            // Registration liveness, not sink liveness (codex-r8 P1 #1): the
            // queue and any in-flight drainer hold the sink strongly, so
            // asking the sink would accept new events after the last
            // registration dropped.
            let Some(sink) = state
                .sinks
                .get(session_key)
                .and_then(|registration| registration.upgrade())
                .map(|registration| Arc::clone(&registration.sink))
            else {
                // The host dropped the registration (or never started the
                // session): TS looks a live sink up in its map, this port
                // prunes the dead weak reference on the way out.
                state.sinks.remove(session_key);
                return false;
            };
            // `with_run_id` panics on the null/undefined edge like the TS
            // `event.runId` throw; the guard keeps that panic from poisoning
            // the composite-state lock for every other session.
            let event = match catch_unwind(AssertUnwindSafe(|| {
                self.replay.with_run_id(session_key, event)
            })) {
                Ok(event) => event,
                Err(panic) => {
                    drop(state);
                    resume_unwind(panic);
                }
            };
            #[cfg(test)]
            self.fire_emit_run_id();
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                self.replay.record(session_key, &event);
            })) {
                drop(state);
                resume_unwind(panic);
            }
            let sequence = state.next_delivery_seq;
            state.next_delivery_seq = state.next_delivery_seq.wrapping_add(1);
            state
                .deliveries
                .entry(session_key.clone())
                .or_default()
                .push_back((sink, event, sequence));
            // This thread is inside the session's own sink: it already owns
            // the queue, so the nested delivery runs synchronously inside this
            // call, behind the events already recorded by concurrent emitters
            // and up to (and including) this call's own event. A marker left
            // stale by `dispose` no longer owns the queue and takes the
            // ordinary claim path, behind the new drainer.
            match marker_token.filter(|&token| owns_drain(&state, session_key, token)) {
                Some(token) => (Some(token), Some(sequence)),
                None => (claim_drain(&mut state, session_key), None),
            }
        };
        if let Some(token) = token {
            self.drain(session_key, token, boundary);
        }
        true
    }
    /// The sink is removed even when `run_id` does not match the replay, as in
    /// TS. Unlike the single-threaded TS host, the drainer that is still
    /// inside this session's sink may deliver one more already-accepted event
    /// after this returns; the sink must stay valid until then.
    pub fn retain_terminal(&self, session_key: &JsString, run_id: &JsString) {
        let mut state = lock(&self.state);
        state.sinks.remove(session_key);
        #[cfg(test)]
        self.fire_retain_remove();
        // The replay store may panic through its injected timer (#805),
        // after the sink removal — the same partial state a throwing TS
        // retainTerminal leaves. The guard is dropped first so the unwind
        // cannot poison the composite-state lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.retain_terminal(session_key, run_id);
        })) {
            drop(state);
            resume_unwind(panic);
        }
    }
    /// Detaches the sink registered for `session_key` without touching the
    /// replay buffer: later `emit` calls return `false`, exactly like
    /// `retain_terminal` after it removed the sink, but the replay data, the
    /// run and any terminal retention stay as they are, and events already
    /// accepted still deliver through the strong sink handle their queue
    /// entry owns. A port-only ownership tool — TS has no equivalent because
    /// its `Map` keeps the sink and GC reclaims the graph: it lets a host end
    /// delivery deterministically instead of relying on drop order, and it
    /// takes effect even while the host still holds the registration (see
    /// A7.2-COORDINATOR-SINK-OWNERSHIP-001).
    pub fn unregister(&self, session_key: &JsString) {
        lock(&self.state).sinks.remove(session_key);
    }
    pub fn snapshot(&self, input: &ActiveTurnSnapshotInput) -> ActiveTurnSnapshot {
        self.replay.snapshot(input)
    }
    pub fn dispose(&self) {
        let mut state = lock(&self.state);
        state.sinks.clear();
        #[cfg(test)]
        self.fire_dispose_clear();
        // Dropping the drain tokens makes an in-flight drainer exit before it
        // touches a restarted session's queue; its cleanup guard is a no-op.
        state.deliveries.clear();
        state.draining.clear();
        // The replay store may panic through its injected timer (#805), with
        // the sinks and drain tokens already cleared — the same partial state
        // a throwing TS dispose leaves. The guard is dropped first so the
        // unwind cannot poison the composite-state lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.replay.dispose();
        })) {
            drop(state);
            resume_unwind(panic);
        }
    }

    /// The drain token of the innermost sink call this thread is running for
    /// the session, if any.
    fn drain_token_on_this_thread(&self, session_key: &JsString) -> Option<u64> {
        DRAIN_STACK.with(|stack| {
            stack
                .borrow()
                .iter()
                .rev()
                .find(|(id, key, _)| *id == self.id && key == session_key)
                .map(|(_, _, token)| *token)
        })
    }

    /// Delivers this session's queued events in record order. The caller owns
    /// `token`; if `dispose` invalidates it, the loop stops before touching
    /// the queue again and the guard leaves the restarted state alone. On a
    /// sink panic the guard resets the drain state it still owns, dropping any
    /// events that were queued behind the panicking one.
    ///
    /// A nested drain (this thread is inside a sink call of the same loop,
    /// reached through a reentrant `emit`) carries its call's own event
    /// sequence as `boundary`: it stops at the first later entry — which,
    /// after its own event's delivery completed, is the very next pop — so
    /// events recorded while that delivery runs are left to the outer
    /// drainer, and it never releases the queue. If a deeper nested drain
    /// already delivered the boundary event, the same first-later-entry
    /// check tells it so and it returns without delivering anything more.
    fn drain(&self, session_key: &JsString, token: u64, boundary: Option<u64>) {
        struct Guard<'a> {
            coordinator: &'a TurnEventCoordinator,
            session_key: &'a JsString,
            token: u64,
            armed: bool,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if !self.armed {
                    return;
                }
                // The same tolerant lock as the drain loop: a poisoned
                // composite state must not strand the queue on a stale
                // `draining` entry (#805), which would wedge every later
                // emit for the session behind a drainer that never returns.
                let mut state = lock(&self.coordinator.state);
                if owns_drain(&state, self.session_key, self.token) {
                    state.draining.remove(self.session_key);
                    state.deliveries.remove(self.session_key);
                }
            }
        }
        let mut guard = Guard {
            coordinator: self,
            session_key,
            token,
            armed: boundary.is_none(),
        };
        loop {
            let next = {
                let mut state = lock(&self.state);
                if !owns_drain(&state, session_key, token) {
                    // dispose took the queue over; this drainer has no
                    // business left.
                    guard.armed = false;
                    return;
                }
                match state
                    .deliveries
                    .get_mut(session_key)
                    .and_then(VecDeque::pop_front)
                {
                    Some((sink, event, sequence)) => {
                        if boundary.is_some_and(|boundary| sequence > boundary) {
                            // Everything up to and including this call's own
                            // event has been delivered — FIFO order means
                            // what is left was recorded later and belongs to
                            // the outer drainer (this also covers a deeper
                            // nested drain having consumed the boundary
                            // event itself).
                            if let Some(queue) = state.deliveries.get_mut(session_key) {
                                queue.push_front((sink, event, sequence));
                            }
                            guard.armed = false;
                            return;
                        }
                        Some((sink, event, sequence))
                    }
                    None => {
                        // Only an outer drainer releases the queue: a nested
                        // one that finds it empty (its boundary event was
                        // consumed by a deeper drain) still sits inside the
                        // outer loop's sink call.
                        if boundary.is_none() {
                            state.draining.remove(session_key);
                            state.deliveries.remove(session_key);
                        }
                        guard.armed = false;
                        return;
                    }
                }
            };
            if let Some((sink, event, _sequence)) = next {
                let _marker = DrainMarker::push(self.id, session_key, token);
                sink(&event);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_start_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hooks.after_start_replay.lock().unwrap() = Some(hook);
    }
    /// Poisons the composite-state lock so a test can prove every entry —
    /// including the drain cleanup guard — recovers the guard and keeps
    /// serving sessions (#805); the same test-only hook shape as
    /// `runtime/steer.rs` and `runtime/hooks/exec.rs`.
    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _state = self.state.lock().unwrap();
            panic!("injected coordinator state poison");
        }));
    }
    #[cfg(test)]
    pub(crate) fn set_after_emit_run_id_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hooks.after_emit_run_id.lock().unwrap() = Some(hook);
    }
    #[cfg(test)]
    pub(crate) fn set_after_retain_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hooks.after_retain_remove.lock().unwrap() = Some(hook);
    }
    #[cfg(test)]
    pub(crate) fn set_after_dispose_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hooks.after_dispose_clear.lock().unwrap() = Some(hook);
    }
    #[cfg(test)]
    fn fire_start(&self) {
        fire(&self.hooks.after_start_replay);
    }
    #[cfg(test)]
    fn fire_emit_run_id(&self) {
        fire(&self.hooks.after_emit_run_id);
    }
    #[cfg(test)]
    fn fire_retain_remove(&self) {
        fire(&self.hooks.after_retain_remove);
    }
    #[cfg(test)]
    fn fire_dispose_clear(&self) {
        fire(&self.hooks.after_dispose_clear);
    }
}
impl Default for TurnEventCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Marks this thread as running a session's sink under the given drain token,
/// so a reentrant `emit` reuses that token for its synchronous delivery.
/// Dropping it (including during a panic) pops the stack entry.
struct DrainMarker;
impl DrainMarker {
    fn push(coordinator_id: u64, session_key: &JsString, token: u64) -> Self {
        DRAIN_STACK.with(|stack| {
            stack
                .borrow_mut()
                .push((coordinator_id, session_key.clone(), token));
        });
        DrainMarker
    }
}
impl Drop for DrainMarker {
    fn drop(&mut self) {
        DRAIN_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Registers the caller as the session's drainer unless another emitter
/// already owns the queue; returns the ownership token.
fn claim_drain(state: &mut CoordinatorState, session_key: &JsString) -> Option<u64> {
    if state.draining.contains_key(session_key) {
        return None;
    }
    state.next_drain_token = state.next_drain_token.wrapping_add(1);
    let token = state.next_drain_token;
    state.draining.insert(session_key.clone(), token);
    Some(token)
}

fn owns_drain(state: &CoordinatorState, session_key: &JsString, token: u64) -> bool {
    state.draining.get(session_key) == Some(&token)
}

#[cfg(test)]
fn fire(hook: &Mutex<Option<Arc<dyn Fn() + Send + Sync>>>) {
    // One-shot: a second start/emit must not park on the same test hook.
    let hook = hook.lock().unwrap().take();
    if let Some(hook) = hook {
        hook();
    }
}

/// TS `sameTurnEpoch` (`InProcessGateway.ts:3659-3665`): a snapshot epoch is
/// active/runId/terminal only; event contents and truncation do not move it.
pub fn same_turn_epoch(left: &ActiveTurnSnapshot, right: &ActiveTurnSnapshot) -> bool {
    left.active == right.active && left.run_id == right.run_id && left.terminal == right.terminal
}

#[cfg(test)]
mod tests {
    use super::super::replay::TurnReplayOptions;
    use super::super::replay::test_support::{
        ManualTimer, PanicTimer, production_timer_with_exit_flag,
    };
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    const ORACLE: &str = include_str!("../../../tests/fixtures/turn-replay/oracle.json");

    fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
        value.get(name).unwrap_or_else(|| panic!("missing {name}"))
    }
    fn text(value: &JsValue) -> &JsString {
        value.as_string().expect("string")
    }
    fn number(value: &JsValue) -> f64 {
        value.as_number().expect("number")
    }
    fn items(value: &JsValue) -> &[JsValue] {
        match value {
            JsValue::Array(items) => items,
            _ => panic!("expected array"),
        }
    }
    fn rendered(value: &JsValue) -> String {
        stringify_json(value)
            .expect("stringify")
            .expect("JSON value")
    }
    fn coordinator_cases(oracle: &JsValue) -> &[JsValue] {
        items(field(oracle, "coordinatorCases"))
    }
    fn find_coordinator_case<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
        coordinator_cases(oracle)
            .iter()
            .find(|case| text(field(case, "name")).equals_str(name))
            .unwrap_or_else(|| panic!("missing coordinator case {name}"))
    }
    fn session(step: &JsValue) -> JsString {
        text(field(step, "session")).clone()
    }
    fn run(step: &JsValue) -> JsString {
        text(field(step, "run")).clone()
    }
    fn op(step: &JsValue) -> String {
        text(field(step, "op")).to_utf8().expect("UTF-8 op")
    }
    fn options(case_value: &JsValue, timer: &Arc<ManualTimer>) -> TurnReplayOptions {
        let mut options = TurnReplayOptions::new();
        if let Some(configured) = case_value.get("options").and_then(JsValue::as_object) {
            if let Some(value) = configured.get("eventLimit") {
                options = options.with_event_limit(number(value) as usize);
            }
            if let Some(value) = configured.get("byteLimit") {
                options = options.with_byte_limit(number(value) as usize);
            }
            if let Some(value) = configured.get("terminalRetentionMs") {
                options = options.with_terminal_retention_ms(number(value) as u64);
            }
        }
        options.with_timer(timer.clone())
    }
    fn delivered(entries: &[(String, JsValue)]) -> JsValue {
        JsValue::Array(
            entries
                .iter()
                .map(|(sink, event)| {
                    let mut object = JsObject::new();
                    object.insert("sink".into(), JsValue::String(sink.as_str().into()));
                    object.insert("event".into(), event.clone());
                    JsValue::Object(object)
                })
                .collect(),
        )
    }

    fn kind_of(event: &JsValue) -> String {
        event
            .get("type")
            .and_then(JsValue::as_string)
            .and_then(|value| value.to_utf8().ok())
            .unwrap_or_default()
    }

    fn run_coordinator_case(case_value: &JsValue) {
        let name = text(field(case_value, "name"))
            .to_utf8()
            .expect("UTF-8 case name");
        let timer = ManualTimer::new();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(options(case_value, &timer)),
        ));
        let delivered_events = Arc::new(Mutex::new(Vec::<(String, JsValue)>::new()));
        let marker_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // TS keeps every started sink in its map until the case removes it;
        // this port keeps the strong owners here for the whole case, so the
        // golden compares the same sink lifetimes.
        let mut registrations = Vec::new();
        let mut sink_sequence = 0_u64;
        let steps = items(field(case_value, "steps"));
        let expected = items(field(case_value, "results"));
        assert_eq!(steps.len(), expected.len(), "{name}: result count");
        for (index, (step, expected)) in steps.iter().zip(expected).enumerate() {
            let step_op = op(step);
            let actual = match step_op.as_str() {
                "start" => {
                    sink_sequence += 1;
                    let sink = format!("s{sink_sequence}");
                    let log = Arc::clone(&delivered_events);
                    registrations.push(coordinator.start(
                        &session(step),
                        &run(step),
                        move |event| {
                            log.lock().unwrap().push((sink.clone(), event.clone()));
                        },
                    ));
                    JsValue::Null
                }
                // A sink that re-emits from inside itself, recording markers
                // around the nested call so the golden pins the TS return
                // boundary: the inner delivery runs inside the inner `emit`.
                "startNested" => {
                    sink_sequence += 1;
                    let sink = format!("s{sink_sequence}");
                    let log = Arc::clone(&delivered_events);
                    let markers = Arc::clone(&marker_log);
                    let nested = Arc::clone(&coordinator);
                    let nested_key = session(step);
                    registrations.push(coordinator.start(
                        &nested_key.clone(),
                        &run(step),
                        move |event| {
                            log.lock().unwrap().push((sink.clone(), event.clone()));
                            if kind_of(event) == "inner" {
                                markers.lock().unwrap().push("inner-enter".into());
                            }
                            if kind_of(event) == "outer" {
                                markers.lock().unwrap().push("outer-enter".into());
                                let returned = nested.emit(&nested_key, &tick_event("inner"));
                                markers
                                    .lock()
                                    .unwrap()
                                    .push(format!("inner-returned:{returned}"));
                                markers.lock().unwrap().push("outer-exit".into());
                            }
                        },
                    ));
                    JsValue::Null
                }
                // A sink whose own body fails for the inner event: the nested
                // delivery reaches the sink, the failure surfaces at the
                // nested `emit` call site, and the outer sink continues.
                "startThrowing" => {
                    sink_sequence += 1;
                    let sink = format!("s{sink_sequence}");
                    let log = Arc::clone(&delivered_events);
                    let markers = Arc::clone(&marker_log);
                    let nested = Arc::clone(&coordinator);
                    let nested_key = session(step);
                    registrations.push(coordinator.start(
                        &nested_key.clone(),
                        &run(step),
                        move |event| {
                            log.lock().unwrap().push((sink.clone(), event.clone()));
                            if kind_of(event) == "inner" {
                                markers.lock().unwrap().push("inner-sink".into());
                                panic!("inner sink failed");
                            }
                            if kind_of(event) == "outer" {
                                markers.lock().unwrap().push("outer-enter".into());
                                let outcome = catch_unwind(AssertUnwindSafe(|| {
                                    nested.emit(&nested_key, &tick_event("inner"));
                                }));
                                if outcome.is_err() {
                                    markers.lock().unwrap().push("caught-at-inner".into());
                                } else {
                                    markers.lock().unwrap().push("not-reached".into());
                                }
                                markers.lock().unwrap().push("outer-exit".into());
                            }
                        },
                    ));
                    JsValue::Null
                }
                // A sink that catches a failing nested `emit` at the call
                // site, like the TS `try`/`catch` around the same call.
                "startCatching" => {
                    sink_sequence += 1;
                    let sink = format!("s{sink_sequence}");
                    let log = Arc::clone(&delivered_events);
                    let markers = Arc::clone(&marker_log);
                    let nested = Arc::clone(&coordinator);
                    let nested_key = session(step);
                    registrations.push(coordinator.start(
                        &nested_key.clone(),
                        &run(step),
                        move |event| {
                            if kind_of(event) == "trigger" {
                                markers.lock().unwrap().push("trigger-enter".into());
                                let outcome = catch_unwind(AssertUnwindSafe(|| {
                                    nested.emit(&nested_key, &JsValue::Undefined);
                                }));
                                if outcome.is_err() {
                                    markers.lock().unwrap().push("caught".into());
                                } else {
                                    markers.lock().unwrap().push("not-reached".into());
                                }
                                markers.lock().unwrap().push("continued".into());
                            }
                            log.lock().unwrap().push((sink.clone(), event.clone()));
                        },
                    ));
                    JsValue::Null
                }
                "markers" => JsValue::Array(
                    marker_log
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|marker| JsValue::String(marker.as_str().into()))
                        .collect(),
                ),
                "emit" => {
                    let before = delivered_events.lock().unwrap().len();
                    let accepted = coordinator.emit(&session(step), field(step, "event"));
                    let entries = delivered_events.lock().unwrap()[before..].to_vec();
                    let mut result = JsObject::new();
                    result.insert("accepted".into(), JsValue::Bool(accepted));
                    result.insert("delivered".into(), delivered(&entries));
                    JsValue::Object(result)
                }
                "record" => {
                    coordinator.record(&session(step), field(step, "event"));
                    JsValue::Null
                }
                "retainTerminal" => {
                    coordinator.retain_terminal(&session(step), &run(step));
                    JsValue::Null
                }
                "snapshot" => {
                    let mut input = ActiveTurnSnapshotInput::new(session(step));
                    if step.get("includeEvents").and_then(JsValue::as_bool) == Some(false) {
                        input = input.without_events();
                    }
                    coordinator.snapshot(&input).to_json()
                }
                "dispose" => {
                    coordinator.dispose();
                    JsValue::Null
                }
                "advanceTime" => {
                    timer.advance(number(field(step, "ms")) as u64);
                    JsValue::Null
                }
                other => panic!("{name}: unknown coordinator op {other}"),
            };
            assert_eq!(
                rendered(&actual),
                rendered(expected),
                "{name}: step {index} ({step_op})"
            );
        }
    }

    fn run_cases(names: &[&str]) {
        let oracle_value = crate::test_baseline::oracle(ORACLE);
        for name in names {
            run_coordinator_case(find_coordinator_case(&oracle_value, name));
        }
    }

    #[test]
    fn emit_requires_started_sink_matches_ts() {
        run_cases(&["emit-requires-a-started-sink"]);
    }
    #[test]
    fn emit_fills_and_preserves_run_id_matches_ts() {
        run_cases(&[
            "emit-fills-run-and-records-before-delivery",
            "emit-preserves-own-run-and-blank-run-becomes-current",
        ]);
    }
    #[test]
    fn record_does_not_deliver_matches_ts() {
        run_cases(&["record-does-not-deliver"]);
    }
    #[test]
    fn start_replaces_sink_and_buffer_matches_ts() {
        run_cases(&["start-replaces-sink-and-buffer"]);
    }
    #[test]
    fn retain_terminal_removes_sink_matches_ts() {
        run_cases(&["retain-terminal-removes-sink-even-on-run-mismatch"]);
    }
    #[test]
    fn dispose_and_terminal_expiry_match_ts() {
        run_cases(&[
            "dispose-clears-sinks-and-replay",
            "record-after-terminal-keeps-event-without-sink",
            "coordinator-terminal-expiry-uses-default-30s",
        ]);
    }
    /// Regression for codex-r2 P1 #3: a reentrant `emit` from inside the
    /// session's own sink must complete its delivery (and record its markers)
    /// before the nested call returns — exactly the TS return boundary.
    #[test]
    fn reentrant_emit_completes_before_the_outer_sink_returns_matches_ts() {
        run_cases(&["reentrant-emit-delivers-inner-before-outer-returns"]);
    }
    /// Same, for the failure path: the nested `emit` panics (throws in TS)
    /// at the call site, the outer sink catches it and continues, and nothing
    /// is recorded or delivered for the failed event.
    #[test]
    fn reentrant_emit_failure_is_caught_at_the_inner_call_site_matches_ts() {
        run_cases(&["reentrant-emit-failure-is-caught-at-the-inner-call-site"]);
    }
    /// Same boundary, with the failure raised by the inner sink itself: the
    /// inner event is recorded and delivered into the sink, the sink's own
    /// failure surfaces at the nested `emit` call site (caught there in TS and
    /// in the Rust port), and the outer sink finishes.
    #[test]
    fn reentrant_inner_sink_failure_surfaces_at_the_inner_call_site_matches_ts() {
        run_cases(&["reentrant-emit-inner-sink-throw-is-caught-at-the-inner-call-site"]);
    }

    fn coordinator(timer: &Arc<ManualTimer>) -> Arc<TurnEventCoordinator> {
        Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone())),
        ))
    }

    fn snapshot_run(coordinator: &TurnEventCoordinator, key: &JsString) -> Option<JsString> {
        coordinator
            .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
            .run_id
    }

    /// Deterministic interleaving used by the four start/emit/retain/dispose
    /// tests: the first thread parks inside the composite operation, the main
    /// thread starts a competing operation, and the test proves the competitor
    /// cannot finish until the first operation has released the state lock.
    struct Pause {
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        hook: Arc<dyn Fn() + Send + Sync>,
    }

    fn pause() -> Pause {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let hook = Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        });
        Pause {
            entered,
            release,
            hook,
        }
    }

    #[test]
    fn concurrent_starts_keep_replay_and_sink_in_the_same_epoch() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let p = pause();
        coordinator.set_after_start_hook(p.hook.clone());
        let key = JsString::from("s");

        let first = Arc::clone(&coordinator);
        let first_key = key.clone();
        let first_thread = std::thread::spawn(move || {
            first.start(&first_key, &JsString::from("r1"), |_| {});
        });
        p.entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let second = Arc::clone(&coordinator);
        let second_key = key.clone();
        let (done_tx, done) = mpsc::channel();
        let second_thread = std::thread::spawn(move || {
            second.start(&second_key, &JsString::from("r2"), |_| {});
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(150)).is_err(),
            "a concurrent start must wait for the composite state update"
        );
        p.release.send(()).unwrap();
        first_thread.join().unwrap();
        second_thread.join().unwrap();

        assert_eq!(snapshot_run(&coordinator, &key), Some(JsString::from("r2")));
        let delivered = Arc::new(Mutex::new(Vec::<(String, JsValue)>::new()));
        let log = Arc::clone(&delivered);
        let _registration = coordinator.start(&key, &JsString::from("r3"), move |event| {
            log.lock().unwrap().push(("r3".into(), event.clone()));
        });
        assert!(coordinator.emit(&key, &tick_event("e")));
        let entries = delivered.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            field(&entries[0].1, "runId").as_string(),
            Some(&JsString::from("r3"))
        );
    }

    fn tick_event(kind: &str) -> JsValue {
        let mut event = JsObject::new();
        event.insert("type".into(), JsValue::String(kind.into()));
        JsValue::Object(event)
    }

    /// Codex-r1 P1 #1: a start landing after `with_run_id` used to move the
    /// event into the new replay while it was stamped with the old run.
    #[test]
    fn concurrent_start_cannot_split_emit_run_id_from_its_record() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<(String, JsValue)>::new()));
        let log = Arc::clone(&delivered);
        let _registration = coordinator.start(&key, &JsString::from("r1"), move |event| {
            log.lock().unwrap().push(("r1".into(), event.clone()));
        });

        let p = pause();
        coordinator.set_after_emit_run_id_hook(p.hook.clone());
        let emitting = Arc::clone(&coordinator);
        let emit_key = key.clone();
        let emit_thread = std::thread::spawn(move || {
            emitting.emit(&emit_key, &tick_event("e"));
        });
        p.entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let starting = Arc::clone(&coordinator);
        let start_key = key.clone();
        let (done_tx, done) = mpsc::channel();
        let start_thread = std::thread::spawn(move || {
            starting.start(&start_key, &JsString::from("r2"), |_| {});
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(150)).is_err(),
            "start must wait for emit's runId+record segment"
        );
        p.release.send(()).unwrap();
        emit_thread.join().unwrap();
        start_thread.join().unwrap();

        let entries = delivered.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            field(&entries[0].1, "runId").as_string(),
            Some(&JsString::from("r1")),
            "the delivered event belongs to the replay that recorded it"
        );
        let after = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert_eq!(after.run_id, Some(JsString::from("r2")));
        assert!(after.events.is_empty(), "the new run starts empty");
    }

    #[test]
    fn concurrent_retain_waits_for_the_composite_state_update() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        coordinator.start(&key, &JsString::from("r1"), |_| {});
        let p = pause();
        coordinator.set_after_retain_hook(p.hook.clone());
        let retaining = Arc::clone(&coordinator);
        let retain_key = key.clone();
        let retain_thread = std::thread::spawn(move || {
            retaining.retain_terminal(&retain_key, &JsString::from("r1"));
        });
        p.entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let starting = Arc::clone(&coordinator);
        let start_key = key.clone();
        let (done_tx, done) = mpsc::channel();
        let start_thread = std::thread::spawn(move || {
            starting.start(&start_key, &JsString::from("r2"), |_| {});
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(150)).is_err(),
            "start must wait for retain's sink+replay transition"
        );
        p.release.send(()).unwrap();
        retain_thread.join().unwrap();
        start_thread.join().unwrap();
        assert_eq!(snapshot_run(&coordinator, &key), Some(JsString::from("r2")));
        assert_eq!(timer.pending(&key), 0, "the r1 terminal timer was replaced");
    }

    #[test]
    fn concurrent_dispose_waits_and_leaves_a_consistent_start() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        coordinator.start(&key, &JsString::from("r1"), |_| {});
        let p = pause();
        coordinator.set_after_dispose_hook(p.hook.clone());
        let disposing = Arc::clone(&coordinator);
        let dispose_thread = std::thread::spawn(move || disposing.dispose());
        p.entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let starting = Arc::clone(&coordinator);
        let start_key = key.clone();
        let (done_tx, done) = mpsc::channel();
        let start_thread = std::thread::spawn(move || {
            starting.start(&start_key, &JsString::from("r2"), |_| {});
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(150)).is_err(),
            "start must wait for dispose's sink+replay clear"
        );
        p.release.send(()).unwrap();
        dispose_thread.join().unwrap();
        start_thread.join().unwrap();

        let delivered = Arc::new(Mutex::new(Vec::<(String, JsValue)>::new()));
        let log = Arc::clone(&delivered);
        let registration = coordinator.start(&key, &JsString::from("r3"), move |event| {
            log.lock().unwrap().push(("r3".into(), event.clone()));
        });
        assert!(coordinator.emit(&key, &tick_event("e")));
        let entries = delivered.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            field(&entries[0].1, "runId").as_string(),
            Some(&JsString::from("r3")),
            "a start after dispose gets a live replay again"
        );
        drop(registration);
    }

    /// A sink may re-enter the coordinator; delivery must not deadlock and the
    /// nested emit must be delivered after the current event.
    #[test]
    fn reentrant_sink_delivers_in_record_order() {
        let timer = ManualTimer::new();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone())),
        ));
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = Arc::clone(&delivered);
        let nested = Arc::clone(&coordinator);
        let nested_key = key.clone();
        let _registration = coordinator.start(&key, &JsString::from("r1"), move |event| {
            let kind = text(field(event, "type")).to_utf8().unwrap();
            log.lock().unwrap().push(kind.clone());
            if kind == "outer" {
                let mut inner = JsObject::new();
                inner.insert("type".into(), "inner".into());
                nested.emit(&nested_key, &JsValue::Object(inner));
            }
        });
        assert!(coordinator.emit(&key, &tick_event("outer")));
        assert_eq!(*delivered.lock().unwrap(), vec!["outer", "inner"]);
    }

    /// Regression for codex-r3 P1 #1: a synchronous reentrant `emit` must not
    /// jump the events a concurrent emitter already recorded — the live sink
    /// order and the replay's record order must stay the same.
    #[test]
    fn reentrant_emit_waits_for_concurrently_queued_events() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (outer_entered_tx, outer_entered) = mpsc::channel();
        let (release_outer_tx, release_outer) = mpsc::channel::<()>();
        let release_outer = Arc::new(Mutex::new(release_outer));
        let _registration = {
            let log = Arc::clone(&delivered);
            let entered = outer_entered_tx;
            let release = Arc::clone(&release_outer);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "outer" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    // Thread B's e2 is recorded and queued by now; the
                    // reentrant emit must deliver it before its own event.
                    nested.emit(&nested_key, &tick_event("inner"));
                }
            })
        };
        let outer = Arc::clone(&coordinator);
        let outer_key = key.clone();
        let outer_thread = std::thread::spawn(move || {
            outer.emit(&outer_key, &tick_event("outer"));
        });
        outer_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // A concurrent emitter records e2, queues it behind the in-flight
        // outer, and returns true while the drainer is still inside sink.
        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        release_outer_tx.send(()).unwrap();
        outer_thread.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["outer".to_string(), "e2".to_string(), "inner".to_string()],
            "live delivery order must equal the record order"
        );
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        let order: Vec<String> = snapshot.events.iter().map(kind_of).collect();
        assert_eq!(
            order,
            vec!["outer".to_string(), "e2".to_string(), "inner".to_string()],
            "replay records in the same order the sink observed"
        );
    }

    /// Regression for codex-r3 P1 #2 / grok-r3 P1 #1: a sink left over from
    /// before `dispose` + `start` must not reach the new sink through the
    /// reentrant path. Its stale drain marker takes the ordinary queue path
    /// behind the new drainer.
    #[test]
    fn stale_reentrant_marker_goes_through_the_restarted_queue() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let reentered = Arc::new(AtomicBool::new(false));
        let in_sink = Arc::new(AtomicBool::new(false));

        // The old sink parks; when released it re-enters the coordinator with
        // a drain marker the restart has already invalidated.
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _old_registration = {
            let log = Arc::clone(&delivered);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e1" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    let accepted = nested.emit(&nested_key, &tick_event("inner"));
                    assert!(accepted, "the stale re-emit is still accepted");
                }
            })
        };
        let old = Arc::clone(&coordinator);
        let old_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            old.emit(&old_key, &tick_event("e1"));
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // dispose + restart; the new sink detects any concurrent entry.
        coordinator.dispose();
        let (new_entered_tx, new_entered) = mpsc::channel();
        let (release_new_tx, release_new) = mpsc::channel::<()>();
        let release_new = Arc::new(Mutex::new(release_new));
        let _new_registration = {
            let log = Arc::clone(&delivered);
            let in_sink = Arc::clone(&in_sink);
            let reentered = Arc::clone(&reentered);
            let entered = new_entered_tx;
            let release = Arc::clone(&release_new);
            coordinator.start(&key, &JsString::from("r2"), move |event| {
                if in_sink.swap(true, Ordering::SeqCst) {
                    reentered.store(true, Ordering::SeqCst);
                }
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e2" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
                in_sink.store(false, Ordering::SeqCst);
            })
        };

        // This thread becomes the new drainer and parks inside sink(e2).
        let parked = Arc::clone(&coordinator);
        let parked_key = key.clone();
        let parking = std::thread::spawn(move || {
            parked.emit(&parked_key, &tick_event("e2"));
        });
        new_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // A third emitter queues e3 behind the in-flight e2 and returns true.
        let third = Arc::clone(&coordinator);
        let third_key = key.clone();
        let (queued_tx, queued) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = third.emit(&third_key, &tick_event("e3"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued.recv_timeout(Duration::from_secs(5)).unwrap());

        // Now the old sink returns and re-emits: its stale marker must not
        // call the new sink directly; inner is queued behind e3.
        release_old_tx.send(()).unwrap();
        old_thread.join().unwrap();
        release_new_tx.send(()).unwrap();
        parking.join().unwrap();
        queuing.join().unwrap();

        assert!(
            !reentered.load(Ordering::SeqCst),
            "the stale marker reached the new sink directly"
        );
        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                "e1".to_string(),
                "e2".to_string(),
                "e3".to_string(),
                "inner".to_string()
            ],
            "the new drainer delivers e3 and inner in record order"
        );
    }

    /// Regression for codex-r3 P2 #4: a nested delivery that fails inside
    /// the sink surfaces its panic at the reentrant `emit` call site, and the
    /// outer drainer keeps the queue — events queued behind the failing one
    /// are still delivered.
    #[test]
    fn nested_delivery_panic_keeps_the_queue_for_the_outer_drainer() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (outer_entered_tx, outer_entered) = mpsc::channel();
        let (release_outer_tx, release_outer) = mpsc::channel::<()>();
        let release_outer = Arc::new(Mutex::new(release_outer));
        let _registration = {
            let log = Arc::clone(&delivered);
            let entered = outer_entered_tx;
            let release = Arc::clone(&release_outer);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e2" {
                    panic!("the concurrent emitter's event fails in the sink");
                }
                if kind_of(event) == "outer" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    // e2 is queued by now; the nested drain delivers it first
                    // and its panic must be caught here, leaving inner queued.
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        nested.emit(&nested_key, &tick_event("inner"));
                    }));
                    assert!(outcome.is_err(), "the nested sink panic surfaced here");
                }
            })
        };
        let outer = Arc::clone(&coordinator);
        let outer_key = key.clone();
        let outer_thread = std::thread::spawn(move || {
            outer.emit(&outer_key, &tick_event("outer"));
        });
        outer_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        release_outer_tx.send(()).unwrap();
        outer_thread.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["outer".to_string(), "e2".to_string(), "inner".to_string()],
            "the outer drainer delivers inner after the caught nested panic"
        );
        // The queue is fully released: a later emit still delivers.
        assert!(coordinator.emit(&key, &tick_event("later")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                "outer".to_string(),
                "e2".to_string(),
                "inner".to_string(),
                "later".to_string()
            ]
        );
    }

    /// Regression for codex-r4 P1 #1: the nested drain's return boundary is
    /// its own event's completed delivery — an event another thread records
    /// while the inner sink runs (the "tail") stays queued for the outer
    /// drainer, so a tail sink panic can never surface at the inner `emit`
    /// call site and make the successful inner delivery look failed.
    #[test]
    fn nested_drain_returns_at_the_inner_call_boundary_not_the_tail() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let inner_ok = Arc::new(Mutex::new(false));
        let (inner_entered_tx, inner_entered) = mpsc::channel();
        let (release_inner_tx, release_inner) = mpsc::channel::<()>();
        let release_inner = Arc::new(Mutex::new(release_inner));
        let _registration = {
            let log = Arc::clone(&delivered);
            let inner_ok = Arc::clone(&inner_ok);
            let entered = inner_entered_tx;
            let release = Arc::clone(&release_inner);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(kind.clone());
                match kind.as_str() {
                    "outer" => {
                        // These assertions must not escape into the outer
                        // drain: a panic from this closure would surface at
                        // emit(outer) and mask itself as the expected tail
                        // panic. Record the outcome for the main thread.
                        let outcome = catch_unwind(AssertUnwindSafe(|| {
                            let returned = nested.emit(&nested_key, &tick_event("inner"));
                            assert!(returned, "the inner emit must return successfully");
                            assert_eq!(
                                *log.lock().unwrap(),
                                vec!["outer".to_string(), "inner".to_string()],
                                "the inner emit returns before the tail is delivered"
                            );
                        }));
                        *inner_ok.lock().unwrap() = outcome.is_ok();
                    }
                    "inner" => {
                        // Park inside the inner sink so the concurrent
                        // emitter's tail lands in the queue behind us.
                        entered.send(()).unwrap();
                        release.lock().unwrap().recv().unwrap();
                    }
                    "tail" => panic!("the tail sink fails"),
                    _ => {}
                }
            })
        };
        let outer = Arc::clone(&coordinator);
        let outer_key = key.clone();
        let outer_thread = std::thread::spawn(move || {
            catch_unwind(AssertUnwindSafe(|| {
                outer.emit(&outer_key, &tick_event("outer"));
            }))
            .is_err()
        });
        inner_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // The tail is recorded and accepted while the drainer is inside the
        // inner sink; its emitter returns immediately.
        let tail = Arc::clone(&coordinator);
        let tail_key = key.clone();
        let (tail_tx, tail_done) = mpsc::channel();
        let tail_thread = std::thread::spawn(move || {
            let accepted = tail.emit(&tail_key, &tick_event("tail"));
            let _ = tail_tx.send(accepted);
        });
        assert!(tail_done.recv_timeout(Duration::from_secs(5)).unwrap());

        release_inner_tx.send(()).unwrap();
        assert!(
            outer_thread.join().unwrap(),
            "the tail panic surfaces at the outer emit, not the inner one"
        );
        assert!(
            *inner_ok.lock().unwrap(),
            "the inner emit returned and its assertions held before the tail was delivered"
        );
        tail_thread.join().unwrap();
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["outer".to_string(), "inner".to_string(), "tail".to_string()],
            "the outer drainer delivers the tail after the inner call returned"
        );
    }

    /// Regression for grok-r4 P2 #1: a nested drain that finds the queue empty
    /// (its own event was consumed by a deeper nested drain) must not hand the
    /// queue back — the outer drainer still owns it. Releasing would let
    /// another thread claim the drain and call the sink while the outer sink
    /// call is still on the stack.
    #[test]
    fn nested_drain_does_not_release_the_outer_drainers_queue() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (e1_entered_tx, e1_entered) = mpsc::channel();
        let (release_e1_tx, release_e1) = mpsc::channel::<()>();
        let release_e1 = Arc::new(Mutex::new(release_e1));
        let (parked_tx, parked) = mpsc::channel();
        let (release_parked_tx, release_parked) = mpsc::channel::<()>();
        let release_parked = Arc::new(Mutex::new(release_parked));
        let _registration = {
            let log = Arc::clone(&delivered);
            let entered = e1_entered_tx;
            let parked = parked_tx;
            let gate = Arc::clone(&release_e1);
            let release = Arc::clone(&release_parked);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(kind.clone());
                match kind.as_str() {
                    "e1" => {
                        entered.send(()).unwrap();
                        gate.lock().unwrap().recv().unwrap();
                        let outcome = catch_unwind(AssertUnwindSafe(|| {
                            nested.emit(&nested_key, &tick_event("inner"))
                        }));
                        assert!(
                            outcome.expect("the nested emit returns cleanly"),
                            "the nested emit succeeded"
                        );
                        assert_eq!(
                            *log.lock().unwrap(),
                            vec![
                                "e1".to_string(),
                                "e2".to_string(),
                                "inner".to_string(),
                                "inner2".to_string()
                            ],
                            "the whole nested chain delivered in record order"
                        );
                        parked.send(()).unwrap();
                        release.lock().unwrap().recv().unwrap();
                    }
                    "e2" => {
                        let outcome = catch_unwind(AssertUnwindSafe(|| {
                            nested.emit(&nested_key, &tick_event("inner2"))
                        }));
                        assert!(
                            outcome.expect("the deeper nested emit returns cleanly"),
                            "the deeper nested emit succeeded"
                        );
                    }
                    _ => {}
                }
            })
        };
        let outer = Arc::clone(&coordinator);
        let outer_key = key.clone();
        let outer_thread = std::thread::spawn(move || {
            outer.emit(&outer_key, &tick_event("e1"));
        });
        e1_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // e2 is queued behind the in-flight e1 before the reentrant emit, so
        // the nested drain must deliver it ahead of its own event.
        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());
        release_e1_tx.send(()).unwrap();

        parked.recv_timeout(Duration::from_secs(5)).unwrap();
        // The outer sink call is still on the stack here; the queue is empty
        // but must stay owned, so a fresh emit is accepted and parked, never
        // delivered by another thread's drainer.
        let third = Arc::clone(&coordinator);
        let third_key = key.clone();
        let (third_tx, third_done) = mpsc::channel();
        let third_thread = std::thread::spawn(move || {
            let accepted = third.emit(&third_key, &tick_event("e3"));
            let _ = third_tx.send(accepted);
        });
        assert!(third_done.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                "e1".to_string(),
                "e2".to_string(),
                "inner".to_string(),
                "inner2".to_string()
            ],
            "e3 must not be delivered while the outer sink call is parked"
        );

        release_parked_tx.send(()).unwrap();
        outer_thread.join().unwrap();
        queuing.join().unwrap();
        third_thread.join().unwrap();
        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                "e1".to_string(),
                "e2".to_string(),
                "inner".to_string(),
                "inner2".to_string(),
                "e3".to_string()
            ],
            "the outer drainer delivers e3 after its sink call returns"
        );
    }

    /// Regression for codex-r2 P1 #2 (normal return): a drainer that is still
    /// inside its sink when `dispose` + `start` restart the session must stop
    /// before touching the restarted queue — it may neither re-enter the new
    /// sink concurrently nor drop the new generation's events.
    #[test]
    fn disposed_drainer_cannot_touch_the_restarted_delivery_queue() {
        let timer = ManualTimer::new();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer)),
        ));
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let in_sink = Arc::new(AtomicBool::new(false));
        let reentered = Arc::new(AtomicBool::new(false));

        // The old sink parks; the old drainer stays inside it across dispose.
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _old_registration = {
            let log = Arc::clone(&delivered);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                entered.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
            })
        };
        let first = Arc::clone(&coordinator);
        let first_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            first.emit(&first_key, &tick_event("e1"));
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // dispose + restart; the new sink detects concurrent entry.
        coordinator.dispose();
        let (new_entered_tx, new_entered) = mpsc::channel();
        let (release_new_tx, release_new) = mpsc::channel::<()>();
        let release_new = Arc::new(Mutex::new(release_new));
        let _new_registration = {
            let log = Arc::clone(&delivered);
            let in_sink = Arc::clone(&in_sink);
            let reentered = Arc::clone(&reentered);
            let entered = new_entered_tx;
            let release = Arc::clone(&release_new);
            coordinator.start(&key, &JsString::from("r2"), move |event| {
                if in_sink.swap(true, Ordering::SeqCst) {
                    reentered.store(true, Ordering::SeqCst);
                }
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e2" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
                in_sink.store(false, Ordering::SeqCst);
            })
        };

        // This thread becomes the new drainer and parks inside sink(e2).
        let parked = Arc::clone(&coordinator);
        let parked_key = key.clone();
        let parking = std::thread::spawn(move || {
            parked.emit(&parked_key, &tick_event("e2"));
        });
        new_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // A third emitter queues e3 behind the in-flight e2 and returns true.
        let third = Arc::clone(&coordinator);
        let third_key = key.clone();
        let (queued_tx, queued) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = third.emit(&third_key, &tick_event("e3"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued.recv_timeout(Duration::from_secs(5)).unwrap());

        // Now the old sink returns: its drainer must exit without touching the
        // restarted queue, and the new drainer delivers e3 after e2.
        release_old_tx.send(()).unwrap();
        old_thread.join().unwrap();
        release_new_tx.send(()).unwrap();
        parking.join().unwrap();
        queuing.join().unwrap();

        assert!(
            !reentered.load(Ordering::SeqCst),
            "the old drainer re-entered the new sink"
        );
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e2".to_string(), "e3".to_string()],
            "e2 and e3 are delivered by the new drainer in order"
        );
    }

    /// Same race, panic variant: the old sink panics instead of returning, so
    /// its cleanup guard must not drop the restarted session's queued e3.
    #[test]
    fn panicking_disposed_drainer_keeps_the_restarted_queue_intact() {
        let timer = ManualTimer::new();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer)),
        ));
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));

        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _old_registration = {
            let log = Arc::clone(&delivered);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                entered.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
                panic!("the old sink fails");
            })
        };
        let first = Arc::clone(&coordinator);
        let first_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            catch_unwind(AssertUnwindSafe(|| {
                first.emit(&first_key, &tick_event("e1"));
            }))
            .is_err()
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        coordinator.dispose();
        let (new_entered_tx, new_entered) = mpsc::channel();
        let (release_new_tx, release_new) = mpsc::channel::<()>();
        let release_new = Arc::new(Mutex::new(release_new));
        let _new_registration = {
            let log = Arc::clone(&delivered);
            let entered = new_entered_tx;
            let release = Arc::clone(&release_new);
            coordinator.start(&key, &JsString::from("r2"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e2" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
            })
        };
        let parked = Arc::clone(&coordinator);
        let parked_key = key.clone();
        let parking = std::thread::spawn(move || {
            parked.emit(&parked_key, &tick_event("e2"));
        });
        new_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let third = Arc::clone(&coordinator);
        let third_key = key.clone();
        let (queued_tx, queued) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = third.emit(&third_key, &tick_event("e3"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued.recv_timeout(Duration::from_secs(5)).unwrap());

        // The old sink panics; its guard must leave the restarted queue alone.
        release_old_tx.send(()).unwrap();
        assert!(
            old_thread.join().unwrap(),
            "the old sink panic surfaced from the old emit"
        );
        release_new_tx.send(()).unwrap();
        parking.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e2".to_string(), "e3".to_string()],
            "e3 queued behind the panicking old drainer must still be delivered"
        );
    }

    /// Registered divergence A7.2-COORDINATOR-DELIVERY-001(a): on TS the last
    /// sink call always precedes the `retainTerminal` transition; here the
    /// in-flight drainer still delivers an event that was accepted — and whose
    /// emitter already returned true — to the removed sink after
    /// `retain_terminal` returned.
    #[test]
    fn accepted_events_reach_the_old_sink_after_retain_terminal_returns() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let retain_returned = Arc::new(AtomicBool::new(false));
        let e2_after_retain = Arc::new(Mutex::new(false));
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _registration = {
            let log = Arc::clone(&delivered);
            let retain_returned = Arc::clone(&retain_returned);
            let e2_after_retain = Arc::clone(&e2_after_retain);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(kind.clone());
                if kind == "e1" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
                if kind == "e2" {
                    *e2_after_retain.lock().unwrap() = retain_returned.load(Ordering::SeqCst);
                }
            })
        };
        let old = Arc::clone(&coordinator);
        let old_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            old.emit(&old_key, &tick_event("e1"));
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // e2 is accepted and queued while the drainer is still inside sink(e1).
        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        coordinator.retain_terminal(&key, &JsString::from("r1"));
        retain_returned.store(true, Ordering::SeqCst);
        release_old_tx.send(()).unwrap();
        old_thread.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e2".to_string()],
            "registered divergence A7.2-COORDINATOR-DELIVERY-001(a): the in-flight drainer still delivers e2 through the sink removed by retain_terminal"
        );
        assert!(
            *e2_after_retain.lock().unwrap(),
            "e2 must be delivered after retain_terminal returned"
        );
        assert!(
            !coordinator.emit(&key, &tick_event("e3")),
            "later emits find no sink after retain_terminal"
        );
    }

    /// Registered divergence A7.2-COORDINATOR-DELIVERY-001(a), the replacing
    /// `start` variant: the event accepted before the replacement is still
    /// delivered to the replaced sink after `start` returned.
    #[test]
    fn accepted_events_reach_the_old_sink_after_a_replacing_start_returns() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let start_returned = Arc::new(AtomicBool::new(false));
        let e2_after_start = Arc::new(Mutex::new(false));
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _old_registration = {
            let log = Arc::clone(&delivered);
            let start_returned = Arc::clone(&start_returned);
            let e2_after_start = Arc::clone(&e2_after_start);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(("old".into(), kind.clone()));
                if kind == "e1" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
                if kind == "e2" {
                    *e2_after_start.lock().unwrap() = start_returned.load(Ordering::SeqCst);
                }
            })
        };
        let old = Arc::clone(&coordinator);
        let old_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            old.emit(&old_key, &tick_event("e1"));
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        let _new_registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&key, &JsString::from("r2"), move |event| {
                log.lock().unwrap().push(("new".into(), kind_of(event)));
            })
        };
        start_returned.store(true, Ordering::SeqCst);
        release_old_tx.send(()).unwrap();
        old_thread.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                ("old".to_string(), "e1".to_string()),
                ("old".to_string(), "e2".to_string()),
            ],
            "registered divergence A7.2-COORDINATOR-DELIVERY-001(a): the in-flight drainer still delivers e2 through the replaced sink"
        );
        assert!(
            *e2_after_start.lock().unwrap(),
            "e2 must be delivered after the replacing start returned"
        );
        assert!(coordinator.emit(&key, &tick_event("e3")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec![
                ("old".to_string(), "e1".to_string()),
                ("old".to_string(), "e2".to_string()),
                ("new".to_string(), "e3".to_string()),
            ],
            "later emits reach the replacement sink"
        );
    }

    /// Registered divergence A7.2-COORDINATOR-DELIVERY-001(b): events that
    /// were accepted before `dispose` but not yet delivered are dropped
    /// without any signal to their emitters; only the entry the in-flight
    /// drainer already popped still travels with its sink handle.
    #[test]
    fn dispose_drops_events_accepted_but_not_yet_delivered() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _registration = {
            let log = Arc::clone(&delivered);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(kind.clone());
                if kind == "e1" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                }
            })
        };
        let old = Arc::clone(&coordinator);
        let old_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            old.emit(&old_key, &tick_event("e1"));
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // e2 is accepted — its emitter returns true — and queued behind the
        // in-flight e1.
        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        coordinator.dispose();
        release_old_tx.send(()).unwrap();
        old_thread.join().unwrap();
        queuing.join().unwrap();

        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string()],
            "registered divergence A7.2-COORDINATOR-DELIVERY-001(b): dispose drops the accepted-but-undelivered e2 without any signal"
        );
        assert!(
            !coordinator.emit(&key, &tick_event("e3")),
            "dispose left no sink for later emits"
        );
    }

    /// Registered divergence A7.2-COORDINATOR-DELIVERY-001(b), the panic
    /// variant: the failing drainer's cleanup drops the events queued behind
    /// it — their emitters already returned true — and a later emit starts a
    /// fresh delivery instead of resurrecting them.
    #[test]
    fn sink_panic_drops_events_queued_behind_it() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (old_entered_tx, old_entered) = mpsc::channel();
        let (release_old_tx, release_old) = mpsc::channel::<()>();
        let release_old = Arc::new(Mutex::new(release_old));
        let _registration = {
            let log = Arc::clone(&delivered);
            let entered = old_entered_tx;
            let release = Arc::clone(&release_old);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                let kind = kind_of(event);
                log.lock().unwrap().push(kind.clone());
                if kind == "e1" {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    panic!("the sink fails for e1");
                }
            })
        };
        let old = Arc::clone(&coordinator);
        let old_key = key.clone();
        let old_thread = std::thread::spawn(move || {
            catch_unwind(AssertUnwindSafe(|| {
                old.emit(&old_key, &tick_event("e1"));
            }))
            .is_err()
        });
        old_entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // e2 is accepted — its emitter returns true — and queued behind the
        // event whose delivery will fail.
        let queued = Arc::clone(&coordinator);
        let queued_key = key.clone();
        let (queued_tx, queued_done) = mpsc::channel();
        let queuing = std::thread::spawn(move || {
            let accepted = queued.emit(&queued_key, &tick_event("e2"));
            let _ = queued_tx.send(accepted);
        });
        assert!(queued_done.recv_timeout(Duration::from_secs(5)).unwrap());

        release_old_tx.send(()).unwrap();
        assert!(
            old_thread.join().unwrap(),
            "the e1 sink panic surfaced at its own emit"
        );
        queuing.join().unwrap();

        assert!(
            coordinator.emit(&key, &tick_event("e3")),
            "the sink stays registered after the panic"
        );
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e3".to_string()],
            "registered divergence A7.2-COORDINATOR-DELIVERY-001(b): the panicking drainer drops the accepted-but-undelivered e2"
        );
    }

    /// Regression for codex-r2 P2 #6 (gate2 boundary P2a): a non-serializable
    /// event panics like the TS throw at the call site, but must not poison
    /// the coordinator lock for every other session.
    #[test]
    fn non_serializable_events_through_the_coordinator_do_not_poison_other_sessions() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let other = JsString::from("other");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let _key_registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
            })
        };
        let _other_registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&other, &JsString::from("r2"), move |event| {
                log.lock().unwrap().push(kind_of(event));
            })
        };

        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                coordinator.record(&key, &JsValue::Undefined);
            }))
            .is_err(),
            "undefined has no JSON text in TS either"
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                coordinator.emit(&key, &JsValue::Undefined);
            }))
            .is_err(),
            "emit throws like the TS record call inside it"
        );

        // Every session still works after both panics.
        assert!(coordinator.emit(&other, &tick_event("later")));
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(other.clone()));
        assert!(snapshot.active);
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(*delivered.lock().unwrap(), vec!["later".to_string()]);
    }

    fn panic_coordinator(timer: &Arc<PanicTimer>) -> Arc<TurnEventCoordinator> {
        Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone())),
        ))
    }

    /// Regression for #805 (probe cell C1): a timer panic surfacing through
    /// `start` throws after `replay.start`'s clearTerminal and before the
    /// sink registration, so the old terminal replay survives, `emit`
    /// rejects the session (its sink was removed by the earlier retain), and
    /// every other session keeps delivering — the composite-state lock is
    /// never poisoned.
    #[test]
    fn timer_panic_in_coordinator_start_keeps_state_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let coordinator = panic_coordinator(&timer);
        let key = JsString::from("s");
        let other = JsString::from("other");
        let run = JsString::from("r1");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let _key_registration = coordinator.start(&key, &run, |_| {});
        coordinator.record(&key, &tick_event("e0"));
        let _other_registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&other, &run, move |event| {
                log.lock().unwrap().push(kind_of(event));
            })
        };
        // The prior retain gives the panicking start a pending terminal
        // timer, the same setup where a throwing host clearTimeout makes
        // the TS call throw.
        coordinator.retain_terminal(&key, &run);

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            coordinator.start(&key, &JsString::from("r2"), |_| {})
        }));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !coordinator.state.is_poisoned(),
            "the panic must unwind outside the composite-state guard"
        );

        // TS C1: the old terminal replay survives; the failed start never
        // registered its sink, so emit rejects the session.
        let same = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(same.terminal);
        assert_eq!(same.run_id.as_ref(), Some(&run));
        assert!(!coordinator.emit(&key, &tick_event("e1")));
        // The other session still delivers, and explicit teardown of it
        // completes instead of panicking on a poisoned lock.
        assert!(coordinator.emit(&other, &tick_event("e2")));
        assert_eq!(*delivered.lock().unwrap(), vec!["e2".to_string()]);
        coordinator.unregister(&other);
        assert!(!coordinator.emit(&other, &tick_event("e3")));
        // A healthy retry starts the same session.
        let _again = coordinator.start(&key, &JsString::from("r3"), |_| {});
        let recovered = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(recovered.active);
        assert_eq!(recovered.run_id.as_ref(), Some(&JsString::from("r3")));
    }

    /// Regression for #805 (probe cell C2): `retain_terminal` removes the
    /// sink before the replay transition, so a panicking `set` leaves the
    /// session terminal-but-unscheduled while every other session keeps
    /// working.
    #[test]
    fn timer_panic_in_coordinator_retain_keeps_state_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let coordinator = panic_coordinator(&timer);
        let key = JsString::from("s");
        let other = JsString::from("other");
        let run = JsString::from("r1");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let _key_registration = coordinator.start(&key, &run, |_| {});
        coordinator.record(&key, &tick_event("e0"));
        let _other_registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&other, &run, move |event| {
                log.lock().unwrap().push(kind_of(event));
            })
        };

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| coordinator.retain_terminal(&key, &run)));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !coordinator.state.is_poisoned(),
            "the panic must unwind outside the composite-state guard"
        );

        // TS C2: the sink was removed and the replay is terminal, but no
        // timer was registered, so the terminal replay never expires.
        assert!(!coordinator.emit(&key, &tick_event("e1")));
        let same = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(same.terminal);
        assert_eq!(same.run_id.as_ref(), Some(&run));
        timer.advance(30_000);
        assert!(
            coordinator
                .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
                .terminal,
            "a terminal replay without a timer never expires on its own"
        );
        // The other session delivers and tears down normally.
        assert!(coordinator.emit(&other, &tick_event("e2")));
        assert_eq!(*delivered.lock().unwrap(), vec!["e2".to_string()]);
        coordinator.unregister(&other);
        // A healthy retry terminalizes and expires the stranded replay.
        coordinator.retain_terminal(&key, &run);
        timer.advance(30_000);
        let expired = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(!expired.terminal);
        assert!(expired.run_id.is_none());
    }

    /// Regression for #805 (probe cell C3): `dispose` clears the sinks and
    /// drain tokens before the replay store's `cancel_all`, so a panicking
    /// timer leaves both replays in place while `emit` already rejects; a
    /// healthy retry clears everything.
    #[test]
    fn timer_panic_in_coordinator_dispose_keeps_state_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let coordinator = panic_coordinator(&timer);
        let key = JsString::from("s");
        let other = JsString::from("other");
        let run = JsString::from("r1");
        let _key_registration = coordinator.start(&key, &run, |_| {});
        coordinator.record(&key, &tick_event("e0"));
        coordinator.retain_terminal(&key, &run);
        let _other_registration = coordinator.start(&other, &run, |_| {});
        coordinator.record(&other, &tick_event("e1"));

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| coordinator.dispose()));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !coordinator.state.is_poisoned(),
            "the panic must unwind outside the composite-state guard"
        );

        // TS C3: the sinks were cleared, the replays were not.
        assert!(!coordinator.emit(&other, &tick_event("e2")));
        let terminal = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(terminal.terminal);
        assert_eq!(terminal.run_id.as_ref(), Some(&run));
        let active = coordinator.snapshot(&ActiveTurnSnapshotInput::new(other.clone()));
        assert!(active.active);
        assert_eq!(active.run_id.as_ref(), Some(&run));
        // Explicit teardown and a healthy retry both complete.
        coordinator.unregister(&other);
        coordinator.dispose();
        assert!(
            coordinator
                .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
                .run_id
                .is_none()
        );
        assert!(
            coordinator
                .snapshot(&ActiveTurnSnapshotInput::new(other.clone()))
                .run_id
                .is_none()
        );
    }

    /// Regression for #805 (matrix cell I1): the panic must not wedge the
    /// composite-state lock while another session's drainer is parked inside
    /// its sink. The drainer finishes within the bound once released, its
    /// cleanup releases the queue, and a later emit claims the drain and
    /// delivers immediately.
    #[test]
    fn timer_panic_during_an_in_flight_delivery_keeps_observers_bounded() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let coordinator = panic_coordinator(&timer);
        let key = JsString::from("s");
        let other = JsString::from("other");
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let _key_registration = coordinator.start(&key, &JsString::from("r1"), move |_event| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        });
        let emitting = Arc::clone(&coordinator);
        let emit_key = key.clone();
        let (done_tx, done) = mpsc::channel();
        let emit_thread = std::thread::spawn(move || {
            assert!(emitting.emit(&emit_key, &tick_event("e0")));
            let _ = done_tx.send(());
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // The drainer is parked inside its sink, holding no lock; a timer
        // panic in another session's retain_terminal must surface without
        // wedging the coordinator state.
        let _other_registration = coordinator.start(&other, &JsString::from("r2"), |_| {});
        coordinator.record(&other, &tick_event("e1"));
        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            coordinator.retain_terminal(&other, &JsString::from("r2"))
        }));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !coordinator.state.is_poisoned(),
            "the panic must unwind outside the coordinator state guard"
        );

        // The composite-state lock is free: an unrelated API call completes
        // while the drainer is still parked.
        let unregistering = Arc::clone(&coordinator);
        let unregister_key = key.clone();
        let (freed_tx, freed) = mpsc::channel();
        let unregister_thread = std::thread::spawn(move || {
            unregistering.unregister(&unregister_key);
            let _ = freed_tx.send(());
        });
        freed
            .recv_timeout(Duration::from_secs(2))
            .expect("the coordinator state must not be wedged by the panic");
        unregister_thread.join().unwrap();

        // The parked drainer finishes once released, within the bound.
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2))
            .expect("the in-flight delivery must finish after the panic");
        emit_thread.join().unwrap();

        // The queue was released by the drainer's cleanup: a new emit claims
        // the drain and delivers synchronously.
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = Arc::clone(&delivered);
        let _again = coordinator.start(&key, &JsString::from("r3"), move |event| {
            log.lock().unwrap().push(kind_of(event));
        });
        assert!(coordinator.emit(&key, &tick_event("e2")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e2".to_string()],
            "the drain queue must be free for the next drainer"
        );
    }

    /// Regression for #805 (matrix cell P1): a coordinator whose
    /// composite-state lock was poisoned by a panic elsewhere still serves
    /// every session — emit delivers through the recovered guard and the
    /// drain cleanup releases the queue.
    #[test]
    fn poisoned_coordinator_state_lock_still_serves_every_session() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let panicking = Arc::new(AtomicBool::new(false));
        let trip = Arc::clone(&panicking);
        let _registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                if trip.load(Ordering::SeqCst) {
                    panic!("injected sink failure while the state is poisoned");
                }
                log.lock().unwrap().push(kind_of(event));
            })
        };
        coordinator.record(&key, &tick_event("e0"));
        coordinator.poison_state_for_test();
        assert!(
            coordinator.state.is_poisoned(),
            "the hook must poison the lock"
        );

        // The same session still emits and delivers through the recovered
        // guard, including the drain loop and its cleanup guard.
        assert!(coordinator.emit(&key, &tick_event("e1")));
        assert_eq!(*delivered.lock().unwrap(), vec!["e1".to_string()]);
        // The cleanup released the queue despite the poison: a second emit
        // claims the drain again instead of queueing behind a stranded one.
        assert!(coordinator.emit(&key, &tick_event("e1b")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e1b".to_string()]
        );
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(snapshot.active);
        assert_eq!(snapshot.events.len(), 3);
        // A sink panic unwinds through the drain cleanup guard while the lock
        // is still poisoned: the guard's tolerant lock must release the queue,
        // or every later emit for the session queues behind the dead drainer.
        panicking.store(true, Ordering::SeqCst);
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            coordinator.emit(&key, &tick_event("e2"))
        }));
        assert!(
            panicked.is_err(),
            "the sink panic must surface at the emitter"
        );
        panicking.store(false, Ordering::SeqCst);
        assert!(coordinator.emit(&key, &tick_event("e2b")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "e1b".to_string(), "e2b".to_string()],
            "the poisoned cleanup guard must release the drain queue"
        );
        // Retain, expiry and explicit teardown all complete.
        coordinator.retain_terminal(&key, &JsString::from("r1"));
        assert!(!coordinator.emit(&key, &tick_event("e2")));
        timer.advance(30_000);
        assert!(
            coordinator
                .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
                .run_id
                .is_none()
        );
        coordinator.unregister(&key);
        coordinator.dispose();
    }

    fn epoch_snapshot(value: &JsValue) -> ActiveTurnSnapshot {
        ActiveTurnSnapshot {
            active: value
                .get("active")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            session_key: text(field(value, "sessionKey")).clone(),
            run_id: value.get("runId").and_then(JsValue::as_string).cloned(),
            events: value
                .get("events")
                .map(items)
                .map(<[JsValue]>::to_vec)
                .unwrap_or_default(),
            truncated: value
                .get("truncated")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            terminal: value
                .get("terminal")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
        }
    }

    #[test]
    fn same_turn_epoch_matches_real_ts_retry_loop() {
        let oracle_value = crate::test_baseline::oracle(ORACLE);
        let cases = items(field(&oracle_value, "epochCases"));
        assert!(!cases.is_empty());
        for case_value in cases {
            let name = text(field(case_value, "name"))
                .to_utf8()
                .expect("UTF-8 case name");
            let attempts = items(field(case_value, "attempts"));
            assert!(!attempts.is_empty(), "{name}: attempts");
            let outcome = field(case_value, "outcome");
            let mut stopped_at = None;
            for (index, attempt) in attempts.iter().enumerate() {
                let left = epoch_snapshot(field(attempt, "before"));
                let right = epoch_snapshot(field(attempt, "after"));
                let expected = field(attempt, "sameEpoch")
                    .as_bool()
                    .expect("sameEpoch boolean");
                assert_eq!(
                    same_turn_epoch(&left, &right),
                    expected,
                    "{name}: attempt {index} same epoch"
                );
                if expected {
                    stopped_at = Some(index);
                    break;
                }
            }
            if outcome.get("result").is_some() {
                assert_eq!(
                    stopped_at,
                    Some(attempts.len() - 1),
                    "{name}: successful read stops on the final attempt"
                );
            } else {
                assert_eq!(attempts.len(), 3, "{name}: retry budget");
                assert_eq!(stopped_at, None, "{name}: changing epoch never stops");
                assert_eq!(
                    text(field(outcome, "error")).to_utf8().unwrap(),
                    "Session changed during transcript synchronization; retry the snapshot."
                );
            }
        }
    }

    /// Turns a `true` flag on when the sink closure holding it is freed, so
    /// the sink-release half of the ownership test can observe the drop.
    struct SinkDropMarker(Arc<AtomicBool>);
    impl Drop for SinkDropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Gate7 P1 #1: the classic TS sink style strongly captures the
    /// coordinator (reentrant emits go through it), which used to form an
    /// `Arc` reference cycle that kept the coordinator — and its replay
    /// store's timer thread — alive after the last host handle dropped. The
    /// weak-registration design removes the coordinator's side of that cycle
    /// (the coordinator holds the sink only weakly; the strong owner is the
    /// registration); this test pins that, written in exactly that
    /// strong-capture style (A7.2-COORDINATOR-SINK-OWNERSHIP-001). The
    /// host-side half of the contract — what a strongly capturing sink does
    /// to its *host* — is pinned by the two host-style tests below.
    #[test]
    fn strongly_capturing_sink_release_drops_the_coordinator_and_stops_the_timer() {
        let (timer, timer_exited) = production_timer_with_exit_flag();
        // The store takes over the timer's strong handle here, so after the
        // coordinator drops nothing keeps the worker thread alive except it.
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer)),
        ));
        let key = JsString::from("s");
        let weak = Arc::downgrade(&coordinator);
        let sink_dropped = Arc::new(AtomicBool::new(false));
        {
            let marker = SinkDropMarker(Arc::clone(&sink_dropped));
            let nested = Arc::clone(&coordinator);
            let registration = coordinator.start(&key, &JsString::from("r1"), move |event| {
                let _ = (&marker, &nested, event);
            });
            assert!(
                coordinator.emit(&key, &tick_event("e")),
                "the strongly capturing sink must deliver normally first"
            );
            drop(registration);
        }
        drop(coordinator);
        assert!(
            sink_dropped.load(Ordering::SeqCst),
            "dropping the registration must release the strongly capturing sink"
        );
        assert!(
            weak.upgrade().is_none(),
            "the coordinator must be reclaimed even though its sink captured it strongly"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !timer_exited.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the coordinator's timer worker thread must exit once the last handle drops"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The other half of the ownership contract: dropping the registration
    /// unregisters the sink (later `emit` calls return `false`, like
    /// `retain_terminal`) but leaves the coordinator itself fully usable.
    #[test]
    fn dropping_the_registration_unregisters_the_sink_but_keeps_the_coordinator() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");

        let registration = coordinator.start(&key, &JsString::from("r1"), |_| {});
        assert!(coordinator.emit(&key, &tick_event("e")));
        drop(registration);
        assert!(
            !coordinator.emit(&key, &tick_event("e2")),
            "a dropped registration means no live sink, so emit is rejected"
        );
        let _restarted = coordinator.start(&key, &JsString::from("r2"), |_| {});
        assert!(
            coordinator.emit(&key, &tick_event("e3")),
            "a session restarted after a dropped registration must deliver again"
        );
    }

    /// codex-r8 P1 #1, deterministic interleaving ①: another thread parked
    /// inside the sink holds the strong sink handle of the queue entry it
    /// popped, which must not keep a dropped registration deliverable. The
    /// emit-side check asks the registration, so the new event is rejected
    /// and never recorded, while the already-accepted one still delivers
    /// (A7.2-COORDINATOR-SINK-OWNERSHIP-001).
    #[test]
    fn registration_check_fails_while_a_delivery_is_in_flight() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel::<()>();
        let release = Arc::new(Mutex::new(release));
        let registration = {
            let log = Arc::clone(&delivered);
            let entered = entered_tx;
            let release = Arc::clone(&release);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                entered.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
            })
        };
        let emitting = Arc::clone(&coordinator);
        let emit_key = key.clone();
        let sink_thread = std::thread::spawn(move || {
            emitting.emit(&emit_key, &tick_event("e1"));
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // The drainer is parked inside the sink, holding the strong sink
        // handle of the entry it popped. Dropping the last registration must
        // unregister the session anyway.
        drop(registration);
        assert!(
            !coordinator.emit(&key, &tick_event("e2")),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: the in-flight sink handle must not keep a dropped registration deliverable"
        );
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert_eq!(
            snapshot.events.iter().map(kind_of).collect::<Vec<_>>(),
            vec!["e1".to_string()],
            "the rejected emit must not record its event"
        );

        release_tx.send(()).unwrap();
        sink_thread.join().unwrap();
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string()],
            "the already-accepted e1 still delivers; e2 never reaches the sink"
        );
    }

    /// codex-r8 P1 #1, single-threaded variant ②: the sink itself drops the
    /// last registration (from a slot it shares with its host) and then
    /// re-enters `emit`. Inside that sink call the drainer still holds the
    /// strong sink handle, yet the reentrant emit must be rejected and must
    /// not record (A7.2-COORDINATOR-SINK-OWNERSHIP-001).
    #[test]
    fn sink_dropping_its_own_registration_rejects_the_reentrant_emit() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let reentrant_accepted = Arc::new(Mutex::new(None));
        let slot = Arc::new(Mutex::new(None));
        let registration = {
            let log = Arc::clone(&delivered);
            let accepted = Arc::clone(&reentrant_accepted);
            let slot = Arc::clone(&slot);
            let nested = Arc::clone(&coordinator);
            let nested_key = key.clone();
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
                if kind_of(event) == "e1" {
                    // The host-style teardown from inside the sink: let go
                    // of the strong owner, then re-enter the coordinator.
                    *slot.lock().unwrap() = None;
                    *accepted.lock().unwrap() = Some(nested.emit(&nested_key, &tick_event("e2")));
                }
            })
        };
        *slot.lock().unwrap() = Some(registration);
        assert!(coordinator.emit(&key, &tick_event("e1")));
        assert_eq!(
            *reentrant_accepted.lock().unwrap(),
            Some(false),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: the reentrant emit must find the dropped registration"
        );
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string()],
            "the rejected reentrant emit delivers nothing"
        );
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert_eq!(
            snapshot.events.iter().map(kind_of).collect::<Vec<_>>(),
            vec!["e1".to_string()],
            "the rejected reentrant emit must not record its event"
        );
    }

    /// The explicit-disconnect half of the ownership contract
    /// (A7.2-COORDINATOR-SINK-OWNERSHIP-001): `unregister` detaches the
    /// sink while the registration is still alive — the rejection below is
    /// the map removal alone, not the dropped-owner path — and, unlike
    /// `retain_terminal`, it leaves the replay buffer, the run and the
    /// terminal-retention state untouched.
    #[test]
    fn unregister_detaches_the_sink_without_touching_the_replay() {
        let timer = ManualTimer::new();
        let coordinator = coordinator(&timer);
        let key = JsString::from("s");
        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let registration = {
            let log = Arc::clone(&delivered);
            coordinator.start(&key, &JsString::from("r1"), move |event| {
                log.lock().unwrap().push(kind_of(event));
            })
        };
        assert!(coordinator.emit(&key, &tick_event("e1")));
        coordinator.unregister(&key);
        assert!(
            !coordinator.emit(&key, &tick_event("e2")),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: unregister detaches the sink even while the host still holds the registration"
        );
        assert_eq!(*delivered.lock().unwrap(), vec!["e1".to_string()]);
        let snapshot = coordinator.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert_eq!(
            snapshot.events.len(),
            1,
            "the rejected emit records nothing"
        );
        assert!(snapshot.active, "unregister leaves the replay active");
        assert!(
            !snapshot.terminal,
            "unregister performs no terminal transition"
        );
        assert_eq!(
            timer.pending(&key),
            0,
            "unregister schedules no retention timer"
        );
        // A later start re-registers and delivers again.
        let restarted = {
            let log = Arc::clone(&delivered);
            coordinator.start(&key, &JsString::from("r2"), move |event| {
                log.lock().unwrap().push(format!("r2:{}", kind_of(event)));
            })
        };
        assert!(coordinator.emit(&key, &tick_event("e3")));
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["e1".to_string(), "r2:e3".to_string()],
            "a session restarted after unregister delivers again"
        );
        drop(registration);
        drop(restarted);
    }

    /// The ownership contract's conforming host (A7.2-COORDINATOR-SINK-
    /// OWNERSHIP-001): the sink calls back into its host through a `Weak`,
    /// and the host holds the registration as an ordinary field. Dropping
    /// the external host and coordinator handles — with no manual
    /// registration drop, no `unregister` and no drop-order care — releases
    /// the sink, the host, the coordinator and the production timer thread.
    struct WeakCaptureHost {
        coordinator: Arc<TurnEventCoordinator>,
        registration: Mutex<Option<TurnSinkRegistration>>,
        delivered: Mutex<Vec<String>>,
    }

    #[test]
    fn weakly_capturing_host_releases_the_sink_coordinator_and_timer_on_drop() {
        let (timer, timer_exited) = production_timer_with_exit_flag();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer)),
        ));
        let key = JsString::from("s");
        let sink_dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(WeakCaptureHost {
            coordinator: Arc::clone(&coordinator),
            registration: Mutex::new(None),
            delivered: Mutex::new(Vec::new()),
        });
        {
            let marker = SinkDropMarker(Arc::clone(&sink_dropped));
            let sink_host = Arc::downgrade(&host);
            let registration = coordinator.start(&key, &JsString::from("r1"), move |event| {
                let _ = &marker;
                if let Some(host) = sink_host.upgrade() {
                    host.delivered.lock().unwrap().push(kind_of(event));
                }
            });
            *host.registration.lock().unwrap() = Some(registration);
        }
        assert!(host.coordinator.emit(&key, &tick_event("e1")));
        assert_eq!(*host.delivered.lock().unwrap(), vec!["e1".to_string()]);

        // Plain drop of the external handles: the registration dies with
        // the host, in whatever order the fields drop.
        let weak_host = Arc::downgrade(&host);
        let weak_coordinator = Arc::downgrade(&coordinator);
        drop(coordinator);
        drop(host);
        assert!(
            sink_dropped.load(Ordering::SeqCst),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: the conforming host releases the sink on a plain drop"
        );
        assert!(
            weak_host.upgrade().is_none(),
            "the host itself is reclaimed"
        );
        assert!(
            weak_coordinator.upgrade().is_none(),
            "the coordinator is reclaimed with its last handle"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !timer_exited.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the timer worker must exit once the coordinator is dropped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The ownership contract's counter-example host, written exactly the
    /// review's way (A7.2-COORDINATOR-SINK-OWNERSHIP-001): the sink
    /// strongly captures `Arc<Host>`, and the host holds both the
    /// coordinator and the registration. Dropping every external handle
    /// leaks — `Host → registration → sink → Host` has no external strong
    /// edge left to break it — which is why the contract tells these hosts
    /// to end the session through the explicit teardown instead of drop
    /// order. The cycle keeps the host reachable through its weak view, so
    /// that teardown still runs, and everything the cycle held is released.
    struct StrongCaptureHost {
        coordinator: Arc<TurnEventCoordinator>,
        registration: Mutex<Option<TurnSinkRegistration>>,
        delivered: Mutex<Vec<String>>,
    }

    impl StrongCaptureHost {
        fn close(&self, key: &JsString) {
            // The explicit teardown the contract names: detach the sink
            // deterministically, then let go of the strong owner.
            self.coordinator.unregister(key);
            *self.registration.lock().unwrap() = None;
        }
    }

    #[test]
    fn strongly_capturing_host_leaks_on_drop_and_releases_through_explicit_unregister() {
        let (timer, timer_exited) = production_timer_with_exit_flag();
        let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer)),
        ));
        let key = JsString::from("s");
        let sink_dropped = Arc::new(AtomicBool::new(false));
        let host = Arc::new(StrongCaptureHost {
            coordinator: Arc::clone(&coordinator),
            registration: Mutex::new(None),
            delivered: Mutex::new(Vec::new()),
        });
        {
            let marker = SinkDropMarker(Arc::clone(&sink_dropped));
            let sink_host = Arc::clone(&host);
            let registration = coordinator.start(&key, &JsString::from("r1"), move |event| {
                let _ = &marker;
                sink_host.delivered.lock().unwrap().push(kind_of(event));
            });
            *host.registration.lock().unwrap() = Some(registration);
        }
        assert!(coordinator.emit(&key, &tick_event("e1")));
        assert_eq!(*host.delivered.lock().unwrap(), vec!["e1".to_string()]);

        // Drop every external host and coordinator handle, the way a host
        // that relies on drop order would.
        let weak_host = Arc::downgrade(&host);
        let weak_coordinator = Arc::downgrade(&coordinator);
        drop(host);
        drop(coordinator);
        assert!(
            weak_host.upgrade().is_some(),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: the counter-example style keeps the host alive after the last external handle drops"
        );
        assert!(
            weak_coordinator.upgrade().is_some(),
            "the coordinator stays alive inside the leaked host"
        );
        assert!(
            !sink_dropped.load(Ordering::SeqCst),
            "the strongly captured sink is not released by the plain drop"
        );
        assert!(
            !timer_exited.load(Ordering::SeqCst),
            "the leaked host keeps the coordinator's timer thread running"
        );

        // The cycle keeps the host reachable, so its explicit teardown still
        // runs: `unregister` plus releasing the registration.
        let leaked_coordinator = weak_coordinator
            .upgrade()
            .expect("the cycle keeps the coordinator reachable");
        weak_host
            .upgrade()
            .expect("the cycle keeps the host reachable")
            .close(&key);
        assert!(
            !leaked_coordinator.emit(&key, &tick_event("late")),
            "the explicit teardown ends delivery"
        );
        assert!(
            sink_dropped.load(Ordering::SeqCst),
            "A7.2-COORDINATOR-SINK-OWNERSHIP-001: releasing the registration drops the strongly captured sink"
        );
        drop(leaked_coordinator);
        assert!(
            weak_host.upgrade().is_none(),
            "the host is reclaimed once the cycle is broken"
        );
        assert!(
            weak_coordinator.upgrade().is_none(),
            "the coordinator is reclaimed once the cycle is broken"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !timer_exited.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the timer worker must exit once the cycle is broken"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
