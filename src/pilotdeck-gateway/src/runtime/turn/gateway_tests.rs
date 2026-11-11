use super::super::{
    projector::GatewayAgentEventProjector,
    publish::{FileRegistry, RegistryFuture, TurnRunRegistry},
    replay::ActiveTurnSnapshotInput,
    submit::{
        PreparedTurn, SubmitError, SubmitFuture, SubmitHost, SubmitWorkspace, WorkspaceAdmissions,
    },
};
use super::*;
use crate::{
    metadata::object,
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory, SessionNext,
            SessionOwner, SessionOwnerOptions, TurnCompletionHandle,
        },
    },
    storage::{FileRunRegistry, RunAcceptInput, RunRef, RunRegistryError},
};
use pilotdeck_protocol::{GATEWAY_PROTOCOL_VERSION, parse_json, stringify_json};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::watch;
const ORACLE: &str = include_str!("../../../tests/fixtures/turn-gateway/oracle.json");
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn value(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
struct Gate(watch::Sender<bool>);
impl Gate {
    fn new() -> Self {
        Self(watch::channel(false).0)
    }
    fn open(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        rx.wait_for(|v| *v).await.unwrap();
    }
}
struct Session {
    thread: std::thread::ThreadId,
    native: Vec<JsValue>,
    coordinator: Arc<TurnEventCoordinator>,
    accepted: Gate,
    terminal: Gate,
    end: Gate,
    aborts: AtomicUsize,
}
impl LiveSession for Arc<Session> {
    fn snapshot(&self) -> JsValue {
        object([
            ("messages", JsValue::Array(vec![])),
            ("cwd", "/unused".into()),
        ])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        assert_eq!(
            std::thread::current().id(),
            self.thread,
            "#813 session abort: must run on the gateway host thread"
        );
        self.aborts.fetch_add(1, Ordering::SeqCst);
        self.accepted.open();
        self.terminal.open();
        self.end.open();
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn submit(&self, _: JsValue, _: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        assert_eq!(
            std::thread::current().id(),
            self.thread,
            "#813 session submit: must run on the gateway host thread"
        );
        Ok(Box::new(Stream {
            session: self.clone(),
            index: 0,
        }))
    }
}
struct Stream {
    session: Arc<Session>,
    index: usize,
}
impl LiveTurn for Stream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if self.index == 1 {
                self.session.accepted.wait().await;
            }
            if self.index >= 2 {
                self.session.terminal.wait().await;
            }
            if let Some(event) = self.session.native.get(self.index) {
                self.index += 1;
                return Ok(Some(event.clone()));
            }
            self.session.end.wait().await;
            self.session.coordinator.emit(
                &"s".into(),
                &object([
                    ("type", "agent_status".into()),
                    ("event", "SessionEnd".into()),
                ]),
            );
            Ok(None)
        })
    }
}
struct Factory(Arc<Session>);
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = self.0.clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(Arc::new(session), None)) })
    }
}
struct Host {
    interactions: Arc<GatewayInteractionCoordinator>,
    finally_failure: bool,
}
impl SubmitHost for Host {
    fn workspace(&self, _: &JsValue) -> Result<SubmitWorkspace, SubmitError> {
        Ok(SubmitWorkspace {
            id: None,
            snapshots_required: false,
            recorder: None,
        })
    }
    fn failure_guard_config(&self) -> Option<JsValue> {
        None
    }
    fn set_session_cwd(&self, _: &JsString, _: &JsString) -> Result<(), SubmitError> {
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        20
    }
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            let binding =
                InteractionBinding::from_value(input.get("interactionBinding").unwrap()).unwrap();
            self.interactions
                .reconnect_for_turn(
                    input
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .unwrap(),
                    binding,
                )
                .unwrap();
            Ok(vec![])
        })
    }
    fn route_input<'a>(
        &'a self,
        _: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn preflight<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn admitted_preflight<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a RunRef,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn observe_event<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn has_sdk_config(&self) -> bool {
        false
    }
    fn validate_sdk_config(&self, _: &JsValue) -> Result<(), SubmitError> {
        Ok(())
    }
    fn set_sdk_config<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(async { Ok(false) })
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<PreparedTurn, SubmitError>> {
        Box::pin(async move {
            Ok(PreparedTurn {
                model_selection: object([("source", "default".into())]),
                input: object([
                    ("type", "text".into()),
                    ("text", input.get("message").cloned().unwrap()),
                ]),
                options: object([("turnId", JsValue::String(run.run_id.clone()))]),
            })
        })
    }
    fn commit_accepted_input<'a>(
        &'a self,
        _: &'a RunRef,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn record_failure<'a>(
        &'a self,
        _: &'a RunRef,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn finish_pump<'a>(&'a self, _: &'a RunRef) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.interactions
            .reject_pending_turn(session, &reason.into());
        Ok(())
    }
    fn release_claim(&self, _: &JsString, _: &JsString) {}
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        if self.finally_failure {
            Err(SubmitError::new(
                "gateway_request_failed",
                "injected finally failure",
            ))
        } else {
            Ok(())
        }
    }
}
struct Requests;
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            Ok(object([
                ("name", "fixture".into()),
                ("capabilities", JsValue::Array(vec![])),
            ]))
        })
    }
    fn request(&self, method: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move { Err(RequestError::new(format!("unexpected method: {method:?}"))) })
    }
}
struct Registry {
    file: FileRegistry,
    fail: bool,
    pending: Arc<AtomicUsize>,
    path: std::path::PathBuf,
}
impl Registry {
    fn track<T: Send + 'static>(&self, future: RegistryFuture<T>) -> RegistryFuture<T> {
        let pending = self.pending.clone();
        pending.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let result = future.await;
            pending.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }
}
impl TurnRunRegistry for Registry {
    fn accept(&self, input: &RunAcceptInput) -> RegistryFuture<bool> {
        self.track(self.file.accept(input))
    }
    fn events(&self, run: &RunRef) -> RegistryFuture<Vec<JsValue>> {
        self.track(self.file.events(run))
    }
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()> {
        if self.fail {
            Box::pin(async {
                Err(RunRegistryError::Invalid(
                    "injected registry append failure".into(),
                ))
            })
        } else {
            self.track(self.file.append(run, event))
        }
    }
}
struct Text(Mutex<Vec<String>>, watch::Sender<u64>);
impl Default for Text {
    fn default() -> Self {
        Self(Mutex::new(vec![]), watch::channel(0).0)
    }
}
impl Text {
    fn changed(&self) {
        self.1.send_modify(|generation| *generation += 1);
    }
    async fn observation_ready(&self, frames: usize, ended: bool, consumer: &AtomicBool) {
        let mut changed = self.1.subscribe();
        loop {
            changed.borrow_and_update();
            if self.0.lock().unwrap().len() >= frames && (!ended || consumer.load(Ordering::SeqCst))
            {
                return;
            }
            changed.changed().await.unwrap();
        }
    }
}
impl GatewayTextConnection for Text {
    fn send_text(&self, text: String) {
        if serde_json::from_str::<Value>(&text).unwrap()["type"] != "hello_ok" {
            self.0.lock().unwrap().push(text);
            self.changed();
        }
    }
    fn close(&self, code: u16, reason: &str) {
        panic!("unexpected close {code}:{reason}");
    }
}
#[test]
fn transport_observation_waits_for_delivery_and_receive_completion() {
    let text = Text::default();
    let ended = AtomicBool::new(false);
    let mut observation = Box::pin(text.observation_ready(2, true, &ended));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        observation.as_mut().poll(&mut context).is_pending(),
        "#813 transport observation: must wait for delivery before the receive completes"
    );
    text.send_text(r#"{"type":"event"}"#.into());
    assert!(
        observation.as_mut().poll(&mut context).is_pending(),
        "observation must not complete after only the event frame"
    );
    text.send_text(r#"{"type":"response"}"#.into());
    assert!(
        observation.as_mut().poll(&mut context).is_pending(),
        "observation must not complete before the receive completes"
    );
    ended.store(true, Ordering::SeqCst);
    text.changed();
    assert!(
        observation.as_mut().poll(&mut context).is_ready(),
        "observation must complete after delivery and receive completion"
    );
}

async fn settle(host: &TurnGateway, registry: &Registry) {
    for _ in 0..4 {
        host.call(async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        while registry.pending.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    }
}
fn request(id: &str, input: &Value) -> String {
    json!({"type":"request", "id":id, "method":"submit_turn", "params":input}).to_string()
}
async fn replay_case(case: &Value) {
    let case_name = case["name"].as_str().unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-turn-gateway-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("registry.json");
    let registry = Arc::new(Registry {
        file: FileRegistry::new(FileRunRegistry::open(&path).unwrap()).unwrap(),
        fail: case["setup"]["registryAppendFailure"] == true,
        pending: Arc::default(),
        path,
    });
    let native = case["native"].as_array().unwrap().iter().map(js).collect();
    let port = registry.clone();
    let finally_failure = case["setup"]["finallyFailure"] == true;
    let (send, ready) = std::sync::mpsc::sync_channel(1);
    let host = TurnGateway::start(TurnReplayOptions::new(), move |coordinator| {
        let session = Arc::new(Session {
            thread: std::thread::current().id(),
            native,
            coordinator: coordinator.clone(),
            accepted: Gate::new(),
            terminal: Gate::new(),
            end: Gate::new(),
            aborts: AtomicUsize::new(0),
        });
        let mut options = SessionOwnerOptions::new(Arc::new(Factory(session.clone())));
        options.idle_sweep_interval_ms = 0;
        let owner = SessionOwner::new(options);
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        let submitter = Arc::new(TurnSubmitter {
            owner,
            coordinator,
            registry: Some(port),
            projector: Arc::new(
                GatewayAgentEventProjector::new(Default::default())
                    .expect("#879 default projector"),
            ),
            host: Arc::new(Host {
                interactions: interactions.clone(),
                finally_failure,
            }),
            workspace_admissions: WorkspaceAdmissions::default(),
        });
        send.send(session).unwrap();
        Ok(GatewayServices {
            submitter,
            interactions,
            requests: Arc::new(Requests),
        })
    })
    .unwrap();
    let session = ready.recv().unwrap();
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello", "protocolVersion":GATEWAY_PROTOCOL_VERSION, "token":"token", "clientName":"web", "clientVersion":"fixture"}).to_string()).await.unwrap();
    let running = connection.clone();
    let first = request("a", &case["input"]);
    let ended = Arc::new(AtomicBool::new(false));
    let done = ended.clone();
    let progress = text.clone();
    let consumer = tokio::spawn(async move {
        running.receive(first).await.unwrap();
        done.store(true, Ordering::SeqCst);
        progress.changed();
    });
    let mut closing = None;
    let mut actual_run = "r".to_string();
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        match step.as_str().unwrap() {
            "observe" => {}, "accept" => session.accepted.open(), "terminal" => session.terminal.open(), "end" => session.end.open(),
            "external_error" => { host.emit("s".into(), parse_json(r#"{"type":"error","code":"injected","message":"external error","recoverable":true}"#).unwrap()).await.unwrap(); }
            "permission" => { host.emit("s".into(), parse_json(r#"{"type":"permission_request","requestId":"q","toolName":"bash","input":{"command":"pwd"},"sequence":41}"#).unwrap()).await.unwrap(); },
            "pending" => {
                let gateway = host.gateway.clone();
                host.call(async move {
                    gateway.0.interactions.permission_bus().register(&"s".into(), crate::runtime::interaction::PendingEntry {
                        request_id: "q".into(), tool_call_id: "tc".into(), tool_name: "bash".into(),
                        resolve: Arc::new(|_| {}), reject: Arc::new(|_| {}),
                    }, None).unwrap()
                }).await.unwrap();
            }
            "describe" => connection.receive(json!({"type":"request", "id":"info", "method":"describe_server", "params":{}}).to_string()).await.unwrap(),
            "second" => { let mut input = case["input"].clone(); input["runId"] = "r2".into(); connection.receive(request("b", &input)).await.unwrap(); },
            "replay" => connection.receive(request("b", &case["input"])).await.unwrap(),
            "conflict" => { let mut input = case["input"].clone(); input["message"] = "different".into(); connection.receive(request("b", &input)).await.unwrap(); },
            "close" => { let c = connection.clone(); closing = Some(tokio::spawn(async move { c.closed().await.unwrap(); })); },
            "abort" => connection.receive(json!({"type":"request","id":"stop","method":"abort_turn","params":case["setup"].get("abortParams").cloned().unwrap_or_else(|| json!({"sessionKey":"s","runId":actual_run}))}).to_string()).await.unwrap(),
            other => panic!("unknown step {other}"),
        }
        // A scheduler yield is not a dispatch or I/O barrier: on Linux the
        // submit task may not even have enqueued its first host job yet. Wait
        // for transport delivery (or receive completion), then inspect bytes.
        let expected = &case["results"][index];
        let ready = tokio::time::timeout(
            Duration::from_secs(5),
            text.observation_ready(
                expected["frames"].as_array().unwrap().len(),
                expected["ended"].as_bool().unwrap(),
                &ended,
            ),
        )
        .await;
        assert!(
            ready.is_ok(),
            "#813 {}: step {step}: transport observation did not arrive",
            case_name
        );
        settle(&host, &registry).await;
        let gateway = host.gateway.clone();
        let active = host
            .call(async move { gateway.0.submitter.owner.has_active_turn(&"s".into()) })
            .await
            .unwrap();
        if case["setup"]["generatedRunId"] == true && index == 0 {
            let gateway = host.gateway.clone();
            actual_run = host
                .call(async move {
                    gateway
                        .0
                        .submitter
                        .owner
                        .active_turn_run_id(&"s".into())
                        .unwrap()
                        .to_utf8()
                        .unwrap()
                })
                .await
                .unwrap();
        }
        let frames: Vec<_> = text
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|frame| frame.replace(&actual_run, "r"))
            .collect();
        let actual =
            json!({"frames":frames, "active":active, "ended":ended.load(Ordering::SeqCst)});
        assert_eq!(
            actual, case["results"][index],
            "#813 {}: step {step}: gateway frames, active and ended must match",
            case_name
        );
    }
    consumer.await.unwrap();
    if let Some(close) = closing {
        close.await.unwrap();
    }
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: actual_run.as_str().into(),
    };
    let registry_path = registry.path.clone();
    let actual = tokio::task::spawn_blocking(move || {
        FileRunRegistry::open(registry_path)
            .unwrap()
            .events(&run, None, None)
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|entry| serde_json::from_str::<Value>(
                &stringify_json(entry)
                    .unwrap()
                    .unwrap()
                    .replace(&actual_run, "r")
            )
            .unwrap())
            .collect::<Vec<_>>(),
        case["registryEvents"].as_array().unwrap().clone(),
        "#813 {}: registry seq: persisted event sequence must match",
        case_name
    );
    let gateway = host.gateway.clone();
    let snapshot = host
        .call(async move {
            gateway
                .0
                .submitter
                .coordinator
                .snapshot(&ActiveTurnSnapshotInput::new("s".into()))
                .to_json()
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(
            &stringify_json(&snapshot)
                .unwrap()
                .unwrap()
                .replace(&actual_run, "r")
        )
        .unwrap(),
        case["replay"],
        "#813 {}: replay: active-turn snapshot must match",
        case_name
    );
    if case_name.starts_with("close-")
        && case_name != "close-after-end"
        && case_name != "close-preserves-pending"
        && case_name != "close-after-busy"
    {
        assert_eq!(
            session.aborts.load(Ordering::SeqCst),
            1,
            "#813 {}: aborts after close: session must abort exactly once",
            case_name
        );
    }
    drop(connection);
    drop(host);
    drop(session);
    drop(registry);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn real_ts_gateway_frames_registry_replay_and_concurrency() {
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let mut selected = 0;
    for case in oracle["cases"].as_array().unwrap() {
        if std::env::var("PDRS_GATEWAY_CASE")
            .is_ok_and(|name| name != case["name"].as_str().unwrap())
        {
            continue;
        }
        selected += 1;
        let case_name = case["name"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(15), replay_case(case))
            .await
            .unwrap_or_else(|_| {
                panic!("#813 gateway watchdog: scenario {case_name} did not finish")
            });
    }
    assert!(
        selected > 0,
        "#813 gateway case filter: must match at least one scenario"
    );
}
#[test]
#[ignore = "requires the fixed TS baseline and tsx"]
fn real_ts_turn_gateway_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-gateway");
}

#[derive(Default)]
struct ClockState {
    now: u64,
    deadlines: HashMap<JsString, (u64, Callback)>,
}
#[derive(Default)]
struct Clock(Mutex<ClockState>);
impl Clock {
    fn advance(&self, ms: u64) {
        let callbacks = {
            let mut clock = self.0.lock().unwrap();
            clock.now += ms;
            let now = clock.now;
            let due: Vec<_> = clock
                .deadlines
                .iter()
                .filter(|(_, (due, _))| *due <= now)
                .map(|(key, _)| key.clone())
                .collect();
            due.into_iter()
                .map(|key| clock.deadlines.remove(&key).unwrap().1)
                .collect::<Vec<_>>()
        };
        for callback in callbacks {
            callback();
        }
    }
}
impl ReplayTimer for Clock {
    fn set(&self, key: &JsString, delay: Duration, callback: Callback) {
        let mut clock = self.0.lock().unwrap();
        let due = clock.now + pilotdeck_protocol::timer_delay_ms(delay.as_millis() as f64);
        clock.deadlines.insert(key.clone(), (due, callback));
    }
    fn cancel(&self, key: &JsString) {
        self.0.lock().unwrap().deadlines.remove(key);
    }
    fn cancel_all(&self) {
        self.0.lock().unwrap().deadlines.clear();
    }
}
fn empty_gateway_with_clock(retention: u64, clock: Arc<Clock>) -> TurnGateway {
    TurnGateway::start_with_clock(
        TurnReplayOptions::new().with_terminal_retention_ms(retention),
        Some(clock),
        |coordinator| {
            let session = Arc::new(Session {
                thread: std::thread::current().id(),
                native: vec![],
                coordinator: coordinator.clone(),
                accepted: Gate::new(),
                terminal: Gate::new(),
                end: Gate::new(),
                aborts: AtomicUsize::new(0),
            });
            let mut options = SessionOwnerOptions::new(Arc::new(Factory(session)));
            options.idle_sweep_interval_ms = 0;
            let interactions = Arc::new(GatewayInteractionCoordinator::default());
            Ok(GatewayServices {
                submitter: Arc::new(TurnSubmitter {
                    owner: SessionOwner::new(options),
                    coordinator,
                    registry: None,
                    projector: Arc::new(
                        GatewayAgentEventProjector::new(Default::default())
                            .expect("#879 default projector"),
                    ),
                    host: Arc::new(Host {
                        interactions: interactions.clone(),
                        finally_failure: false,
                    }),
                    workspace_admissions: WorkspaceAdmissions::default(),
                }),
                interactions,
                requests: Arc::new(Requests),
            })
        },
    )
    .unwrap()
}
#[tokio::test]
async fn serial_retention_matches_real_ts_host_steps_and_dispose() {
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    for case in oracle["timerCases"].as_array().unwrap() {
        let clock = Arc::new(Clock::default());
        let retention = case["retention"].as_u64().unwrap();
        let ordering = case["ordering"].as_str().unwrap().to_owned();
        let host = empty_gateway_with_clock(retention, clock.clone());
        let coordinator = host.gateway.0.submitter.coordinator.clone();
        if ordering == "after-step" {
            let coordinator = coordinator.clone();
            let initial = host
                .call(async move {
                    let key = "s".into();
                    let run = "r".into();
                    let _registration = coordinator.start(&key, &run, |_| {});
                    coordinator.record(
                        &key,
                        &parse_json(r#"{"type":"text","text":"retained","runId":"r"}"#).unwrap(),
                    );
                    coordinator.retain_terminal(&key, &run);
                    value(
                        &coordinator
                            .snapshot(&ActiveTurnSnapshotInput::new(key))
                            .to_json(),
                    )
                })
                .await
                .unwrap();
            // The scheduling step has returned; expiry occurs between steps.
            clock.advance(retention.max(1));
            let coordinator = host.gateway.0.submitter.coordinator.clone();
            let after = host
                .call(async move {
                    for _ in 0..8 {
                        tokio::task::yield_now().await;
                    }
                    value(
                        &coordinator
                            .snapshot(&ActiveTurnSnapshotInput::new("s".into()))
                            .to_json(),
                    )
                })
                .await
                .unwrap();
            assert_eq!(
                json!([initial, after, after, after]),
                case["snapshots"],
                "serial timer: {retention} after-step"
            );
            continue;
        }
        let mut snapshots = host
            .call(async move {
                let key = "s".into();
                let run = "r".into();
                let _registration = coordinator.start(&key, &run, |_| {});
                coordinator.record(
                    &key,
                    &parse_json(r#"{"type":"text","text":"retained","runId":"r"}"#).unwrap(),
                );
                coordinator.retain_terminal(&key, &run);
                let snap = || {
                    value(
                        &coordinator
                            .snapshot(&ActiveTurnSnapshotInput::new(key.clone()))
                            .to_json(),
                    )
                };
                let initial = snap();
                if ordering == "dispose-before-due" {
                    coordinator.dispose();
                }
                clock.advance(retention.max(1)); // Due during the actual production host step.
                if ordering == "dispose-after-due" {
                    coordinator.dispose();
                }
                let _replacement = (ordering == "replace-after-due")
                    .then(|| coordinator.start(&key, &"new".into(), |_| {}));
                let synchronous = snap();
                std::future::ready(()).await; // Ready continuation in the same step.
                vec![initial, synchronous, snap()]
            })
            .await
            .unwrap();
        let coordinator = host.gateway.0.submitter.coordinator.clone();
        snapshots.push(
            host.call(async move {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
                value(
                    &coordinator
                        .snapshot(&ActiveTurnSnapshotInput::new("s".into()))
                        .to_json(),
                )
            })
            .await
            .unwrap(),
        );
        assert_eq!(
            json!(snapshots),
            case["snapshots"],
            "serial timer: {retention} {}",
            case["ordering"]
        );
    }
}

#[tokio::test]
async fn serial_timer_cancels_queued_callbacks_and_replacements() {
    let clock = Arc::new(Clock::default());
    let timer = HostSerialTimer::with_clock(clock.clone());
    let fired = Arc::new(AtomicUsize::new(0));
    let key = "s".into();
    let callback: Callback = {
        let fired = fired.clone();
        Arc::new(move || {
            fired.fetch_add(1, Ordering::SeqCst);
        })
    };
    timer.set(&key, Duration::from_millis(5), callback.clone());
    clock.advance(4);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "#813 serial timer: must respect deadline"
    );
    clock.advance(1);
    // #813 fix-r2: the clock callback only enqueues. Running it inside
    // `advance` (the expiry-runs-on-clock-thread mutant) must trip this
    // assertion, not the cancel assertion that queued-cancel-ignored owns.
    // The default current-thread runtime cannot poll the driver before the
    // next await, so only `advance` itself can change `fired` here.
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "#813 serial timer: clock advance only enqueues the serial expiry"
    );
    timer.cancel(&key); // Cancel a callback already detached from the clock.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "#813 serial timer: cancel queued serial timer"
    );
    timer.set(&key, Duration::from_millis(5), callback.clone());
    clock.advance(5);
    timer.set(&key, Duration::from_millis(5), callback.clone());
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "#813 serial timer: replacement invalidates detached serial timer"
    );
    clock.advance(5);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "#813 serial timer: replacement timer fires once"
    );
    timer.set(&key, Duration::from_millis(5), callback.clone());
    clock.advance(5);
    timer.cancel_all();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "#813 serial timer: dispose cancels queued serial timers"
    );
    timer.set(&key, Duration::from_millis(5), callback);
    clock.advance(5);
    drop(timer);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "#813 serial timer: drop cancels queued serial timers"
    );
}

/// A7.2-COORDINATOR-DELIVERY-001-R1: every external emit enters the one
/// current-thread host, so concurrent callers queue behind each other, the
/// sink never runs off the host thread or overlaps another call, every event
/// is delivered exactly once, and delivery is part of the `emit` call: the
/// event is already in the sink when `emit` returns. The
/// `emit-bypasses-serial-host`, `emit-deferred-after-return` and
/// `emit-duplicated-delivery` mutations in
/// tests/fixtures/turn-gateway/negative.py respectively turn the thread,
/// delivery-before-return and per-event-exactly-once assertions red.
#[tokio::test]
async fn concurrent_external_emits_are_serialized_on_the_serial_host() {
    const CALLERS: usize = 8;
    let host = empty_gateway_with_clock(0, Arc::new(Clock::default()));
    let host_thread = host
        .call(async { std::thread::current().id() })
        .await
        .unwrap();
    let coordinator = host.gateway.0.submitter.coordinator.clone();
    let in_sink = Arc::new(AtomicBool::new(false));
    let overlap = Arc::new(AtomicBool::new(false));
    // One entry per sink call: the event's own text plus the thread that
    // delivered it. Per-event identity, not just a total: a duplicate is
    // named here, while a drop is caught by its own caller below before the
    // join, so a balanced double-delivery/drop cannot pass.
    let deliveries = Arc::new(Mutex::new(Vec::<(String, std::thread::ThreadId)>::new()));
    let registration = {
        let in_sink = in_sink.clone();
        let overlap = overlap.clone();
        let deliveries = deliveries.clone();
        host.call(async move {
            coordinator.start(&"s".into(), &"r".into(), move |event| {
                if in_sink.swap(true, Ordering::SeqCst) {
                    overlap.store(true, Ordering::SeqCst);
                }
                // Widen the window a non-serialized emitter would need; the
                // assertion itself does not depend on this sleep.
                std::thread::sleep(Duration::from_millis(2));
                let text = event
                    .get("text")
                    .and_then(JsValue::as_string)
                    .expect("sink event carries text")
                    .to_utf8()
                    .expect("event text is valid UTF-8");
                deliveries
                    .lock()
                    .unwrap()
                    .push((text, std::thread::current().id()));
                in_sink.store(false, Ordering::SeqCst);
            })
        })
        .await
        .unwrap()
    };
    let barrier = Arc::new(std::sync::Barrier::new(CALLERS));
    let mut callers = Vec::new();
    for index in 0..CALLERS {
        let gateway = host.clone();
        let barrier = barrier.clone();
        let deliveries = deliveries.clone();
        callers.push(std::thread::spawn(move || {
            let text = format!("concurrent-{index}");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            barrier.wait();
            runtime.block_on(async move {
                let event = parse_json(&format!(r#"{{"type":"text","text":"{text}"}}"#)).unwrap();
                let accepted = gateway.emit("s".into(), event).await.expect("emit");
                // Checked the instant `emit` resolves, without joining the
                // other callers: a detached host task that delivers after
                // the call returns must fail here even when it still
                // delivers once, in order, on the host thread.
                let delivered_before_return = deliveries
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(delivered, _)| delivered == &text);
                assert!(
                    delivered_before_return,
                    "#813 emit: returned before its own event reached the sink\n#813 emit event: {text}"
                );
                accepted
            })
        }));
    }
    let accepted = callers
        .into_iter()
        .map(|caller| caller.join().expect("caller thread"))
        .collect::<Vec<_>>();
    assert!(
        accepted.iter().all(|accepted| *accepted),
        "#813 emit: every concurrent external emit must be accepted"
    );
    assert!(
        !overlap.load(Ordering::SeqCst),
        "#813 emit: sink calls must not overlap"
    );
    let deliveries = deliveries.lock().unwrap();
    let mut counts = std::collections::HashMap::new();
    for (text, _) in deliveries.iter() {
        *counts.entry(text.as_str()).or_insert(0usize) += 1;
    }
    for index in 0..CALLERS {
        let text = format!("concurrent-{index}");
        assert_eq!(
            counts.get(text.as_str()).copied().unwrap_or(0),
            1,
            "#813 emit: every event must reach the sink exactly once\n#813 emit event: {text}"
        );
    }
    assert_eq!(
        deliveries.len(),
        CALLERS,
        "#813 emit: every accepted event must reach the sink exactly once"
    );
    assert!(
        deliveries.iter().all(|(_, thread)| *thread == host_thread),
        "#813 emit: the sink must run on the serial host thread, not on callers"
    );
    drop(registration);
}

/// A7.2-COORDINATOR-DELIVERY-001-R1: `HostedConnection::notify` is part of the
/// same serial-host entry as `receive`/`emit`. Unlike `emit` it only sends a
/// frame and mutates no coordinator state, so a direct call on the caller's
/// thread would be silent unless the transport records the delivering thread.
/// Every outbound frame's thread is captured here; `notify` must be the host
/// thread. `closed` is pinned by the `Session::abort` host-thread assertion
/// that the `close-active` replay case exercises.
struct HostText {
    frames: Mutex<Vec<(String, std::thread::ThreadId)>>,
    changed: watch::Sender<u64>,
}
impl Default for HostText {
    fn default() -> Self {
        Self {
            frames: Mutex::new(vec![]),
            changed: watch::channel(0).0,
        }
    }
}
impl HostText {
    async fn wait_for_notification(&self) {
        let mut changed = self.changed.subscribe();
        loop {
            changed.borrow_and_update();
            if self
                .frames
                .lock()
                .unwrap()
                .iter()
                .any(|(frame, _)| frame.contains(r#""type":"notification""#))
            {
                return;
            }
            changed.changed().await.unwrap();
        }
    }
}
impl GatewayTextConnection for HostText {
    fn send_text(&self, text: String) {
        self.frames
            .lock()
            .unwrap()
            .push((text, std::thread::current().id()));
        self.changed.send_modify(|generation| *generation += 1);
    }
    fn close(&self, code: u16, reason: &str) {
        panic!("unexpected close {code}:{reason}");
    }
}
#[tokio::test]
async fn hosted_connection_notify_enters_the_serial_host() {
    let host = empty_gateway_with_clock(0, Arc::new(Clock::default()));
    let host_thread = host
        .call(async { std::thread::current().id() })
        .await
        .unwrap();
    let text = Arc::new(HostText::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello", "protocolVersion":GATEWAY_PROTOCOL_VERSION, "token":"token", "clientName":"web", "clientVersion":"fixture"})
                .to_string(),
        )
        .await
        .unwrap();
    connection
        .notify(
            "fixture".into(),
            Some(parse_json(r#"{"value":1}"#).unwrap()),
        )
        .await
        .unwrap();
    text.wait_for_notification().await;
    let frames = text.frames.lock().unwrap();
    let (_, thread) = frames
        .iter()
        .find(|(frame, _)| frame.contains(r#""type":"notification""#))
        .expect("notification frame");
    assert_eq!(
        *thread, host_thread,
        "#813 HostedConnection::notify: must run on the serial host thread, not on callers"
    );
}

// Poll exactly at fixture observation points. This makes the abort await and
// replacement deterministic without sleeps or assumptions about scheduler turns.
#[tokio::test]
async fn abort_waits_for_latest_real_ts_session_fence() {
    replay_abort_fences(None).await;
}

#[tokio::test]
async fn abort_snapshots_before_ungated_cleanup_replacement() {
    replay_abort_fences(Some("ungated-cleanup-replacement")).await;
}

async fn replay_abort_fences(only: Option<&str>) {
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let mut selected = 0;
    for case in oracle["abortFenceCases"].as_array().unwrap() {
        if only.is_some_and(|name| case["name"] != name) {
            continue;
        }
        selected += 1;
        let case_name = case["name"].as_str().unwrap();
        let host = empty_gateway_with_clock(30_000, Arc::new(Clock::default()));
        let gateway = host.gateway.clone();
        let steps = case["steps"].as_array().unwrap().clone();
        let results = host
            .call(async move {
                let owner = &gateway.0.submitter.owner;
                let mut completions = HashMap::new();
                let mut reservations = HashMap::new();
                let mut router_released = steps.iter().any(|step| step == "ungated");
                let mut drains = HashMap::new();
                let mut abort = None;
                let mut ended = false;
                let mut results = vec![];
                for step in &steps {
                    let raw_step = step.as_str().unwrap();
                    let (step, session) = raw_step.split_once('@').unwrap_or((raw_step, "s"));
                    let key: JsString = session.into();
                    let run: JsString = step.chars().last().unwrap().to_string().into();
                    if step.starts_with("begin-") {
                        reservations.insert(
                            (key.clone(), run.clone()),
                            owner.begin_turn(&key, &run).unwrap(),
                        );
                        completions.insert(
                            (key.clone(), run.clone()),
                            owner.begin_completion(&key, &run),
                        );
                        let drain = gateway.track();
                        drain.observer()(&RunRef {
                            session_key: key.clone(),
                            project_key: None,
                            run_id: run.clone(),
                        });
                        drains.insert((key, run), drain);
                    } else if step.starts_with("reject-") {
                        assert!(owner.begin_turn(&key, &run).is_none());
                        // Identity resolution and a rejected consumer do not
                        // begin a completion and must not overwrite the slot.
                        drains.insert((key, run), gateway.track());
                    } else if step.starts_with("owner-done-") {
                        owner.end_turn(
                            &key,
                            Some(&run),
                            reservations.get(&(key.clone(), run.clone())),
                        );
                    } else if step.starts_with("finish-") {
                        if let Some(completion) = completions.get(&(key.clone(), run.clone())) {
                            owner.complete_turn(&key, completion);
                        }
                        drop(drains.remove(&(key, run)));
                    } else if step == "ungated" {
                        assert!(router_released);
                    } else if step == "release-router" {
                        router_released = true;
                    } else if step.starts_with("abort-") {
                        let params = if step == "abort-a" {
                            js(&json!({"sessionKey":"s", "runId":"a"}))
                        } else {
                            js(&json!({"sessionKey":"s"}))
                        };
                        abort = Some(gateway.abort_on_close(params));
                        // Start the request now, before any replacement steps.
                        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                        if let std::task::Poll::Ready(result) =
                            abort.as_mut().unwrap().as_mut().poll(&mut context)
                        {
                            result.unwrap();
                            ended = true;
                        }
                    } else {
                        assert_eq!(step, "observe");
                        if !ended && router_released {
                            let mut context =
                                std::task::Context::from_waker(std::task::Waker::noop());
                            if let std::task::Poll::Ready(result) =
                                abort.as_mut().unwrap().as_mut().poll(&mut context)
                            {
                                result.unwrap();
                                ended = true;
                            }
                        }
                        results.push(ended);
                    }
                }
                results
            })
            .await
            .unwrap();
        assert_eq!(
            json!(results),
            case["results"],
            "#813 abort fence: {}: results must match",
            case_name
        );
    }
    assert!(
        selected > 0,
        "#813 abort fence filter: must match at least one scenario"
    );
}

/// #749 transport-only fixture reuses the serial host/session apparatus.
pub(crate) fn native_ws_fixture(requests: Arc<dyn GatewayRequests>) -> TurnGateway {
    native_ws_fixture_with_cleanup(requests, async { Ok(()) })
}

#[tokio::test]
async fn native_ws_queued_input_outlives_cancelled_admission_waiter() {
    use crate::{negative_assert, negative_assert_eq, test_negative_receipt::expect_ok};
    use tokio::{sync::Semaphore, time::timeout};
    const BOUND: Duration = Duration::from_secs(5);
    struct Sink;
    impl GatewayTextConnection for Sink {
        fn send_text(&self, _: String) {}
        fn close(&self, _: u16, _: &str) {}
    }
    let host = native_ws_fixture(Arc::new(Requests));
    let connection = host.connect(Arc::new(Sink), "fixture-token".into(), "probe".into());
    let (entered, entry) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    // Deliberately suspend the dedicated host thread, not the caller runtime.
    // recv_timeout bounds the predecessor even if this test panics.
    assert!(
        host.jobs
            .send(Box::pin(async move {
                let _ = entered.send(());
                let _ = gate.recv_timeout(BOUND);
            }))
            .is_ok(),
        "#1079 host gate submitted"
    );
    expect_ok(
        expect_ok(
            timeout(BOUND, entry).await,
            "#1079 host gate entry deadline",
        ),
        "#1079 host gate entered",
    );
    let budget = Arc::new(Semaphore::new(128));
    let message = json!({"type":"hello","protocolVersion":"1.2","clientName":"ui",
        "clientVersion":"p","token":"fixture-token"})
    .to_string();
    let length = message.len();
    let permit = expect_ok(
        budget.clone().try_acquire_many_owned(length as u32),
        "#1079 queued input reservation",
    );
    let mut admission = Box::pin(connection.enqueue_with_input_permit(message, Some(permit)));
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(admission.as_mut().poll(cx).is_pending()))
            .await;
    negative_assert!(pending, "#1079 suspended host leaves admission pending");
    drop(admission);
    negative_assert_eq!(
        budget.available_permits(),
        128 - length,
        "#1079 queued job retains lease after admission waiter cancellation"
    );
    expect_ok(release.send(()), "#1079 release host admission gate");
    expect_ok(
        timeout(BOUND, async {
            while budget.available_permits() != 128 {
                tokio::task::yield_now().await;
            }
        })
        .await,
        "#1079 queued job completion refunds lease",
    );
    expect_ok(
        expect_ok(
            timeout(BOUND, host.shutdown(BOUND)).await,
            "#1079 queued host shutdown deadline",
        ),
        "#1079 queued host shutdown",
    );
    let permit = expect_ok(
        budget.clone().try_acquire_many_owned(8),
        "#1079 stopped host reservation",
    );
    negative_assert!(
        connection
            .enqueue_with_input_permit("rejected".into(), Some(permit))
            .await
            .is_err(),
        "#1079 stopped host rejects input"
    );
    negative_assert_eq!(
        budget.available_permits(),
        128,
        "#1079 failed admission refunds lease"
    );
}
pub(crate) fn native_ws_fixture_with_cleanup(
    requests: Arc<dyn GatewayRequests>,
    cleanup: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
) -> TurnGateway {
    native_ws_fixture_with_disposal(requests, cleanup, false)
}
struct NativeFixtureSession(Arc<Session>, bool);
impl LiveSession for NativeFixtureSession {
    fn snapshot(&self) -> JsValue {
        self.0.snapshot()
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.0.abort(reason)
    }
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.0.submit(input, options)
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        if self.1 {
            Box::pin(async { Err(RuntimeError::new("#749 injected session disposal failure")) })
        } else {
            self.0.dispose()
        }
    }
}
struct NativeFixtureFactory(Arc<Session>, bool);
impl SessionFactory for NativeFixtureFactory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = NativeFixtureSession(self.0.clone(), self.1);
        Box::pin(async move { Ok(LiveSessionHandle::new(Arc::new(session), None)) })
    }
}
pub(crate) fn native_ws_fixture_with_disposal(
    requests: Arc<dyn GatewayRequests>,
    cleanup: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
    fail_disposal: bool,
) -> TurnGateway {
    TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            let session = Arc::new(Session {
                thread: std::thread::current().id(),
                native: vec![
                    parse_json(r#"{"type":"turn_started","sessionId":"s","turnId":"r"}"#)
                        .expect("#749 fixture native event"),
                ],
                coordinator: coordinator.clone(),
                accepted: Gate::new(),
                terminal: Gate::new(),
                end: Gate::new(),
                aborts: AtomicUsize::new(0),
            });
            let mut options =
                SessionOwnerOptions::new(Arc::new(NativeFixtureFactory(session, fail_disposal)));
            options.idle_sweep_interval_ms = 0;
            let interactions = Arc::new(GatewayInteractionCoordinator::default());
            Ok(GatewayServices {
                submitter: Arc::new(TurnSubmitter {
                    owner: SessionOwner::new(options),
                    coordinator,
                    registry: None,
                    projector: Arc::new(
                        GatewayAgentEventProjector::new(Default::default())
                            .expect("#749 fixture projector"),
                    ),
                    host: Arc::new(Host {
                        interactions: interactions.clone(),
                        finally_failure: false,
                    }),
                    workspace_admissions: WorkspaceAdmissions::default(),
                }),
                interactions,
                requests,
            })
        },
        cleanup,
    )
    .expect("#749 fixture serial host")
}

/// #1746: a builder error must not skip the host teardown. The host owns the
/// coordinator it created and the embedding cleanup it was handed; both settle
/// on the host thread before `start_with_cleanup` reports the failure.
#[test]
fn a_builder_error_disposes_the_coordinator_and_the_embedding_cleanup() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let cleanup_ran = Arc::new(AtomicBool::new(false));
    let host_thread = Arc::new(std::sync::Mutex::new(None));
    let disposed = coordinator_disposed.clone();
    let built_on = host_thread.clone();
    let cleaned_on = host_thread.clone();
    let cleaned = cleanup_ran.clone();
    let error = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            *built_on.lock().unwrap() = Some(std::thread::current().id());
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            Err(std::io::Error::other("#1746 injected builder failure"))
        },
        async move {
            assert_eq!(
                Some(std::thread::current().id()),
                *cleaned_on.lock().unwrap(),
                "#1746 the embedding cleanup must run on the host thread"
            );
            cleaned.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .err()
    .expect("#1746 a builder failure must fail the host");
    assert_eq!("#1746 injected builder failure", error.to_string());
    // #1942: the two switches that skip this teardown are bound to this test in
    // the native-ws-entry roster, and that runner only accepts a kill that
    // carries a nonce receipt — so these two assertions go through
    // `negative_assert!` (a plain `assert!` panics without recording one).
    crate::negative_assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1746 a builder error must still dispose the coordinator"
    );
    crate::negative_assert!(
        cleanup_ran.load(Ordering::SeqCst),
        "#1746 a builder error must still poll the embedding cleanup"
    );
}

/// #1746: the refusal branch carries services, so the host also disposes what
/// it refuses — the owner it was handed is shut down, not just dropped.
#[test]
fn a_refused_submitter_is_disposed_before_the_failure_is_reported() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let cleanup_ran = Arc::new(AtomicBool::new(false));
    let refused_owner = Arc::new(std::sync::Mutex::new(None));
    let disposed = coordinator_disposed.clone();
    let cleaned = cleanup_ran.clone();
    let owner_out = refused_owner.clone();
    let started = std::time::Instant::now();
    let error = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            let session = Arc::new(Session {
                thread: std::thread::current().id(),
                native: Vec::new(),
                coordinator: coordinator.clone(),
                accepted: Gate::new(),
                terminal: Gate::new(),
                end: Gate::new(),
                aborts: AtomicUsize::new(0),
            });
            let mut options =
                SessionOwnerOptions::new(Arc::new(NativeFixtureFactory(session, false)));
            options.idle_sweep_interval_ms = 0;
            let owner = SessionOwner::new(options);
            *owner_out.lock().unwrap() = Some(owner.clone());
            let interactions = Arc::new(GatewayInteractionCoordinator::default());
            Ok(GatewayServices {
                submitter: Arc::new(TurnSubmitter {
                    owner,
                    // #1746 fixture: a coordinator this host does not drive.
                    coordinator: Arc::new(TurnEventCoordinator::new()),
                    registry: None,
                    projector: Arc::new(
                        GatewayAgentEventProjector::new(Default::default())
                            .expect("#1746 fixture projector"),
                    ),
                    host: Arc::new(Host {
                        interactions: interactions.clone(),
                        finally_failure: false,
                    }),
                    workspace_admissions: WorkspaceAdmissions::default(),
                }),
                interactions,
                requests: Arc::new(Requests),
            })
        },
        async move {
            cleaned.store(true, Ordering::SeqCst);
            // #1746: the refusal branch must still give a hung cleanup its
            // poll on the host thread; the bound lets the host move on.
            std::future::pending::<()>().await;
            Ok(())
        },
    )
    .err()
    .expect("#1746 a refused submitter must fail the host");
    assert_eq!(
        "submitter must use the serial host coordinator",
        error.to_string()
    );
    assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1746 a refused submitter must still dispose the coordinator"
    );
    assert!(
        cleanup_ran.load(Ordering::SeqCst),
        "#1746 a refused submitter must still poll the embedding cleanup"
    );
    let owner = refused_owner
        .lock()
        .unwrap()
        .clone()
        .expect("#1746 refused owner captured");
    assert!(
        owner.begin_turn(&"project".into(), &"run".into()).is_none(),
        "#1746 the refused owner must be shut down, not merely dropped"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "#1746 the refused branch's hung cleanup must hit the deadline, took {:?}",
        started.elapsed()
    );
}

