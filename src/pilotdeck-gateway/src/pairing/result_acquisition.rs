//! PREPARED_NOT_RUN. Test-only causal taps; never infer JoinSet readiness from COMPLETE.
use super::{
    Route, frame,
    order_probe::{Observer, hash as digest},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Off,
    Natural,
    HoldRouteReturn,
    HoldAcquired,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Mutation {
    #[default]
    None,
    BypassJoin,
    EarlyR2,
    DoubleRelease,
    ReplaceFrame,
}

struct State {
    mode: Mode,
    mutation: Mutation,
    observer: Observer,
    run: String,
    connection: String,
    target: String,
    successor: String,
    return_gate: (Mutex<bool>, Condvar),
    route_held: AtomicBool,
    acquired: AtomicBool,
    successor_parsed: AtomicBool,
    held: Mutex<Option<(frame::Frame, Route)>>,
    bypass: Mutex<Option<(frame::Frame, Route)>>,
    successor_frame: Mutex<Option<frame::Frame>>,
    release_count: AtomicUsize,
    changed: Notify,
    bypass_changed: Notify,
}

#[derive(Clone, Default)]
pub(super) struct Tap(Option<Arc<State>>);
tokio::task_local! { pub(super) static ACTIVE: Tap; }

pub(super) fn current() -> Tap {
    ACTIVE.try_with(Clone::clone).unwrap_or_default()
}

fn identity(frame: &frame::Frame) -> String {
    super::dispatch::parse(&frame.payload)
        .as_ref()
        .and_then(|value| crate::metadata::text_field(value, "id"))
        .unwrap_or_default()
}

fn facts(frame: &frame::Frame, route: &Route) -> Value {
    json!({
        "requestId":identity(frame),"method":super::dispatch::parse(&frame.payload).as_ref().and_then(|v|crate::metadata::text_field(v,"method")),"inputFrameSha256":digest(&frame.wire),
        "route":match route {Route::Forward=>"forward",Route::LocalResponse(_)=>"local"},
        "payloadSha256":match route {Route::Forward=>None,Route::LocalResponse(bytes)=>Some(digest(bytes))},
    })
}

fn copy_frame(frame: &frame::Frame) -> frame::Frame {
    frame::Frame {
        opcode: frame.opcode,
        wire: frame.wire.clone(),
        payload: frame.payload.clone(),
    }
}
fn copy_result(frame: &frame::Frame, route: &Route) -> (frame::Frame, Route) {
    (
        copy_frame(frame),
        match route {
            Route::Forward => Route::Forward,
            Route::LocalResponse(bytes) => Route::LocalResponse(bytes.clone()),
        },
    )
}

impl Tap {
    pub(super) fn new(mode: Mode, observer: Observer, run: String, connection: &str) -> Self {
        Self::with_mutation(mode, Mutation::None, observer, run, connection)
    }

    pub(super) fn with_mutation(
        mode: Mode,
        mutation: Mutation,
        observer: Observer,
        run: String,
        connection: &str,
    ) -> Self {
        assert!(
            mutation == Mutation::None
                || matches!(
                    (mode, mutation),
                    (Mode::HoldRouteReturn, Mutation::BypassJoin)
                        | (
                            Mode::HoldAcquired,
                            Mutation::EarlyR2 | Mutation::DoubleRelease | Mutation::ReplaceFrame
                        )
                ),
            "mutation/intervention mismatch"
        );
        if mode == Mode::Off {
            return Self::default();
        }
        Self(Some(Arc::new(State {
            mode,
            mutation,
            observer,
            run,
            connection: connection.into(),
            target: format!("{connection}-0"),
            successor: format!("{connection}-1"),
            return_gate: (Mutex::new(false), Condvar::new()),
            route_held: AtomicBool::new(false),
            acquired: AtomicBool::new(false),
            successor_parsed: AtomicBool::new(false),
            held: Mutex::new(None),
            bypass: Mutex::new(None),
            successor_frame: Mutex::new(None),
            release_count: AtomicUsize::new(0),
            changed: Notify::new(),
            bypass_changed: Notify::new(),
        })))
    }

    fn event(&self, name: &str, fields: Value) {
        if let Some(state) = &self.0 {
            let mut fields = fields.as_object().unwrap().clone();
            fields.insert("run".into(), state.run.clone().into());
            fields.insert("connection".into(), state.connection.clone().into());
            fields.insert("generation".into(), 1.into());
            fields.insert("mode".into(), format!("{:?}", state.mode).into());
            fields.insert("mutation".into(), format!("{:?}", state.mutation).into());
            state.observer.event(name, Value::Object(fields));
        }
    }

    pub(super) fn parsed(&self, frame: &frame::Frame) {
        if let Some(state) = &self.0 {
            let id = identity(frame);
            self.event("acquisition_input_parsed",json!({"requestId":id,"inputFrameSha256":digest(&frame.wire),"opcode":frame.opcode}));
            if id == state.successor {
                if state.mutation == Mutation::ReplaceFrame {
                    *state.successor_frame.lock().unwrap() = Some(copy_frame(frame));
                }
                state.successor_parsed.store(true, Ordering::SeqCst);
                state.changed.notify_one();
            }
        }
    }

    // Called after actual dispatch/NativeFiles, before the blocking closure returns.
    pub(super) fn route_returned(&self, frame: &frame::Frame, route: &Route) {
        let Some(state) = &self.0 else {
            return;
        };
        self.event("acquisition_route_ready", facts(frame, route));
        if state.mutation == Mutation::EarlyR2 && identity(frame) == state.target {
            self.event("mutant_hit", facts(frame, route));
            self.event("result_acquired", facts(frame, route));
        }
        if state.mode == Mode::HoldRouteReturn && identity(frame) == state.target {
            self.event("route_return_held_unacquired", facts(frame, route));
            state.route_held.store(true, Ordering::SeqCst);
            state.changed.notify_one();
            let (mutex, gate) = &state.return_gate;
            let guard = mutex.lock().unwrap();
            let (_guard, result) = gate
                .wait_timeout_while(guard, Duration::from_secs(8), |released| !*released)
                .unwrap();
            assert!(!result.timed_out(), "owned route-return gate watchdog");
        }
    }

    pub(super) fn bypass_enabled(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|s| s.mutation == Mutation::BypassJoin)
    }

    // The only producer is the real completed dispatch closure. Keep the original
    // return value owned by JoinSet for normal task reclamation when the pair ends.
    pub(super) fn closure_result(&self, frame: &frame::Frame, route: &Route) {
        if self.bypass_enabled() {
            let state = self.0.as_ref().unwrap();
            assert_eq!(identity(frame), state.target, "single target bypass only");
            let previous = state
                .bypass
                .lock()
                .unwrap()
                .replace(copy_result(frame, route));
            assert!(previous.is_none(), "bounded single-result bypass slot");
            state.bypass_changed.notify_one();
        }
    }

    pub(super) async fn bypassed(&self) -> (frame::Frame, Route) {
        let state = self.0.as_ref().expect("configured bypass");
        loop {
            let changed = state.bypass_changed.notified();
            if let Some(result) = state.bypass.lock().unwrap().take() {
                self.event("mutant_hit", facts(&result.0, &result.1));
                return result;
            }
            changed.await;
        }
    }

    // Normally called after actual join_next. M1 deliberately calls it from its
    // real-result bypass; the unchanged validator must reject that fake R2.

    pub(super) fn taken(&self, frame: frame::Frame, route: Route) -> Option<(frame::Frame, Route)> {
        let Some(state) = &self.0 else {
            return Some((frame, route));
        };
        if state.mutation != Mutation::EarlyR2 || identity(&frame) != state.target {
            self.event("result_acquired", facts(&frame, &route));
        }
        if identity(&frame) == state.target {
            state.acquired.store(true, Ordering::SeqCst);
            if state.mode == Mode::HoldAcquired {
                self.event("acquired_and_held", facts(&frame, &route));
                let previous = state.held.lock().unwrap().replace((frame, route));
                assert!(previous.is_none(), "single acquired-result cache");
                state.changed.notify_one();
                return None;
            }
            state.changed.notify_one();
        }
        Some((frame, route))
    }

    // Deliberate A2 intervention: same result moves out once, only after actual parse.
    pub(super) fn released(&self) -> Option<(frame::Frame, Route)> {
        let state = self.0.as_ref()?;
        if state.mode != Mode::HoldAcquired || !state.successor_parsed.load(Ordering::SeqCst) {
            return None;
        }
        let mut result = state.held.lock().unwrap().take()?;
        let release = state.release_count.fetch_add(1, Ordering::SeqCst) + 1;
        if state.mutation == Mutation::DoubleRelease {
            assert!(release <= 2, "bounded double-release mutant");
            if release == 1 {
                *state.held.lock().unwrap() = Some(copy_result(&result.0, &result.1));
            }
            self.event(
                "mutant_hit",
                json!({"release":release,"result":facts(&result.0,&result.1)}),
            );
        }
        if state.mutation == Mutation::ReplaceFrame {
            let replacement = state
                .successor_frame
                .lock()
                .unwrap()
                .take()
                .expect("actual parsed successor frame");
            self.event("mutant_hit", json!({"original":facts(&result.0,&result.1),"replacement":facts(&replacement,&result.1)}));
            result.0 = replacement;
        }
        self.event("acquired_cache_released", facts(&result.0, &result.1));
        Some(result)
    }

    pub(super) fn release_emit_completed(&self) {
        if let Some(state) = &self.0
            && matches!(
                state.mutation,
                Mutation::DoubleRelease | Mutation::ReplaceFrame
            )
        {
            self.event(
                "mutant_emit_completed",
                json!({"release":state.release_count.load(Ordering::SeqCst)}),
            );
        }
    }

    pub(super) async fn wait_admission_barrier(&self) {
        let state = self.0.as_ref().expect("configured acquisition tap");
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let ready = match state.mode {
                    Mode::HoldRouteReturn => state.route_held.load(Ordering::SeqCst),
                    Mode::HoldAcquired => state.acquired.load(Ordering::SeqCst),
                    _ => panic!("no admission intervention in natural mode"),
                };
                if ready {
                    break;
                }
                state.changed.notified().await;
            }
        })
        .await
        .expect("acquisition barrier watchdog");
    }

    pub(super) fn release_return(&self) {
        if let Some(state) = &self.0 {
            let (mutex, gate) = &state.return_gate;
            *mutex
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            gate.notify_all();
        }
    }
}

