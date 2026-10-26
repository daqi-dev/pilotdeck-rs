use super::*;
use crate::runtime::connection::binding::WsBinding;
use crate::runtime::connection::{
    Call, ConnectionGateway, GatewayConnection, GatewayTextConnection, RequestError,
};
use pilotdeck_protocol::{parse_json, stringify_json};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ORACLE: &str = include_str!("../../../tests/fixtures/turn-publish/oracle.json");
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn json(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
fn run_ref() -> RunRef {
    RunRef {
        session_key: "s".into(),
        run_id: "r".into(),
        project_key: Some("p".into()),
    }
}
const MATERIAL: &str = "{\"message\":\"hello\",\"projectKey\":\"p\",\"sessionKey\":\"s\"}";
fn input() -> RunAcceptInput {
    RunAcceptInput {
        run: run_ref(),
        request_material: MATERIAL.into(),
    }
}
struct Request {
    gate: Option<oneshot::Sender<bool>>,
    released: bool,
    done: Arc<AtomicBool>,
}
struct ControlledRegistry {
    file: Arc<FileRunRegistry>,
    calls: Mutex<Vec<Value>>,
    durable: Arc<Mutex<Vec<Value>>>,
    requests: Mutex<Vec<Request>>,
    setup: Value,
    root: std::path::PathBuf,
}
impl ControlledRegistry {
    fn new(setup: &Value) -> Arc<Self> {
        let root = std::env::temp_dir().join(format!("pdrs-turn-publish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let file = Arc::new(FileRunRegistry::open(root.join("registry.json")).unwrap());
        if let Some(seed) = setup.get("seed") {
            let mut input = input();
            if setup["conflict"] == true {
                input.request_material = "different".into();
            }
            file.accept(&input).unwrap();
            for event in seed.as_array().unwrap() {
                file.append(&run_ref(), &js(event)).unwrap();
            }
        }
        Arc::new(Self {
            file,
            calls: Mutex::new(vec![]),
            durable: Arc::default(),
            requests: Mutex::new(vec![]),
            setup: setup.clone(),
            root,
        })
    }
    async fn release(&self, index: usize, fail: bool) {
        {
            let mut requests = self.requests.lock().unwrap();
            requests[index].released = true;
            requests[index].gate.take().unwrap().send(fail).unwrap();
        }
        // Only the released prefix can finish, even if a later gate opened.
        let done: Vec<_> = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .take_while(|r| r.released)
            .map(|r| r.done.clone())
            .collect();
        for done in done {
            while !done.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        }
    }
}
impl Drop for ControlledRegistry {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
impl TurnRunRegistry for ControlledRegistry {
    fn accept(&self, input: &RunAcceptInput) -> RegistryFuture<bool> {
        self.calls.lock().unwrap().push(json!({"op":"accept", "requestMaterial":String::from_utf16_lossy(input.request_material.units())}));
        let result = if self.setup["acceptFailure"] == true {
            Err(RunRegistryError::Invalid("injected accept failure".into()))
        } else if self.setup["acceptStorageFailure"] == true {
            {
                crate::storage::fail_next_registry_persist(&self.file);
                self.file
                    .accept(input)
                    .map(|r| r.duplicate)
                    .map_err(|_| RunRegistryError::Invalid("injected storage failure".into()))
            }
        } else {
            self.file.accept(input).map(|r| r.duplicate)
        };
        Box::pin(async move { result })
    }
    fn events(&self, run: &RunRef) -> RegistryFuture<Vec<JsValue>> {
        self.calls.lock().unwrap().push(json!({"op":"events"}));
        let result = self
            .file
            .events(run, None, None)
            .map(|v| v.iter().map(|v| field(v, "event").clone()).collect());
        Box::pin(async move { result })
    }
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()> {
        let mut requests = self.requests.lock().unwrap();
        let previous = requests.last().map(|r| r.done.clone());
        let index = requests.len();
        self.calls
            .lock()
            .unwrap()
            .push(json!({"op":"append", "index":index, "event":json(event)}));
        let (tx, rx) = oneshot::channel();
        let done = Arc::new(AtomicBool::new(false));
        requests.push(Request {
            gate: Some(tx),
            released: false,
            done: done.clone(),
        });
        let file = self.file.clone();
        let run = run.clone();
        let event = event.clone();
        let durable = self.durable.clone();
        let storage_failure = self.setup["storageFailure"] == true;
        Box::pin(async move {
            if let Some(previous) = previous {
                while !previous.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            }
            let fail = rx.await.unwrap();
            let result = if fail && storage_failure {
                crate::storage::fail_next_registry_persist(&file);
                file.append(&run, &event)
                    .map_err(|_| RunRegistryError::Invalid("injected storage failure".into()))
            } else if fail {
                Err(RunRegistryError::Invalid("injected append failure".into()))
            } else {
                file.append(&run, &event)
            };
            if result.is_ok() {
                durable.lock().unwrap().push(json(&event));
            }
            done.store(true, Ordering::SeqCst);
            result
        })
    }
}
#[derive(Default)]
struct Text(Mutex<Vec<Value>>);
impl GatewayTextConnection for Text {
    fn send_text(&self, text: String) {
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["type"] == "event" {
            self.0.lock().unwrap().push(frame);
        }
    }
    fn close(&self, _: u16, reason: &str) {
        panic!("unexpected close: {reason}");
    }
}
struct TestGateway {
    port: Option<Arc<dyn TurnRunRegistry>>,
    coordinator: Arc<TurnEventCoordinator>,
    publisher: Arc<Mutex<Option<Arc<TurnPublisher>>>>,
    visible: Arc<Mutex<Vec<Value>>>,
    preflight: bool,
}
impl ConnectionGateway for TestGateway {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Ok(js(&json!({"name":"fixture","capabilities":[]}))) })
    }
    fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
        unreachable!()
    }
    fn disconnect(&self, _: JsValue, _: WsBinding) -> Call<'_, Result<bool, RequestError>> {
        unreachable!()
    }
    fn abort_on_close(&self, _: JsValue) -> Call<'_, Result<(), RequestError>> {
        unreachable!()
    }
    fn submit<'a>(
        &'a self,
        _: JsValue,
        emit: &'a (dyn Fn(JsValue) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>> {
        Box::pin(async move {
            let deliver = |event: JsValue| {
                self.visible.lock().unwrap().push(json(&event));
                emit(event);
            };
            match accept_turn(self.port.as_deref(), &input()).await {
                TurnAcceptance::Accepted if self.preflight => {
                    let event = js(
                        &json!({"type":"error","runId":"r","code":"replace_turn_pending",
                        "message":"This session is waiting for its edited replacement turn to be accepted.",
                        "recoverable":true,"userHint":"Wait for the edited message transaction to finish, then try again."}),
                    );
                    deliver(persist_preflight_event(self.port.as_deref(), &run_ref(), event).await);
                }
                TurnAcceptance::Accepted => {
                    let stream = Arc::new(TurnPublisher::start(
                        run_ref(),
                        self.port.clone(),
                        self.coordinator.clone(),
                    ));
                    *self.publisher.lock().unwrap() = Some(stream.clone());
                    while let Some(event) = stream.next().await {
                        deliver(event);
                    }
                }
                TurnAcceptance::Replay(events) => {
                    for event in events {
                        deliver(event);
                    }
                }
                TurnAcceptance::Rejected(event) => deliver(event),
            }
            Ok(())
        })
    }
}
async fn settle() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
}
async fn replay_case(case: &Value) -> Result<(), String> {
    let registry = ControlledRegistry::new(&case["setup"]);
    let no_registry = case["setup"]["noRegistry"] == true;
    let port = (!no_registry).then(|| registry.clone() as Arc<dyn TurnRunRegistry>);
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let publisher = Arc::new(Mutex::new(None::<Arc<TurnPublisher>>));
    let visible = Arc::new(Mutex::new(Vec::<Value>::new()));
    let ended = Arc::new(AtomicBool::new(false));
    let text = Arc::new(Text::default());
    let gateway = Arc::new(TestGateway {
        port: port.clone(),
        coordinator: coordinator.clone(),
        publisher: publisher.clone(),
        visible: visible.clone(),
        preflight: case["setup"]["preflight"] == true,
    });
    let connection = Arc::new(GatewayConnection::new(
        text.clone(),
        gateway,
        "fixture-token".into(),
        "test".into(),
    ));
    connection.receive(r#"{"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"test","token":"fixture-token"}"#).await.unwrap();
    let consumer = {
        let ended = ended.clone();
        tokio::spawn(async move {
            connection.receive(r#"{"type":"request","id":"request-9","method":"submit_turn","params":{"sessionKey":"s","runId":"r","projectKey":"p","message":"hello"}}"#).await.unwrap();
            ended.store(true, Ordering::SeqCst);
        })
    };
    let projector = GatewayAgentEventProjector::default();
    let mut finisher = None;
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        let stream = publisher.lock().unwrap().clone();
        match step["op"].as_str().unwrap() {
            "observe" => {}
            "agent" => stream
                .unwrap()
                .agent_event(&projector, &js(&step["event"]), false)
                .unwrap(),
            "emit" => {
                stream.unwrap().emit(&js(&step["event"]));
            }
            "end" => {
                finisher = Some(tokio::spawn(async move { stream.unwrap().finish().await }));
            }
            "release" => {
                registry
                    .release(
                        step["index"].as_u64().unwrap() as usize,
                        step["fail"] == true,
                    )
                    .await
            }
            op => panic!("unknown op {op}"),
        }
        settle().await;
        let record = registry.file.get(&run_ref()).unwrap().map(|r| {
            let mut value = json!({"state":r.state().unwrap().as_str(),"lastSeq":json(field(r.as_json(),"lastSeq")),"revision":json(field(r.as_json(),"revision"))});
            if let Some(result) = r.result() { value["result"] = json(result); }
            value
        });
        let snapshot = coordinator.snapshot(&super::super::replay::ActiveTurnSnapshotInput {
            session_key: "s".into(),
            include_events: true,
        });
        let actual = json!({"wsFrames":text.0.lock().unwrap().clone(), "visible":visible.lock().unwrap().clone(), "calls":registry.calls.lock().unwrap().clone(),
            "durable":registry.durable.lock().unwrap().clone(), "ended":ended.load(Ordering::SeqCst), "record":record,
            "registryEvents":registry.file.events(&run_ref(),None,None).unwrap().iter().map(json).collect::<Vec<_>>(),
            "replay":json(&snapshot.to_json()) });
        if actual != case["results"][index] {
            consumer.abort();
            if let Some(finisher) = finisher {
                finisher.abort();
            }
            return Err(format!(
                "{} step {} {}\nactual: {}\nexpected: {}",
                case["name"], index, step, actual, case["results"][index]
            ));
        }
    }
    consumer.await.unwrap();
    if let Some(finisher) = finisher {
        finisher.await.unwrap();
    }
    Ok(())
}
async fn replay_named(name: &str) {
    let oracle = json(&crate::test_baseline::oracle(ORACLE));
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 14);
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), replay_case(case))
        .await
        .expect("publication deadlocked")
        .unwrap();
}
macro_rules! scenario {
    ($test:ident, $name:literal) => {
        #[tokio::test]
        async fn $test() {
            replay_named($name).await;
        }
    };
}
scenario!(
    ordered_durable_terminal_after_session_end,
    "ordered-durable-terminal-after-session-end"
);
scenario!(
    append_failure_then_success_single_unknown,
    "append-failure-then-success-single-unknown"
);
scenario!(real_storage_persist_failure, "real-storage-persist-failure");
scenario!(
    accept_storage_failure_result_unknown,
    "accept-storage-failure-result-unknown"
);
scenario!(
    all_appends_fail_only_one_unknown,
    "all-appends-fail-only-one-unknown"
);
scenario!(
    close_waits_for_pending_append,
    "close-waits-for-pending-append"
);
scenario!(
    late_event_while_terminal_append_pending,
    "late-event-while-terminal-append-pending"
);
scenario!(
    duplicate_terminal_is_registry_failure,
    "duplicate-terminal-is-registry-failure-not-second-success"
);
scenario!(without_registry, "without-registry");
scenario!(
    duplicate_replays_durable_events,
    "duplicate-replays-durable-events"
);
scenario!(conflict_does_not_start_pump, "conflict-does-not-start-pump");
scenario!(
    accept_failure_result_unknown,
    "accept-failure-result-unknown"
);
scenario!(preflight_registry_failure, "preflight-registry-failure");
scenario!(
    preflight_durable_before_visible,
    "preflight-durable-before-visible"
);
#[test]
#[ignore = "requires fixed TS baseline and its existing tsx installation"]
fn real_ts_turn_publish_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-publish");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_registry_publication_preserves_fifo_across_workers() {
    let root = std::env::temp_dir().join(format!("pdrs-turn-worker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let registry = Arc::new(
        FileRegistry::new(FileRunRegistry::open(root.join("registry.json")).unwrap()).unwrap(),
    );
    assert_eq!(
        accept_turn(Some(registry.as_ref()), &input()).await,
        TurnAcceptance::Accepted
    );
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let stream = Arc::new(TurnPublisher::start(
        run_ref(),
        Some(registry.clone()),
        coordinator.clone(),
    ));
    let expected: Vec<_> = (0..64)
        .map(|i| js(&json!({"type":"agent_status","runId":"r","event":"tick","moduleSeq":900+i})))
        .chain(std::iter::once(js(
            &json!({"type":"turn_completed","runId":"r","usage":{},"finishReason":"completed"}),
        )))
        .collect();
    for event in &expected {
        assert!(stream.gateway_event(event.clone()));
    }
    let finisher = {
        let stream = stream.clone();
        tokio::spawn(async move { stream.finish().await })
    };
    tokio::time::timeout(Duration::from_secs(10), finisher)
        .await
        .expect("file registry finish deadlocked")
        .unwrap();
    let received = tokio::time::timeout(Duration::from_secs(10), async {
        let mut received = Vec::new();
        while let Some(event) = stream.next().await {
            received.push(event);
        }
        received
    })
    .await
    .expect("file registry publication deadlocked");
    assert_eq!(received, expected);
    assert_eq!(registry.events(&run_ref()).await.unwrap(), expected);
    assert!(!stream.emit(&js(&json!({"type":"late"}))));
    drop(stream);
    drop(registry);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_host_entries_keep_replay_durable_and_live_in_one_order() {
    let root = std::env::temp_dir().join(format!("pdrs-turn-concurrent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let registry = Arc::new(
        FileRegistry::new(FileRunRegistry::open(root.join("registry.json")).unwrap()).unwrap(),
    );
    assert_eq!(
        accept_turn(Some(registry.as_ref()), &input()).await,
        TurnAcceptance::Accepted
    );
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let stream = Arc::new(TurnPublisher::start(
        run_ref(),
        Some(registry.clone()),
        coordinator.clone(),
    ));
    let pump = {
        let stream = stream.clone();
        std::thread::spawn(move || {
            for index in 0..32 {
                assert!(stream.gateway_event(js(
                    &json!({"type":"agent_status","runId":"r","event":"pump","index":index})
                )));
            }
        })
    };
    let external = {
        let stream = stream.clone();
        std::thread::spawn(move || {
            for index in 0..32 {
                assert!(stream.emit(&js(
                    &json!({"type":"agent_status","event":"external","index":index})
                )));
            }
        })
    };
    pump.join().unwrap();
    external.join().unwrap();
    let mut expected = coordinator
        .snapshot(&super::super::replay::ActiveTurnSnapshotInput::new(
            "s".into(),
        ))
        .events;
    assert_eq!(expected.len(), 64);
    let terminal =
        js(&json!({"type":"turn_completed","runId":"r","usage":{},"finishReason":"completed"}));
    stream.gateway_event(terminal.clone());
    expected.push(terminal);
    tokio::time::timeout(Duration::from_secs(10), stream.finish())
        .await
        .expect("concurrent finish deadlocked");
    let received = tokio::time::timeout(Duration::from_secs(10), async {
        let mut received = Vec::new();
        while let Some(event) = stream.next().await {
            received.push(event);
        }
        received
    })
    .await
    .expect("concurrent stream deadlocked");
    assert_eq!(received, expected);
    assert_eq!(registry.events(&run_ref()).await.unwrap(), expected);
    drop(stream);
    drop(registry);
    std::fs::remove_dir_all(root).unwrap();
}

async fn panic_deadline<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(2), future)
        .await
        .expect("spawn completion deadline")
}