/// #1746: the failure teardown is bounded — a cleanup that never completes
/// must not turn a startup failure into a startup hang. The elapsed bound pins
/// the default deadline into the oracle: raising the test constant (or losing
/// the deadline) makes this fail or hang instead of quietly getting slower.
#[test]
fn a_hung_cleanup_does_not_hold_the_failure_report() {
    let started = std::time::Instant::now();
    let error = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        |_| Err(std::io::Error::other("#1746 hung cleanup fixture")),
        std::future::pending::<Result<(), String>>(),
    )
    .err()
    .expect("#1746 a builder failure must fail the host");
    assert_eq!("#1746 hung cleanup fixture", error.to_string());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "#1746 a hung cleanup must hit the host teardown deadline, took {:?}",
        started.elapsed()
    );
}

/// #1746: the refused owner and the embedding cleanup do not share a clock, so
/// a slow owner shutdown cannot leave the cleanup with an expired deadline
/// (which would let `cleanup_ran` suppress the embedding's own fallback while
/// nothing actually disposed factory/project).
///
/// #1960: the argument is the value production actually passes.
/// `start_with_cleanup_budget` turns a declared budget into
/// [`super::declared_cleanup_budget`] before it calls
/// `initialization_teardown_bounds`, so a declared 300 ms arrives here as
/// 300 ms + SLACK. The test takes that value from the same function production
/// calls — copying the arithmetic by hand left the slack unpinned. Feeding the
/// raw declared budget instead made `cleanup_bound == declared` and
/// `owner + cleanup <= declared + DEADLINE` hold for the wrong reason: on the
/// real input they are `declared + SLACK` and exactly
/// `declared + SLACK + DEADLINE`.
#[test]
fn a_slow_refused_owner_does_not_shrink_the_cleanup_bound() {
    let declared = std::time::Duration::from_millis(300);
    let passed = super::declared_cleanup_budget(declared);
    let (owner_bound, cleanup_bound) = super::initialization_teardown_bounds(passed);
    assert_eq!(
        super::INITIALIZATION_TEARDOWN_DEADLINE,
        owner_bound,
        "#1746 the refused owner is the host's own resource and stays on the default bound"
    );
    assert_eq!(
        passed, cleanup_bound,
        "#1746 the cleanup keeps the full deadline its embedding declared"
    );
    assert!(
        cleanup_bound > declared,
        "#1746 the cleanup keeps the slack on top of the declared budget, not the budget itself"
    );
    assert_eq!(
        passed.saturating_add(super::INITIALIZATION_TEARDOWN_DEADLINE),
        owner_bound + cleanup_bound,
        "#1746 the failure path costs exactly the two bounds together"
    );
}