// Scope guard owns test-only cleanup even if a barrier/read/validator panics.
pub(super) struct OwnedRunCleanup {
    tap: Tap,
    task: Option<tokio::task::AbortHandle>,
}
impl OwnedRunCleanup {
    pub(super) fn new(tap: Tap) -> Self {
        Self { tap, task: None }
    }
    pub(super) fn track(&mut self, task: tokio::task::AbortHandle) {
        self.task = Some(task);
    }
}
impl Drop for OwnedRunCleanup {
    fn drop(&mut self) {
        self.tap.release_return();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

// Preserve partial raw events on a test panic without replacing earlier evidence.
pub(super) struct FailureTrace {
    observer: Observer,
    path: std::path::PathBuf,
}
impl FailureTrace {
    pub(super) fn new(observer: Observer, path: std::path::PathBuf) -> Self {
        Self { observer, path }
    }
}
impl Drop for FailureTrace {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Ok(file) = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.path)
        {
            let _ = serde_json::to_writer_pretty(
                file,
                &json!({"status":"FAILED_PARTIAL_TRACE","events":self.observer.snapshot(),"taskConsumable":"NOT_INFERRED","compatibility":"NOT_ACCEPTED"}),
            );
        }
    }
}

// Causal assertions are independent of final response-order equality. They will be
// exercised against actual implementation mutants in the later execution window.
pub(super) fn validate(events: &[Value], mode: Mode, connection: &str) -> Value {
    if mode == Mode::Off {
        return json!({"mode":"Off"});
    }
    // Compare the two real byte streams after collection. poll_write may wake a
    // client on another worker before its W0 log acquires the observer mutex;
    // eventIndex alone cannot order those cross-thread marker publications.
    let mut accepted = String::new();
    let mut received = String::new();
    for event in events {
        if event["event"] == "transport_write" && event["stream"] == "downstream" {
            accepted.push_str(event["wireHex"].as_str().expect("accepted raw bytes"));
        } else if event["event"] == "client_frame_read" {
            assert_eq!(event["observation"], "CLIENT_DECODED_COMPLETE");
            received.push_str(event["wireHex"].as_str().expect("client full raw frame"));
        }
    }
    assert_eq!(
        accepted, received,
        "actual W0 bytes differ from complete C0 stream"
    );
    let native_files_observed = events
        .iter()
        .any(|e| e["event"] == "native_files_api_return");
    if mode == Mode::Natural && connection != "list-close" {
        assert!(
            native_files_observed,
            "A0 must execute actual NativeFiles, not cached payload"
        );
    }
    let target = format!("{connection}-0");
    let successor = format!("{connection}-1");
    let find = |name: &str, id: &str| {
        events
            .iter()
            .position(|e| e["event"] == name && e["requestId"] == id)
    };
    let acquired = find("result_acquired", &target);
    let parsed = find("acquisition_input_parsed", &successor);
    let natural_id = if connection == "supported-list" {
        format!("{connection}-1")
    } else {
        target.clone()
    };
    let natural_acquired = find("result_acquired", &natural_id);
    let first_parse = find("acquisition_input_parsed", &natural_id);
    let next_parse = first_parse.and_then(|first| {
        events
            .iter()
            .enumerate()
            .skip(first + 1)
            .find(|(_, e)| e["event"] == "acquisition_input_parsed")
            .map(|(i, _)| i)
    });
    let natural =
        mode == Mode::Natural && natural_acquired.zip(next_parse).is_some_and(|(a, b)| a < b);
    if matches!(mode, Mode::HoldRouteReturn | Mode::HoldAcquired) {
        assert!(
            events
                .iter()
                .any(|e| e["event"] == "native_files_api_return"),
            "real NativeFiles API evidence missing"
        );
        let ready = find("acquisition_route_ready", &target).expect("real route missing");
        let joined =
            find("join_consumed", &target).expect("actual JoinSet consumption marker missing");
        let acquired = acquired.expect("result_acquired missing");
        assert!(
            ready < joined && joined < acquired,
            "R2 must follow actual join, never R1/COMPLETE"
        );
        assert_eq!(
            events[ready]["payloadSha256"], events[acquired]["payloadSha256"],
            "same acquired payload"
        );
        assert_eq!(
            events[ready]["inputFrameSha256"], events[acquired]["inputFrameSha256"],
            "same acquired request frame"
        );
        let parsed = parsed.expect("successor was not actually parsed");
        let client: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == "client_frame_read" && e["requestId"].is_string())
            .collect();
        assert_eq!(client.len(), 2, "exact two client-decoded responses");
        let acquired_count = events
            .iter()
            .filter(|e| e["event"] == "result_acquired" && e["requestId"] == target)
            .count();
        assert_eq!(acquired_count, 1, "single real result acquisition");
        match mode {
            Mode::HoldRouteReturn => {
                assert!(
                    parsed < acquired,
                    "A1 successor must be parsed before actual take"
                );
                assert_eq!(client[0]["requestId"], successor);
                assert_eq!(client[1]["requestId"], target);
            }
            Mode::HoldAcquired => {
                let held = find("acquired_and_held", &target).expect("actual cache missing");
                let released =
                    find("acquired_cache_released", &target).expect("cache was not consumed");
                assert!(
                    acquired < held && held < parsed && parsed < released,
                    "A2 take->hold->parse->release"
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e["event"] == "acquired_cache_released")
                        .count(),
                    1,
                    "single release"
                );
                assert_eq!(
                    events[held]["inputFrameSha256"], events[released]["inputFrameSha256"],
                    "cached request changed"
                );
                assert_eq!(
                    events[held]["payloadSha256"], events[released]["payloadSha256"],
                    "cached payload changed"
                );
                assert_eq!(client[0]["requestId"], target);
                assert_eq!(client[1]["requestId"], successor);
            }
            _ => unreachable!(),
        }
        assert!(
            events.iter().any(|e| e["event"] == "transport_write"
                && e["stream"] == "downstream"
                && e["bytesAccepted"].as_u64().is_some_and(|n| n > 0)),
            "W0 accepted bytes missing"
        );
    }
    json!({"mode":format!("{:?}",mode),"status":"RECORDED_NOT_ACCEPTED","naturalWindow":if natural {"OBSERVED_ONCE_IN_INSTRUMENTED_PRODUCTION_PATH"}else{"NOT_OBSERVED"},"intervention":match mode{Mode::HoldRouteReturn=>"ROUTE_RETURN_HELD_UNACQUIRED",Mode::HoldAcquired=>"ACQUIRED_AND_HELD",_=>"NONE"},"actualNativeFilesObserved":native_files_observed,"unconsumedJoinSetNaturalWindow":"NOT_OBSERVED","p3":"UNMAPPED_COMPLETION_DOMAIN","compatibility":"NOT_EVALUATED_BY_THIS_CAUSAL_VALIDATOR"})
}

