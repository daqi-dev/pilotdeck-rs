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
    parse_json(&value.to_string()).expect("#748 submit fixture prerequisite")
}
fn json(value: &JsValue) -> Value {
    serde_json::from_str(
        &stringify_json(value)
            .expect("#748 submit fixture prerequisite")
            .expect("#748 submit fixture prerequisite"),
    )
    .expect("#748 submit fixture prerequisite")
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
    /// Observed terminal-writer counts, recorded by interleaving steps.
    observations: Mutex<Vec<usize>>,
}
impl Log {
    fn host(&self, value: &str) {
        self.host
            .lock()
            .expect("#748 submit fixture prerequisite")
            .push(value.into());
    }
}
#[derive(Clone)]
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
    status_recorder: Option<Arc<crate::storage::recorder::Recorder>>,
    native: Vec<JsValue>,
    submits: AtomicUsize,
    log: Arc<Log>,
    coordinator: Arc<TurnEventCoordinator>,
    accepted: Gate,
    terminal: Gate,
    end: Gate,
    /// Set by `abort`, so a stream can surface the pump error a takeover causes.
    aborted: AtomicBool,
    /// Holds the timeout's durable terminal write (#727 interleaving cell 1).
    failure_gate: Gate,
    /// Holds the pump error until the fence has completed (#727 cell 2).
    pump_error_gate: Gate,
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
        self.aborted.store(true, Ordering::SeqCst);
        if self.setup["failureGuard"].is_object() {
            self.log
                .durable
                .lock()
                .expect("#748 submit fixture prerequisite")
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
        if self.setup["pumpPanic"] == true {
            // #2498: the worker task dies before its own `finish`, so the
            // manage arm's JoinError branch owns the delivery close.
            panic!("#2498 injected worker panic");
        }
        if self.setup["input"]["timeoutMs"]
            .as_f64()
            .is_some_and(|v| v > 0.0)
        {
            self.log
                .durable
                .lock()
                .expect("#748 submit fixture prerequisite")
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
                session.log.durable.lock().expect("#748 submit fixture prerequisite").push(json!({"op":"accept_begin", "messages":[{"role":"user","content":[{"type":"text", "text":json(field(&self.input,"text"))}]}]}));
                session.accepted.wait().await;
                session.log.durable.lock().expect("#748 submit fixture prerequisite").push(json!({"op":if session.setup["transcriptFailure"] == true {"accept_failed"} else {"accept_committed"}}));
            }
            if self.index >= 2 && session.setup["transcriptFailure"] != true {
                session.terminal.wait().await;
                if session.setup["pumpErrorGate"] == true {
                    session.pump_error_gate.wait().await;
                }
                if session.setup["pumpFailure"] == true {
                    return Err(RuntimeError::new("injected pump failure"));
                }
                // A live session aborted by the deadline surfaces as a pump
                // error, exactly like the real sidecar exiting after the
                // timeout cancel.
                if session.setup["abortPumpFailure"] == true
                    && session.aborted.load(Ordering::SeqCst)
                {
                    session.log.host("pump_error");
                    return Err(RuntimeError::new(
                        "timeout takeover aborted the live session",
                    ));
                }
            }
            if let Some(event) = self.native.get(self.index) {
                if event.get("type") == Some(&"turn_completed".into())
                    && session.setup["statusRecordGate"] == true
                {
                    session.status_recorder.as_ref().expect("#748 status probe recorder")
                        .complete_turn(&"s".into(), &"r".into(), js(&json!({"type":"success","sessionId":"s","turnId":"r","stopReason":"completed","turns":1})))
                        .expect("#748 native terminal transcript");
                }
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
                self.session.log.durable.lock().expect("#748 submit fixture prerequisite").push(json!({"op":"snapshot_post", "input": {"workspaceId": input.workspace_id, "sessionId": input.session_id, "turnId":input.turn_id, "runId":input.run_id, "workspaceDir":input.workspace_dir, "workspaceStable":input.workspace_stable, "failureKind":input.failure_kind, "failureReason":input.failure_reason}}));
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
        _input: &'a JsValue,
        run: &'a RunRef,
        _completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            if self.session.setup["recovery"] == true {
                self.session.log.host("recovery");
                if self.session.setup["recoveryGate"] == true {
                    self.recovery.wait().await;
                }
                if self.session.setup["recoveryFailure"] == true {
                    let mut event = error_event(
                        run,
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
        _: &'a RunRef,
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
            // #2498: this gated record is a *cancellable* `Gate::wait()`
            // future — when a budget drops the await, the gate future is
            // simply cancelled and its continuation never runs. Production
            // `record_failure` (native_process/services.rs) dispatches a
            // `spawn_blocking` task that cannot be cancelled: its durable
            // write keeps running and can land after the budget and the
            // close. The #2498 cells therefore prove the wire side (no
            // terminal frame enqueues past the budget) and the bounded
            // close; the durable late write is registered separately in
            // specs/gateway-parity/diffs/A7.2-TURN-PANIC-FINISH-001-R3.md.
            if self.session.setup["failureGate"] == true {
                self.session.log.host("failure_enter");
                self.session.failure_gate.wait().await;
            }
            if self.session.setup["failureGuard"].is_object() {
                self.session
                    .log
                    .durable
                    .lock()
                    .expect("#748 submit fixture prerequisite")
                    .push(json!({"op":"status", "status":json(status), "wire":stringify_json(status).expect("#748 submit fixture prerequisite").expect("#748 submit fixture prerequisite")}));
            }
            if self.session.setup["statusRecordGate"] == true
                && field(status, "event") == &JsValue::from("context_budget")
            {
                self.session.log.host("status_waiting");
                let _owned = StatusProbeDrop(self.session.log.clone());
                self.prepare.wait().await;
                self.session
                    .status_recorder
                    .as_ref()
                    .expect("#748 status probe recorder")
                    .writer()
                    .record_agent_status_message(&"s".into(), &"r".into(), status)
                    .expect("#748 delayed status transcript");
                self.session.log.host("status_released");
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
    /// #2498: holds every append behind a step-opened gate, so the panic
    /// arm's bounded drain is provably parked on in-flight writes.
    append_gate: Gate,
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
            .expect("#748 submit fixture prerequisite")
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
            .expect("#748 submit fixture prerequisite")
            .push(json!({"op":"events"}));
        self.track(self.port.events(run))
    }
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()> {
        self.log
            .registry
            .lock()
            .expect("#748 submit fixture prerequisite")
            .push(json!({"op":"append", "event":json(event)}));
        if self.setup["registryAppendFailure"] == true {
            return Box::pin(async {
                Err(RunRegistryError::Invalid(
                    "injected registry append failure".into(),
                ))
            });
        }
        if self.setup["registryAppendStuck"] == true {
            // #2498 fix-r1: an append whose future never settles and never
            // reaches the registry worker — the never-settling host append
            // the consumer's trailing wait must not wait for. Untracked on
            // purpose: the post-step settle joins only writes that complete.
            return Box::pin(std::future::pending());
        }
        if self.setup["registryAppendGate"] == true {
            let gate = self.append_gate.clone();
            let future = self.port.append(run, event);
            return self.track(Box::pin(async move {
                gate.wait().await;
                future.await
            }));
        }
        self.track(self.port.append(run, event))
    }
}
fn context() -> SessionContext {
    SessionContext {
        session_key: "s".into(),
        session_id: "s".into(),
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
/// Yield until no task can make progress without a gate: the deterministic
/// barrier used by the #727 interleaving cells. A paused clock means time
/// never advances here, so only ready tasks run.
async fn quiesce(log: &Log) {
    let mut last = usize::MAX;
    let mut stable = 0;
    for _ in 0..4096 {
        tokio::task::yield_now().await;
        let size = log
            .host
            .lock()
            .expect("#748 submit fixture prerequisite")
            .len();
        if size == last {
            stable += 1;
            if stable >= 32 {
                return;
            }
        } else {
            stable = 0;
            last = size;
        }
    }
    panic!("#727 submit fixture never quiesced");
}
fn host_count(log: &Log, call: &str) -> usize {
    log.host
        .lock()
        .expect("#748 submit fixture prerequisite")
        .iter()
        .filter(|entry| *entry == call)
        .count()
}
/// A terminal wire frame is the error the gateway publishes for the one
/// terminal status (`turn_timeout` or `gateway_submit_failed`). Status frames
/// are not counted twice: they carry the same code as an `agent_status`.
fn terminal_wire_codes(log: &Log) -> Vec<String> {
    log.visible
        .lock()
        .expect("#748 submit fixture prerequisite")
        .iter()
        .filter(|event| event["type"] == "error")
        .filter_map(|event| event["code"].as_str().map(str::to_string))
        .filter(|code| code == "turn_timeout" || code == "gateway_submit_failed")
        .collect()
}
/// The failure frames the #2498 panic arm enqueues behind the registry gate:
/// the failure status (`agent_status` carries the code as `event`) and the
/// terminal error frame (carries it as `code`).
fn failure_appends(log: &Log) -> usize {
    log.registry
        .lock()
        .expect("#748 submit fixture prerequisite")
        .iter()
        .filter(|entry| entry["op"] == "append")
        .filter(|entry| {
            entry["event"]["code"] == "gateway_submit_failed"
                || entry["event"]["event"] == "gateway_submit_failed"
        })
        .count()
}
/// Wait until the panicked arm has enqueued both failure frames behind the
/// registry append gate, then let it park inside its bounded drain. The bound
/// is wall-clock, not a spin count or a tokio timer: the fixture registry
/// runs its FIFO jobs on a real OS thread, so a paused tokio clock cannot
/// bound the accept I/O that precedes the panic, while a genuine regression
/// still fails at the bound instead of hanging.
async fn await_panic_failure_records(log: &Log) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while failure_appends(log) < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "#2498 the panicked arm never recorded its failure frames (appends={}, registry={:?})",
            failure_appends(log),
            log.registry
                .lock()
                .expect("#748 submit fixture prerequisite")
                .len(),
        );
        tokio::task::yield_now().await;
    }
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}
/// #2498: spin — wall-clock bounded, like `await_panic_failure_records` —
/// until the panic arm has closed the whole turn: the stream ended,
/// admission released and the host's after-turn hook ran. A missing close
/// is this assertion, never a hang, and the bound is wall clock because the
/// registry worker is a real OS thread a paused tokio clock cannot bound.
async fn await_panic_closure(owned_probe: impl Fn() -> bool, name: &str, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !owned_probe() {
        assert!(std::time::Instant::now() < deadline, "#2498 {name}: {what}");
        tokio::task::yield_now().await;
    }
}
/// The closure probe shared by every #2498 panic cell: the turn stream
/// ended, the admission is released and `after_turn` has run.
fn panic_turn_closed(log: &Log, owner: &SessionOwner) -> bool {
    log.ended.load(Ordering::SeqCst)
        && !owner.has_active_turn(&"s".into())
        && host_count(log, "after") > 0
}
async fn collect(mut stream: TurnSubmission, log: Arc<Log>) {
    loop {
        match stream.next().await {
            Ok(Some(event)) => {
                let timeout_error =
                    is(&event, "type", "error") && is(&event, "code", "turn_timeout");
                log.guard_wire
                    .lock()
                    .expect("#748 submit fixture prerequisite")
                    .push(
                        stringify_json(&event)
                            .expect("#748 submit fixture prerequisite")
                            .expect("#748 submit fixture prerequisite"),
                    );
                log.visible
                    .lock()
                    .expect("#748 submit fixture prerequisite")
                    .push(json(&event));
                let hold = if timeout_error {
                    log.consumer_hold
                        .lock()
                        .expect("#748 submit fixture prerequisite")
                        .clone()
                } else {
                    None
                };
                if let Some(gate) = hold {
                    gate.wait().await;
                }
            }
            Ok(None) => break,
            Err(error) => {
                *log.thrown.lock().expect("#748 submit fixture prerequisite") = Some(error.message);
                break;
            }
        }
    }
    log.ended.store(true, Ordering::SeqCst);
}
async fn replay_case(case: &Value) {
    let name = case["name"]
        .as_str()
        .expect("#748 submit fixture prerequisite");
    // Order-only cases (inline, not oracle-replayed) still drive the full
    // turn; they skip the byte-for-byte per-step comparison because their
    // expected values are not part of the frozen fixture.
    let order_only = case["orderOnly"] == true;
    let controlled_clock = case["steps"]
        .as_array()
        .expect("#748 submit fixture prerequisite")
        .iter()
        .any(|s| {
            s == "timeout"
                || s.as_str() == Some("panic-drain")
                || s.as_str() == Some("panic-budget")
                || s.as_str() == Some("panic-close")
                || s.as_str() == Some("panic-stuck")
                || s.as_str() == Some("panic-record-stuck")
                || s.as_str() == Some("panic-record-slow-release")
                || s.as_str() == Some("timer-record-stuck")
        });
    if controlled_clock {
        tokio::time::pause();
    }
    let root = std::env::temp_dir().join(format!("pdrs-submit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("#748 submit fixture prerequisite");
    let log = Arc::new(Log::default());
    let setup = &case["setup"];
    if setup["holdTimeoutConsumer"] == true {
        *log.consumer_hold
            .lock()
            .expect("#748 submit fixture prerequisite") = Some(Arc::new(Gate::new()));
    }
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let session = Arc::new(Session {
        setup: setup.clone(),
        status_recorder: if setup["statusRecordGate"] == true {
            let recorder = Arc::new(crate::storage::recorder::Recorder::new(
                Arc::new(crate::storage::JsonlTranscriptStore::new()),
                crate::storage::TranscriptKey {
                    project_root: root.clone(),
                    pilot_home: root.join("home"),
                    transcript_path: root.join("status-transcript.jsonl"),
                    session_id: "s".into(),
                },
            ));
            recorder
                .start_turn(&"s".into(), &"r".into())
                .expect("#748 status probe start");
            Some(recorder)
        } else {
            None
        },
        native: case["native"]
            .as_array()
            .expect("#748 submit fixture prerequisite")
            .iter()
            .map(js)
            .collect(),
        submits: AtomicUsize::new(0),
        log: log.clone(),
        coordinator: coordinator.clone(),
        accepted: Gate::new(),
        terminal: Gate::new(),
        end: Gate::new(),
        aborted: AtomicBool::new(false),
        failure_gate: Gate::new(),
        pump_error_gate: Gate::new(),
    });
    let factory = Arc::new(Factory {
        session: session.clone(),
        recreate: AtomicBool::new(false),
        created: AtomicUsize::new(0),
    });
    let mut options = SessionOwnerOptions::new(factory.clone());
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    owner
        .get_or_create(context())
        .await
        .expect("#748 submit fixture prerequisite");
    if setup["busy"] == true {
        assert!(owner.begin_turn(&"s".into(), &"other".into()).is_some());
    }
    let registry = Arc::new(Registry {
        port: FileRegistry::new(
            FileRunRegistry::open(root.join("registry.json"))
                .expect("#748 submit fixture prerequisite"),
        )
        .expect("#748 submit fixture prerequisite"),
        log: log.clone(),
        setup: setup.clone(),
        pending: Arc::default(),
        append_gate: Gate::new(),
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
        projector: Arc::new(
            GatewayAgentEventProjector::new(GatewayAgentEventProjectorOptions::default())
                .expect("#879 default projector"),
        ),
    });
    if setup["workspaceBusy"] == true {
        let workspace = JsString::from(
            setup["workspace"]
                .as_str()
                .expect("#748 submit fixture prerequisite"),
        );
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
    for (index, step) in case["steps"]
        .as_array()
        .expect("#748 submit fixture prerequisite")
        .iter()
        .enumerate()
    {
        match step.as_str().expect("#748 submit fixture prerequisite") {
            "observe" => {}
            "status-enter" => {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !log
                        .host
                        .lock()
                        .expect("#748 context gate log")
                        .iter()
                        .any(|call| call == "status_waiting")
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("#748 context gate entered");
                crate::negative_assert!(
                    !status_probe_types(&root)
                        .iter()
                        .any(|kind| kind == "turn_result"),
                    "#748 context status must settle before native terminal"
                );
            }
            "status-release" => services.prepare.open(),
            "status-deadline" => {
                tokio::time::advance(Duration::from_millis(21)).await;
                settle(&registry).await;
                crate::negative_assert!(
                    log.host
                        .lock()
                        .expect("#748 context deadline log")
                        .iter()
                        .any(|call| call == "status_dropped"),
                    "#748 context status wait ends at abort budget"
                );
            }
            "accept" => session.accepted.open(),
            "terminal" => session.terminal.open(),
            "end" => session.end.open(),
            "prepare" => services.prepare.open(),
            "sdk" => admission.sdk.open(),
            "recovery" => admission.recovery.open(),
            "resume" => log
                .consumer_hold
                .lock()
                .expect("#748 submit fixture prerequisite")
                .as_ref()
                .expect("#748 submit fixture prerequisite")
                .open(),
            "timeout" => {
                tokio::time::advance(Duration::from_millis(1000)).await;
            }
            // #727 cell 1: quiesce while the timeout's durable write is still
            // gated, and record how many terminal writers reached it. The pump
            // error caused by the abort must not be one of them.
            "single-terminal-writer" => {
                // Wait for the takeover-induced pump error to actually surface
                // first; otherwise the count below could be taken before the
                // losing writer has had a chance to reach the gate.
                let mut spins = 0;
                while host_count(&log, "pump_error") == 0 {
                    tokio::task::yield_now().await;
                    spins += 1;
                    assert!(
                        spins < 10_000,
                        "#727 the takeover-induced pump error never surfaced"
                    );
                }
                quiesce(&log).await;
                log.observations
                    .lock()
                    .expect("#748 submit fixture prerequisite")
                    .push(host_count(&log, "failure_enter"));
            }
            "failure-release" => session.failure_gate.open(),
            // #727 cell 2: hold the pump error until the turn has finished, i.e.
            // until `run` returned and `cleanup.release()` completed the
            // completion fence. This is the ordering the TS baseline reaches
            // naturally (its catch runs after the fence); the takeover must stay
            // single-terminal here too, without relying on that ordering.
            "fence-complete" => {
                // The takeover waits up to `abort_turn_timeout_ms` for the pump
                // to settle; the paused clock only moves on explicit advances.
                tokio::time::advance(Duration::from_millis(21)).await;
                settle(&registry).await;
                crate::negative_assert!(
                    log.ended.load(Ordering::SeqCst),
                    "#727 the turn must complete its fence before the late pump error"
                );
            }
            "pump-error-release" => session.pump_error_gate.open(),
            "abort" => {
                let owner = owner.clone();
                abort = Some(tokio::spawn(async move {
                    owner
                        .abort_turn(&"s".into(), Some(&"r".into()), Some(&"stop".into()))
                        .await
                        .expect("#748 submit fixture prerequisite");
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
                while let Some(event) = stream
                    .next()
                    .await
                    .expect("#748 submit fixture prerequisite")
                {
                    log.guard_wire
                        .lock()
                        .expect("#748 submit fixture prerequisite")
                        .push(
                            stringify_json(&event)
                                .expect("#748 submit fixture prerequisite")
                                .expect("#748 submit fixture prerequisite"),
                        );
                    log.visible
                        .lock()
                        .expect("#748 submit fixture prerequisite")
                        .push(json(&event));
                }
            }
            // #2498 cells: the pump panics inside the worker, so the manage
            // arm's JoinError branch owns the failure record and the delivery
            // close. The failure appends stay behind the registry gate until
            // this step, so the arm's bounded drain is provably parked on them
            // before the clock moves; `settle` after the step then joins the
            // released writes.
            "panic-drain" => {
                await_panic_failure_records(&log).await;
                registry.append_gate.open();
            }
            "panic-budget" => {
                await_panic_failure_records(&log).await;
                tokio::time::advance(Duration::from_millis(500)).await;
                registry.append_gate.open();
            }
            "panic-close" => {
                await_panic_failure_records(&log).await;
                tokio::time::advance(Duration::from_millis(1100)).await;
                // #2498 fix-r1: the expired budget closes delivery before
                // the appends settle — assert the closure while the appends
                // are still gated. An acceptance that only runs after the
                // gate opens cannot tell the hard close from the appends
                // eventually settling (the shape the r1 review rejected).
                await_panic_closure(
                    || panic_turn_closed(&log, &owner),
                    name,
                    "the hard close must end the turn while the failure appends are still gated",
                )
                .await;
                registry.append_gate.open();
            }
            // #2498 fix-r1 cells: the gate never opens at all — the failure
            // appends are stuck host futures — and the turn must still close
            // within the abort budget. The registry pending counter is not
            // held (the stuck appends bypass `track`), so the post-step
            // settle below cannot hang on them.
            "panic-stuck" => {
                await_panic_failure_records(&log).await;
                tokio::time::advance(Duration::from_millis(1100)).await;
            }
            // #2498 fix-r1: the durable failure record itself hangs — a stuck
            // host future that, before the fix, sat ahead of the budget so
            // the close never even started. Wait for the record to enter
            // its gate, let the budget expire, and require the close.
            "panic-record-stuck" => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while host_count(&log, "failure_enter") == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "#2498 {name}: the panicked arm never entered the stuck failure record (host={:?})",
                        log.host.lock().expect("#748 submit fixture prerequisite")
                    );
                    tokio::task::yield_now().await;
                }
                tokio::time::advance(Duration::from_millis(1100)).await;
            }
            // #2498 fix-r2: the failure record settles, but only after the
            // budget — the third cell the shared budget introduces. The close
            // must happen while the record is still gated, and releasing the
            // record afterwards must not enqueue its frames: the budget
            // abandoned the record, so the terminal frames never enter the
            // queue (the wire assertion below pins the empty queue).
            "panic-record-slow-release" => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while host_count(&log, "failure_enter") == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "#2498 {name}: the panicked arm never entered the slow failure record (host={:?})",
                        log.host.lock().expect("#748 submit fixture prerequisite")
                    );
                    tokio::task::yield_now().await;
                }
                tokio::time::advance(Duration::from_millis(1100)).await;
                await_panic_closure(
                    || panic_turn_closed(&log, &owner),
                    name,
                    "the hard close must end the turn while the slow failure record is still gated",
                )
                .await;
                session.failure_gate.open();
            }
            // #2498 fix-r2: the timer winner's durable `turn_timeout` record
            // hangs — before the fix the manage arm parked on it ahead of the
            // settlement and the `close_live`, the same shape the panic arm
            // had before fix-r1 (and the same shape TS exhibits: its
            // `settleTimeout` awaits the record and the pump finally awaits
            // that settlement). The record now shares the panic arm's abort
            // budget; the stuck record is abandoned at the budget and the
            // close ends the turn with no terminal frame on the wire.
            "timer-record-stuck" => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while host_count(&log, "failure_enter") == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "#2498 {name}: the timer arm never entered the stuck failure record (host={:?})",
                        log.host.lock().expect("#748 submit fixture prerequisite")
                    );
                    tokio::task::yield_now().await;
                }
                tokio::time::advance(Duration::from_millis(1100)).await;
                await_panic_closure(
                    || panic_turn_closed(&log, &owner),
                    name,
                    "the timer arm must close the turn without waiting for the stuck failure record",
                )
                .await;
            }
            other => panic!("unknown fixture step: {other}"),
        }
        settle(&registry).await;
        let expected = &case["results"][index];
        // Dirty is observed via a real recreation below, independently of the
        // SDK setter's return. Do not mirror the implementation in its mock.
        let mut expected = expected.clone();
        if let Some(calls) = expected["hostCalls"].as_array_mut() {
            calls.retain(|v| v != "sdk_dirty");
        }
        let mut actual = json!({"visible":*log.visible.lock().expect("#748 submit fixture prerequisite"), "hostCalls":*log.host.lock().expect("#748 submit fixture prerequisite"), "durableCalls":*log.durable.lock().expect("#748 submit fixture prerequisite"), "registryCalls":*log.registry.lock().expect("#748 submit fixture prerequisite"), "active":owner.has_active_turn(&"s".into()), "ended":log.ended.load(Ordering::SeqCst), "thrown":*log.thrown.lock().expect("#748 submit fixture prerequisite")});
        if setup["failureGuard"].is_object() {
            actual["guardWire"] = json!(
                *log.guard_wire
                    .lock()
                    .expect("#748 submit fixture prerequisite")
            );
        }
        if case["capabilityOnly"] == true {
            assert!(
                actual["visible"]
                    .as_array()
                    .expect("#748 submit fixture prerequisite")
                    .iter()
                    .any(|e| e["code"] == "CAPABILITY_UNAVAILABLE"),
                "assertion failed: {name}: explicit capability refusal"
            );
            assert_eq!(actual["active"], false, "{name}: admission released");
            assert_eq!(actual["ended"], true, "{name}: refusal settled");
            assert!(
                !actual["hostCalls"]
                    .as_array()
                    .expect("#748 submit fixture prerequisite")
                    .iter()
                    .any(|c| c == "submit"),
                "{name}: no native submit"
            );
        } else if !order_only {
            assert_eq!(
                actual, expected,
                "#813 {name}: step {index} {step}: submit scenario must match"
            );
        }
    }
    // #2498: the panic arm's close must end the turn within the abort budget,
    // without waiting for durable appends that never settle. The spin is
    // wall-clock bounded (the registry worker is a real OS thread; a paused
    // tokio clock cannot bound it) so a missing close is an assertion
    // failure here, never a hang. This must precede `consumer.await` for the
    // same reason.
    if let Some(wire_expectation) = case["panicWire"].as_str() {
        await_panic_closure(
            || panic_turn_closed(&log, &owner),
            name,
            "the panic arm must close the turn without waiting for never-settling appends",
        )
        .await;
        let wire = terminal_wire_codes(&log);
        if wire_expectation == "dropped" {
            crate::negative_assert!(
                wire.is_empty(),
                "#2498 {name}: failure frames settling after the hard close are dropped (wire {wire:?})"
            );
        } else {
            crate::negative_assert_eq!(
                wire,
                vec!["gateway_submit_failed".to_string()],
                "#2498 {name}: the bounded drain delivers the in-flight failure frames (wire {wire:?})"
            );
            crate::negative_assert!(
                log.visible
                    .lock()
                    .expect("#748 submit fixture prerequisite")
                    .iter()
                    .any(|event| event["type"] == "agent_status"
                        && event["event"] == "gateway_submit_failed"),
                "#2498 {name}: the failure status frame must survive the bounded drain"
            );
        }
    }
    consumer.await.expect("#748 submit fixture prerequisite");
    if setup["statusRecordGate"] == true {
        let released = case["steps"]
            .as_array()
            .expect("#748 status steps")
            .iter()
            .any(|step| step == "status-release");
        crate::negative_assert!(
            !owner.has_active_turn(&"s".into()),
            "#748 pending status cannot retain admission"
        );
        let rows = status_probe_types(&root);
        let expected = if released {
            vec!["turn_started", "agent_status_message", "turn_result"]
        } else {
            vec!["turn_started", "turn_result"]
        };
        crate::negative_assert_eq!(
            rows,
            expected,
            "#748 context status transcript order at EOF"
        );
        crate::negative_assert!(
            log.host
                .lock()
                .expect("#748 status lifetime log")
                .iter()
                .any(|call| call == "status_dropped"),
            "#748 context status callback has no detached owner"
        );
        services.prepare.open();
    }
    // InProcessGateway releases the attachment lease in the pump finally
    // (:1767-1773) before afterTurnCompleted (:1850) — including the timeout
    // takeover, where the outer finally only `void`s the pump. Measured by
    // the real-TS probe ts-probe-lease-after-order.out; pinned for every
    // scenario that acquires a lease.
    let host = log
        .host
        .lock()
        .expect("#748 submit fixture prerequisite")
        .clone();
    // Tri-state: a scenario that presents attachments must acquire a lease,
    // so a missing record is a harness failure, not a pass. A scenario that
    // presents none must not record a release; otherwise the harness could
    // fake the lease path without the fixture asking for it.
    let expects_lease = matches!(
        input.get("uploadedAttachments").and_then(Value::as_array),
        Some(values) if !values.is_empty()
    );
    let lease = host.iter().position(|call| call == "lease_release");
    let after = host.iter().position(|call| call == "after");
    match (expects_lease, lease, after) {
        (true, Some(lease), Some(after)) => assert!(
            lease < after,
            "{name}: lease_release must precede after_turn (lease@{lease} after@{after}): {host:?}"
        ),
        (true, lease, after) => panic!(
            "{name}: attachments must record lease_release and after_turn (lease@{lease:?} after@{after:?}): {host:?}"
        ),
        (false, Some(lease), _) => panic!(
            "{name}: lease_release recorded without an attachment lease (lease@{lease}): {host:?}"
        ),
        (false, None, _) => {}
    }
    if let Some(abort) = abort {
        abort.await.expect("#748 submit fixture prerequisite");
    }
    if let Some(close) = close {
        close
            .await
            .expect("#748 submit fixture prerequisite")
            .expect("#748 submit fixture prerequisite");
    }
    // Reopen after all FIFO appends have drained, to compare persisted entries
    // rather than a second in-memory FileRunRegistry cache.
    let reopened = FileRunRegistry::open(root.join("registry.json"))
        .expect("#748 submit fixture prerequisite");
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: "r".into(),
    };
    if case["capabilityOnly"] == true {
        assert!(
            reopened
                .events(&run, None, None)
                .expect("#748 submit fixture prerequisite")
                .iter()
                .any(|e| is(field(e, "event"), "code", "CAPABILITY_UNAVAILABLE")),
            "{name}: durable capability refusal"
        );
    } else if order_only {
        // Inline lease-order cases carry no frozen durable expectations.
        // #727: whichever way the deadline/pump race interleaves, the turn
        // must leave exactly one terminal status and one terminal wire error.
        if let Some(code) = case["singleTerminal"].as_str() {
            let wire = terminal_wire_codes(&log);
            // Registry entries are `{ seq, event }`, so the recorded terminal is
            // `entry["event"]["event"]` (see `capabilityOnly` above).
            let durable: Vec<String> = reopened
                .events(&run, None, None)
                .expect("#748 submit fixture prerequisite")
                .iter()
                .map(json)
                .filter_map(|entry| entry["event"]["event"].as_str().map(str::to_string))
                .filter(|code| code == "turn_timeout" || code == "gateway_submit_failed")
                .collect();
            let writers = log
                .observations
                .lock()
                .expect("#748 submit fixture prerequisite")
                .clone();
            // Every surface that can carry a terminal, for the record.
            eprintln!("{name}: writers {writers:?} durable {durable:?} wire {wire:?}");
            // Recorded only by cell 1, while the timeout's durable write was
            // still gated: exactly one writer may reach it.
            if case["steps"]
                .as_array()
                .expect("#748 submit fixture prerequisite")
                .iter()
                .any(|step| step == "single-terminal-writer")
            {
                crate::negative_assert_eq!(
                    writers,
                    vec![1],
                    "{name}: only the takeover may reach the gated durable write (durable {durable:?}, wire {wire:?})"
                );
            }
            // Exactly one terminal row on the durable surface: the takeover owns
            // `turn_timeout`; a losing pump writer would add
            // `gateway_submit_failed`, which is the defect this cell pins. Two
            // rows of the same code are the same defect (two writers).
            crate::negative_assert_eq!(
                durable,
                vec![code.to_string()],
                "{name}: exactly one durable terminal row (durable {durable:?}, wire {wire:?})"
            );
            // The visible frame is dropped by `close_live` before the spawned
            // durable write finishes, so an empty wire is legitimate; only a
            // foreign or repeated code is a failure here.
            crate::negative_assert!(
                wire.iter().all(|seen| seen.as_str() == code) && wire.len() <= 1,
                "{name}: at most one terminal frame, never a foreign one (wire {wire:?})"
            );
        }
    } else {
        assert_eq!(
            reopened
                .events(&run, None, None)
                .expect("#748 submit fixture prerequisite")
                .iter()
                .map(json)
                .collect::<Vec<_>>(),
            case["registryEvents"]
                .as_array()
                .expect("#748 submit fixture prerequisite")
                .clone(),
            "{name}: durable events"
        );
        let record = reopened
            .get(&run)
            .expect("#748 submit fixture prerequisite");
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
        owner
            .get_or_create(context())
            .await
            .expect("#748 submit fixture prerequisite");
        assert_eq!(
            factory.created.load(Ordering::SeqCst),
            2,
            "changed SDK config must dirty the cached handle"
        );
    }
    owner
        .shutdown()
        .await
        .expect("#748 submit fixture prerequisite");
    drop(submitter);
    drop(host);
    drop(registry);
    drop(reopened);
    std::fs::remove_dir_all(root).expect("#748 submit fixture prerequisite");
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
    for case in oracle["cases"]
        .as_array()
        .expect("#748 submit fixture prerequisite")
    {
        if std::env::var("PDRS_SUBMIT_CASE").ok().is_none_or(|name| {
            name == case["name"]
                .as_str()
                .expect("#748 submit fixture prerequisite")
        }) {
            selected += 1;
            tokio::time::timeout(Duration::from_secs(20), replay_case(case))
                .await
                .unwrap_or_else(|_| panic!("{} deadlocked", case["name"]));
        }
    }
    assert!(selected > 0, "PDRS_SUBMIT_CASE matched no scenario");
}

