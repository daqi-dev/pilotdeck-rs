#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::{
    host::{HostFuture, ModuleError},
    runtime::{
        async_hooks::{
            AsyncHookClock, AsyncHookRegistration, AsyncHookRegistry, AsyncHookStatus,
            AsyncHookTimer, AsyncHookTurn, timer_delay_ms,
        },
        steer::{MutationRecorder, SteerMailbox},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;
static UNDEFINED: JsValue = JsValue::Undefined;
const ORACLE: &str = include_str!("fixtures/async-hooks/oracle.json");
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue) -> JsString {
    value.as_string().expect("string").clone()
}
fn number(value: &JsValue) -> f64 {
    if let JsValue::Number(n) = value {
        *n
    } else {
        panic!("number")
    }
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(a) = value {
        a
    } else {
        panic!("array")
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}
fn js(s: &JsString) -> JsValue {
    JsValue::String(s.clone())
}
fn ok(value: JsValue) -> JsValue {
    object([("ok", JsValue::Bool(true)), ("value", value)])
}
async fn capture(future: HostFuture<JsValue>) -> JsValue {
    match future.await {
        Ok(value) => ok(value),
        Err(error) => {
            let module = error.module_error.unwrap();
            let mut result = JsObject::new();
            result.insert("ok".into(), JsValue::Bool(false));
            result.insert("error".into(), js(&module.message));
            if let Some(code) = module.code {
                result.insert("code".into(), js(&code));
            }
            JsValue::Object(result)
        }
    }
}
struct TimerGuard(Arc<AtomicBool>);
impl AsyncHookTimer for TimerGuard {}
impl Drop for TimerGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Timer {
    at: f64,
    cancelled: Arc<AtomicBool>,
    callback: Box<dyn FnOnce() + Send>,
}
#[derive(Default)]
struct Clock {
    now: Mutex<f64>,
    timers: Mutex<Vec<Timer>>,
}
impl AsyncHookClock for Clock {
    fn now_ms(&self) -> f64 {
        *self.now.lock().unwrap()
    }
    fn schedule(&self, delay: f64, callback: Box<dyn FnOnce() + Send>) -> Box<dyn AsyncHookTimer> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let at = self.now_ms() + timer_delay_ms(delay) as f64;
        self.timers.lock().unwrap().push(Timer {
            at,
            cancelled: cancelled.clone(),
            callback,
        });
        Box::new(TimerGuard(cancelled))
    }
}
impl Clock {
    fn advance(&self, ms: f64, fire: bool) {
        let target = self.now_ms() + ms;
        if fire {
            loop {
                let next = {
                    let mut timers = self.timers.lock().unwrap();
                    timers.retain(|t| !t.cancelled.load(Ordering::SeqCst));
                    let index = timers
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.at <= target)
                        .min_by(|(i, a), (j, b)| a.at.total_cmp(&b.at).then(i.cmp(j)))
                        .map(|(i, _)| i);
                    index.map(|index| timers.remove(index))
                };
                let Some(timer) = next else { break };
                *self.now.lock().unwrap() = self.now_ms().max(timer.at);
                (timer.callback)();
            }
        }
        *self.now.lock().unwrap() = target;
    }
}
#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}
#[derive(Default)]
struct Recording {
    mutations: Mutex<Vec<JsValue>>,
    gate: Mutex<Option<Arc<Gate>>>,
    fail: AtomicBool,
}
struct Recorder {
    session: JsString,
    state: Arc<Recording>,
}
impl MutationRecorder for Recorder {
    fn record(&self, turn: JsString, mutation: JsObject) -> HostFuture<()> {
        let state = self.state.clone();
        let session = self.session.clone();
        Box::pin(async move {
            let insert = mutation
                .get("mutation")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str("insert"));
            state.mutations.lock().unwrap().push(object([
                ("session", js(&session)),
                ("turn", js(&turn)),
                ("mutation", JsValue::Object(mutation)),
            ]));
            let gate = if insert {
                state.gate.lock().unwrap().take()
            } else {
                None
            };
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            if insert && state.fail.swap(false, Ordering::SeqCst) {
                return Err(ModuleError::new("fixture storage failure").into());
            }
            Ok(())
        })
    }
}
struct Harness {
    clock: Arc<Clock>,
    registry: AsyncHookRegistry,
    active: Arc<Mutex<HashMap<JsString, AsyncHookTurn>>>,
    mailboxes: HashMap<JsString, SteerMailbox>,
    recording: Arc<Recording>,
    events: Arc<Mutex<Vec<JsValue>>>,
}
impl Harness {
    fn new() -> Self {
        let clock = Arc::new(Clock::default());
        let active = Arc::new(Mutex::new(HashMap::<JsString, AsyncHookTurn>::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let lookup = active.clone();
        let sink = events.clone();
        let registry = AsyncHookRegistry::new(
            clock.clone(),
            move |session| lookup.lock().unwrap().get(session).cloned(),
            move |session, event| {
                sink.lock().unwrap().push(object([
                    ("session", js(session)),
                    ("event", JsValue::Object(event)),
                ]))
            },
        );
        Self {
            clock,
            registry,
            active,
            mailboxes: HashMap::new(),
            recording: Arc::new(Recording::default()),
            events,
        }
    }
    fn mailbox(&mut self, session: &JsString) -> SteerMailbox {
        self.mailboxes
            .entry(session.clone())
            .or_insert_with(|| {
                SteerMailbox::new(Some(Arc::new(Recorder {
                    session: session.clone(),
                    state: self.recording.clone(),
                })))
            })
            .clone()
    }
    fn invoke(&mut self, step: &JsValue) -> HostFuture<JsValue> {
        let op = string(field(step, "op"));
        let session = step.get("session").map(string).unwrap_or_default();
        let run = step.get("run").map(string).unwrap_or_default();
        let mut value = JsValue::Null;
        match op.to_utf8().unwrap().as_str() {
            "start" => {
                let mailbox = self.mailbox(&session);
                mailbox.start(run.clone());
                self.active.lock().unwrap().insert(
                    session,
                    AsyncHookTurn {
                        run_id: run,
                        mailbox,
                    },
                );
            }
            "finish" => {
                self.active.lock().unwrap().remove(&session);
                self.mailbox(&session).finish(&run);
            }
            "register" => {
                let input = field(step, "input");
                let result = self.registry.register(AsyncHookRegistration {
                    session_key: string(field(input, "sessionKey")),
                    invocation_id: string(field(input, "invocationId")),
                    hook_name: string(field(input, "hookName")),
                    hook_event: string(field(input, "hookEvent")),
                    timeout_ms: input.get("timeoutMs").map(number),
                    include_hook_events: matches!(
                        field(input, "includeHookEvents"),
                        JsValue::Bool(true)
                    ),
                });
                return Box::pin(async { result.map(|_| JsValue::Null) });
            }
            "submit" => {
                let input = field(step, "input");
                let future = self.registry.submit(
                    string(field(input, "sessionKey")),
                    string(field(input, "invocationId")),
                    field(input, "output").clone(),
                );
                return Box::pin(async move {
                    future.await.map(|r| {
                        object([
                            ("invocationId", js(&r.invocation_id)),
                            ("status", r.status.as_str().into()),
                        ])
                    })
                });
            }
            "deepSubmit" => {
                let mut event = JsValue::String("event".into());
                for _ in 0..number(field(step, "depth")) as usize {
                    event = JsValue::Array(vec![event]);
                }
                let output = object([("hookSpecificOutput", object([("hookEventName", event)]))]);
                let future = self.registry.submit("s".into(), "a".into(), output);
                return Box::pin(async move {
                    future.await.map(|r| {
                        object([
                            ("invocationId", js(&r.invocation_id)),
                            ("status", r.status.as_str().into()),
                        ])
                    })
                });
            }
            "tick" => self.clock.advance(number(field(step, "ms")), true),
            "now" => self.clock.advance(number(field(step, "ms")), false),
            "expire" => self.registry.expire_for_turn(&session, &run),
            "drain" => {
                value = JsValue::Array(
                    self.mailbox(&session)
                        .drain(&run)
                        .iter()
                        .map(|m| JsValue::Object(m.to_object()))
                        .collect(),
                )
            }
            "close" => {
                let result = self.mailbox(&session).drain_or_close(&run);
                value = object([
                    (
                        "messages",
                        JsValue::Array(
                            result
                                .messages
                                .iter()
                                .map(|m| JsValue::Object(m.to_object()))
                                .collect(),
                        ),
                    ),
                    ("closed", JsValue::Bool(result.closed)),
                ]);
            }
            _ => panic!("unknown op {op:?}"),
        }
        Box::pin(async { Ok(value) })
    }
}
#[tokio::test]
async fn replay_real_gateway_async_hooks() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 38);
    let mut count = 0;
    for scenario in scenarios {
        let name = string(field(scenario, "name")).to_utf8().unwrap();
        let mut h = Harness::new();
        let mut pending = HashMap::new();
        let mut blocked = None;
        for (index, step) in array(field(scenario, "steps")).iter().enumerate() {
            count += 1;
            let mut actual = JsValue::Null;
            match string(field(step, "op")).to_utf8().unwrap().as_str() {
                "block" => {
                    let gate = Arc::new(Gate::default());
                    *h.recording.gate.lock().unwrap() = Some(gate.clone());
                    blocked = Some(gate);
                }
                "wait" => blocked.as_ref().unwrap().entered.notified().await,
                "release" => blocked.as_ref().unwrap().release.notify_one(),
                "fail" => h.recording.fail.store(true, Ordering::SeqCst),
                "begin" => {
                    let future = h.invoke(field(step, "action"));
                    pending.insert(string(field(step, "key")), tokio::spawn(capture(future)));
                }
                "join" => {
                    actual = pending
                        .remove(&string(field(step, "key")))
                        .unwrap()
                        .await
                        .unwrap()
                }
                _ => actual = capture(h.invoke(step)).await,
            }
            assert_eq!(
                &actual,
                field(step, "result"),
                "{name:?} step {index}: {step:?}"
            );
        }
        assert!(pending.is_empty());
        assert_eq!(
            JsValue::Array(h.events.lock().unwrap().clone()),
            *field(scenario, "events"),
            "{name:?} events"
        );
        assert_eq!(
            JsValue::Array(h.recording.mutations.lock().unwrap().clone()),
            *field(scenario, "mutations"),
            "{name:?} mutations"
        );
    }
    assert_eq!(count, 285);
}