// Do not run with the preserved 379 binary: each entry requires new exact Cargo identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PREPARED_NOT_RUN; root-assigned compile/execution budget and exact source/binary required"]
async fn a0_natural_result_acquisition() {
    super::order_probe::record_native_cells(Mode::Natural).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PREPARED_NOT_RUN; real NativeFiles route-return gate intervention"]
async fn a1_route_return_held_unacquired() {
    super::order_probe::record_native_cells(Mode::HoldRouteReturn).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PREPARED_NOT_RUN; actual JoinSet take plus single-result cache intervention"]
async fn a2_acquired_then_successor_parsed() {
    super::order_probe::record_native_cells(Mode::HoldAcquired).await;
}

#[test]
fn owned_cleanup_releases_gate_on_normal_and_panic_paths() {
    for panic_path in [false, true] {
        let tap = Tap::new(
            Mode::HoldRouteReturn,
            Observer::new(),
            "cleanup-control".into(),
            "owned",
        );
        let state = tap.0.as_ref().unwrap().clone();
        let waiter_state = state.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let (mutex, gate) = &waiter_state.return_gate;
            let guard = mutex.lock().unwrap();
            ready_tx.send(()).unwrap();
            let (guard, timed) = gate
                .wait_timeout_while(guard, Duration::from_secs(1), |released| !*released)
                .unwrap();
            assert!(
                !timed.timed_out() && *guard,
                "scope cleanup did not release real waiter"
            );
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _cleanup = OwnedRunCleanup::new(tap.clone());
            assert!(!panic_path, "intentional owned cleanup control");
        }));
        assert_eq!(outcome.is_err(), panic_path);
        waiter.join().unwrap();
    }
}

