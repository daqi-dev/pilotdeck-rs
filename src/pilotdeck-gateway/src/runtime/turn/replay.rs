//! Bounded volatile replay of already-derived Gateway events.
//!
//! Mirrors `GatewayTurnReplayStore.ts:8-136`. The store never admits turns or
//! writes durable history; the Gateway host decides when a turn starts, reaches
//! a terminal state, or is discarded.
//!
//! Terminal retention is driven by a timer per session, exactly like the TS
//! `setTimeout`/`clearTimeout` pair: expiry never happens synchronously inside a
//! store call, an idle store still releases its terminal buffers, and reading
//! the wall clock is not part of the retention contract. The production timer
//! waits on `Instant` (monotonic), so an NTP jump cannot shorten or extend a
//! terminal window.
//!
//! The standalone default has a different scheduling boundary: Node runs a `setTimeout` callback only on a later timer
//! turn — after the scheduling stack returns to the event loop and the
//! pending microtasks drain — while this store's timer thread fires a due
//! deadline without waiting for the caller's current synchronous segment or
//! microtasks. That holds for every retention duration, not just zero
//! (verified against the real TS store with real timers for retention 0, 5
//! and the 30000 default: a deadline that elapses inside the caller's busy
//! segment still leaves the terminal replay visible to the synchronous
//! snapshot and to a Promise microtask, and only the next timer turn
//! reclaims it — `_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`).
//! A zero delay is still deferred by Node's own 1 ms floor, so a `snapshot`
//! immediately after `retain_terminal` sees the terminal replay in the
//! common case; tests that need the exact boundary inject `ManualTimer`,
//! whose `advance` plays that host turn deterministically. The production
//! connection host (`gateway::TurnGateway`) wraps the deadline clock in a
//! serial queue: a due callback only enqueues, and replay is reclaimed on a
//! later host step. That path addresses A7.2-REPLAY-RETENTION-TIMER-001;
//! standalone users of this default timer still need to inject their own host
//! boundary through [`TurnReplayOptions::with_timer`]. The historical ledger
//! entry is kept in `specs/gateway-parity/accepted-diffs.md`.
//!
//! The injected timer is host code without a no-panic guarantee (#805): a
//! panicking `cancel`/`set`/`cancel_all` must surface at the call site like a
//! throwing host `setTimeout`/`clearTimeout` in TS — with the same partial
//! state, since the TS methods apply their mutations in the same order — but
//! it must never poison the store's state lock for every session. Every state
//! entry therefore recovers a poisoned lock instead of panicking, and each
//! timer call releases the guard before resuming the unwind.

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json, timer_delay_ms};
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

pub const DEFAULT_EVENT_LIMIT: usize = 500;
pub const DEFAULT_BYTE_LIMIT: usize = 256 * 1024;
pub const DEFAULT_TERMINAL_RETENTION_MS: u64 = 30_000;

/// Recovers the state guard from a poisoned lock instead of panicking. TS
/// keeps no lock, so a throwing host timer only fails its own call; a store
/// whose lock a panic elsewhere poisoned must keep serving every session
/// (#805, the same tolerant-lock shape as `mcp/wire.rs` and
/// `runtime/refresh.rs`). Every state entry uses it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A per-session deadline timer, the Rust counterpart of the TS store's
/// `Map<string, setTimeout>`: `set` replaces the session's timer and `cancel`
/// removes it. Callbacks run on the timer's own thread, never inside `set`.
pub trait ReplayTimer: Send + Sync {
    fn set(&self, session_key: &JsString, delay: Duration, callback: Arc<dyn Fn() + Send + Sync>);
    fn cancel(&self, session_key: &JsString);
    fn cancel_all(&self);
}

/// Production timer backed by one worker thread per store.
///
/// The worker waits on a `Condvar` with an `Instant` deadline (never
/// `SystemTime`), wakes on `set`/`cancel`, and fires the earliest entry outside
/// its own lock. A zero delay is deferred by Node's 1 ms floor, which keeps
/// the TS `setTimeout(0)` next-macrotask boundary observable for adjacent
/// calls in the common case (see the module note for the exact scope of that
/// promise).
pub(super) struct ThreadTimer {
    shared: Arc<(Mutex<TimerWorker>, Condvar)>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Flipped by the worker immediately before it returns, so an observer
    /// that holds neither the timer nor the store can still prove the thread
    /// exited (`Drop` joins it first). Tests use this to pin that a dropped
    /// store really stops its worker; no production path reads it.
    #[cfg_attr(not(test), allow(dead_code))]
    exited: Arc<AtomicBool>,
}

struct TimerEntry {
    due: Instant,
    sequence: u64,
    callback: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct TimerWorker {
    entries: HashMap<JsString, TimerEntry>,
    next_sequence: u64,
    shutdown: bool,
}

impl ThreadTimer {
    pub(super) fn new() -> Self {
        let shared = Arc::new((Mutex::new(TimerWorker::default()), Condvar::new()));
        let exited = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&shared);
        let worker_exited = Arc::clone(&exited);
        let handle = std::thread::Builder::new()
            .name("turn-replay-timer".into())
            .spawn(move || run_timer(worker, worker_exited))
            .expect("spawn turn replay timer thread");
        Self {
            shared,
            handle: Mutex::new(Some(handle)),
            exited,
        }
    }
    #[cfg(test)]
    fn exit_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.exited)
    }
}

fn run_timer(shared: Arc<(Mutex<TimerWorker>, Condvar)>, exited: Arc<AtomicBool>) {
    let (lock, wake) = &*shared;
    loop {
        let callback = {
            let mut worker = lock.lock().unwrap();
            loop {
                if worker.shutdown {
                    exited.store(true, Ordering::SeqCst);
                    return;
                }
                let now = Instant::now();
                let next = worker
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| (entry.due, entry.sequence))
                    .map(|(key, entry)| (key.clone(), entry.due, entry.sequence));
                match next {
                    None => {
                        worker = wake.wait(worker).unwrap();
                    }
                    Some((key, due, _)) if due <= now => {
                        let entry = worker
                            .entries
                            .remove(&key)
                            .expect("entry was observed above");
                        break entry.callback;
                    }
                    Some((_, due, _)) => {
                        let (guard, _) = wake.wait_timeout(worker, due - now).unwrap();
                        worker = guard;
                    }
                }
            }
        };
        callback();
    }
}

impl ReplayTimer for ThreadTimer {
    fn set(&self, session_key: &JsString, delay: Duration, callback: Arc<dyn Fn() + Send + Sync>) {
        let (lock, wake) = &*self.shared;
        let mut worker = lock.lock().unwrap();
        // Node coerces a `setTimeout` delay that is out of range (zero,
        // negative, above `2^31 - 1` ms) down to 1 ms;
        // `pilotdeck_protocol::timer_delay_ms` is the single source of that
        // rule, and honouring the upper bound also keeps `Instant + Duration`
        // from overflowing inside `retain_terminal`'s critical section. The
        // 1 ms floor is what keeps the `setTimeout(0)` next-macrotask
        // boundary observable for a synchronous caller in the common case;
        // the JS boundary itself is not a promise (see the module note).
        let delay = Duration::from_millis(timer_delay_ms(delay.as_millis() as f64));
        let sequence = worker.next_sequence;
        worker.next_sequence = worker.next_sequence.wrapping_add(1);
        worker.entries.insert(
            session_key.clone(),
            TimerEntry {
                due: Instant::now() + delay,
                sequence,
                callback,
            },
        );
        wake.notify_all();
    }