struct PanicRegistry(Mutex<VecDeque<RegistryFuture<()>>>);
impl TurnRunRegistry for PanicRegistry {
    fn accept(&self, _: &RunAcceptInput) -> RegistryFuture<bool> {
        Box::pin(async { Ok(false) })
    }
    fn events(&self, _: &RunRef) -> RegistryFuture<Vec<JsValue>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn append(&self, _: &RunRef, _: &JsValue) -> RegistryFuture<()> {
        self.0.lock().unwrap().pop_front().unwrap()
    }
}

struct WakeFlag(AtomicBool);
impl std::task::Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn durable_spawn_panic_matrix() {
    let oracle = include_str!("../../../tests/fixtures/runtime-spawn-panic/oracle.json");
    crate::test_baseline::oracle(oracle);
    let oracle: Value = serde_json::from_str(oracle).unwrap();
    let mut ids = HashSet::new();
    for case in oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"].as_str().unwrap().starts_with("durable-"))
    {
        let id = case["id"].as_str().unwrap();
        assert!(ids.insert(id));
        let (release, gate) = oneshot::channel();
        let (started, entered) = oneshot::channel();
        let after = id.contains("-P1-");
        let first: RegistryFuture<()> = Box::pin(async move {
            started.send(()).unwrap();
            if after {
                gate.await.unwrap();
            }
            panic!("injected panic");
        });
        let second: RegistryFuture<()> = Box::pin(async { panic!("injected panic") });
        let registry = Arc::new(PanicRegistry(Mutex::new(VecDeque::from([first, second]))));
        let publisher = TurnPublisher::start(run_ref(), Some(registry), Arc::default());
        publisher.gateway_event(js(&json!({"type":"delta"})));
        publisher.gateway_event(js(&json!({"type":"delta"})));
        let writes = publisher.publication.state.lock().unwrap().pending.clone();
        let n = case["outcomes"].as_array().unwrap().len();
        let mut waiters: Vec<_> = (0..n)
            .map(|_| Box::pin(publisher.publication.wait_for(&writes)))
            .collect();
        let mut next = Box::pin(publisher.next());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        for waiter in &mut waiters {
            assert!(waiter.as_mut().poll(&mut cx).is_pending());
        }
        assert!(next.as_mut().poll(&mut cx).is_pending());
        if id.ends_with("-C") {
            drop(waiters);
            drop(next);
            waiters = (0..n)
                .map(|_| Box::pin(publisher.publication.wait_for(&writes)))
                .collect();
            next = Box::pin(publisher.next());
        }
        let flags: Vec<_> = (0..n)
            .map(|_| Arc::new(WakeFlag(AtomicBool::new(false))))
            .collect();
        for (waiter, flag) in waiters.iter_mut().zip(&flags) {
            let waker = std::task::Waker::from(flag.clone());
            assert!(
                waiter
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(&waker))
                    .is_pending()
            );
        }
        panic_deadline(entered).await.unwrap();
        if after {
            assert!(
                next.as_mut().poll(&mut cx).is_pending(),
                "{id}: gate remains pending"
            );
            release.send(()).unwrap();
        }
        panic_deadline(publisher.publication.wait_for(&writes)).await;
        assert!(
            flags.iter().all(|f| f.0.load(Ordering::SeqCst)),
            "every drain waiter notified: {id}"
        );
        for waiter in waiters {
            panic_deadline(waiter).await;
        }
        assert!(
            publisher
                .publication
                .state
                .lock()
                .unwrap()
                .pending
                .is_empty(),
            "pending cleanup"
        );
        let event = panic_deadline(next).await.unwrap();
        assert_eq!(json(&event), case["visible"][0], "failure event: {id}");
        panic_deadline(publisher.finish()).await;
        assert_eq!(
            panic_deadline(publisher.next()).await,
            None,
            "single failure: {id}"
        );
    }
    assert_eq!(ids.len(), 8, "complete durable matrix");
}