// One exact artifact, one explicitly selected child per mutant. An observed
// assertion failure is evidence for the external runner, never a passing test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PREPARED_NOT_RUN; explicit M1-M4, fresh output and root execution budget required"]
async fn m1_m4_actual_path_mutant() {
    let mutation = match std::env::var("PDRS_ACQUISITION_MUTANT").as_deref() {
        Ok("M1") => Mutation::BypassJoin,
        Ok("M2") => Mutation::EarlyR2,
        Ok("M3") => Mutation::DoubleRelease,
        Ok("M4") => Mutation::ReplaceFrame,
        _ => panic!("explicit PDRS_ACQUISITION_MUTANT=M1|M2|M3|M4 required"),
    };
    let mode = if mutation == Mutation::BypassJoin {
        Mode::HoldRouteReturn
    } else {
        Mode::HoldAcquired
    };
    let output = std::path::PathBuf::from(std::env::var("PDRS_ORDER_OUTPUT").unwrap());
    assert!(!output.exists(), "fresh mutant evidence directory required");
    let outcome = tokio::spawn(super::order_probe::record_native_cells_with_mutation(
        mode, mutation,
    ))
    .await;
    let (status, reason) = match &outcome {
        Ok(()) => (
            "MUTANT_ACCEPTED_REQUIRES_P1_INVESTIGATION",
            "validator accepted actual mutant".to_string(),
        ),
        Err(error) => ("FAILED_REQUIRES_RAW_CLASSIFICATION", error.to_string()),
    };
    // create_new forbids replacement even after setup/source identity failures.
    if output.is_dir()
        && let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join("mutant-outcome.json"))
    {
        serde_json::to_writer_pretty(file, &json!({"status":status,"reason":reason,"mutation":format!("{mutation:?}"),"negativeControlAccepted":false})).unwrap();
    }
    match outcome {
        Ok(()) => panic!("actual mutant accepted; preserve raw and investigate as P1"),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("mutant task infrastructure failure: {error}"),
    }
}