#[tokio::test(start_paused = true)]
async fn production_timer_expires_and_cancels_on_drop() {
    use pilotdeck_gateway::runtime::async_hooks::TokioHookClock;
    let clock = TokioHookClock;
    let fired = Arc::new(AtomicBool::new(false));
    let mark = fired.clone();
    let timer = clock.schedule(
        10.0,
        Box::new(move || {
            mark.store(true, Ordering::SeqCst);
        }),
    );
    // Do not poll the spawned timer before advancing: its deadline must
    // already be fixed at schedule time, just like setTimeout.
    tokio::time::advance(std::time::Duration::from_millis(9)).await;
    assert!(!fired.load(Ordering::SeqCst));
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(fired.load(Ordering::SeqCst));
    drop(timer);
    let mark = fired.clone();
    fired.store(false, Ordering::SeqCst);
    let timer = clock.schedule(
        1.0,
        Box::new(move || {
            mark.store(true, Ordering::SeqCst);
        }),
    );
    drop(timer);
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    assert!(!fired.load(Ordering::SeqCst));
}

#[tokio::test]
async fn concurrent_empty_submissions_match_ts_duplicate_and_event_counts() {
    // The statuses and event stream come from the real Promise pair. Exercise
    // that same pair on separate Rust threads, not just one Tokio executor.
    let oracle = baseline::oracle(ORACLE);
    let scenario = array(field(&oracle, "scenarios"))
        .iter()
        .find(|s| {
            field(s, "name")
                .as_string()
                .unwrap()
                .equals_str("concurrent-empty")
        })
        .unwrap();
    let steps = array(field(scenario, "steps"));
    let mut expected = steps
        .iter()
        .filter(|s| field(s, "op").as_string().unwrap().equals_str("join"))
        .map(|s| {
            pilotdeck_protocol::stringify_json(field(s, "result"))
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    expected.sort();
    for _ in 0..64 {
        let mut h = Harness::new();
        capture(h.invoke(&steps[0])).await;
        capture(h.invoke(&steps[1])).await;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let futures = std::thread::scope(|scope| {
            let tasks = (0..2)
                .map(|_| {
                    let registry = h.registry.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        registry.submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
                    })
                })
                .collect::<Vec<_>>();
            tasks
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut actual = Vec::new();
        for future in futures {
            let value = capture(Box::pin(async move {
                future.await.map(|r| {
                    object([
                        ("invocationId", js(&r.invocation_id)),
                        ("status", r.status.as_str().into()),
                    ])
                })
            }))
            .await;
            actual.push(pilotdeck_protocol::stringify_json(&value).unwrap().unwrap());
        }
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(
            JsValue::Array(h.events.lock().unwrap().clone()),
            *field(scenario, "events")
        );
        assert_eq!(
            JsValue::Array(h.recording.mutations.lock().unwrap().clone()),
            *field(scenario, "mutations")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_delivery_is_cleared_before_immediate_retry() {
    let oracle = baseline::oracle(ORACLE);
    let scenario = array(field(&oracle, "scenarios"))
        .iter()
        .find(|s| {
            field(s, "name")
                .as_string()
                .unwrap()
                .equals_str("delivery-failure-retry")
        })
        .unwrap();
    let steps = array(field(scenario, "steps"));
    for _ in 0..128 {
        let mut h = Harness::new();
        capture(h.invoke(&steps[0])).await;
        capture(h.invoke(&steps[1])).await;
        h.recording.fail.store(true, Ordering::SeqCst);
        assert_eq!(
            capture(h.invoke(&steps[3])).await,
            *field(&steps[3], "result")
        );
        assert_eq!(
            capture(h.invoke(&steps[4])).await,
            *field(&steps[4], "result")
        );
        assert_eq!(
            JsValue::Array(h.events.lock().unwrap().clone()),
            *field(scenario, "events")
        );
        assert_eq!(
            JsValue::Array(h.recording.mutations.lock().unwrap().clone()),
            *field(scenario, "mutations")
        );
    }
}

#[tokio::test]
async fn empty_context_settlement_does_not_overwrite_expiry_that_won_the_race() {
    // TS settles empty context synchronously, so expiry cannot interleave.
    // With threaded timers it can: fire the expiry timer from inside the run
    // lookup (after admission, before settlement) and require the expiry to
    // stand, as if it had run first in TS (`expired`, one expired event).
    let clock = Arc::new(Clock::default());
    let mailbox = SteerMailbox::new(None);
    mailbox.start("r".into());
    let turn = AsyncHookTurn {
        run_id: "r".into(),
        mailbox,
    };
    let lookups = Arc::new(Mutex::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let (timer_clock, count, sink) = (clock.clone(), lookups.clone(), events.clone());
    let registry = AsyncHookRegistry::new(
        clock.clone(),
        move |_| {
            let mut n = count.lock().unwrap();
            *n += 1;
            if *n == 2 {
                timer_clock.advance(10.0, true);
            }
            Some(turn.clone())
        },
        move |_, event| {
            let status = event.get("status").and_then(JsValue::as_string).cloned();
            sink.lock()
                .unwrap()
                .push(status.unwrap().to_utf8().unwrap());
        },
    );
    registry
        .register(AsyncHookRegistration {
            session_key: "s".into(),
            invocation_id: "a".into(),
            hook_name: "hook".into(),
            hook_event: "PreToolUse".into(),
            timeout_ms: Some(10.0),
            include_hook_events: true,
        })
        .unwrap();
    let result = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(result.status.as_str(), "expired");
    assert_eq!(*lookups.lock().unwrap(), 2);
    assert_eq!(*events.lock().unwrap(), vec!["expired".to_string()]);
    let again = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(again.status.as_str(), "expired");
}

fn registration(id: &str, timeout_ms: f64) -> AsyncHookRegistration {
    AsyncHookRegistration {
        session_key: "s".into(),
        invocation_id: id.into(),
        hook_name: "hook".into(),
        hook_event: "PreToolUse".into(),
        timeout_ms: Some(timeout_ms),
        include_hook_events: true,
    }
}
fn status_sink(events: &Arc<Mutex<Vec<String>>>) -> impl Fn(&JsString, JsObject) + use<> {
    let sink = events.clone();
    move |_, event| {
        let status = event.get("status").and_then(JsValue::as_string).cloned();
        sink.lock()
            .unwrap()
            .push(status.unwrap().to_utf8().unwrap());
    }
}
fn started_turn() -> AsyncHookTurn {
    let mailbox = SteerMailbox::new(None);
    mailbox.start("r".into());
    AsyncHookTurn {
        run_id: "r".into(),
        mailbox,
    }
}

/// Blocks one `now_ms` call (the deadline check of a chosen submit) until
/// released, then reports a time past every deadline.
struct GatedClock {
    inner: Clock,
    calls: Mutex<usize>,
    gate_call: usize,
    entered: Mutex<std::sync::mpsc::Sender<()>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl AsyncHookClock for GatedClock {
    fn now_ms(&self) -> f64 {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call == self.gate_call {
            self.entered.lock().unwrap().send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            return self.inner.now_ms() + 1_000.0;
        }
        self.inner.now_ms()
    }
    fn schedule(&self, delay: f64, callback: Box<dyn FnOnce() + Send>) -> Box<dyn AsyncHookTimer> {
        self.inner.schedule(delay, callback)
    }
}

#[tokio::test]
async fn deadline_expiry_reports_settlement_that_won_after_admission() {
    // Submit A passes the outcome/pending checks, then stalls in its deadline
    // read. Submit B settles `delivered` meanwhile. TS runs A's prefix
    // synchronously, so A either expires first (and B sees `expired`) or sees
    // B's outcome (`duplicate`). A must not report `expired` beside a stored
    // `delivered` outcome and a lone delivered event.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let clock = Arc::new(GatedClock {
        inner: Clock::default(),
        calls: Mutex::new(0),
        gate_call: 2, // 1: register, 2: A's deadline check, 3: B's.
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let turn = started_turn();
    let registry = AsyncHookRegistry::new(
        clock.clone(),
        move |_| Some(turn.clone()),
        status_sink(&events),
    );
    registry.register(registration("a", 10.0)).unwrap();
    let a = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            registry.submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        })
    };
    entered_rx.recv().unwrap();
    let b = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(b.status.as_str(), "delivered");
    release_tx.send(()).unwrap();
    let a = a.join().unwrap().await.unwrap();
    assert_eq!(a.status.as_str(), "duplicate");
    assert_eq!(*events.lock().unwrap(), vec!["delivered".to_string()]);
    let again = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(again.status.as_str(), "duplicate");
}

#[tokio::test]
async fn old_run_expiry_is_not_interleaved_by_a_concurrent_submit() {
    // A's run fence finds no matching run. While A is inside that lookup,
    // submit B starts on another thread and would see the run active. TS
    // expires A synchronously, so B must observe `expired`, never deliver
    // between A's fence and A's expiry.
    // Under the fix B cannot pass admission until A has expired, so every
    // iteration passes deterministically. Without it the bad interleaving is
    // only probabilistic (B must win the admission lock right after A drops
    // it); 10k iterations caught it in every recorded negative-control run.
    for _ in 0..10_000 {
        let clock = Arc::new(Clock::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let turn = started_turn();
        let calls = Arc::new(Mutex::new(0));
        let slot = Arc::new(Mutex::new(None::<AsyncHookRegistry>));
        let b_thread = Arc::new(Mutex::new(None));
        let (count, own, spawned) = (calls.clone(), slot.clone(), b_thread.clone());
        let registry = AsyncHookRegistry::new(
            clock.clone(),
            move |_| {
                let call = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n
                };
                if call != 2 {
                    return Some(turn.clone());
                }
                // A's fence lookup: start B, then report the run as gone.
                let registry = own.lock().unwrap().take().unwrap();
                *spawned.lock().unwrap() = Some(std::thread::spawn(move || {
                    registry.submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
                }));
                std::thread::yield_now();
                None
            },
            status_sink(&events),
        );
        *slot.lock().unwrap() = Some(registry.clone());
        registry.register(registration("a", 10_000.0)).unwrap();
        let a = registry
            .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
            .await
            .unwrap();
        let b_thread = b_thread.lock().unwrap().take().unwrap();
        let b = b_thread.join().unwrap().await.unwrap();
        assert_eq!(
            (a.status.as_str(), b.status.as_str()),
            ("expired", "expired")
        );
        assert_eq!(*events.lock().unwrap(), vec!["expired".to_string()]);
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_registry_cancels_pending_and_retention_timers() {
    use pilotdeck_gateway::runtime::async_hooks::TokioHookClock;
    let metrics = tokio::runtime::Handle::current().metrics();
    let events = Arc::new(Mutex::new(Vec::new()));
    let turn = started_turn();
    let registry = AsyncHookRegistry::new(
        Arc::new(TokioHookClock),
        move |_| Some(turn.clone()),
        status_sink(&events),
    );
    // `a` keeps a pending expiry timer; `b` settles and keeps a retention timer.
    registry.register(registration("a", 10.0)).unwrap();
    registry.register(registration("b", 10.0)).unwrap();
    let b = registry
        .submit("s".into(), "b".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(b.status.as_str(), "delivered");
    tokio::task::yield_now().await;
    assert_eq!(metrics.num_alive_tasks(), 2);
    drop(registry);
    tokio::task::yield_now().await;
    assert_eq!(metrics.num_alive_tasks(), 0);
    tokio::time::advance(std::time::Duration::from_millis(300_001)).await;
    tokio::task::yield_now().await;
    assert_eq!(*events.lock().unwrap(), vec!["delivered".to_string()]);
}

/// Sink that records `id:status`; its first call waits for `go` before
/// recording, holding the drainer inside the sink while the test settles
/// another hook on a different thread.
fn blocking_first_sink(
    events: &Arc<Mutex<Vec<String>>>,
) -> (
    impl Fn(&JsString, JsObject) + use<>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(Some((entered_tx, go_rx)));
    let sink = events.clone();
    let callback = move |_: &JsString, event: JsObject| {
        let text = |key| {
            event
                .get(key)
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap()
        };
        let first = gate.lock().unwrap().take();
        if let Some((entered, go)) = first {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        sink.lock()
            .unwrap()
            .push(format!("{}:{}", text("invocationId"), text("status")));
    };
    (callback, entered_rx, go_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_event_precedes_delivery_that_settled_after_it() {
    // TS `expiry-during-write-then-accepted`: expiry settles and emits while
    // the steer write is pending; the accepted write then settles delivered.
    // Hold the expiry thread between settlement and the sink, let delivery
    // settle meanwhile, and require the TS order [expired, delivered] so the
    // last event matches the stored outcome.
    let clock = Arc::new(Clock::default());
    let recording = Arc::new(Recording::default());
    let gate = Arc::new(Gate::default());
    *recording.gate.lock().unwrap() = Some(gate.clone());
    let mailbox = SteerMailbox::new(Some(Arc::new(Recorder {
        session: "s".into(),
        state: recording.clone(),
    })));
    mailbox.start("r".into());
    let turn = AsyncHookTurn {
        run_id: "r".into(),
        mailbox,
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let (sink, entered, go) = blocking_first_sink(&events);
    let registry = AsyncHookRegistry::new(clock.clone(), move |_| Some(turn.clone()), sink);
    registry.register(registration("a", 10.0)).unwrap();
    let context = object([(
        "hookSpecificOutput",
        object([
            ("hookEventName", "PreToolUse".into()),
            ("additionalContext", "context".into()),
        ]),
    )]);
    let submitted = registry.submit("s".into(), "a".into(), context);
    gate.entered.notified().await;
    let timer = {
        let clock = clock.clone();
        std::thread::spawn(move || clock.advance(10.0, true))
    };
    tokio::task::spawn_blocking(move || entered.recv().unwrap())
        .await
        .unwrap();
    gate.release.notify_one();
    assert_eq!(submitted.await.unwrap().status.as_str(), "delivered");
    go.send(()).unwrap();
    timer.join().unwrap();
    assert_eq!(*events.lock().unwrap(), ["a:expired", "a:delivered"]);
    let again = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(again.status.as_str(), "duplicate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expire_for_turn_burst_is_not_interleaved() {
    // TS expires a whole turn in one synchronous loop. Hold the burst's first
    // event in the sink while another session settles on this thread: its
    // event must follow the burst, not split it or jump ahead of it.
    let clock = Arc::new(Clock::default());
    let turns = HashMap::from([
        (JsString::from("s"), started_turn()),
        (
            JsString::from("t"),
            AsyncHookTurn {
                run_id: "r".into(),
                mailbox: {
                    let mailbox = SteerMailbox::new(None);
                    mailbox.start("r".into());
                    mailbox
                },
            },
        ),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let (sink, entered, go) = blocking_first_sink(&events);
    let registry = AsyncHookRegistry::new(
        clock.clone(),
        move |session| turns.get(session).cloned(),
        sink,
    );
    registry.register(registration("a", 10_000.0)).unwrap();
    registry.register(registration("b", 10_000.0)).unwrap();
    registry
        .register(AsyncHookRegistration {
            session_key: "t".into(),
            ..registration("c", 10_000.0)
        })
        .unwrap();
    let burst = {
        let registry = registry.clone();
        std::thread::spawn(move || registry.expire_for_turn(&"s".into(), &"r".into()))
    };
    tokio::task::spawn_blocking(move || entered.recv().unwrap())
        .await
        .unwrap();
    let c = registry
        .submit("t".into(), "c".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    assert_eq!(c.status.as_str(), "delivered");
    go.send(()).unwrap();
    burst.join().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["a:expired", "b:expired", "c:delivered"]
    );
}

#[tokio::test]
async fn sink_may_reenter_registry_without_deadlock() {
    // The sink runs outside every registry lock. A re-entrant settlement is
    // queued and delivered after the current sink call returns.
    let clock = Arc::new(Clock::default());
    let turn = started_turn();
    let events = Arc::new(Mutex::new(Vec::new()));
    let own = Arc::new(std::sync::OnceLock::<AsyncHookRegistry>::new());
    let (sink, reenter) = (events.clone(), own.clone());
    let registry = AsyncHookRegistry::new(
        clock.clone(),
        move |_| Some(turn.clone()),
        move |_, event| {
            let id = event.get("invocationId").and_then(JsValue::as_string);
            let id = id.unwrap().to_utf8().unwrap();
            if id == "a" {
                reenter
                    .get()
                    .unwrap()
                    .expire_for_turn(&"s".into(), &"r".into());
            }
            sink.lock().unwrap().push(id);
        },
    );
    own.set(registry.clone()).ok().unwrap();
    registry.register(registration("a", 10.0)).unwrap();
    registry.register(registration("b", 10_000.0)).unwrap();
    clock.advance(10.0, true);
    assert_eq!(*events.lock().unwrap(), ["a", "b"]);
}

/// Signals each `now_ms` read made on the thread named `b` (its deadline
/// check, taken after admission and right before the admission lock).
struct SignalClock {
    inner: Clock,
    b_admitted: Mutex<std::sync::mpsc::Sender<()>>,
}
impl AsyncHookClock for SignalClock {
    fn now_ms(&self) -> f64 {
        if std::thread::current().name() == Some("b") {
            let _ = self.b_admitted.lock().unwrap().send(());
        }
        self.inner.now_ms()
    }
    fn schedule(&self, delay: f64, callback: Box<dyn FnOnce() + Send>) -> Box<dyn AsyncHookTimer> {
        self.inner.schedule(delay, callback)
    }
}

#[tokio::test]
async fn old_run_expiry_event_reaches_sink_after_admission_lock_is_released() {
    // A's run fence fails while B (same id) has passed admission and waits
    // for A's per-pending admission lock. The expiry event must reach the
    // sink only after A released every registry lock, like the delivered
    // branch: here the sink waits for B to finish, which cannot happen while
    // A still holds that lock (negative control: flush before drop(in_flight)
    // turns this into a 2 s timeout).
    let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
    let clock = Arc::new(SignalClock {
        inner: Clock::default(),
        b_admitted: Mutex::new(admitted_tx),
    });
    let admitted_rx = Mutex::new(admitted_rx);
    let turn = started_turn();
    let calls = Arc::new(Mutex::new(0));
    let own = Arc::new(std::sync::OnceLock::<AsyncHookRegistry>::new());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<AsyncHookStatus>();
    let b_thread = Arc::new(Mutex::new(None));
    let sink_saw_b = Arc::new(Mutex::new(None));
    let (count, lookup_own, spawned) = (calls.clone(), own.clone(), b_thread.clone());
    let saw = sink_saw_b.clone();
    let done_rx = Mutex::new(done_rx);
    let registry = AsyncHookRegistry::new(
        clock.clone(),
        move |_| {
            let call = {
                let mut n = count.lock().unwrap();
                *n += 1;
                *n
            };
            if call != 2 {
                return Some(turn.clone());
            }
            // A's fence lookup (A holds the admission lock): start B and
            // wait until B is past admission, then report the run as gone.
            let registry = lookup_own.get().unwrap().clone();
            let done = done_tx.clone();
            *spawned.lock().unwrap() = Some(
                std::thread::Builder::new()
                    .name("b".into())
                    .spawn(move || {
                        let future = registry.submit(
                            "s".into(),
                            "a".into(),
                            JsValue::Object(JsObject::new()),
                        );
                        let status = poll_ready_status(future);
                        let _ = done.send(status);
                    })
                    .unwrap(),
            );
            admitted_rx.lock().unwrap().recv().unwrap();
            None
        },
        move |_, _| {
            let status = done_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(2))
                .ok();
            *saw.lock().unwrap() = Some(status);
        },
    );
    own.set(registry.clone()).ok().unwrap();
    registry.register(registration("a", 10_000.0)).unwrap();
    let a = registry
        .submit("s".into(), "a".into(), JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    b_thread.lock().unwrap().take().unwrap().join().unwrap();
    assert_eq!(a.status, AsyncHookStatus::Expired);
    assert_eq!(
        *sink_saw_b.lock().unwrap(),
        Some(Some(AsyncHookStatus::Expired)),
        "sink ran while A still held the admission lock"
    );
}

/// `submit` futures that settle synchronously are already ready.
fn poll_ready_status(
    future: HostFuture<pilotdeck_gateway::runtime::async_hooks::AsyncHookResult>,
) -> AsyncHookStatus {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let mut future = future;
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(result) => result.unwrap().status,
        std::task::Poll::Pending => panic!("synchronous submit must be ready"),
    }
}

/// Regenerates the oracle from the real TS baseline and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn async_hooks_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("async-hooks");
}
