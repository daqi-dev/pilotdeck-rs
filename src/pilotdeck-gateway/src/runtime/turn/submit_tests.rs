use super::super::{
    projector::GatewayAgentEventProjectorOptions,
    publish::{FileRegistry, RegistryFuture},
    submit_host::{
        GatewaySubmitHost, SubmitAdmission, SubmitAttachmentLease, SubmitCwdService,
        SubmitHostServices, SubmitReplacement,
    },
};
use super::*;
use crate::{
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, LiveTurn, SessionFactory, SessionNext,
            SessionOwnerOptions,
        },
    },
    storage::{FileRunRegistry, RunRegistryError},
};
use pilotdeck_protocol::{parse_json, stringify_json};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::watch;

const ORACLE: &str = include_str!("../../../tests/fixtures/turn-submit/oracle.json");
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn json(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
#[derive(Default)]
struct Log {
    host: Mutex<Vec<String>>,
    durable: Mutex<Vec<Value>>,
    registry: Mutex<Vec<Value>>,
    visible: Mutex<Vec<Value>>,
    guard_wire: Mutex<Vec<String>>,
    ended: AtomicBool,
    thrown: Mutex<Option<String>>,
    consumer_hold: Mutex<Option<Arc<Gate>>>,
}
impl Log {
    fn host(&self, value: &str) {
        self.host.lock().unwrap().push(value.into());
    }
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
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|ready| *ready).await;
    }
}
struct Session {
    setup: Value,
    native: Vec<JsValue>,
    submits: AtomicUsize,
    log: Arc<Log>,
    coordinator: Arc<TurnEventCoordinator>,
    accepted: Gate,
    terminal: Gate,
    end: Gate,
}
impl LiveSession for Arc<Session> {
    fn snapshot(&self) -> JsValue {
        if self.setup["snapshotCwdUnavailable"] == true {
            object([])
        } else {
            object([
                ("messages", JsValue::Array(vec![])),
                ("cwd", "/unused".into()),
            ])
        }
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.log.host("session_abort");
        if self.setup["failureGuard"].is_object() {
            self.log
                .durable
                .lock()
                .unwrap()
                .push(json!({"op":"abort", "reason":json(&JsValue::String(reason.clone()))}));
        }
        self.accepted.open();
        self.terminal.open();
        self.end.open();
        if self.setup["sessionAbortFailure"] == true {
            return Err(RuntimeError::new("injected abort failure"));
        }
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.log.host("session_dispose");
            Ok(())
        })
    }
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.log.host("submit");
        if self.setup["input"]["timeoutMs"]
            .as_f64()
            .is_some_and(|v| v > 0.0)
        {
            self.log
                .durable
                .lock()
                .unwrap()
                .push(json!({"op":"execution", "execution":json(field(&options,"execution"))}));
        }
        let submission = self.submits.fetch_add(1, Ordering::SeqCst);
        let starts: Vec<_> = self
            .native
            .iter()
            .enumerate()
            .filter_map(|(i, e)| is(e, "type", "turn_started").then_some(i))
            .collect();
        let start = starts.get(submission).copied().unwrap_or(self.native.len());
        let end = starts
            .get(submission + 1)
            .copied()
            .unwrap_or(self.native.len());
        Ok(Box::new(Stream {
            native: self.native[start..end].to_vec(),
            session: self.clone(),
            input,
            index: 0,
            accepted: false,
        }))
    }
}
struct Stream {
    native: Vec<JsValue>,
    session: Arc<Session>,
    input: JsValue,
    index: usize,
    accepted: bool,
}
impl LiveTurn for Stream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            let session = &self.session;
            if self.index == 1 && !self.accepted {
                self.accepted = true;
                session.log.durable.lock().unwrap().push(json!({"op":"accept_begin", "messages":[{"role":"user","content":[{"type":"text", "text":json(field(&self.input,"text"))}]}]}));
                session.accepted.wait().await;
                session.log.durable.lock().unwrap().push(json!({"op":if session.setup["transcriptFailure"] == true {"accept_failed"} else {"accept_committed"}}));
            }
            if self.index >= 2 && session.setup["transcriptFailure"] != true {
                session.terminal.wait().await;
                if session.setup["pumpFailure"] == true {
                    return Err(RuntimeError::new("injected pump failure"));
                }
            }
            if let Some(event) = self.native.get(self.index) {
                self.index += 1;
                return Ok(Some(event.clone()));
            }
            session.end.wait().await;
            session.coordinator.emit(
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
struct Factory {
    session: Arc<Session>,
    recreate: AtomicBool,
    created: AtomicUsize,
}
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        self.created.fetch_add(1, Ordering::SeqCst);
        let session = Arc::new(self.session.clone());
        Box::pin(async move { Ok(LiveSessionHandle::new(session, None)) })
    }
    fn can_recreate(&self) -> bool {
        self.recreate.load(Ordering::SeqCst)
    }
}
struct TestSnapshots {
    session: Arc<Session>,
}
impl SubmitSnapshots for TestSnapshots {
    fn capture<'a>(
        &'a self,
        input: WorkspaceSnapshotInput,
        pre: bool,
    ) -> SubmitFuture<'a, Result<WorkspaceSnapshotResult, SubmitError>> {
        Box::pin(async move {
            self.session
                .log
                .host(if pre { "snapshot_pre" } else { "snapshot_post" });
            if !pre {
                self.session.log.durable.lock().unwrap().push(json!({"op":"snapshot_post", "input": {"workspaceId": input.workspace_id, "sessionId": input.session_id, "turnId":input.turn_id, "runId":input.run_id, "workspaceDir":input.workspace_dir, "workspaceStable":input.workspace_stable, "failureKind":input.failure_kind, "failureReason":input.failure_reason}}));
            }
            Ok(WorkspaceSnapshotResult {
                snapshot_id: "fixture".into(),
                phase: if pre {
                    crate::storage::SnapshotPhase::PreUser
                } else {
                    crate::storage::SnapshotPhase::PostAgent
                },
                state: if pre && self.session.setup["snapshotFailure"] == true {
                    SnapshotState::Failed
                } else {
                    SnapshotState::Committed
                },
                error: (pre && self.session.setup["snapshotFailure"] == true)
                    .then(|| "injected snapshot failure".into()),
                manifest_path: None,
                failure_marker_path: None,
            })
        })
    }
}
/// Admission windows, as wired by the admission packages; the host windows run
/// through the production [`GatewaySubmitHost`] below.
struct Host {
    session: Arc<Session>,
    sdk: Gate,
    recovery: Gate,
}
impl SubmitAdmission for Host {
    fn normalize<'a>(
        &'a self,
        _: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn route_input<'a>(
        &'a self,
        _: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            if self.session.setup["recovery"] == true {
                self.session.log.host("recovery");
                if self.session.setup["recoveryGate"] == true {
                    self.recovery.wait().await;
                }
                if self.session.setup["recoveryFailure"] == true {
                    let run = RunRef {
                        session_key: string(field(input, "sessionKey")),
                        project_key: None,
                        run_id: completion.run_id().clone(),
                    };
                    let mut event = error_event(
                        &run,
                        "gateway_dialog_recovery_cleanup_failed",
                        "injected recovery failure",
                        true,
                    );
                    set(
                        &mut event,
                        "userHint",
                        "Retry the turn after the Gateway storage is available.".into(),
                    );
                    return Ok(vec![event]);
                }
            }
            Ok(vec![])
        })
    }
    fn observe_event<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn preflight<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn has_sdk_config(&self) -> bool {
        self.session.setup["sdkAvailable"] == true
    }
    fn validate_sdk_config(&self, config: &JsValue) -> Result<(), SubmitError> {
        if is(config, "permissionMode", "wrong") {
            Err(SubmitError::new(
                "INVALID_SDK_SESSION_CONFIG",
                "permissionMode must be acceptEdits or dontAsk.",
            ))
        } else {
            Ok(())
        }
    }
    fn set_sdk_config<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(async {
            self.session.log.host("sdk_config");
            if self.session.setup["sdkGate"] == true {
                self.sdk.wait().await;
            }
            if self.session.setup["sdkFailure"] == true {
                return Err(SubmitError::new(
                    "INVALID_SDK_SESSION_CONFIG",
                    "injected SDK config failure",
                ));
            }
            Ok(self.session.setup["sdkChanged"] == true)
        })
    }
    fn reject_pending(&self, _: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.session.log.host(&format!("reject:{reason}"));
        Ok(())
    }
}
struct Cwd {
    session: Arc<Session>,
}
impl SubmitCwdService for Cwd {
    fn set(&self, _: &JsString, _: &JsString) -> Result<(), SubmitError> {
        self.session.log.host("cwd");
        Ok(())
    }
}
/// The oracle's gateway options, as `SubmitHostServices` ports.
struct Services {
    session: Arc<Session>,
    refreshed: AtomicBool,
    prepare: Gate,
}
impl SubmitHostServices for Services {
    fn workspace_id(&self, _: &JsValue) -> Result<Option<JsString>, SubmitError> {
        Ok(self.session.setup["workspace"].as_str().map(Into::into))
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        (self.session.setup["snapshots"] == true
            && self.session.setup["snapshotsUnavailable"] != true)
            .then(|| {
                Arc::new(TestSnapshots {
                    session: self.session.clone(),
                }) as Arc<dyn SubmitSnapshots>
            })
    }
    fn snapshots_required(&self) -> bool {
        self.session.setup["snapshots"] == true
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        (self.session.setup["cwdUnavailable"] != true).then(|| {
            Arc::new(Cwd {
                session: self.session.clone(),
            }) as Arc<dyn SubmitCwdService>
        })
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            self.session.log.host("refresh");
            self.refreshed.store(true, Ordering::SeqCst);
            if self.session.setup["refreshFailure"] == true {
                Err(SubmitError::new("CONFIG", "transient config error"))
            } else {
                Ok(())
            }
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        let config = if self.refreshed.load(Ordering::SeqCst) {
            self.session
                .setup
                .get("guardAfterRefresh")
                .or_else(|| self.session.setup.get("failureGuard"))
        } else {
            self.session.setup.get("failureGuard")
        };
        config.map(|config| object([("failureGuard", js(config))]))
    }
    fn resolve_attachments<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        let session = self.session.clone();
        Box::pin(async {
            Ok(SubmitAttachmentLease {
                attachments: vec![],
                release: Box::new(move || {
                    Box::pin(async move {
                        session.log.host("lease_release");
                        if session.setup["leaseFailure"] == true {
                            Err(SubmitError::new("LEASE", "injected lease release failure"))
                        } else {
                            Ok(())
                        }
                    })
                }),
            })
        })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        _attachments: Vec<JsValue>,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async {
            self.session.log.host("prepare");
            if self.session.setup["prepareGate"] == true {
                self.prepare.wait().await;
            }
            if self.session.setup["epochFailure"] == true {
                return Err(SubmitError::new("HISTORY_EPOCH", "history epoch changed"));
            }
            Ok(object([("type", "text".into()), ("text", message.clone())]))
        })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async {
            Ok(if self.session.setup["selection"].is_object() {
                js(&self.session.setup["selection"])
            } else {
                object([("source", "default".into())])
            })
        })
    }
    fn record_failure<'a>(
        &'a self,
        _: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async {
            if self.session.setup["failureGuard"].is_object() {
                self.session
                    .log
                    .durable
                    .lock()
                    .unwrap()
                    .push(json!({"op":"status", "status":json(status), "wire":stringify_json(status).unwrap().unwrap()}));
            }
            if self.session.setup["statusRecordFailure"] == true {
                Err(SubmitError::new("RECORD", "injected status record failure"))
            } else {
                Ok(())
            }
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        self.session.log.host("after");
        if self.session.setup["finallyFailure"] == true {
            Err(SubmitError::new("FINALLY", "injected finally failure"))
        } else {
            Ok(())
        }
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        20
    }
}
struct FakeReplacement {
    session: Arc<Session>,
}
impl SubmitReplacement for FakeReplacement {
    fn commit_accepted_input<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsString,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async {
            self.session.log.host("commit");
            if self.session.setup["commitFailure"] == true {
                Err(SubmitError::new("COMMIT", "injected commit failure"))
            } else {
                Ok(())
            }
        })
    }
    fn release_claim(&self, _: &JsString, _: &JsString) {
        self.session.log.host("release");
    }
}
struct Registry {
    port: FileRegistry,
    log: Arc<Log>,
    setup: Value,
    pending: Arc<AtomicUsize>,
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
        self.log
            .registry
            .lock()
            .unwrap()
            .push(json!({"op":"accept","requestMaterial":display(&input.request_material)}));
        if self.setup["registryAcceptFailure"] == true {
            return Box::pin(async {
                Err(RunRegistryError::Invalid(
                    "injected registry accept failure".into(),
                ))
            });
        }
        self.track(self.port.accept(input))
    }
    fn events(&self, run: &RunRef) -> RegistryFuture<Vec<JsValue>> {
        self.log
            .registry
            .lock()
            .unwrap()
            .push(json!({"op":"events"}));
        self.track(self.port.events(run))
    }
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()> {
        self.log
            .registry
            .lock()
            .unwrap()
            .push(json!({"op":"append", "event":json(event)}));
        if self.setup["registryAppendFailure"] == true {
            return Box::pin(async {
                Err(RunRegistryError::Invalid(
                    "injected registry append failure".into(),
                ))
            });
        }
        self.track(self.port.append(run, event))
    }
}
fn context() -> SessionContext {
    SessionContext {
        session_key: "s".into(),
        project_key: Some("p".into()),
        channel_key: "test".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
async fn settle(registry: &Registry) {
    // Every disk operation is explicitly joined. Yielding alone is not an I/O
    // barrier, and comparing only a final transcript would miss early frames.
    for _ in 0..4 {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        while registry.pending.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    }
}
async fn collect(mut stream: TurnSubmission, log: Arc<Log>) {
    loop {
        match stream.next().await {
            Ok(Some(event)) => {
                let timeout_error =
                    is(&event, "type", "error") && is(&event, "code", "turn_timeout");
                log.guard_wire
                    .lock()
                    .unwrap()
                    .push(stringify_json(&event).unwrap().unwrap());
                log.visible.lock().unwrap().push(json(&event));
                let hold = if timeout_error {
                    log.consumer_hold.lock().unwrap().clone()
                } else {
                    None
                };
                if let Some(gate) = hold {
                    gate.wait().await;
                }
            }
            Ok(None) => break,
            Err(error) => {
                *log.thrown.lock().unwrap() = Some(error.message);
                break;
            }
        }
    }
    log.ended.store(true, Ordering::SeqCst);
}
async fn replay_case(case: &Value) {
    let name = case["name"].as_str().unwrap();
    let controlled_clock = case["steps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "timeout");
    if controlled_clock {
        tokio::time::pause();
    }
    let root = std::env::temp_dir().join(format!("pdrs-submit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let log = Arc::new(Log::default());
    let setup = &case["setup"];
    if setup["holdTimeoutConsumer"] == true {
        *log.consumer_hold.lock().unwrap() = Some(Arc::new(Gate::new()));
    }
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let session = Arc::new(Session {
        setup: setup.clone(),
        native: case["native"].as_array().unwrap().iter().map(js).collect(),
        submits: AtomicUsize::new(0),
        log: log.clone(),
        coordinator: coordinator.clone(),
        accepted: Gate::new(),
        terminal: Gate::new(),
        end: Gate::new(),
    });
    let factory = Arc::new(Factory {
        session: session.clone(),
        recreate: AtomicBool::new(false),
        created: AtomicUsize::new(0),
    });
    let mut options = SessionOwnerOptions::new(factory.clone());
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    owner.get_or_create(context()).await.unwrap();
    if setup["busy"] == true {
        assert!(owner.begin_turn(&"s".into(), &"other".into()).is_some());
    }
    let registry = Arc::new(Registry {
        port: FileRegistry::new(FileRunRegistry::open(root.join("registry.json")).unwrap())
            .unwrap(),
        log: log.clone(),
        setup: setup.clone(),
        pending: Arc::default(),
    });
    let services = Arc::new(Services {
        session: session.clone(),
        refreshed: AtomicBool::new(false),
        prepare: Gate::new(),
    });
    let admission = Arc::new(Host {
        session: session.clone(),
        sdk: Gate::new(),
        recovery: Gate::new(),
    });
    let host = Arc::new(GatewaySubmitHost::new(
        services.clone(),
        Arc::new(FakeReplacement {
            session: session.clone(),
        }),
        admission.clone(),
    ));
    let submitter = Arc::new(TurnSubmitter {
        owner: owner.clone(),
        coordinator,
        registry: if setup["noRegistry"] == true {
            None
        } else {
            Some(registry.clone())
        },
        host: host.clone(),
        workspace_admissions: WorkspaceAdmissions::default(),
        projector: Arc::new(GatewayAgentEventProjector::new(
            GatewayAgentEventProjectorOptions::default(),
        )),
    });
    if setup["workspaceBusy"] == true {
        let workspace = JsString::from(setup["workspace"].as_str().unwrap());
        assert!(submitter.workspace_admissions.reserve(
            &workspace,
            &RunRef {
                session_key: "other".into(),
                project_key: None,
                run_id: "other".into()
            }
        ));
    }
    let mut input = json!({"sessionKey":"s","projectKey":"p","runId":"r","message":"hello","channelKey":"test"});
    if let Some(fields) = setup["input"].as_object() {
        for (key, value) in fields {
            input[key] = value.clone();
        }
    }
    let stream = submitter.submit(js(&input));
    let consumer = tokio::spawn(collect(stream, log.clone()));
    let mut abort = None;
    let mut close = None;
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        match step.as_str().unwrap() {
            "observe" => {}
            "accept" => session.accepted.open(),
            "terminal" => session.terminal.open(),
            "end" => session.end.open(),
            "prepare" => services.prepare.open(),
            "sdk" => admission.sdk.open(),
            "recovery" => admission.recovery.open(),
            "resume" => log.consumer_hold.lock().unwrap().as_ref().unwrap().open(),
            "timeout" => {
                tokio::time::advance(Duration::from_millis(1000)).await;
            }
            "abort" => {
                let owner = owner.clone();
                abort = Some(tokio::spawn(async move {
                    owner
                        .abort_turn(&"s".into(), Some(&"r".into()), Some(&"stop".into()))
                        .await
                        .unwrap();
                }));
            }
            "close" => {
                close = Some(tokio::spawn(owner.close(&"s".into())));
            }
            "supersede" => {
                owner.begin_completion(&"s".into(), &"replacement".into());
            }
            "second" => {
                let mut second = input.clone();
                second["runId"] = "r2".into();
                let mut stream = submitter.submit(js(&second));
                while let Some(event) = stream.next().await.unwrap() {
                    log.guard_wire
                        .lock()
                        .unwrap()
                        .push(stringify_json(&event).unwrap().unwrap());
                    log.visible.lock().unwrap().push(json(&event));
                }
            }
            other => panic!("unknown fixture step: {other}"),
        }
        settle(&registry).await;
        let expected = &case["results"][index];
        // Dirty is observed via a real recreation below, independently of the
        // SDK setter's return. Do not mirror the implementation in its mock.
        let mut expected = expected.clone();
        expected["hostCalls"]
            .as_array_mut()
            .unwrap()
            .retain(|v| v != "sdk_dirty");
        let mut actual = json!({"visible":*log.visible.lock().unwrap(), "hostCalls":*log.host.lock().unwrap(), "durableCalls":*log.durable.lock().unwrap(), "registryCalls":*log.registry.lock().unwrap(), "active":owner.has_active_turn(&"s".into()), "ended":log.ended.load(Ordering::SeqCst), "thrown":*log.thrown.lock().unwrap()});
        if setup["failureGuard"].is_object() {
            actual["guardWire"] = json!(*log.guard_wire.lock().unwrap());
        }
        if case["capabilityOnly"] == true {
            assert!(
                actual["visible"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["code"] == "CAPABILITY_UNAVAILABLE"),
                "assertion failed: {name}: explicit capability refusal"
            );
            assert_eq!(actual["active"], false, "{name}: admission released");
            assert_eq!(actual["ended"], true, "{name}: refusal settled");
            assert!(
                !actual["hostCalls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c == "submit"),
                "{name}: no native submit"
            );
        } else {
            assert_eq!(actual, expected, "{name}: step {index} {step}");
        }
    }
    consumer.await.unwrap();
    if let Some(abort) = abort {
        abort.await.unwrap();
    }
    if let Some(close) = close {
        close.await.unwrap().unwrap();
    }
    // Reopen after all FIFO appends have drained, to compare persisted entries
    // rather than a second in-memory FileRunRegistry cache.
    let reopened = FileRunRegistry::open(root.join("registry.json")).unwrap();
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: "r".into(),
    };
    if case["capabilityOnly"] == true {
        assert!(
            reopened
                .events(&run, None, None)
                .unwrap()
                .iter()
                .any(|e| is(field(e, "event"), "code", "CAPABILITY_UNAVAILABLE")),
            "{name}: durable capability refusal"
        );
    } else {
        assert_eq!(
            reopened
                .events(&run, None, None)
                .unwrap()
                .iter()
                .map(json)
                .collect::<Vec<_>>(),
            case["registryEvents"].as_array().unwrap().clone(),
            "{name}: durable events"
        );
        let record = reopened.get(&run).unwrap();
        let actual_record = record.map(|record| {
        let record = json(record.as_json());
        let mut value = json!({"state":record["state"],"lastSeq":record["lastSeq"],"revision":record["revision"]});
        if let Some(result) = record.get("result") { value["result"] = result.clone(); }
        value
    }).unwrap_or(Value::Null);
        assert_eq!(actual_record, case["record"], "{name}: persisted run state");
    }
    if setup["sdkChanged"] == true && setup["workspaceBusy"] != true {
        factory.recreate.store(true, Ordering::SeqCst);
        owner.get_or_create(context()).await.unwrap();
        assert_eq!(
            factory.created.load(Ordering::SeqCst),
            2,
            "changed SDK config must dirty the cached handle"
        );
    }
    owner.shutdown().await.unwrap();
    drop(submitter);
    drop(host);
    drop(registry);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
    if controlled_clock {
        tokio::time::resume();
    }
}

#[tokio::test]
async fn real_ts_submit_scenarios() {
    let mut oracle = crate::test_baseline::oracle(ORACLE);
    // serde_json rejects isolated UTF-16 surrogates; the separate protocol
    // replay below compares those cases without transcoding to UTF-8.
    if let JsValue::Object(value) = &mut oracle {
        value.insert("validationProbes".into(), JsValue::Undefined);
    }
    let oracle = json(&oracle);
    let mut selected = 0;
    for case in oracle["cases"].as_array().unwrap() {
        if std::env::var("PDRS_SUBMIT_CASE")
            .ok()
            .is_none_or(|name| name == case["name"].as_str().unwrap())
        {
            selected += 1;
            tokio::time::timeout(Duration::from_secs(20), replay_case(case))
                .await
                .unwrap_or_else(|_| panic!("{} deadlocked", case["name"]));
        }
    }
    assert!(selected > 0, "PDRS_SUBMIT_CASE matched no scenario");
}
#[tokio::test]
async fn unavailable_capabilities_are_explicitly_refused() {
    let mut oracle = crate::test_baseline::oracle(ORACLE);
    set(&mut oracle, "validationProbes", JsValue::Undefined);
    let oracle = json(&oracle);
    let base = &oracle["cases"][0];
    let mut selected = 0;
    for (name, setup) in [
        (
            "snapshot-recorder-unavailable",
            json!({"workspace":"w", "snapshots":true, "snapshotsUnavailable":true}),
        ),
        (
            "snapshot-cwd-unavailable",
            json!({"workspace":"w", "snapshots":true, "snapshotCwdUnavailable":true}),
        ),
        (
            "cwd-host-unavailable",
            json!({"cwdUnavailable":true, "input":{"workspaceCwd":"/work"}}),
        ),
    ] {
        if std::env::var("PDRS_SUBMIT_CAPABILITY")
            .ok()
            .is_some_and(|v| v != name)
        {
            continue;
        }
        selected += 1;
        let mut case = base.clone();
        case["name"] = name.into();
        case["setup"] = setup;
        case["steps"] = json!(["observe"]);
        case["capabilityOnly"] = true.into();
        tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
            .await
            .unwrap();
    }
    assert!(selected > 0, "PDRS_SUBMIT_CAPABILITY matched no scenario");
}

#[test]
#[ignore = "requires real fixed TS baseline and Node 22"]
fn real_ts_turn_submit_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-submit");
}

#[test]
fn real_ts_utf16_preflight() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let JsValue::Array(probes) = field(&oracle, "validationProbes") else {
        panic!("missing TS probes");
    };
    for probe in probes {
        let input = field(probe, "input");
        let JsValue::Array(events) = field(probe, "events") else {
            panic!("missing TS events");
        };
        if field(input, "busy") == &JsValue::Bool(true) {
            let message = busy_message(&string(field(input, "sessionKey")));
            assert_eq!(
                &message,
                field(field(&events[0], "detail"), "message"),
                "UTF-16 busy status message"
            );
            assert_eq!(&message, field(&events[1], "message"));
        } else {
            let event = validate(input).expect("TS rejected invalid permission mode");
            for key in ["type", "code", "message", "recoverable"] {
                assert_eq!(field(&event, key), field(&events[0], key), "{key}");
            }
        }
    }
}