/// #1746: the failure teardown bound comes from the embedding's own teardown
/// budget, not from the host default. A budget longer than the default must be
/// waited out — otherwise a legitimate slow cleanup is cut mid-poll — and the
/// wait must still end at the declared budget.
#[test]
fn a_caller_budget_decides_the_failure_teardown_bound() {
    let started = std::time::Instant::now();
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        std::time::Duration::from_millis(300),
        |_| Err(std::io::Error::other("#1746 budgeted cleanup fixture")),
        std::future::pending::<Result<(), String>>(),
    )
    .err()
    .expect("#1746 a builder failure must fail the host");
    let elapsed = started.elapsed();
    assert_eq!("#1746 budgeted cleanup fixture", error.to_string());
    assert!(
        elapsed >= std::time::Duration::from_millis(290),
        "#1746 the host must wait out the caller's teardown budget (expected ~310 ms), took {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "#1746 the caller's budget must bound the wait, took {elapsed:?}"
    );
}

/// #2081: the production wiring test for `start_with_cleanup_budget`. The
/// bound test above feeds `super::declared_cleanup_budget` to the bounds
/// function by hand, so nothing went through the production entry — changing
/// `start_with_cleanup_budget`'s call to pass the raw budget (dropping the
/// slack) left every test green while a legitimate slow cleanup got cut
/// mid-poll. The observable is the deadline the host actually enforces: a
/// cleanup that finishes just inside `declared + SLACK` (but past the raw
/// `declared`) must be allowed to complete.
#[test]
fn the_declared_cleanup_budget_reaches_the_initialization_teardown() {
    let declared = std::time::Duration::from_millis(300);
    let finished = Arc::new(AtomicBool::new(false));
    let completed = finished.clone();
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        declared,
        |_| Err(std::io::Error::other("#2081 wiring fixture")),
        async move {
            tokio::time::sleep(declared + super::INITIALIZATION_TEARDOWN_SLACK / 2).await;
            completed.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
        },
    )
    .err()
    .expect("#2081 a builder failure must fail the host");
    assert_eq!("#2081 wiring fixture", error.to_string());
    assert!(
        finished.load(Ordering::SeqCst),
        "#2081 the initialization cleanup must run on the production deadline \
         (declared budget + slack), not the raw declared budget"
    );
}