    fn cancel(&self, session_key: &JsString) {
        let (lock, wake) = &*self.shared;
        let mut worker = lock.lock().unwrap();
        worker.entries.remove(session_key);
        wake.notify_all();
    }

    fn cancel_all(&self) {
        let (lock, wake) = &*self.shared;
        let mut worker = lock.lock().unwrap();
        worker.entries.clear();
        wake.notify_all();
    }
}

impl Drop for ThreadTimer {
    fn drop(&mut self) {
        {
            let mut worker = self.shared.0.lock().unwrap();
            worker.shutdown = true;
            worker.entries.clear();
        }
        self.shared.1.notify_all();
        if let Some(handle) = self.handle.lock().unwrap().take()
            && handle.thread().id() != std::thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
pub struct TurnReplayOptions {
    event_limit: usize,
    byte_limit: usize,
    terminal_retention_ms: u64,
    /// `None` (the default) means every store built from these options owns a
    /// private `ThreadTimer`; only an explicitly injected timer is shared
    /// between the stores built from these options and their clones.
    timer: Option<Arc<dyn ReplayTimer>>,
}
impl TurnReplayOptions {
    pub fn new() -> Self {
        Self {
            event_limit: DEFAULT_EVENT_LIMIT,
            byte_limit: DEFAULT_BYTE_LIMIT,
            terminal_retention_ms: DEFAULT_TERMINAL_RETENTION_MS,
            timer: None,
        }
    }
    /// TS `Math.max(1, eventLimit ?? 500)`.
    pub fn with_event_limit(mut self, value: usize) -> Self {
        self.event_limit = value.max(1);
        self
    }
    /// TS `Math.max(1, byteLimit ?? 262144)`.
    pub fn with_byte_limit(mut self, value: usize) -> Self {
        self.byte_limit = value.max(1);
        self
    }
    /// TS `Math.max(0, terminalRetentionMs ?? 30000)`.
    pub fn with_terminal_retention_ms(mut self, value: u64) -> Self {
        self.terminal_retention_ms = value;
        self
    }
    /// Replace the deadline timer. Tests use a manually advanced timer so the
    /// golden data stays wall-clock free; the production connection host wraps the default in its serial queue.
    /// The default timer keeps Node's 1 ms floor for a zero delay but cannot
    /// promise the JS timer-turn boundary for any retention duration (see the
    /// module note); a host that needs that boundary in production injects its
    /// own timer here, for example one that queues expiry callbacks for the
    /// host's own scheduler turn instead of a dedicated thread.
    ///
    /// Ownership: an injected timer is shared by every store built from these
    /// options or their clones (the oracle harness relies on that), while the
    /// default timer is created per store — cloning these options never
    /// couples two stores' deadlines.
    pub fn with_timer(mut self, timer: Arc<dyn ReplayTimer>) -> Self {
        self.timer = Some(timer);
        self
    }
    pub fn event_limit(&self) -> usize {
        self.event_limit
    }
    pub fn byte_limit(&self) -> usize {
        self.byte_limit
    }
    pub fn terminal_retention_ms(&self) -> u64 {
        self.terminal_retention_ms
    }
}
impl Default for TurnReplayOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveTurnSnapshotInput {
    pub session_key: JsString,
    /// TS defaults to including events; only an explicit `includeEvents: false`
    /// suppresses them.
    pub include_events: bool,
}
impl ActiveTurnSnapshotInput {
    pub fn new(session_key: JsString) -> Self {
        Self {
            session_key,
            include_events: true,
        }
    }
    pub fn without_events(mut self) -> Self {
        self.include_events = false;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveTurnSnapshot {
    pub active: bool,
    pub session_key: JsString,
    pub run_id: Option<JsString>,
    pub events: Vec<JsValue>,
    pub truncated: bool,
    pub terminal: bool,
}
impl ActiveTurnSnapshot {
    /// Same object shape and key order as the TS return value.
    pub fn to_json(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("active".into(), JsValue::Bool(self.active));
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.session_key.clone()),
        );
        if let Some(run_id) = &self.run_id {
            object.insert("runId".into(), JsValue::String(run_id.clone()));
        }
        object.insert("events".into(), JsValue::Array(self.events.clone()));
        if self.truncated {
            object.insert("truncated".into(), JsValue::Bool(true));
        }
        if self.terminal {
            object.insert("terminal".into(), JsValue::Bool(true));
        }
        JsValue::Object(object)
    }
}

struct StoredEvent {
    event: JsValue,
    bytes: usize,
}

struct ActiveReplay {
    /// Store-wide identity of the run that created this replay. It never
    /// repeats, so a timer callback or a racing `record` captured before a
    /// `start` cannot act on the replacement generation.
    identity: u64,
    run_id: JsString,
    active: bool,
    events: VecDeque<StoredEvent>,
    bytes: usize,
    truncated: bool,
    /// The `retain_terminal` serial this replay last scheduled; only that
    /// exact terminal generation expires.
    retain_serial: u64,
}

#[derive(Default)]
struct State {
    replays: HashMap<JsString, ActiveReplay>,
    next_identity: u64,
}

pub struct TurnReplayStore {
    options: TurnReplayOptions,
    timer: Arc<dyn ReplayTimer>,
    state: Arc<Mutex<State>>,
    #[cfg(test)]
    record_gap_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl TurnReplayStore {
    pub fn new() -> Self {
        Self::with_options(TurnReplayOptions::new())
    }
    pub fn with_options(options: TurnReplayOptions) -> Self {
        let timer = options
            .timer
            .clone()
            .unwrap_or_else(|| Arc::new(ThreadTimer::new()));
        Self {
            options,
            timer,
            state: Arc::new(Mutex::new(State::default())),
            #[cfg(test)]
            record_gap_hook: Mutex::new(None),
        }
    }
    pub fn start(&self, session_key: &JsString, run_id: &JsString) {
        let mut state = lock(&self.state);
        // TS `start` calls `clearTerminal` first: the previous terminal timer
        // must not fire against the new run. Cancelling only reaches
        // callbacks still queued in the timer; a callback the worker already
        // detached compares the replay identity below and does nothing.
        // The timer is injected code without a no-panic guarantee (#805): a
        // panic still surfaces at the call site like a throwing host
        // `clearTimeout`, but the guard is dropped first so the unwind
        // cannot poison the store's lock for every session.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel(session_key);
        })) {
            drop(state);
            resume_unwind(panic);
        }
        state.next_identity = state.next_identity.wrapping_add(1);
        let identity = state.next_identity;
        state.replays.insert(
            session_key.clone(),
            ActiveReplay {
                identity,
                run_id: run_id.clone(),
                active: true,
                events: VecDeque::new(),
                bytes: 0,
                truncated: false,
                retain_serial: 0,
            },
        );
    }
    pub fn record(&self, session_key: &JsString, event: &JsValue) {
        // TS looks the replay up before cloning, so an event for a session
        // with no replay never panics on serialization.
        let identity = {
            let state = lock(&self.state);
            match state.replays.get(session_key) {
                None => return,
                Some(replay) => replay.identity,
            }
        };
        // TS clones before measuring and keeps the clone, so later mutation of
        // the caller's object cannot change replay bytes or contents. A
        // non-serializable event panics like the TS `JSON.parse` call, but the
        // serialization happens outside the state lock so one bad event cannot
        // poison the store for every session.
        let (event, bytes) = clone_event_with_bytes(event);
        #[cfg(test)]
        self.fire_record_gap();
        let mut state = lock(&self.state);
        let Some(replay) = state.replays.get_mut(session_key) else {
            return;
        };
        if replay.identity != identity {
            // A `start` replaced the run while the event was serialized; TS
            // keeps such an event with the replay it was recorded against,
            // which the replacement then discards.
            return;
        }
        replay.events.push_back(StoredEvent { event, bytes });
        replay.bytes += bytes;
        while replay.events.len() > self.options.event_limit
            || replay.bytes > self.options.byte_limit
        {
            let Some(dropped) = replay.events.pop_front() else {
                break;
            };
            replay.bytes -= dropped.bytes;
            replay.truncated = true;
        }
    }
    pub fn retain_terminal(&self, session_key: &JsString, run_id: &JsString) {
        let mut state = lock(&self.state);
        if !state
            .replays
            .get(session_key)
            .is_some_and(|replay| replay.run_id == *run_id)
        {
            return;
        }
        state.next_identity = state.next_identity.wrapping_add(1);
        let serial = state.next_identity;
        let Some(replay) = state.replays.get_mut(session_key) else {
            return;
        };
        replay.active = false;
        replay.retain_serial = serial;
        let identity = replay.identity;
        let deadline_state = Arc::downgrade(&self.state);
        let deadline_key = session_key.clone();
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Some(state) = Weak::upgrade(&deadline_state) else {
                return;
            };
            // The tolerant lock keeps a poisoned state from failing the
            // expiry on the timer thread (#805).
            let mut state = lock(&state);
            let expired = state.replays.get(&deadline_key).is_some_and(|replay| {
                !replay.active && replay.identity == identity && replay.retain_serial == serial
            });
            if expired {
                state.replays.remove(&deadline_key);
            }
        });
        // A panicking `set` surfaces at the call site like a throwing host
        // `setTimeout` — with the replay already terminal and no timer
        // registered, the same partial state TS leaves (#805) — but the
        // guard is dropped first so it cannot poison the lock.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.set(
                session_key,
                Duration::from_millis(self.options.terminal_retention_ms),
                callback,
            );
        })) {
            drop(state);
            resume_unwind(panic);
        }
    }
    pub fn clear_terminal(&self, session_key: &JsString) {
        let mut state = lock(&self.state);
        // The panic surfaces at the call site like a throwing host
        // `clearTimeout`, before either removal below (#805); the guard is
        // dropped first so it cannot poison the lock.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel(session_key);
        })) {
            drop(state);
            resume_unwind(panic);
        }
        if state
            .replays
            .get(session_key)
            .is_some_and(|replay| !replay.active)
        {
            state.replays.remove(session_key);
        }
    }
    /// TS `{ ...event, runId }`. The spread copies a JSON object's own entries,
    /// an array's elements under their index keys, and a string's UTF-16 code
    /// units under theirs (`{ ..."a😀b" }` is
    /// `{"0":"a","1":"\ud83d","2":"\ude00","3":"b"}` — string indices count
    /// code units, so the emoji spreads as two single-unit strings). A number
    /// or boolean spreads to nothing: `{ ...5, runId }` is `{ runId }`. Reading
    /// `event.runId` on `null`/`undefined` throws in TS, so those panic here at
    /// the same property access.
    pub fn with_run_id(&self, session_key: &JsString, event: &JsValue) -> JsValue {
        if matches!(event, JsValue::Null | JsValue::Undefined) {
            panic!(
                "TS hasRunId reads `event.runId`; on null or undefined that property access throws"
            );
        }
        if has_run_id(event) {
            return event.clone();
        }
        let run_id = {
            let state = lock(&self.state);
            state
                .replays
                .get(session_key)
                .map(|replay| replay.run_id.clone())
        };
        let Some(run_id) = run_id else {
            return event.clone();
        };
        let mut object = match event {
            JsValue::Object(object) => object.clone(),
            JsValue::Array(items) => {
                let mut object = JsObject::new();
                for (index, item) in items.iter().enumerate() {
                    object.insert(index_key(index), item.clone());
                }
                object
            }
            JsValue::String(text) => {
                let mut object = JsObject::new();
                for (index, unit) in text.units().iter().enumerate() {
                    object.insert(
                        index_key(index),
                        JsValue::String(JsString::from_utf16(vec![*unit])),
                    );
                }
                object
            }
            _ => JsObject::new(),
        };
        object.insert("runId".into(), JsValue::String(run_id));
        JsValue::Object(object)
    }
    pub fn snapshot(&self, input: &ActiveTurnSnapshotInput) -> ActiveTurnSnapshot {
        let state = lock(&self.state);
        let Some(replay) = state.replays.get(&input.session_key) else {
            return ActiveTurnSnapshot {
                active: false,
                session_key: input.session_key.clone(),
                run_id: None,
                events: Vec::new(),
                truncated: false,
                terminal: false,
            };
        };
        let events = if input.include_events {
            // Stored events already crossed the JSON codec in `record`, so
            // `Clone` — a deep copy of the owned entries — is the same value
            // a stringify/parse round-trip would return, without re-paying
            // serialization for every event on every snapshot.
            replay
                .events
                .iter()
                .map(|stored| stored.event.clone())
                .collect()
        } else {
            Vec::new()
        };
        ActiveTurnSnapshot {
            active: replay.active,
            session_key: input.session_key.clone(),
            run_id: Some(replay.run_id.clone()),
            events,
            truncated: replay.truncated,
            terminal: !replay.active,
        }
    }
    pub fn dispose(&self) {
        let mut state = lock(&self.state);
        // The panic surfaces at the call site like a throwing host
        // `clearTimeout` inside the TS `forEach`, before the maps clear
        // (#805); the guard is dropped first so it cannot poison the lock.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
            self.timer.cancel_all();
        })) {
            drop(state);
            resume_unwind(panic);
        }
        state.replays.clear();
    }
    #[cfg(test)]
    pub(crate) fn set_after_record_gap_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.record_gap_hook.lock().unwrap() = Some(hook);
    }
    /// Poisons the state lock so a test can prove every entry recovers the
    /// guard and keeps serving sessions (#805); the same test-only hook shape
    /// as `runtime/steer.rs` and `runtime/hooks/exec.rs`.
    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _state = self.state.lock().unwrap();
            panic!("injected replay state poison");
        }));
    }
    #[cfg(test)]
    fn fire_record_gap(&self) {
        if let Some(hook) = self.record_gap_hook.lock().unwrap().as_ref() {
            hook();
        }
    }
}
impl Default for TurnReplayStore {
    fn default() -> Self {
        Self::new()
    }
}