#[tokio::test]
async fn durable_spawn_cancelled_predecessor_releases_successor() {
    let (cancel, gate) = oneshot::channel::<()>();
    let (started, entered) = oneshot::channel();
    let polled = Arc::new(AtomicBool::new(false));
    let seen = polled.clone();
    let first: RegistryFuture<()> = Box::pin(async move {
        started.send(()).unwrap();
        gate.await
            .map_err(|_| RunRegistryError::Invalid("cancelled predecessor".into()))
    });
    let second: RegistryFuture<()> = Box::pin(async move {
        seen.store(true, Ordering::SeqCst);
        Ok(())
    });
    let publisher = TurnPublisher::start(
        run_ref(),
        Some(Arc::new(PanicRegistry(Mutex::new(VecDeque::from([
            first, second,
        ]))))),
        Arc::default(),
    );
    publisher.gateway_event(js(&json!({"type":"first"})));
    publisher.gateway_event(js(
        &json!({"type":"assistant_text_delta", "text":"again", "runId":"r"}),
    ));
    panic_deadline(entered).await.unwrap();
    let mut next = Box::pin(publisher.next());
    assert!(
        next.as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    assert!(
        !polled.load(Ordering::SeqCst),
        "successor must wait for predecessor"
    );
    drop(cancel);
    panic_deadline(publisher.finish()).await;
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/runtime-spawn-panic/oracle.json"
    ))
    .unwrap();
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "predecessor-durable")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        assert_eq!(json(&next.await.unwrap()), case["visible"][0]);
        assert_eq!(json(&publisher.next().await.unwrap()), case["visible"][1]);
        assert_eq!(publisher.next().await, None);
    })
    .await
    .expect("successor delivery deadline");
}