/// #2081: the other side of the wiring pin — the deadline must not be larger
/// than `declared_cleanup_budget` either, or a runaway cleanup would hold the
/// failure report for longer than the embedding declared. A cleanup sleeping
/// past the declared bound must be cut.
#[test]
fn the_declared_cleanup_budget_does_not_outlive_the_declared_bound() {
    let declared = std::time::Duration::from_millis(300);
    let finished = Arc::new(AtomicBool::new(false));
    let completed = finished.clone();
    let started = std::time::Instant::now();
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        declared,
        |_| Err(std::io::Error::other("#2081 cut fixture")),
        async move {
            tokio::time::sleep(declared + super::INITIALIZATION_TEARDOWN_SLACK * 2).await;
            completed.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .err()
    .expect("#2081 a builder failure must fail the host");
    assert_eq!("#2081 cut fixture", error.to_string());
    assert!(
        !finished.load(Ordering::SeqCst),
        "#2081 a cleanup sleeping past the declared bound must be cut"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "#2081 the declared bound must bound the wait, took {:?}",
        started.elapsed()
    );
}

/// #1949: the refusal branch's failure path costs the two bounds together.
/// The refused owner runs on the default teardown deadline (the host's own
/// resource) and the embedding cleanup then gets its own full deadline on a
/// fresh clock, so a builder failure with both hung costs
/// `owner_bound + cleanup_bound` — the two-segment-clock contract the failure
/// path documents. A regression back to one shared deadline (the r2→r3 fix)
/// leaves only the larger of the two.
///
/// The owner's hang needs the creation worker parked **inside** the factory
/// before the host exists: `create_active` re-checks `state.stopped` before
/// every factory call, so a worker first polled after the host's `shutdown()`
/// set that flag bails with "Session router is shut down." instead of hanging,
/// and the drain resolves immediately. Firing the creation on the test's own
/// runtime and waiting for the factory's entered flag parks the worker past
/// that check for good — the drain on the host thread then waits on a
/// completion only that parked worker can send.
#[tokio::test]
async fn a_refused_owner_and_a_hung_cleanup_cost_the_two_bounds_together() {
    let declared = std::time::Duration::from_millis(300);
    let (owner_bound, cleanup_bound) =
        super::initialization_teardown_bounds(super::declared_cleanup_budget(declared));
    // #1949 fixture: an owner with a creation that never settles, so its
    // shutdown() waits on the pending drain and the host must cut it at the
    // owner bound instead of the cleanup's.
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut options = SessionOwnerOptions::new(Arc::new(PendingCreationFactory {
        entered: entered.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    // Fire the creation and let go of the public waiter: the drain below (the
    // spawned worker) is what the shutdown must wait on.
    drop(owner.get_or_create(SessionContext {
        session_key: "s".into(),
        session_id: "s".into(),
        project_key: None,
        channel_key: "channel".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }));
    for _ in 0..100 {
        if entered.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        "#1949 fixture: the creation worker must reach the factory before the host starts"
    );
    let started = std::time::Instant::now();
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        declared,
        move |_coordinator| {
            let interactions = Arc::new(GatewayInteractionCoordinator::default());
            Ok(GatewayServices {
                submitter: Arc::new(TurnSubmitter {
                    owner,
                    // #1949 fixture: a coordinator this host does not drive.
                    coordinator: Arc::new(TurnEventCoordinator::new()),
                    registry: None,
                    projector: Arc::new(
                        GatewayAgentEventProjector::new(Default::default())
                            .expect("#1949 fixture projector"),
                    ),
                    host: Arc::new(Host {
                        interactions: interactions.clone(),
                        finally_failure: false,
                    }),
                    workspace_admissions: WorkspaceAdmissions::default(),
                }),
                interactions,
                requests: Arc::new(Requests),
            })
        },
        // #1949 fixture: an embedding cleanup that never completes.
        std::future::pending::<Result<(), String>>(),
    )
    .err()
    .expect("#1949 a refused submitter must fail the host");
    assert_eq!(
        "submitter must use the serial host coordinator",
        error.to_string()
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= owner_bound + cleanup_bound - std::time::Duration::from_millis(30),
        "#1949 the failure path must wait out both bounds (expected ~{:?}), took {elapsed:?}",
        owner_bound + cleanup_bound
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "#1949 the two bounds together must bound the wait, took {elapsed:?}"
    );
}

/// #1949: `Duration::MAX` is the idiomatic "no bound" a library caller can
/// pass straight into the public `start_with_cleanup_budget`. The deadline
/// arithmetic must not overflow on it — the std panic used to kill the host
/// thread, so the caller got an opaque "gateway host initialization stopped"
/// with the coordinator and its interactions left live instead of the
/// declared deadline semantics. A fast cleanup still completes, the builder
/// error is reported verbatim, and the coordinator is disposed.
#[test]
fn a_maximal_teardown_budget_still_reports_the_builder_error() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let disposed = coordinator_disposed.clone();
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        std::time::Duration::MAX,
        move |coordinator| {
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            Err(std::io::Error::other("#1949 maximal budget fixture"))
        },
        async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Ok(())
        },
    )
    .err()
    .expect("#1949 a builder failure must fail the host");
    assert_eq!("#1949 maximal budget fixture", error.to_string());
    assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1949 a maximal budget must still dispose the coordinator after its cleanup"
    );
}