/// JSON deep clone (`JSON.parse(JSON.stringify(event))`) plus the UTF-8 byte
/// length of that exact serialization, from a single stringify. Preserves lone
/// UTF-16 surrogate escapes through the protocol codec.
fn clone_event_with_bytes(event: &JsValue) -> (JsValue, usize) {
    let text = stringify_json(event)
        .expect("event stringify cannot fail")
        .expect("GatewayEvent is JSON-serializable");
    let bytes = text.len();
    let clone = parse_json(&text).expect("JSON round-trip preserves a serialized event");
    (clone, bytes)
}

/// `Buffer.byteLength(JSON.stringify(event), "utf8")`. The codec writes lone
/// surrogates as six ASCII escape bytes and non-ASCII scalars as UTF-8, so the
/// Rust string length is exactly the Node byte count. Production measures the
/// same text inside `clone_event_with_bytes`; only fixtures call this directly.
#[cfg(test)]
fn event_bytes(event: &JsValue) -> usize {
    stringify_json(event)
        .expect("event stringify cannot fail")
        .expect("GatewayEvent is JSON-serializable")
        .len()
}

/// JS `typeof event.runId === "string" && event.runId.trim().length > 0`.
fn has_run_id(event: &JsValue) -> bool {
    event
        .get("runId")
        .and_then(JsValue::as_string)
        .is_some_and(|run_id| run_id.units().iter().any(|unit| !js_whitespace(*unit)))
}