/// DeepSeek P1-2: the TS pump releases the attachment lease in its finally
/// (InProcessGateway.ts:1767-1773) before the outer finally's afterTurnCompleted
/// (:1850) on every exit path. Real-TS measurement: probe
/// ts-probe-lease-after-order.out shows `lease_release < after` for normal
/// completion, a throwing pump, and a timeout takeover. These inline cases pin
/// that order against the same harness the oracle replay uses; the oracle
/// itself already covers the plain completion path (`lease-release`).
#[tokio::test]
async fn lease_release_precedes_after_turn_across_exit_paths() {
    let started = vec![
        json!({"type": "turn_started", "sessionId": "s", "turnId": "r"}),
        json!({"type": "input_accepted", "sessionId": "s", "turnId": "r",
         "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]}),
    ];
    let mut native = started.clone();
    native.push(
        json!({"type": "model_event", "event": {"type": "text_delta", "text": "hello back"}}),
    );
    native.push(json!({"type": "turn_completed", "result": {"type": "success", "stopReason": "completed", "usage": {}}}));
    let mut aborted = started.clone();
    aborted.push(json!({"type": "turn_completed", "result": {"type": "aborted", "stopReason": "aborted_streaming", "usage": {}}}));
    let cases = [
        (
            "lease-normal",
            json!({"input": {"uploadedAttachments": [{"uploadId": "fixture"}]}}),
            json!(["observe", "accept", "terminal", "end"]),
            native.clone(),
        ),
        (
            "lease-pump-failed",
            json!({"pumpFailure": true, "input": {"uploadedAttachments": [{"uploadId": "fixture"}]}}),
            json!(["observe", "accept", "terminal"]),
            started,
        ),
        (
            "lease-timeout",
            json!({"input": {"timeoutMs": 100, "uploadedAttachments": [{"uploadId": "fixture"}]}}),
            json!(["observe", "accept", "timeout"]),
            aborted,
        ),
        // No uploadedAttachments: the same exit path must not invent a lease.
        (
            "lease-absent",
            json!({}),
            json!(["observe", "accept", "terminal", "end"]),
            native,
        ),
    ];
    for (name, setup, steps, native) in cases {
        let case = json!({"name": name, "setup": setup, "steps": steps, "native": native,
            "results": [], "orderOnly": true});
        tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
            .await
            .unwrap_or_else(|_| panic!("{} deadlocked", name));
    }
}
#[tokio::test]
async fn timeout_takeover_writes_exactly_one_terminal() {
    let started = vec![
        json!({"type": "turn_started", "sessionId": "s", "turnId": "r"}),
        json!({"type": "input_accepted", "sessionId": "s", "turnId": "r",
         "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]}),
    ];
    let mut aborted = started.clone();
    aborted.push(json!({"type": "turn_completed", "result": {"type": "aborted", "stopReason": "aborted_streaming", "usage": {}}}));
    // Cell 1: the timer's durable `turn_timeout` write is gated, so the pump
    // error caused by its abort reaches the catch first (the real ordering that
    // produced the extra `gateway_submit_failed`).
    // Cell 2: the pump error is gated until `run` returns, i.e. until after the
    // completion fence; the catch must still not register a terminal.
    let cases = [
        (
            "timeout-durable-blocked-behind-catch",
            json!({"input": {"timeoutMs": 100}, "abortPumpFailure": true, "failureGate": true}),
            json!([
                "observe",
                "accept",
                "timeout",
                "single-terminal-writer",
                "failure-release"
            ]),
        ),
        (
            "catch-after-fence-complete",
            json!({"input": {"timeoutMs": 100}, "abortPumpFailure": true, "pumpErrorGate": true}),
            json!([
                "observe",
                "accept",
                "timeout",
                "fence-complete",
                "pump-error-release"
            ]),
        ),
    ];
    for (name, setup, steps) in cases {
        let case = json!({"name": name, "setup": setup, "steps": steps,
            "native": aborted.clone(), "results": [], "orderOnly": true,
            "singleTerminal": "turn_timeout"});
        tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
            .await
            .unwrap_or_else(|_| panic!("{} deadlocked", name));
    }
}
/// #2498: a panicked worker never reaches its own `finish`, so the manage
/// arm's JoinError branch owns the delivery close. Real-TS measurement
/// (`_tmp/20261026-issue-2498/ts-probe-2498.out`, InProcessGateway at
/// cc7c4e65, Node v22.22.0): a pump throw whose failure registry appends
/// never settle hangs the TS turn forever (probe A: ended=false,
/// active=true, after never called), while appends settling late still
/// deliver both failure frames (probe B). The Rust close is bounded by the
/// same abort budget as the timer-path backstop, deliberately diverging for
/// never- and late-settling appends (specs/gateway-parity/diffs/
/// A7.2-TURN-PANIC-FINISH-001.md).
#[tokio::test]
async fn panicked_worker_closes_delivery_within_abort_budget() {
    // Cell 1 ("panic-drain"): the gated appends settle with the clock
    // frozen — the bounded drain delivers both failure frames.
    // Cell 2 ("panic-budget"): 500ms elapse, still inside the 1000ms budget
    // floor — still delivered, pinning `.max(1_000)`.
    // Cell 3 ("panic-close"): the budget expires before the appends settle
    // — the hard close ends the turn and the late frames are dropped. The
    // closure is asserted while the appends are still gated (fix-r1): only
    // asserting after the gate opens could not tell the close from the
    // appends settling.
    let cases = [
        ("panic-drain-delivers", "panic-drain", "delivered"),
        ("panic-budget-delivers", "panic-budget", "delivered"),
        ("panic-close-drops-late-frames", "panic-close", "dropped"),
    ];
    for (name, step, wire) in cases {
        let case = json!({
            "name": name,
            "setup": {"pumpPanic": true, "registryAppendGate": true},
            "steps": [step],
            "native": [],
            "results": [],
            "orderOnly": true,
            "panicWire": wire,
        });
        tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
            .await
            .unwrap_or_else(|_| panic!("{} deadlocked", name));
    }
}
/// #2498 fix-r1: the never-settling cell the r1 review required — the
/// failure appends are stuck host futures the gate never releases, and the
/// turn (stream end, admission, completion fence, after-turn hook) must
/// still close within the abort budget. Pre-fix this is the P0 minimal
/// reproduction: only the manage arm was bounded, the consumer's `next()`
/// still parked on the trailing `pending` set forever
/// (`specs/gateway-parity/diffs/A7.2-TURN-PANIC-FINISH-001-R2.md`).
#[tokio::test]
async fn never_settling_appends_still_close_the_panic_turn() {
    let case = json!({
        "name": "panic-stuck-never-settling",
        "setup": {"pumpPanic": true, "registryAppendStuck": true},
        "steps": ["panic-stuck"],
        "native": [],
        "results": [],
        "orderOnly": true,
        "panicWire": "dropped",
    });
    tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
        .await
        .unwrap_or_else(|_| panic!("panic-stuck-never-settling deadlocked"));
}
/// #2498 fix-r1: the durable failure record hangs ahead of the budget —
/// pre-fix the abort-budget timer was never even created, so the turn (and
/// its admission and completion fence) hung on the stuck host future. With
/// the record inside the same budget, the budget drops the await of the
/// record future and the hard close ends the turn; no failure frame is
/// enqueued, so the wire carries no terminal frame at all. What the budget
/// drops is the await — the fixture's gated record is a cancellable
/// `Gate::wait()`, while production `record_failure` is `spawn_blocking`
/// (uncancellable; its durable write can still land after the close, which
/// is registered in
/// `specs/gateway-parity/diffs/A7.2-TURN-PANIC-FINISH-001-R3.md`, not
/// proven here).
#[tokio::test]
async fn stuck_failure_record_still_closes_the_panic_turn() {
    let case = json!({
        "name": "panic-record-stuck",
        "setup": {"pumpPanic": true, "failureGate": true},
        "steps": ["panic-record-stuck"],
        "native": [],
        "results": [],
        "orderOnly": true,
        "panicWire": "dropped",
    });
    tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
        .await
        .unwrap_or_else(|_| panic!("panic-record-stuck deadlocked"));
}
/// #2498 fix-r2: the third cell the shared abort budget introduces — the
/// failure record settles, but only after the budget has expired. TS has no
/// budget at all: the record and the appends share one unbounded wait chain
/// (probe C, `_tmp/20261026-issue-2498/fix-r2/ts-probe-2498-r2.out`), so both
/// failure frames are delivered once the slow record settles. Rust drops
/// the await of the record at the budget, so the terminal frames never
/// enter the queue — a wire-visible divergence registered in
/// `specs/gateway-parity/diffs/A7.2-TURN-PANIC-FINISH-001-R3.md`. What the
/// budget drops is the await: the fixture's gated record is a cancellable
/// `Gate::wait()`, while production `record_failure` is `spawn_blocking`
/// (uncancellable — its durable write can still land after the budget and
/// the close, registered alongside in the R3 entry); this cell proves the
/// wire side and the bounded close only.
#[tokio::test]
async fn slow_failure_record_settling_after_budget_drops_the_failure_frames() {
    let case = json!({
        "name": "panic-record-slow-release",
        "setup": {"pumpPanic": true, "failureGate": true},
        "steps": ["panic-record-slow-release"],
        "native": [],
        "results": [],
        "orderOnly": true,
        "panicWire": "dropped",
    });
    tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
        .await
        .unwrap_or_else(|_| panic!("panic-record-slow-release deadlocked"));
}
/// #2498 fix-r2: the timer winner's durable `turn_timeout` record was an
/// unbounded host future ahead of the settlement and the `close_live` — a
/// stuck record parked the manage arm forever (and the worker parks on the
/// settlement), the same hang shape the panic arm had before fix-r1. TS
/// exhibits the same wait (`settleTimeout` awaits the record, the pump
/// finally awaits that settlement — probe D shows the turn closing only
/// after a slow record settles,
/// `_tmp/20261026-issue-2498/fix-r3/ts-probe-2498-r3.out`), so bounding it
/// is the registered "close over hang" trade-off, not a TS-matching fix
/// (`specs/gateway-parity/diffs/A7.2-TURN-PANIC-FINISH-001-R3.md`). The
/// fixture's gated record is a cancellable `Gate::wait()`; production
/// `record_failure` is `spawn_blocking` and its durable write can land
/// after the budget and the close (registered in the R3 entry) — this cell
/// proves the manage arm does not wait past the budget and the turn closes,
/// not the durable late write.
#[tokio::test]
async fn stuck_timer_record_still_closes_the_turn() {
    let case = json!({
        "name": "timer-record-stuck",
        "setup": {"input": {"timeoutMs": 100}, "failureGate": true},
        // #2498: `observe` first so the settle after it starts the spawned
        // worker and the turn timer registers its deadline before the
        // `timeout` step's clock advance — advancing first would strand the
        // deadline past the (already moved) clock and the timer would never
        // fire, exactly the shape the #727 timer cells avoid with their
        // leading `observe`.
        "steps": ["observe", "timeout", "timer-record-stuck"],
        "native": [],
        "results": [],
        "orderOnly": true,
        "panicWire": "dropped",
    });
    tokio::time::timeout(Duration::from_secs(20), replay_case(&case))
        .await
        .unwrap_or_else(|_| panic!("timer-record-stuck deadlocked"));
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
            .expect("#748 submit fixture prerequisite");
    }
    assert!(
        selected > 0,
        "#813 submit capability filter: must match at least one scenario"
    );
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

#[tokio::test(start_paused = true)]
async fn pending_context_budget_status_cannot_hold_turn_completion() {
    let case = json!({"name":"pending-context-budget", "orderOnly":true,
        "setup":{"statusRecordGate":true}, "steps":["observe","accept","terminal","status-enter","status-deadline","end"], "results":[],
        "native":[{"type":"turn_started"},{"type":"input_accepted"},
            {"type":"context_budget","snapshot":{"tokens":0,"maxContextTokens":128000,"reservedOutputTokens":0,"totalContextTokens":128000,"canContinue":true}},
            {"type":"turn_completed","result":{"type":"success","stopReason":"completed","usage":{}}}]});
    tokio::time::timeout(Duration::from_secs(5), replay_case(&case))
        .await
        .expect("#748 pending status bounded turn");
}

struct StatusProbeDrop(Arc<Log>);
impl Drop for StatusProbeDrop {
    fn drop(&mut self) {
        self.0.host("status_dropped");
    }
}
fn status_probe_types(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join("status-transcript.jsonl"))
        .expect("#748 status transcript after EOF")
        .lines()
        .map(|line| {
            let value: Value =
                serde_json::from_str(line).expect("#748 status transcript complete JSONL");
            value["type"]
                .as_str()
                .expect("#748 status transcript row type")
                .to_owned()
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn context_budget_delayed_write_precedes_terminal_and_eof() {
    let case = json!({"name":"ordered-context-budget", "orderOnly":true,
        "setup":{"statusRecordGate":true}, "steps":["observe","accept","terminal","status-enter","status-release","end"], "results":[],
        "native":[{"type":"turn_started"},{"type":"input_accepted"},
            {"type":"context_budget","snapshot":{"tokens":0,"maxContextTokens":128000,"reservedOutputTokens":0,"totalContextTokens":128000,"canContinue":true}},
            {"type":"turn_completed","result":{"type":"success","stopReason":"completed","usage":{}}}]});
    tokio::time::timeout(Duration::from_secs(5), replay_case(&case))
        .await
        .expect("#748 ordered status bounded turn");
}

#[tokio::test(start_paused = true)]
async fn context_budget_cancelled_wait_does_not_retain_admission() {
    let case = json!({"name":"cancelled-context-budget", "orderOnly":true,
        "setup":{"statusRecordGate":true}, "steps":["observe","accept","terminal","status-enter","abort","status-deadline","end"], "results":[],
        "native":[{"type":"turn_started"},{"type":"input_accepted"},
            {"type":"context_budget","snapshot":{"tokens":0,"maxContextTokens":128000,"reservedOutputTokens":0,"totalContextTokens":128000,"canContinue":true}},
            {"type":"turn_completed","result":{"type":"success","stopReason":"completed","usage":{}}}]});
    tokio::time::timeout(Duration::from_secs(5), replay_case(&case))
        .await
        .expect("#748 cancelled status bounded turn");
}