/// #1949: the failed-initialization twin of the clock-edge drain test below
/// — a budget that fits the clock but lands within the tick-conversion
/// headroom of its ceiling still gets the waiver, and the teardown still
/// completes, the builder error still reaches the caller verbatim, and the
/// coordinator is still disposed. Behavior anchor, not a regression
/// discriminator (#2501): the teardown site reads its own, later clock, and
/// this fixture's cleanup is immediately `Ready` — `Timeout::poll` polls the
/// future before the delay (tokio 1.48), so the timer registration the
/// headroom guards against is not even reached on this path. The tier
/// routing itself is pinned deterministically by
/// `the_clock_edge_tier_decision_is_deterministic` below.
#[test]
fn a_clock_edge_teardown_budget_still_reports_the_builder_error() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let disposed = coordinator_disposed.clone();
    // The same maximal-fit binary search the drain twin documents, backed
    // off into the middle of the tick-conversion headroom so the teardown
    // site's own (later) clock read still admits it inside that tier.
    let mut lo = std::time::Duration::ZERO;
    let mut hi = std::time::Duration::MAX;
    for _ in 0..512 {
        let mid = lo + (hi - lo) / 2;
        if tokio::time::Instant::now().checked_add(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let error = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        lo - TICK_CONVERSION_HEADROOM / 2,
        move |coordinator| {
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            Err(std::io::Error::other("#1949 clock-edge budget fixture"))
        },
        async { Ok(()) },
    )
    .err()
    .expect("#1949 a builder failure must fail the host");
    assert_eq!("#1949 clock-edge budget fixture", error.to_string());
    assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1949 a clock-edge budget must still dispose the coordinator after its cleanup"
    );
}