/// The decimal key a string/array spread gives element `index`.
fn index_key(index: usize) -> JsString {
    JsString::from(index.to_string())
}

/// ECMAScript `String.prototype.trim` WhiteSpace + LineTerminator set.
fn js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        9..=13
            | 32
            | 160
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{JsString, ReplayTimer, ThreadTimer};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type Callback = Arc<dyn Fn() + Send + Sync>;

    /// Manually advanced timer used by the oracle replay and regression tests.
    /// `advance` mirrors the fixture's intercepted `setTimeout` loop: timers
    /// due at or before the new virtual time fire in `(due, sequence)` order.
    pub(crate) struct ManualTimer {
        state: Mutex<ManualState>,
    }

    struct ManualState {
        now_ms: u64,
        next_sequence: u64,
        entries: HashMap<JsString, (u64, u64, Callback)>,
    }

    impl ManualTimer {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(ManualState {
                    now_ms: 0,
                    next_sequence: 0,
                    entries: HashMap::new(),
                }),
            })
        }

        pub(crate) fn advance(&self, ms: u64) {
            let now_ms = {
                let mut state = self.state.lock().unwrap();
                state.now_ms = state.now_ms.saturating_add(ms);
                state.now_ms
            };
            loop {
                let callback = {
                    let mut state = self.state.lock().unwrap();
                    let key = state
                        .entries
                        .iter()
                        .filter(|(_, (at, _, _))| *at <= now_ms)
                        .min_by_key(|(_, (at, sequence, _))| (*at, *sequence))
                        .map(|(key, _)| key.clone());
                    key.and_then(|key| state.entries.remove(&key))
                        .map(|(_, _, callback)| callback)
                };
                match callback {
                    Some(callback) => callback(),
                    None => return,
                }
            }
        }

        /// Live timer count for one session. TS keeps at most one per session;
        /// repeated `retainTerminal` calls must not accumulate entries.
        pub(crate) fn pending(&self, session_key: &JsString) -> usize {
            usize::from(self.state.lock().unwrap().entries.contains_key(session_key))
        }

        pub(crate) fn pending_total(&self) -> usize {
            self.state.lock().unwrap().entries.len()
        }
    }

    /// The production `ThreadTimer` plus its worker-exit flag: the flag flips
    /// when the timer's worker thread returns, which `ThreadTimer`'s `Drop`
    /// joins first, so a test can prove the thread exited while holding
    /// neither the timer nor the store (and therefore without keeping them
    /// alive). The timer is injected through `with_timer`, but it is the same
    /// production worker the default options build.
    pub(crate) fn production_timer_with_exit_flag()
    -> (Arc<dyn ReplayTimer>, Arc<std::sync::atomic::AtomicBool>) {
        let timer: Arc<ThreadTimer> = Arc::new(ThreadTimer::new());
        let exited = timer.exit_flag();
        (timer, exited)
    }

    /// A timer that panics exactly on the next call while armed and otherwise
    /// delegates to the wrapped manual timer: the deterministic stand-in for
    /// a real panicking injected `ReplayTimer` (#805). The TS store has no
    /// timer injection port; the throwing-host-timer expectations were
    /// measured with a real probe on the fixed baseline
    /// (`_tmp/20261026-issue-805/ts-timer-throw-probe.log`).
    pub(crate) struct PanicTimer {
        inner: Arc<ManualTimer>,
        armed: AtomicBool,
    }

    impl PanicTimer {
        pub(crate) fn wrap(inner: Arc<ManualTimer>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                armed: AtomicBool::new(false),
            })
        }

        /// The next timer call panics once, then the timer is healthy again.
        pub(crate) fn arm(&self) {
            self.armed.store(true, Ordering::SeqCst);
        }

        fn trip(&self) {
            if self.armed.swap(false, Ordering::SeqCst) {
                panic!("injected ReplayTimer panic");
            }
        }

        pub(crate) fn advance(&self, ms: u64) {
            self.inner.advance(ms);
        }

        /// Live timer count for one session, through to the wrapped timer.
        pub(crate) fn pending(&self, session_key: &JsString) -> usize {
            self.inner.pending(session_key)
        }
    }

    impl ReplayTimer for PanicTimer {
        fn set(&self, session_key: &JsString, delay: Duration, callback: Callback) {
            self.trip();
            self.inner.set(session_key, delay, callback);
        }

        fn cancel(&self, session_key: &JsString) {
            self.trip();
            self.inner.cancel(session_key);
        }

        fn cancel_all(&self) {
            self.trip();
            self.inner.cancel_all();
        }
    }

    impl ReplayTimer for ManualTimer {
        fn set(&self, session_key: &JsString, delay: Duration, callback: Callback) {
            let mut state = self.state.lock().unwrap();
            let sequence = state.next_sequence;
            state.next_sequence = state.next_sequence.wrapping_add(1);
            let due = state.now_ms.saturating_add(delay.as_millis() as u64);
            // Like `Map.set`, replacing the session's timer drops the old one.
            state
                .entries
                .insert(session_key.clone(), (due, sequence, callback));
        }

        fn cancel(&self, session_key: &JsString) {
            self.state.lock().unwrap().entries.remove(session_key);
        }

        fn cancel_all(&self) {
            self.state.lock().unwrap().entries.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{ManualTimer, PanicTimer};
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};

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
    fn store_cases(oracle: &JsValue) -> &[JsValue] {
        items(field(oracle, "storeCases"))
    }
    fn find_case<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
        store_cases(oracle)
            .iter()
            .find(|case| text(field(case, "name")).equals_str(name))
            .unwrap_or_else(|| panic!("missing store case {name}"))
    }
    fn tick(index: usize) -> JsValue {
        let mut event = JsObject::new();
        event.insert("type".into(), "tick".into());
        event.insert("index".into(), JsValue::Number(index as f64));
        JsValue::Object(event)
    }
    fn padding(json_bytes: usize) -> JsValue {
        let mut empty = JsObject::new();
        empty.insert("type".into(), "padding".into());
        empty.insert("text".into(), JsValue::String(JsString::from("")));
        let base = event_bytes(&JsValue::Object(empty));
        let mut event = JsObject::new();
        event.insert("type".into(), "padding".into());
        event.insert(
            "text".into(),
            JsValue::String(JsString::from("a".repeat(json_bytes.saturating_sub(base)))),
        );
        JsValue::Object(event)
    }
    fn byte_result(name: &str, value: usize) -> JsValue {
        let mut object = JsObject::new();
        object.insert(name.into(), JsValue::Number(value as f64));
        JsValue::Object(object)
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

    fn run_store_case(case_value: &JsValue) {
        let name = text(field(case_value, "name"))
            .to_utf8()
            .expect("UTF-8 case name");
        let timer = ManualTimer::new();
        let store = TurnReplayStore::with_options(options(case_value, &timer));
        let steps = items(field(case_value, "steps"));
        let expected = items(field(case_value, "results"));
        assert_eq!(steps.len(), expected.len(), "{name}: result count");
        for (index, (step, expected)) in steps.iter().zip(expected).enumerate() {
            let step_op = op(step);
            let actual = match step_op.as_str() {
                "start" => {
                    store.start(&session(step), &run(step));
                    JsValue::Null
                }
                "record" => {
                    store.record(&session(step), field(step, "event"));
                    JsValue::Null
                }
                "recordMany" => {
                    let count = number(field(step, "count")) as usize;
                    for item in 0..count {
                        store.record(&session(step), &tick(item));
                    }
                    JsValue::Null
                }
                "recordPadded" => {
                    let target = number(field(step, "jsonBytes")) as usize;
                    let event = padding(target);
                    let bytes = event_bytes(&event);
                    assert_eq!(bytes, target, "{name}: padded event byte count");
                    store.record(&session(step), &event);
                    byte_result("actualBytes", bytes)
                }
                "eventBytes" => byte_result("bytes", event_bytes(field(step, "event"))),
                "retainTerminal" => {
                    store.retain_terminal(&session(step), &run(step));
                    JsValue::Null
                }
                "clearTerminal" => {
                    store.clear_terminal(&session(step));
                    JsValue::Null
                }
                "withRunId" => store.with_run_id(&session(step), field(step, "event")),
                // The non-object edge of the same call: number, boolean,
                // string, array and null spread (or throw) exactly like the
                // TS `{ ...event, runId }`, recorded through the same catch
                // the generator wraps the step in.
                "withRunIdEdge" => {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        store.with_run_id(&session(step), field(step, "event"))
                    }));
                    let mut result = JsObject::new();
                    match outcome {
                        Ok(value) => {
                            result.insert("value".into(), value);
                        }
                        // TS throws a TypeError reading `runId` off `null`;
                        // the Rust port panics at the same property access —
                        // the port-wide stand-in for a JS throw.
                        Err(_panic) => {
                            result.insert("thrown".into(), "TypeError".into());
                        }
                    }
                    JsValue::Object(result)
                }
                "snapshot" => {
                    let mut input = ActiveTurnSnapshotInput::new(session(step));
                    if step.get("includeEvents").and_then(JsValue::as_bool) == Some(false) {
                        input = input.without_events();
                    }
                    store.snapshot(&input).to_json()
                }
                "dispose" => {
                    store.dispose();
                    JsValue::Null
                }
                "advanceTime" => {
                    timer.advance(number(field(step, "ms")) as u64);
                    JsValue::Null
                }
                other => panic!("{name}: unknown store op {other}"),
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
            run_store_case(find_case(&oracle_value, name));
        }
    }

    #[test]
    fn default_event_limit_and_sticky_truncation_match_ts() {
        run_cases(&["default-count-limit-and-sticky-truncation"]);
    }
    #[test]
    fn custom_event_limit_matches_ts() {
        run_cases(&["custom-event-limit"]);
    }
    #[test]
    fn multibyte_utf8_byte_boundary_matches_ts() {
        run_cases(&["multibyte-byte-boundary-one-over"]);
    }
    #[test]
    fn lone_surrogate_byte_count_matches_ts() {
        run_cases(&[
            "lone-surrogate-exact-limit-kept",
            "lone-surrogate-one-over-drops-itself",
        ]);
    }
    #[test]
    fn mixed_surrogate_multibyte_byte_count_matches_ts() {
        run_cases(&[
            "mixed-surrogate-multibyte-exact-limit-kept",
            "mixed-surrogate-multibyte-one-over-drops-itself",
        ]);
    }
    #[test]
    fn single_event_over_limit_deletes_itself_matches_ts() {
        run_cases(&["single-event-over-custom-byte-limit"]);
    }
    #[test]
    fn default_byte_limit_and_eviction_order_match_ts() {
        run_cases(&[
            "default-byte-limit-exact-boundary",
            "default-byte-limit-one-over",
        ]);
    }
    #[test]
    fn terminal_retention_and_expiry_match_ts() {
        run_cases(&[
            "terminal-default-30s-expiry",
            "terminal-custom-retention-and-restart-cancels-timer",
            "terminal-retain-twice-resets-deadline",
            "terminal-zero-retention-synchronous-snapshot",
        ]);
    }
    /// The generator's intercepted `setTimeout` delay is provenance for the
    /// retention default; consume it instead of leaving a dead field behind.
    #[test]
    fn recorded_timer_delays_match_the_default_retention() {
        let oracle = crate::test_baseline::oracle(ORACLE);
        let delays = |name: &str| {
            items(field(find_case(&oracle, name), "timerDelays"))
                .iter()
                .map(number)
                .collect::<Vec<_>>()
        };
        assert_eq!(delays("terminal-default-30s-expiry"), vec![30_000.0]);
        assert_eq!(
            delays("terminal-zero-retention-synchronous-snapshot"),
            vec![0.0]
        );
    }
    #[test]
    fn terminal_mismatch_record_and_clear_match_ts() {
        run_cases(&[
            "terminal-mismatched-run-is-noop",
            "record-while-terminal-then-clear",
            "clear-terminal-active-keeps-replay",
            "dispose-clears-replay-and-timer",
        ]);
    }
    #[test]
    fn with_run_id_matches_ts() {
        run_cases(&["with-run-id-variants"]);
    }
    /// gate7 P1 #2: non-object events spread like the TS `{ ...event, runId }`
    /// — number/boolean keep only `runId`, strings spread their UTF-16 code
    /// units under index keys (the emoji splits into its surrogate pair),
    /// arrays spread their elements, and `null` throws at the `runId` read.
    #[test]
    fn with_run_id_spreads_non_object_events_like_ts() {
        run_cases(&["with-run-id-non-object-spread"]);
    }
    #[test]
    fn snapshot_without_replay_matches_ts() {
        run_cases(&["snapshot-without-replay"]);
    }

    /// Regression for codex-r1 P1 #2: an idle store must release terminal
    /// buffers at the deadline without any intervening public call.
    #[test]
    fn idle_terminal_expiry_reclaims_without_store_access() {
        let timer = ManualTimer::new();
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("idle");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &run);
        assert_eq!(timer.pending(&key), 1);

        // No store API is called between these observations.
        timer.advance(29_999);
        assert!(store.state.lock().unwrap().replays.contains_key(&key));
        timer.advance(1);
        let state = store.state.lock().unwrap();
        assert!(!state.replays.contains_key(&key));
        assert_eq!(timer.pending(&key), 0);
    }

    /// `ManualTimer` plus a callback-execution flag: the wrapped timer's
    /// expiry callback marks itself as run, so a test can pin the boundary to
    /// the timer's own turn instead of an elapsed-time guess (fix round 5,
    /// codex-r5 P1 #3).
    struct FlaggedTimer {
        inner: Arc<ManualTimer>,
        fired: Arc<AtomicBool>,
    }

    impl FlaggedTimer {
        fn wrap(inner: Arc<ManualTimer>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                fired: Arc::new(AtomicBool::new(false)),
            })
        }

        fn fired(&self) -> bool {
            self.fired.load(Ordering::SeqCst)
        }

        fn advance(&self, ms: u64) {
            self.inner.advance(ms);
        }
    }

    impl ReplayTimer for FlaggedTimer {
        fn set(
            &self,
            session_key: &JsString,
            delay: Duration,
            callback: Arc<dyn Fn() + Send + Sync>,
        ) {
            let fired = Arc::clone(&self.fired);
            self.inner.set(
                session_key,
                delay,
                Arc::new(move || {
                    fired.store(true, Ordering::SeqCst);
                    callback();
                }),
            );
        }

        fn cancel(&self, session_key: &JsString) {
            self.inner.cancel(session_key);
        }

        fn cancel_all(&self) {
            self.inner.cancel_all();
        }
    }

    /// Regression for codex-r1 P1 #3, widened in fix round 5 per codex-r5
    /// P1 #3: a terminal replay stays visible to a synchronous snapshot until
    /// the injected timer actually runs its expiry callback — for every
    /// retention duration the real-timer TS probe keeps visible through the
    /// caller's busy segment (0, 5 and the 30000 default). The `FlaggedTimer`
    /// callback-execution mark proves the boundary is the timer's own turn,
    /// not elapsed time.
    #[test]
    fn terminal_stays_visible_until_the_injected_timer_runs() {
        for retention_ms in [0_u64, 5, 30_000] {
            let timer = FlaggedTimer::wrap(ManualTimer::new());
            let store = TurnReplayStore::with_options(
                TurnReplayOptions::new()
                    .with_terminal_retention_ms(retention_ms)
                    .with_timer(timer.clone()),
            );
            let key = JsString::from("s");
            let run = JsString::from("r1");
            store.start(&key, &run);
            store.record(&key, &tick(0));
            store.retain_terminal(&key, &run);

            assert!(
                !timer.fired(),
                "retention {retention_ms}: the expiry callback must not have run"
            );
            let sync = store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
            assert!(!sync.active);
            assert!(
                sync.terminal,
                "retention {retention_ms}: visible before the timer turn"
            );
            assert_eq!(sync.run_id.as_ref(), Some(&run));
            assert_eq!(sync.events.len(), 1);

            timer.advance(retention_ms);
            assert!(
                timer.fired(),
                "retention {retention_ms}: the expiry callback ran on the timer turn"
            );
            let expired = store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
            assert!(!expired.active);
            assert!(!expired.terminal);
            assert!(expired.run_id.is_none());
            assert!(expired.events.is_empty());
        }
    }

    /// Regression for codex-r1 P2 #5: repeated retain/clear/start keeps a
    /// single live timer per session and removes it with the replay.
    #[test]
    fn terminal_timer_metadata_does_not_accumulate() {
        let timer = ManualTimer::new();
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        for _ in 0..10_000 {
            store.retain_terminal(&key, &run);
        }
        assert_eq!(timer.pending(&key), 1, "one live timer per session");
        assert_eq!(timer.pending_total(), 1);
        assert_eq!(store.state.lock().unwrap().replays.len(), 1);

        store.clear_terminal(&key);
        assert_eq!(timer.pending(&key), 0);
        assert!(store.state.lock().unwrap().replays.is_empty());

        store.start(&key, &JsString::from("r2"));
        store.retain_terminal(&key, &JsString::from("r2"));
        assert_eq!(timer.pending(&key), 1);
        store.start(&key, &JsString::from("r3"));
        assert_eq!(timer.pending(&key), 0, "start cancels the previous timer");
        let state = store.state.lock().unwrap();
        assert!(state.replays.get(&key).is_some_and(|replay| replay.active));
        drop(state);
        store.dispose();
        assert_eq!(timer.pending_total(), 0);
    }

    /// Regression for gate1 concurrency P2 #3: a non-serializable event panics
    /// like TS `JSON.parse`, but must not poison the store's mutex.
    #[test]
    fn non_serializable_record_does_not_poison_the_store() {
        let timer = ManualTimer::new();
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        store.start(&key, &JsString::from("r1"));
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            store.record(&key, &JsValue::Undefined);
        }));
        assert!(panicked.is_err(), "undefined has no JSON text in TS either");
        assert!(
            !store.state.is_poisoned(),
            "the serialization panic must unwind outside the state guard (#805 made the \
             tolerant lock keep serving sessions anyway, so the poison flag is the \
             observable that the serialization stayed outside the lock)"
        );

        // A healthy session still works after the panic.
        let other = JsString::from("other");
        store.start(&other, &JsString::from("r2"));
        store.record(&other, &tick(0));
        let snapshot = store.snapshot(&ActiveTurnSnapshotInput::new(other.clone()));
        assert!(snapshot.active);
        assert_eq!(snapshot.events.len(), 1);
    }

    fn snapshot_of(store: &TurnReplayStore, key: &JsString) -> ActiveTurnSnapshot {
        store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
    }

    /// Regression for #805 (probe cell T1): a panicking injected timer in
    /// `start` must surface the panic at the call site like a throwing host
    /// `clearTimeout`, leaving the old terminal replay and every other
    /// session's state untouched — no lock poisoning.
    #[test]
    fn timer_panic_in_start_keeps_the_store_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &run);

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            store.start(&key, &JsString::from("r2"))
        }));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !store.state.is_poisoned(),
            "the panic must unwind outside the state guard"
        );

        // TS T1: start threw inside clearTerminal, so the replay is still
        // the old terminal one.
        let same = snapshot_of(&store, &key);
        assert!(same.terminal);
        assert_eq!(same.run_id.as_ref(), Some(&run));
        // No other session was touched by the panic.
        let other = JsString::from("other");
        store.start(&other, &JsString::from("r3"));
        store.record(&other, &tick(0));
        let other_snapshot = snapshot_of(&store, &other);
        assert!(other_snapshot.active);
        assert_eq!(other_snapshot.events.len(), 1);

        // The store recovers once the timer is healthy again.
        store.start(&key, &JsString::from("r4"));
        let recovered = snapshot_of(&store, &key);
        assert!(recovered.active);
        assert_eq!(recovered.run_id.as_ref(), Some(&JsString::from("r4")));
    }

    /// Regression for #805 (probe cell T3): `retain_terminal` has already
    /// marked the replay terminal when `set` panics, but no timer was
    /// registered, so the terminal replay never expires on its own — exactly
    /// like a throwing host `setTimeout`.
    #[test]
    fn timer_panic_in_retain_leaves_a_terminal_replay_without_a_timer() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| store.retain_terminal(&key, &run)));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !store.state.is_poisoned(),
            "the panic must unwind outside the state guard"
        );

        let terminal = snapshot_of(&store, &key);
        assert!(terminal.terminal);
        assert_eq!(terminal.run_id.as_ref(), Some(&run));
        // No timer was scheduled, so advancing past the retention changes
        // nothing: the reclamation boundary stays the timer's own turn.
        timer.advance(DEFAULT_TERMINAL_RETENTION_MS);
        assert!(
            snapshot_of(&store, &key).terminal,
            "a terminal replay without a timer never expires on its own"
        );
        // Another session is unaffected and the store recovers.
        let other = JsString::from("other");
        store.start(&other, &JsString::from("r2"));
        assert!(snapshot_of(&store, &other).active);
        // TS T3: clearTerminal recovers the stranded terminal replay.
        store.clear_terminal(&key);
        let cleared = snapshot_of(&store, &key);
        assert!(!cleared.terminal);
        assert!(cleared.run_id.is_none());
    }

    /// Regression for #805 (probe cell T4): the panic in `clear_terminal`
    /// precedes both removals, so the terminal replay survives and a retry
    /// with a healthy timer clears it.
    #[test]
    fn timer_panic_in_clear_terminal_keeps_the_store_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &run);
        assert_eq!(timer.pending(&key), 1);

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| store.clear_terminal(&key)));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !store.state.is_poisoned(),
            "the panic must unwind outside the state guard"
        );

        // TS T4: the throw precedes the timer-map and replay removals.
        let same = snapshot_of(&store, &key);
        assert!(same.terminal);
        assert_eq!(same.run_id.as_ref(), Some(&run));
        assert_eq!(timer.pending(&key), 1, "the cancel never reached the timer");
        // A healthy retry clears both.
        store.clear_terminal(&key);
        let cleared = snapshot_of(&store, &key);
        assert!(!cleared.terminal);
        assert!(cleared.run_id.is_none());
        assert_eq!(timer.pending(&key), 0);
    }

    /// Regression for #805 (probe cell T5): a `dispose` whose `cancel_all`
    /// panics throws before either map is cleared, so both sessions keep
    /// their replays and a retry with a healthy timer clears everything.
    #[test]
    fn timer_panic_in_dispose_keeps_the_store_usable() {
        let timer = PanicTimer::wrap(ManualTimer::new());
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        let other = JsString::from("other");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &run);
        store.start(&other, &JsString::from("r2"));
        store.record(&other, &tick(1));

        timer.arm();
        let panicked = catch_unwind(AssertUnwindSafe(|| store.dispose()));
        assert!(
            panicked.is_err(),
            "the timer panic must surface at the call site"
        );
        assert!(
            !store.state.is_poisoned(),
            "the panic must unwind outside the state guard"
        );

        // TS T5: neither map was cleared.
        let terminal = snapshot_of(&store, &key);
        assert!(terminal.terminal);
        assert_eq!(terminal.run_id.as_ref(), Some(&run));
        let active = snapshot_of(&store, &other);
        assert!(active.active);
        assert_eq!(active.run_id.as_ref(), Some(&JsString::from("r2")));
        // A healthy retry disposes both sessions.
        store.dispose();
        assert!(snapshot_of(&store, &key).run_id.is_none());
        assert!(snapshot_of(&store, &other).run_id.is_none());
    }

    /// Regression for #805 (matrix cell P1): a state lock poisoned by a panic
    /// elsewhere must not take the store down for any session — every entry
    /// recovers the guard, including the deadline callback on the timer.
    #[test]
    fn poisoned_replay_state_lock_still_serves_every_session() {
        let timer = ManualTimer::new();
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.poison_state_for_test();
        assert!(store.state.is_poisoned(), "the hook must poison the lock");

        // The same session and another session both keep working.
        store.record(&key, &tick(1));
        let same = snapshot_of(&store, &key);
        assert!(same.active);
        assert_eq!(same.events.len(), 2);
        let other = JsString::from("other");
        store.start(&other, &JsString::from("r2"));
        assert!(snapshot_of(&store, &other).active);

        // The deadline callback also runs through the poisoned lock.
        store.retain_terminal(&key, &run);
        timer.advance(DEFAULT_TERMINAL_RETENTION_MS);
        let expired = snapshot_of(&store, &key);
        assert!(!expired.terminal);
        assert!(expired.run_id.is_none());
    }

    /// Regression for gate2 concurrency P2 #2: `record` serializes outside
    /// the state lock, so a `start` can replace the run in that window. The
    /// old run's event must not be adopted by the new replay — in TS it stays
    /// with the discarded replay object.
    #[test]
    fn record_serialized_across_a_start_is_not_adopted_by_the_new_run() {
        let timer = ManualTimer::new();
        let store = Arc::new(TurnReplayStore::with_options(
            TurnReplayOptions::new().with_timer(timer.clone()),
        ));
        let key = JsString::from("s");
        store.start(&key, &JsString::from("r1"));

        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        store.set_after_record_gap_hook(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }));
        let recording = Arc::clone(&store);
        let record_key = key.clone();
        let record_thread = std::thread::spawn(move || {
            recording.record(&record_key, &tick(0));
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();

        // The state lock is free while the event serializes, so the restart
        // can and does complete inside the gap.
        let starting = Arc::clone(&store);
        let start_key = key.clone();
        let (done_tx, done) = std::sync::mpsc::channel();
        let start_thread = std::thread::spawn(move || {
            starting.start(&start_key, &JsString::from("r2"));
            let _ = done_tx.send(());
        });
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        record_thread.join().unwrap();
        start_thread.join().unwrap();
        store.set_after_record_gap_hook(Arc::new(|| {}));

        let after = store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert_eq!(after.run_id, Some(JsString::from("r2")));
        assert!(
            after.events.is_empty(),
            "the r1-era event must not appear in the r2 replay"
        );

        // The new run still records normally afterwards.
        store.record(&key, &tick(1));
        let after = store.snapshot(&ActiveTurnSnapshotInput::new(key));
        assert_eq!(after.events.len(), 1);
    }

    /// A timer whose callbacks the test fires by hand: the worker in
    /// `ThreadTimer` pops an entry before running it, so a callback can be
    /// "detached but not yet executed" exactly when `start` cancels the timer.
    struct CaptureTimer {
        callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    }
    impl CaptureTimer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                callbacks: Mutex::new(Vec::new()),
            })
        }
    }
    impl ReplayTimer for CaptureTimer {
        fn set(
            &self,
            _session_key: &JsString,
            _delay: Duration,
            callback: Arc<dyn Fn() + Send + Sync>,
        ) {
            self.callbacks.lock().unwrap().push(callback);
        }
        fn cancel(&self, _session_key: &JsString) {}
        fn cancel_all(&self) {
            self.callbacks.lock().unwrap().clear();
        }
    }

    /// Regression for codex-r2 P1 #1: a callback the timer worker already
    /// detached cannot be cancelled by a restart, but it must also not delete a
    /// newer run's terminal replay — the same runId restart is the sharpest
    /// case because only the replay identity, not the runId, can tell the
    /// generations apart.
    #[test]
    fn stale_timer_callback_cannot_delete_a_restarted_terminal_replay() {
        let timer = CaptureTimer::new();
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_timer(timer.clone()));
        let key = JsString::from("s");
        store.start(&key, &JsString::from("r1"));
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &JsString::from("r1"));
        let first = timer.callbacks.lock().unwrap().pop().expect("first retain");

        // Restart with the same runId, then terminalize the new replay: its
        // retain produces the same generation number the stale callback saw.
        store.start(&key, &JsString::from("r1"));
        store.retain_terminal(&key, &JsString::from("r1"));
        let second = timer
            .callbacks
            .lock()
            .unwrap()
            .pop()
            .expect("second retain");

        // The detached first callback runs after the restart: it must be a
        // no-op against the new generation.
        first();
        let snapshot = store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(
            snapshot.terminal,
            "the restarted run's terminal replay survives the stale callback"
        );
        assert_eq!(snapshot.run_id.as_ref(), Some(&JsString::from("r1")));

        second();
        let expired = store.snapshot(&ActiveTurnSnapshotInput::new(key.clone()));
        assert!(!expired.terminal);
        assert!(expired.run_id.is_none());
    }

    fn wait_for_removal(store: &TurnReplayStore, key: &JsString, context: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !store.state.lock().unwrap().replays.contains_key(key) {
                return;
            }
            assert!(Instant::now() < deadline, "{context}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Regression for codex-r2 P1 #5: stores built from cloned options must not
    /// cancel or clear each other's timers. A same-key restart on the sibling
    /// store used to cancel the shared timer entry and leak the terminal
    /// replay forever.
    #[test]
    fn cloned_options_do_not_share_terminal_timers_same_key() {
        let options = TurnReplayOptions::new().with_terminal_retention_ms(40);
        let first = TurnReplayStore::with_options(options.clone());
        let second = TurnReplayStore::with_options(options);
        let key = JsString::from("s");
        first.start(&key, &JsString::from("r1"));
        first.record(&key, &tick(0));
        first.retain_terminal(&key, &JsString::from("r1"));
        second.start(&key, &JsString::from("r2"));
        wait_for_removal(
            &first,
            &key,
            "the sibling store's restart must not cancel first's timer",
        );
    }

    /// Same, for `dispose`: clearing one store's scheduler must not drop the
    /// other store's pending deadline.
    #[test]
    fn cloned_options_do_not_share_terminal_timers_dispose() {
        let options = TurnReplayOptions::new().with_terminal_retention_ms(40);
        let first = TurnReplayStore::with_options(options.clone());
        let second = TurnReplayStore::with_options(options);
        let key = JsString::from("s");
        let other = JsString::from("other");
        first.start(&key, &JsString::from("r1"));
        first.retain_terminal(&key, &JsString::from("r1"));
        second.start(&other, &JsString::from("r2"));
        second.retain_terminal(&other, &JsString::from("r2"));
        first.dispose();
        wait_for_removal(
            &second,
            &other,
            "first.dispose() must not clear second's timer",
        );
    }

    /// Regression for codex-r2 P1 #4: the production timer's 1 ms floor keeps
    /// the next-macrotask boundary observable for adjacent calls, and a
    /// zero-delay expiry still fires on the timer thread with no store call in
    /// between (the JS macrotask boundary itself is not promised — see the
    /// module note and accepted-diffs.md). The "still visible right after
    /// retain" side is not asserted here: with a real 1 ms timer that single
    /// observation races the worker thread under load; the exact boundary is
    /// pinned deterministically by
    /// `terminal_stays_visible_until_the_injected_timer_runs` with the
    /// injected timer. This test pins the other side race-free: the reclamation
    /// happens on the timer thread with no store call at all.
    #[test]
    fn production_zero_retention_reclaims_without_store_access() {
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_terminal_retention_ms(0));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.record(&key, &tick(0));
        store.retain_terminal(&key, &run);
        wait_for_removal(&store, &key, "the zero-delay production timer did not fire");
    }

    /// codex-r3 P1 #3, original scenario, production path, reworded in fix
    /// round 5 per codex-r5 P1 #3: the real TS host keeps a terminal replay
    /// visible through the caller's whole synchronous segment and the
    /// pending microtasks — for every retention duration, verified with real
    /// timers for retention 0, 5 and the 30000 default
    /// (`_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`). The
    /// Rust production timer does not wait for the caller's segment: it
    /// fires on its own thread as soon as the deadline elapses. This test
    /// claims only the eventual half — the reclamation happens with no store
    /// call at all, though whether it landed inside the busy gap or after it
    /// is not observable by polling — and registers the divergence as
    /// A7.2-REPLAY-RETENTION-TIMER-001 (implementation choice, pending
    /// decision) in `specs/gateway-parity/accepted-diffs.md`. The "still
    /// visible before the timer runs" side stays pinned deterministically by
    /// `terminal_stays_visible_until_the_injected_timer_runs`.
    #[test]
    fn production_retention_reclaims_eventually_after_a_busy_gap() {
        for retention_ms in [0_u64, 5] {
            let store = TurnReplayStore::with_options(
                TurnReplayOptions::new().with_terminal_retention_ms(retention_ms),
            );
            let key = JsString::from("s");
            let run = JsString::from("r1");
            store.start(&key, &run);
            store.record(&key, &tick(0));
            store.retain_terminal(&key, &run);
            // The caller stays busy across the deadline without calling the
            // store, like the real-timer TS probe that busy-spins past the
            // retention and still reads a terminal snapshot.
            let busy_until = Instant::now() + Duration::from_millis(20);
            while Instant::now() < busy_until {
                std::hint::spin_loop();
            }
            // Reading internal state is not a store call: the reclamation is
            // attributed to the timer thread alone. Only the eventual
            // reclamation is claimed — the polling cannot prove it happened
            // inside the busy gap.
            wait_for_removal(
                &store,
                &key,
                &format!(
                    "registered divergence A7.2-REPLAY-RETENTION-TIMER-001: the \
                     production timer eventually reclaims the \
                     retention-{retention_ms} terminal replay with no store \
                     call, unlike the TS timer-turn boundary"
                ),
            );
        }
    }

    /// Node coerces `setTimeout` delays above `2^31 - 1` ms to 1 ms; without
    /// that clamp, `Instant + Duration` would overflow (and panic while the
    /// store lock is held) for an extreme retention configuration.
    #[test]
    fn huge_terminal_retention_is_clamped_like_node_settimeout() {
        let store = TurnReplayStore::with_options(
            TurnReplayOptions::new().with_terminal_retention_ms(u64::MAX),
        );
        let key = JsString::from("s");
        store.start(&key, &JsString::from("r1"));
        store.retain_terminal(&key, &JsString::from("r1"));
        wait_for_removal(
            &store,
            &key,
            "the clamped retention did not fire within 5 s",
        );
    }

    /// Regression for codex-r1 P1 #4: expiry is timer-driven. The default timer
    /// is the only production clock and waits on `Instant`, so a wall-clock jump
    /// cannot move a deadline; the injected timer proves no store call consults
    /// a wall clock and that only the scheduled elapsed time fires.
    #[test]
    fn terminal_expiry_is_driven_by_elapsed_timer_time_not_wall_clock() {
        let timer = ManualTimer::new();
        let store = TurnReplayStore::with_options(
            TurnReplayOptions::new()
                .with_terminal_retention_ms(5_000)
                .with_timer(timer.clone()),
        );
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.retain_terminal(&key, &run);
        // Advancing only the harness clock, without firing due timers, must not
        // expire anything: no store path reads a clock.
        timer.advance(4_999);
        assert!(store.state.lock().unwrap().replays.contains_key(&key));
        timer.advance(1);
        assert!(!store.state.lock().unwrap().replays.contains_key(&key));
    }

    /// The default `ThreadTimer` is the production path; the oracle replay uses
    /// `ManualTimer`. A short real retention proves the worker thread wakes on
    /// its own and deletes the replay with no store call in between.
    #[test]
    fn production_timer_fires_at_the_scheduled_delay() {
        let store =
            TurnReplayStore::with_options(TurnReplayOptions::new().with_terminal_retention_ms(40));
        let key = JsString::from("s");
        let run = JsString::from("r1");
        store.start(&key, &run);
        store.retain_terminal(&key, &run);
        assert!(
            store
                .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
                .terminal,
            "the terminal replay is visible right after scheduling"
        );
        wait_for_removal(&store, &key, "the production timer did not fire within 5 s");
    }

    #[test]
    #[ignore = "re-runs the real TS generator; see README.md"]
    fn real_ts_turn_replay_regenerates_identically() {
        crate::test_baseline::regenerates_identically("turn-replay");
    }

    #[test]
    fn defaults_are_the_three_ts_limits() {
        let options = TurnReplayOptions::new();
        assert_eq!(options.event_limit(), 500);
        assert_eq!(options.byte_limit(), 256 * 1024);
        assert_eq!(options.terminal_retention_ms(), 30_000);
    }
    #[test]
    fn limits_are_clamped_like_ts() {
        let options = TurnReplayOptions::new()
            .with_event_limit(0)
            .with_byte_limit(0)
            .with_terminal_retention_ms(0);
        assert_eq!(options.event_limit(), 1);
        assert_eq!(options.byte_limit(), 1);
        assert_eq!(options.terminal_retention_ms(), 0);
    }
}
