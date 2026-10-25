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
            "gateway host thread"
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
            "gateway host thread"
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
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn admitted_preflight<'a>(
        &'a self,
        _: &'a JsValue,
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
    assert!(observation.as_mut().poll(&mut context).is_pending());
    text.send_text(r#"{"type":"event"}"#.into());
    assert!(observation.as_mut().poll(&mut context).is_pending());
    text.send_text(r#"{"type":"response"}"#.into());
    assert!(observation.as_mut().poll(&mut context).is_pending());
    ended.store(true, Ordering::SeqCst);
    text.changed();
    assert!(observation.as_mut().poll(&mut context).is_ready());
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
            projector: Arc::new(GatewayAgentEventProjector::default()),
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
            "transport observation did not arrive: {} step {step}",
            case["name"]
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
            "{}: step {step}",
            case["name"]
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
        "{}: registry seq",
        case["name"]
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
        "{}: replay",
        case["name"]
    );
    if case["name"].as_str().unwrap().starts_with("close-")
        && case["name"] != "close-after-end"
        && case["name"] != "close-preserves-pending"
        && case["name"] != "close-after-busy"
    {
        assert_eq!(session.aborts.load(Ordering::SeqCst), 1);
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
        tokio::time::timeout(Duration::from_secs(15), replay_case(case))
            .await
            .expect("gateway case deadlock");
    }
    assert!(selected > 0, "PDRS_GATEWAY_CASE matched no scenario");
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
                    projector: Arc::new(GatewayAgentEventProjector::default()),
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
        "serial timer must respect deadline"
    );
    clock.advance(1);
    timer.cancel(&key); // Cancel a callback already detached from the clock.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "cancel queued serial timer"
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
        "replacement invalidates detached serial timer"
    );
    clock.advance(5);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "replacement timer fires once"
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
        "dispose cancels queued serial timers"
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
        "drop cancels queued serial timers"
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
                    "A7.2-COORDINATOR-DELIVERY-001-R1: emit returned before its own event reached the sink ({text})"
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
        "A7.2-COORDINATOR-DELIVERY-001-R1: every concurrent external emit must be accepted"
    );
    assert!(
        !overlap.load(Ordering::SeqCst),
        "A7.2-COORDINATOR-DELIVERY-001-R1: sink calls must not overlap"
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
            "A7.2-COORDINATOR-DELIVERY-001-R1: event {text} must reach the sink exactly once"
        );
    }
    assert_eq!(
        deliveries.len(),
        CALLERS,
        "A7.2-COORDINATOR-DELIVERY-001-R1: every accepted event must reach the sink exactly once"
    );
    assert!(
        deliveries.iter().all(|(_, thread)| *thread == host_thread),
        "A7.2-COORDINATOR-DELIVERY-001-R1: the sink must run on the serial host thread, not on callers"
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
        "A7.2-COORDINATOR-DELIVERY-001-R1: HostedConnection::notify must run on the serial host thread, not on callers"
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
            "abort fence: {}",
            case["name"]
        );
    }
    assert!(selected > 0, "abort fence selection matched no scenario");
}