/// #1942: the embedding cleanup is third-party code polled on the host
/// thread; a panic in it must not skip the coordinator and interaction
/// disposals behind it (`caught_cleanup` converts the panic to an `Err` the
/// teardown already ignores). The caller still gets the builder error
/// verbatim instead of the host thread's panic.
#[test]
fn a_panicking_cleanup_still_disposes_the_coordinator() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let disposed = coordinator_disposed.clone();
    let error = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            Err(std::io::Error::other("#1942 panicking cleanup fixture"))
        },
        async {
            panic!("#1942 injected cleanup panic");
        },
    )
    .err()
    .expect("#1942 a builder failure must fail the host");
    // #1942: the native-ws-entry roster binds the
    // `native-ws-initialization-cleanup-panic-uncaught` switch to the
    // coordinator assertion below, and that runner only accepts a kill that
    // carries a nonce receipt — so it must run before the verbatim-error
    // check: with the panic escaping, the host thread dies before the
    // disposals below it and the caller gets the opaque host error instead.
    crate::negative_assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1942 a panicking cleanup must still dispose the coordinator"
    );
    assert_eq!("#1942 panicking cleanup fixture", error.to_string());
}

/// #1942: the drain polls the same third-party cleanup the failed
/// initialization does, so the panic guard must cover it there too — with
/// the panic escaping (the roster switch below), the host thread dies before
/// the disposals behind it and the drain never settles; the guard timeout
/// stands in for that hang so the kill runs through the coordinator
/// assertion, which is the one the roster binds (the same ordering rule the
/// initialization twin above documents).
#[tokio::test]
async fn a_panicking_drain_cleanup_still_disposes_the_coordinator() {
    let coordinator_disposed = Arc::new(AtomicBool::new(false));
    let disposed = coordinator_disposed.clone();
    let host = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            coordinator.set_after_dispose_hook(Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
            }));
            let session = Arc::new(Session {
                thread: std::thread::current().id(),
                native: vec![
                    parse_json(r#"{"type":"turn_started","sessionId":"s","turnId":"r"}"#)
                        .expect("#1942 fixture native event"),
                ],
                coordinator: coordinator.clone(),
                accepted: Gate::new(),
                terminal: Gate::new(),
                end: Gate::new(),
                aborts: AtomicUsize::new(0),
            });
            let mut options =
                SessionOwnerOptions::new(Arc::new(NativeFixtureFactory(session, false)));
            options.idle_sweep_interval_ms = 0;
            let interactions = Arc::new(GatewayInteractionCoordinator::default());
            Ok(GatewayServices {
                submitter: Arc::new(TurnSubmitter {
                    owner: SessionOwner::new(options),
                    coordinator,
                    registry: None,
                    projector: Arc::new(
                        GatewayAgentEventProjector::new(Default::default())
                            .expect("#1942 fixture projector"),
                    ),
                    host: Arc::new(Host {
                        interactions: interactions.clone(),
                        finally_failure: false,
                    }),
                    workspace_admissions: WorkspaceAdmissions::default(),
                }),
                interactions,
                requests: Arc::new(Requests),
            })
        },
        async move {
            panic!("#1942 injected drain cleanup panic");
        },
    )
    .expect("#1942 fixture serial host");
    let settled = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.shutdown(std::time::Duration::from_secs(5)),
    )
    .await;
    crate::negative_assert!(
        coordinator_disposed.load(Ordering::SeqCst),
        "#1942 a panicking drain cleanup must still dispose the coordinator"
    );
    let error = settled
        .expect("#1942 the drain must settle once the panic is caught")
        .expect_err("#1942 a caught drain cleanup panic must surface as the drain error");
    assert_eq!(
        "#1942 injected drain cleanup panic", error,
        "#1942 the caught panic message must reach the drain result"
    );
}

