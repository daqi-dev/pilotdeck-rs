//! Transport fixtures share the production serial host and durable registry.
//! Gates acknowledge entry before release; deadlines only bound a failed test.
use pilotdeck_gateway::{
    runtime::{
        connection::{Call, GatewayTextConnection, RequestError},
        interaction::{GatewayInteractionCoordinator, InteractionBinding},
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory, SessionNext,
            SessionOwner, SessionOwnerOptions, TurnCompletionHandle,
        },
        turn::{
            gateway::{GatewayRequests, GatewayServices, HostedConnection, TurnGateway},
            projector::GatewayAgentEventProjector,
            publish::FileRegistry,
            replay::TurnReplayOptions,
            submit::{
                PreparedTurn, SubmitError, SubmitFuture, SubmitHost, SubmitWorkspace,
                TurnSubmitter, WorkspaceAdmissions,
            },
        },
    },
    storage::{FileRunRegistry, RunRef},
};
use pilotdeck_protocol::{GATEWAY_PROTOCOL_VERSION, JsString, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

pub fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
pub async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("fixture barrier timed out")
}
pub struct Gate(watch::Sender<bool>);
impl Default for Gate {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Gate {
    pub fn open(&self) {
        self.0.send_replace(true);
    }
    pub async fn wait(&self) {
        self.0.subscribe().wait_for(|ready| *ready).await.unwrap();
    }
}
#[derive(Default)]
pub struct Session {
    pub entered: Gate,
    pub release: Gate,
    pub aborts: AtomicUsize,
    pub submits: AtomicUsize,
}
struct SessionPort(Arc<Session>);
impl std::ops::Deref for SessionPort {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.0
    }
}
impl LiveSession for SessionPort {
    fn snapshot(&self) -> JsValue {
        js(json!({"messages":[],"cwd":"/fixture"}))
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        self.aborts.fetch_add(1, Ordering::SeqCst);
        self.release.open();
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn submit(&self, _: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let id = options
            .get("turnId")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        Ok(Box::new(Stream {
            session: self.0.clone(),
            id,
            step: 0,
        }))
    }
}
struct Stream {
    session: Arc<Session>,
    id: String,
    step: usize,
}
impl LiveTurn for Stream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            let event = match self.step {
                0 => Some(js(json!({"type":"turn_started","turnId":self.id}))),
                1 => Some(js(
                    json!({"type":"input_accepted","turnId":self.id,"messages":[]}),
                )),
                2 => {
                    self.session.entered.open();
                    self.session.release.wait().await;
                    Some(js(json!({"type":"turn_completed","turnId":self.id,
                        "result":{"type":"success","stopReason":"completed","usage":{}}})))
                }
                _ => None,
            };
            self.step += 1;
            Ok(event)
        })
    }
}
pub struct Factory(pub HashMap<String, Arc<Session>>);
impl SessionFactory for Factory {
    fn create(
        &self,
        context: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = self.0[&context.session_key.to_utf8().unwrap()].clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(Arc::new(SessionPort(session)), None)) })
    }
}
struct Host(Arc<GatewayInteractionCoordinator>);
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
        100
    }
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            let binding =
                InteractionBinding::from_value(input.get("interactionBinding").unwrap()).unwrap();
            self.0
                .reconnect_for_turn(
                    input.get("sessionKey").unwrap().as_string().unwrap(),
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
                model_selection: js(json!({"source":"default"})),
                input: input.clone(),
                options: js(json!({"turnId":run.run_id.to_utf8().unwrap()})),
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
        Box::pin(async { panic!("unexpected failure in successful transport fixture") })
    }
    fn finish_pump<'a>(&'a self, _: &'a RunRef) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.0.reject_pending_turn(session, &reason.into());
        Ok(())
    }
    fn release_claim(&self, _: &JsString, _: &JsString) {}
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        Ok(())
    }
}
struct Requests(Arc<GatewayInteractionCoordinator>);
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Ok(js(json!({"name":"a52","capabilities":[]}))) })
    }
    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            if method.equals_str("reconnect_interaction") {
                let owner = params.get("sessionKey").unwrap().as_string().unwrap();
                let next =
                    InteractionBinding::from_value(params.get("nextBinding").unwrap()).unwrap();
                let previous =
                    InteractionBinding::from_value(params.get("previousBinding").unwrap()).unwrap();
                return self
                    .0
                    .reconnect(owner, Some(next.clone()), Some(&previous))
                    .inspect(|_| {
                        assert_eq!(self.0.get_binding(owner), Some(next));
                    })
                    .map_err(|e| RequestError::new(format!("{e:?}")));
            }
            Err(RequestError::new(format!("unexpected method: {method:?}")))
        })
    }
}
#[derive(Default)]
pub struct Text(pub Mutex<Vec<Value>>);
impl GatewayTextConnection for Text {
    fn send_text(&self, text: String) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_str(&text).unwrap());
    }
    fn close(&self, code: u16, reason: &str) {
        panic!("unexpected close {code}: {reason}");
    }
}
impl Text {
    pub fn frames(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }
}
pub fn gateway(path: &Path, sessions: HashMap<String, Arc<Session>>) -> TurnGateway {
    let file = FileRegistry::new(FileRunRegistry::open(path).unwrap()).unwrap();
    TurnGateway::start(TurnReplayOptions::new(), move |coordinator| {
        let mut options = SessionOwnerOptions::new(Arc::new(Factory(sessions)));
        options.idle_sweep_interval_ms = 0;
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        Ok(GatewayServices {
            submitter: Arc::new(TurnSubmitter {
                owner: SessionOwner::new(options),
                coordinator,
                projector: Arc::new(GatewayAgentEventProjector::default()),
                registry: Some(Arc::new(file)),
                host: Arc::new(Host(interactions.clone())),
                workspace_admissions: WorkspaceAdmissions::default(),
            }),
            requests: Arc::new(Requests(interactions.clone())),
            interactions,
        })
    })
    .unwrap()
}
pub async fn connect(gateway: &TurnGateway) -> (HostedConnection, Arc<Text>) {
    let text = Arc::new(Text::default());
    let connection = gateway.connect(text.clone(), "fixture-token".into(), "a52".into());
    bounded(
        connection.receive(
            json!({"type":"hello","protocolVersion":GATEWAY_PROTOCOL_VERSION,
        "token":"fixture-token","clientName":"a52","clientVersion":"1"})
            .to_string(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(text.frames()[0]["type"], "hello_ok");
    (connection, text)
}
pub fn request(id: &str, session: &str, run: &str) -> String {
    json!({"type":"request","id":id,"method":"submit_turn","params":{
        "sessionKey":session,"runId":run,"message":"hello","channelKey":"web"}})
    .to_string()
}
pub fn run(session: &str, id: &str) -> RunRef {
    RunRef {
        session_key: session.into(),
        run_id: id.into(),
        project_key: None,
    }
}