// Keep real Tap bypass-result notifications separate from admission-barrier progress.
#[cfg(test)]
mod notify_controls {
    use super::*;
    use std::{
        future::Future,
        task::{Context, Poll, Wake, Waker},
    };

    #[derive(Default)]
    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl CountWake {
        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn fixture(connection: &str) -> (Tap, frame::Frame, Route) {
        let id = format!("{connection}-0");
        let payload = serde_json::to_vec(&json!({
            "type":"request", "id":&id, "method":"project_files_list"
        }))
        .unwrap();
        let wire = frame::client_frame(0x81, 126, &payload).into();
        let route = Route::LocalResponse(
            serde_json::to_vec(&json!({"type":"response", "id":id, "result":{"control":true}}))
                .unwrap(),
        );
        let tap = Tap::with_mutation(
            Mode::HoldRouteReturn,
            Mutation::BypassJoin,
            Observer::new(),
            "actual-tap-notify-control".into(),
            connection,
        );
        (
            tap,
            frame::Frame {
                opcode: 1,
                wire,
                payload,
            },
            route,
        )
    }

    fn assert_result(actual: Poll<(frame::Frame, Route)>, expected: &(frame::Frame, Route)) {
        let Poll::Ready((frame, route)) = actual else {
            panic!("published real Tap bypass slot must be ready");
        };
        assert_eq!(frame.wire, expected.0.wire);
        assert_eq!(frame.payload, expected.0.payload);
        match (route, &expected.1) {
            (Route::LocalResponse(actual), Route::LocalResponse(expected)) => {
                assert_eq!(&actual, expected);
            }
            _ => panic!("actual Tap route changed"),
        }
    }

    #[tokio::test]
    async fn actual_tap_notify_before_await_preserves_bypass_and_barrier() {
        let (tap, frame, route) = fixture("notify-before");
        let expected = copy_result(&frame, &route);
        // The actual cleanup releases the real gate before synchronous producer
        // calls. Both notifications precede creation of either consumer future.
        drop(OwnedRunCleanup::new(tap.clone()));
        tap.route_returned(&frame, &route);
        tap.closure_result(&frame, &route);

        let wake = Waker::from(Arc::new(CountWake::default()));
        let mut context = Context::from_waker(&wake);
        let mut barrier = Box::pin(tap.wait_admission_barrier());
        assert!(barrier.as_mut().poll(&mut context).is_ready());
        let mut bypass = Box::pin(tap.bypassed());
        assert_result(bypass.as_mut().poll(&mut context), &expected);
        let mut empty_bypass = Box::pin(tap.bypassed());
        assert!(empty_bypass.as_mut().poll(&mut context).is_pending());
    }

    #[tokio::test]
    async fn actual_tap_pending_bypass_cannot_steal_admission_barrier_wake() {
        let (tap, frame, route) = fixture("notify-pending");
        let expected = copy_result(&frame, &route);
        drop(OwnedRunCleanup::new(tap.clone()));
        let bypass_count = Arc::new(CountWake::default());
        let barrier_count = Arc::new(CountWake::default());
        let bypass_wake = Waker::from(bypass_count.clone());
        let barrier_wake = Waker::from(barrier_count.clone());
        let mut bypass_context = Context::from_waker(&bypass_wake);
        let mut barrier_context = Context::from_waker(&barrier_wake);

        // Polling to Pending registers the actual Notify waiters in this exact
        // order. No sleep/yield is used to guess which future got registered.
        let mut bypass = Box::pin(tap.bypassed());
        assert!(bypass.as_mut().poll(&mut bypass_context).is_pending());
        let mut barrier = Box::pin(tap.wait_admission_barrier());
        assert!(barrier.as_mut().poll(&mut barrier_context).is_pending());
        // notify_one calls Wake synchronously: count ownership immediately,
        // without scheduler deadlines or a producer thread.
        tap.route_returned(&frame, &route);
        assert_eq!(
            barrier_count.count(),
            1,
            "R80_SHARED_NOTIFY_ROUTE_WAKE_STOLEN"
        );
        assert_eq!(
            bypass_count.count(),
            0,
            "route-held wake belongs to the barrier"
        );
        assert!(bypass.as_mut().poll(&mut bypass_context).is_pending());
        assert!(barrier.as_mut().poll(&mut barrier_context).is_ready());
        tap.closure_result(&frame, &route);
        assert_eq!(
            bypass_count.count(),
            1,
            "real bypass publication must wake its consumer"
        );
        assert_result(bypass.as_mut().poll(&mut bypass_context), &expected);
        let mut empty_bypass = Box::pin(tap.bypassed());
        assert!(empty_bypass.as_mut().poll(&mut bypass_context).is_pending());
    }
}