/// #1949: an embedding that declares `Duration::MAX` for shutdown has
/// explicitly waived the drain deadline (see `declared_cleanup_budget` for
/// the waiver), so the drain must settle with its real result (here `Ok`)
/// rather than hang. Behavior anchor, not a regression discriminator:
/// tokio's `timeout` saturates a MAX bound into a far-future timer, so the
/// pre-#1949 `timeout(budget, drain)` settled too — the drain-side
/// `checked_add` spells that waiver out and additionally waives the
/// tick-conversion band (`TICK_CONVERSION_HEADROOM`). The drain's own `Result` has
/// been asserted here from the start: the guard above consumes its outer
/// `Result` with `.expect`, and the pre-image asserted the inner `is_ok()`;
/// the later restatement as `expect` only unified the form, adding no
/// discriminating power (a host panic that escapes the drain settles the
/// inner `Err` under either form).
#[tokio::test]
async fn a_maximal_drain_budget_still_settles_the_drain() {
    let host = native_ws_fixture_with_cleanup(Arc::new(Requests), async { Ok(()) });
    let settled = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.shutdown(std::time::Duration::MAX),
    )
    .await
    .expect("#1949 the maximal-budget drain must settle within the guard");
    settled.expect("#1949 a maximal drain budget must settle the drain itself");
}

/// #1949: a budget that fits the clock but lands within the tick-conversion
/// headroom of its ceiling still gets the waiver, and the drain must settle
/// with its own result (here `Ok`) without panicking the host thread.
/// Behavior anchor, not a regression discriminator (#2501): the 500 ms
/// backoff is far outside the ~999_999 ns band tokio actually panics in
/// (tokio 1.48 `TimeSource::deadline_to_tick`'s unchecked
/// `Duration::from_nanos(999_999)` addition; see `TICK_CONVERSION_HEADROOM`),
/// and the drain site reads its own, later clock, so a pre-fix tree that
/// registered this very deadline through `timeout_at` settles `Ok` too —
/// the binary search below only keeps the budget off the overflowing tier
/// at that later read. The tier routing itself is pinned deterministically
/// by `the_clock_edge_tier_decision_is_deterministic` below, and the panic
/// band by the registration probe beside it.
#[tokio::test]
async fn a_budget_at_the_clock_edge_is_treated_as_the_waiver() {
    let host = native_ws_fixture_with_cleanup(Arc::new(Requests), async { Ok(()) });
    // Binary-search the maximal duration the monotonic clock still admits;
    // 512 rounds exhaust the nanosecond resolution long before they run out.
    let mut lo = std::time::Duration::ZERO;
    let mut hi = std::time::Duration::MAX;
    for _ in 0..512 {
        let mid = lo + (hi - lo) / 2;
        if tokio::time::Instant::now().checked_add(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // Back the maximum off into the middle of the tick-conversion headroom:
    // the drain site reads its clock after this one (the host thread wakes
    // to observe the shutdown), so the exact maximum overflows at that later
    // read, while this budget still lands within the headroom of the
    // ceiling — the tier `TICK_CONVERSION_HEADROOM` treats as the waiver.
    let budget = lo - TICK_CONVERSION_HEADROOM / 2;
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), host.shutdown(budget))
        .await
        .expect("#1949 a clock-edge budget must still settle the drain within the guard");
    settled.expect("#1949 a clock-edge budget is the waiver, so the drain keeps its own result");
}

/// #2501: the clock-edge tier decision is a pure function of one captured
/// clock read, so its boundaries are testable deterministically — the
/// e2e twins above cannot do this: each poll site reads its own, later
/// clock, so their binary searches drift past the exact band, and their
/// 500 ms backoff lands far outside the ~999_999 ns band tokio actually
/// panics in, which is why they stay behavior anchors. Binary-search the
/// maximal budget this very `now` admits: its deadline is the clock's
/// ceiling, inside the [`super::TICK_CONVERSION_HEADROOM`] waiver tier;
/// backed off two seconds it is past that headroom and must map back to a
/// deadline. The same captured `now` makes both boundaries exact — the
/// window the headroom keeps open covers the `+999_999 ns` tokio adds
/// unchecked when converting a deadline to a timer tick, which the probe
/// test below pins directly.
#[test]
fn the_clock_edge_tier_decision_is_deterministic() {
    let now = tokio::time::Instant::now();
    // Binary-search the maximal duration this very `now` still admits; 512
    // rounds exhaust the nanosecond resolution long before they run out.
    let mut lo = std::time::Duration::ZERO;
    let mut hi = std::time::Duration::MAX;
    for _ in 0..512 {
        let mid = lo + (hi - lo) / 2;
        if now.checked_add(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    assert!(
        super::deadline_or_waiver(now, lo).is_none(),
        "#2501 a budget whose deadline lands within the tick-conversion headroom of the clock's ceiling must take the waiver, not a deadline"
    );
    let backed_off = lo - std::time::Duration::from_secs(2);
    assert!(
        super::deadline_or_waiver(now, backed_off).is_some(),
        "#2501 a budget backed off past the tick-conversion headroom must still get its deadline"
    );
}

/// #2501: the mechanism the headroom guards against, measured in-tree
/// instead of asserted by prose. Tokio 1.48 converts a deadline into a
/// timer tick through `TimeSource::deadline_to_tick`'s unchecked
/// `deadline + Duration::from_nanos(999_999)` (`src/runtime/time/source.rs`),
/// so registering a deadline that fits the clock but sits within that band
/// of its ceiling panics — here, at the maximal fitting deadline itself.
/// The inner future must be `pending` for the registration to happen at
/// all: `Timeout::poll` polls the future first and returns on `Ready`
/// without ever polling the delay, which is why the failed-initialization
/// twin above, whose cleanup is `async { Ok(()) }`, never reaches the
/// registration path either. The panic is observed through a spawned
/// task's join error rather than `#[should_panic]` so the assertion
/// messages stay bindable for the negative controls, and the guard timeout
/// turns a non-panicking registration into a marked failure instead of a
/// hang. If tokio ever stops panicking here, this probe fails and the
/// headroom can be revisited; until then it is the measured fact
/// `TICK_CONVERSION_HEADROOM` is sized against.
#[tokio::test]
async fn a_maximal_deadline_panics_on_timer_registration() {
    let now = tokio::time::Instant::now();
    let mut lo = std::time::Duration::ZERO;
    let mut hi = std::time::Duration::MAX;
    for _ in 0..512 {
        let mid = lo + (hi - lo) / 2;
        if now.checked_add(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let registration = tokio::spawn(async move {
        let _ = tokio::time::timeout_at(now + lo, std::future::pending::<()>()).await;
    });
    // The guard bounds the probe: a deadline that no longer lands in the
    // panic band leaves the registration pending forever, so without it a
    // mutant would hang the test instead of failing at this marker.
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), registration)
        .await
        .expect("#2501 the timer-registration probe must settle within the guard");
    let failure = settled.expect_err("#2501 registering the maximal fitting deadline must panic");
    let panic = failure
        .try_into_panic()
        .expect("#2501 the registration failure must be a panic, not a cancellation");
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("#2501 the registration panic carried no message");
    assert!(
        message.contains("overflow when adding duration to instant"),
        "#2501 the registration panic must be the unchecked tick conversion, got {message:?}"
    );
}

/// #1949 fixture: a factory whose creation never settles, so the owner's
/// shutdown() drains its pending creation forever — the host must cut that
/// wait at the owner bound. `entered` flips on the first poll so the test can
/// wait for the worker to be parked inside the factory before it starts the
/// host (see the test above for why that ordering is the whole fixture).
struct PendingCreationFactory {
    entered: Arc<std::sync::atomic::AtomicBool>,
}
impl SessionFactory for PendingCreationFactory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending().await
        })
    }
}
