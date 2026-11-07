//! Native Scheduler backend. Only L-C requests cross this boundary. The gateway
//! owns identity and a rebuildable UI view; Kernel owns acceptance and effects.
use super::{
    LiveSession, LiveTurn, SessionNext,
    backend::{BackendContext, SessionBackend, SessionBackendFactory},
};
use crate::runtime::{
    project::{DisposeFuture, RuntimeError, RuntimeResult},
    refresh::BoxFuture,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

fn error(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(e.to_string())
}
fn js(value: &Value) -> Result<JsValue, RuntimeError> {
    parse_json(&value.to_string()).map_err(error)
}
fn json_value(value: &JsValue) -> Result<Value, RuntimeError> {
    serde_json::from_str(
        &stringify_json(value)
            .map_err(error)?
            .unwrap_or_else(|| "null".into()),
    )
    .map_err(error)
}
fn text(value: &JsString) -> Result<String, RuntimeError> {
    String::from_utf16(value.units()).map_err(error)
}

/// Every persisted field participates in reopening: mode changes never switch an
/// existing session, and canonical project identity prevents cross-project aliases.
#[derive(Clone)]
pub struct KernelBackendFactory {
    pub socket: PathBuf,
    pub loop_version: String,
    pub owner_path: PathBuf,
    pub project: PathBuf,
    pub session: JsString,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    agent_config: JsObject,
    permission_mode_source: Option<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>,
}
pub enum BackendOwner {
    Legacy,
    Kernel(KernelBackendFactory),
}
impl KernelBackendFactory {
    pub fn with_turn_preferences(
        mut self,
        config: JsObject,
        source: Arc<dyn Fn() -> Option<JsString> + Send + Sync>,
    ) -> Self {
        self.agent_config = config;
        self.permission_mode_source = Some(source);
        self
    }
    pub fn with_interactions(
        mut self,
        interactions: Arc<crate::runtime::interaction::GatewayInteractionCoordinator>,
    ) -> Self {
        self.interactions = Some(interactions);
        self
    }
    pub fn select(
        binding: &super::factory::SessionBinding,
        env: &crate::config::Env,
        project: &std::path::Path,
    ) -> Result<BackendOwner, RuntimeError> {
        let project = std::fs::canonicalize(project).map_err(error)?;
        let mut owner_path = binding.recorder.writer().path().as_os_str().to_os_string();
        owner_path.push(".backend.json");
        let owner_path = PathBuf::from(owner_path);
        let session = binding.context.session_key.clone();
        let saved = match std::fs::read(&owner_path) {
            Ok(bytes) => Some(serde_json::from_slice::<Value>(&bytes).map_err(error)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(error(e)),
        };
        if let Some(saved) = saved {
            if saved["schema"] != 1
                || saved["project"] != project.to_string_lossy().as_ref()
                || saved["session"] != text(&session)?
            {
                return Err(error("Session backend owner identity mismatch."));
            }
            return match saved["backend"].as_str() {
                Some("legacy") => Ok(BackendOwner::Legacy),
                Some("kernel") => Ok(BackendOwner::Kernel(Self {
                    socket: PathBuf::from(
                        saved["socket"]
                            .as_str()
                            .ok_or_else(|| error("Missing Kernel socket owner."))?,
                    ),
                    loop_version: saved["loop_version"]
                        .as_str()
                        .ok_or_else(|| error("Missing Kernel loop version."))?
                        .into(),
                    owner_path,
                    project,
                    session,
                    interactions: None,
                    agent_config: JsObject::new(),
                    permission_mode_source: None,
                })),
                _ => Err(error("Unsupported persisted session backend.")),
            };
        }
        let mode = env
            .get("PILOTDECK_SESSION_BACKEND")
            .map(String::as_str)
            .unwrap_or("legacy");
        if !matches!(mode, "legacy" | "kernel") {
            return Err(error("PILOTDECK_SESSION_BACKEND must be legacy or kernel."));
        }
        // Historical transcripts keep their original execution owner. Explicit
        // import, rather than opening history, creates a new Kernel session.
        if mode == "legacy" || binding.historical_transcript {
            write_owner(
                &owner_path,
                &json!({"schema":1,"backend":"legacy","project":project,"session":text(&session)?}),
            )?;
            return Ok(BackendOwner::Legacy);
        }
        let socket = PathBuf::from(
            env.get("PILOTDECK_KERNEL_SOCKET")
                .ok_or_else(|| error("Kernel mode requires PILOTDECK_KERNEL_SOCKET."))?,
        );
        if !socket.is_absolute() {
            return Err(error("Kernel socket must be absolute."));
        }
        let loop_version = env
            .get("PILOTDECK_KERNEL_LOOP_VERSION")
            .cloned()
            .unwrap_or_default();
        Ok(BackendOwner::Kernel(Self {
            socket,
            loop_version,
            owner_path,
            project,
            session,
            interactions: None,
            agent_config: JsObject::new(),
            permission_mode_source: None,
        }))
    }
}
fn write_owner(path: &std::path::Path, owner: &Value) -> RuntimeResult {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(error)?;
    file.write_all(&serde_json::to_vec(owner).map_err(error)?)
        .map_err(error)?;
    file.sync_all().map_err(error)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(error)?;
    }
    Ok(())
}
struct Rpc {
    socket: PathBuf,
    stream: Option<BufReader<UnixStream>>,
    next: u64,
    node: String,
    lease: Option<(String, u64, std::time::Instant)>,
}
impl Rpc {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let mut phase = format!("{method}:connect");
        let timed=tokio::time::timeout(std::time::Duration::from_secs(30), async {
            if self.stream.is_none() {
                self.stream=Some(BufReader::new(UnixStream::connect(&self.socket).await.map_err(error)?));
                phase="initialize".into();
                self.exchange("initialize",json!({"abi_version":2,"api_version":"0.1.0","features":["model-session/1.0","model-response-metadata/1.0"],"node_id":self.node})).await?;
            }
            if matches!(method,"execution.input"|"execution.respond"|"execution.history.import") {
                if let Some((session,epoch,since))=self.lease.clone() {
                    if since.elapsed()>std::time::Duration::from_secs(60) {
                        let lease_method=if since.elapsed()>std::time::Duration::from_secs(240)&&method=="execution.input"{"session.lease_acquire"}else{"session.lease_renew"};
                        phase=lease_method.into();
                        let renewed=self.exchange(lease_method,json!({"session_id":session,"epoch":epoch,"ttl_ms":300000})).await?;
                        self.lease=Some((session,renewed["data"]["epoch"].as_u64().unwrap_or(epoch),std::time::Instant::now()));
                    }
                }
            }
            phase=method.into();
            self.exchange(method,params).await
        }).await;
        let result = match timed {
            Ok(result) => result,
            Err(_) => Err(error(format!(
                "Kernel RPC {phase} timed out after 30 seconds; inspect the original execution before retrying effects."
            ))),
        };
        if result.is_err() {
            self.stream = None;
        }
        result.map_err(|cause| {
            let message = format!("Kernel RPC {phase} failed: {}", cause.message);
            error(message.clone()).with_js_error(crate::runtime::project::RuntimeJsError {
                name: "KernelBackendError".into(),
                code: Some("KERNEL_EXECUTION_UNAVAILABLE".into()),
                message: message.into(),
                requires_host: false,
            })
        })
    }
    async fn exchange(&mut self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        self.next += 1;
        let id = self.next;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        // Taking ownership before the first await makes cancellation close the
        // socket. A late reply can never be mistaken for a later request.
        let mut stream = self
            .stream
            .take()
            .ok_or_else(|| error("Kernel connection unavailable."))?;
        stream
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(error)?;
        loop {
            let mut bytes = Vec::new();
            let n = (&mut stream)
                .take(16 * 1024 * 1024)
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(error)?;
            if n == 0 || bytes.last() != Some(&b'\n') {
                return Err(error(
                    "Kernel connection ended or frame exceeded 16 MiB; execution outcome is unknown.",
                ));
            }
            let reply: Value = serde_json::from_slice(&bytes).map_err(error)?;
            if reply.get("id").is_none() {
                // The public ABI2 session methods predate the Scheduler JSON-RPC
                // wrapper and can return their native {status,effect,data}
                // envelope directly. Only these known one-request-at-a-time
                // calls admit that shape; notifications must carry method.
                if matches!(
                    method,
                    "session.create"
                        | "session.get"
                        | "session.lease_acquire"
                        | "session.lease_renew"
                        | "session.lease_release"
                ) && reply["status"].is_string()
                    && reply["effect"].is_string()
                    && reply.get("data").is_some()
                {
                    self.stream = Some(stream);
                    return Ok(reply);
                }
                if reply["jsonrpc"] == "2.0" && reply["method"].is_string() {
                    continue;
                }
                return Err(error(format!(
                    "Kernel RPC {method} received an unrecognized response without request identity."
                )));
            }
            if reply["id"] != id {
                return Err(error("Kernel response identity mismatch."));
            }
            let result = if let Some(e) = reply.get("error") {
                Err(error(format!("Kernel request failed: {e}")))
            } else {
                reply
                    .get("result")
                    .cloned()
                    .ok_or_else(|| error("Kernel result missing."))
            };
            self.stream = Some(stream);
            return result;
        }
    }
}
#[derive(Default)]
struct State {
    disposed: bool,
    run: Option<String>,
    call: Option<String>,
    call_run: Option<String>,
    status: String,
    cursor: u64,
    waits: BTreeMap<String, String>,
    cancelled: Option<String>,
    attached: bool,
}
async fn recover(rpc: &mut Rpc, session: &str) -> Result<State, RuntimeError> {
    let events = rpc
        .call("execution.events", json!({"session_id":session,"after":0}))
        .await?;
    let events = events
        .as_array()
        .ok_or_else(|| error("Kernel event journal must be an array."))?;
    let mut state = State::default();
    state.cursor = events
        .last()
        .and_then(|e| e["cursor"].as_u64())
        .unwrap_or(0);
    if let Some(root) = events.iter().rev().find(|event| {
        event["kind"] == "call.accepted" && event["payload"]["cause_execution_ref"].is_null()
    }) {
        let call = root["call_ref"]
            .as_str()
            .ok_or_else(|| error("Root execution identity missing."))?;
        let view = rpc
            .call(
                "execution.get",
                json!({"session_id":session,"call_ref":call}),
            )
            .await?;
        state.call = Some(call.into());
        state.call_run = view["invocation"]["correlation_id"]
            .as_str()
            .map(str::to_string);
        state.status = view["state"].as_str().unwrap_or("unknown").into();
        if matches!(
            state.status.as_str(),
            "accepted" | "running" | "needs_human"
        ) {
            state.run = view["invocation"]["correlation_id"]
                .as_str()
                .map(str::to_string);
        }
        if let Some(wait) = view["waiting"]["id"].as_str() {
            state.waits.insert(wait.into(), call.into());
        }
    } else {
        state.status = "idle".into();
    }
    Ok(state)
}
struct KernelSession {
    rpc: Arc<tokio::sync::Mutex<Rpc>>,
    kernel_id: String,
    session: String,
    state: Arc<Mutex<State>>,
    started: Arc<AtomicBool>,
    context_ref: String,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    agent_config: JsObject,
    permission_mode_source: Option<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>,
}
impl SessionBackendFactory for KernelBackendFactory {
    fn build(
        self: Box<Self>,
        _context: BackendContext,
    ) -> BoxFuture<Result<Box<dyn SessionBackend>, RuntimeError>> {
        Box::pin(async move {
            let mut rpc = Rpc {
                socket: self.socket.clone(),
                stream: None,
                next: 0,
                node: format!("pilotdeck-{}", uuid::Uuid::new_v4()),
                lease: None,
            };
            let saved = std::fs::read(&self.owner_path)
                .ok()
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(error))
                .transpose()?;
            if let Some(node) = saved.as_ref().and_then(|v| v["node_id"].as_str()) {
                rpc.node = node.into();
            }
            let kernel_id = if let Some(saved) = &saved {
                saved["kernel_session_id"]
                    .as_str()
                    .ok_or_else(|| error("Missing persisted Kernel session identity."))?
                    .to_string()
            } else {
                let created = rpc.call("session.create", json!({})).await?;
                created["data"]["session_id"]
                    .as_str()
                    .ok_or_else(|| error("Kernel session.create omitted session_id."))?
                    .into()
            };
            let opened = rpc
                .call("execution.open", json!({"session_id":kernel_id}))
                .await?;
            if !opened["input_controller_ref"].is_null() {
                return Err(error(
                    "Kernel product backend requires a frozen assembly without an input controller.",
                ));
            }
            let context_ref = "default".to_string();
            let binding = opened["default_loop_ref"]
                .as_str()
                .ok_or_else(|| error("Kernel did not expose its default binding."))?;
            if !self.loop_version.is_empty() && opened["loop_revision"] != self.loop_version {
                return Err(error(
                    "Kernel loop revision does not match this session's pinned owner.",
                ));
            }
            if let Some(saved) = &saved {
                if saved["binding"] != binding || saved["context_ref"] != context_ref {
                    return Err(error("Kernel binding/context owner mismatch."));
                }
            }
            if opened["protocol"].as_str().is_none() {
                return Err(error("Kernel Scheduler protocol unavailable."));
            }
            if let Some(saved) = &saved {
                if let Some(node) = saved["node_id"].as_str() {
                    rpc.node = node.to_string();
                }
            }
            let lease = rpc
                .call(
                    "session.lease_acquire",
                    json!({"session_id":kernel_id,"ttl_ms":300000}),
                )
                .await?;
            let epoch = lease["data"]["epoch"]
                .as_u64()
                .ok_or_else(|| error("Kernel lease epoch missing."))?;
            rpc.lease = Some((kernel_id.clone(), epoch, std::time::Instant::now()));
            if saved.is_none() {
                write_owner(
                    &self.owner_path,
                    &json!({"schema":1,"backend":"kernel","project":self.project,"session":text(&self.session)?,"socket":self.socket,"loop_version":opened["loop_revision"],"kernel_session_id":kernel_id,"binding":binding,"context_ref":context_ref,"node_id":rpc.node}),
                )?;
            }
            let recovered = recover(&mut rpc, &kernel_id).await?;
            let rpc = Arc::new(tokio::sync::Mutex::new(rpc));
            let state = Arc::new(Mutex::new(recovered));
            let weak_rpc = Arc::downgrade(&rpc);
            let weak_state = Arc::downgrade(&state);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let (Some(rpc), Some(state)) = (weak_rpc.upgrade(), weak_state.upgrade())
                    else {
                        break;
                    };
                    if state.lock().unwrap().disposed {
                        break;
                    }
                    let mut rpc = rpc.lock().await;
                    if let Some((session, epoch, _)) = rpc.lease.clone() {
                        match rpc
                            .call(
                                "session.lease_renew",
                                json!({"session_id":session,"epoch":epoch,"ttl_ms":300000}),
                            )
                            .await
                        {
                            Ok(_) => rpc.lease = Some((session, epoch, std::time::Instant::now())),
                            Err(_) => {
                                state.lock().unwrap().status = "unknown".into();
                            }
                        }
                    }
                }
            });
            Ok(Box::new(KernelSession {
                rpc,
                kernel_id,
                session: text(&self.session)?,
                state,
                started: Arc::new(AtomicBool::new(false)),
                context_ref,
                interactions: self.interactions,
                agent_config: self.agent_config,
                permission_mode_source: self.permission_mode_source,
            }) as Box<dyn SessionBackend>)
        })
    }
}
impl SessionBackend for KernelSession {
    fn permission_mode(&self) -> JsString {
        let stored = self
            .permission_mode_source
            .as_ref()
            .and_then(|source| source())
            .or_else(|| {
                self.agent_config
                    .get("permissionMode")
                    .and_then(JsValue::as_string)
                    .cloned()
            });
        crate::runtime::permission::effective_permission_mode(&self.agent_config, stored)
    }
    fn execution_started(&self) -> Arc<AtomicBool> {
        self.started.clone()
    }
}
impl LiveSession for KernelSession {
    fn backend_kind(&self) -> &'static str {
        "kernel"
    }
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        let options = json_value(&options)?;
        let run = options["turnId"]
            .as_str()
            .or_else(|| options["runId"].as_str())
            .ok_or_else(|| error("Kernel turn requires turnId or runId."))?
            .to_string();
        let mut payload = json_value(&input)?;
        if !payload.is_object() {
            return Err(error("Native input must be an object."));
        }
        if payload["type"] == "blocks" {
            return Err(error(
                "Native block/attachment ingress is awaiting its canonical binding; no input was accepted.",
            ));
        }

        let config = json_value(&JsValue::Object(self.agent_config.clone()))?;
        let mut agent = serde_json::Map::new();
        for key in [
            "systemPrompt",
            "appendSystemPrompt",
            "planModeInstructions",
            "runtimeContextSurface",
            "thinking",
            "toolChoice",
            "maxContextMessages",
            "jsonSelfCorrect",
        ] {
            if let Some(value) = config.get(key) {
                agent.insert(key.into(), value.clone());
            }
        }
        agent.insert(
            "permissionMode".into(),
            json!(text(&self.permission_mode())?),
        );
        payload["agent"] = Value::Object(agent);
        for key in ["runMode", "allowPlanModeTools"] {
            if let Some(value) = options.get(key) {
                payload[key] = value.clone();
            }
        }
        let mut state = self.state.lock().unwrap();
        if state.disposed || state.run.is_some() {
            return Err(error("Kernel session is disposed or already running."));
        }
        state.run = Some(run.clone());
        state.status = "running".into();
        state.cancelled = None;
        state.attached = true;
        self.started.store(false, Ordering::SeqCst);
        Ok(Box::new(KernelTurn {
            rpc: self.rpc.clone(),
            kernel_id: self.kernel_id.clone(),
            session: self.session.clone(),
            state: self.state.clone(),
            started: self.started.clone(),
            input: Some(payload),
            run,
            queue: VecDeque::new(),
            done: false,
            final_event: None,
            failure_event: false,
            accepted: false,
            interactions: self.interactions.clone(),
        }))
    }
    fn snapshot(&self) -> JsValue {
        let state = self.state.lock().unwrap();
        js(&json!({"status":state.status,"currentTurnId":state.run,"backend":"kernel","kernelSessionId":self.kernel_id,"executionId":state.call})).unwrap_or(JsValue::Null)
    }
    fn backend_request(
        &self,
        method: String,
        params: JsValue,
    ) -> BoxFuture<Result<Option<JsValue>, RuntimeError>> {
        let rpc = self.rpc.clone();
        let kernel_id = self.kernel_id.clone();
        let state = self.state.clone();
        let context_ref = self.context_ref.clone();
        let interactions = self.interactions.clone();
        let session = JsString::from(self.session.clone());
        Box::pin(async move {
            let params = json_value(&params)?;
            let (call, _cursor) = {
                let s = state.lock().unwrap();
                (s.call.clone(), s.cursor)
            };
            let value = match method.as_str() {
                "active_turn_snapshot" => {
                    let mut rpc = rpc.lock().await;
                    let facts = recover(&mut rpc, &kernel_id).await?;
                    let current = if let Some(call) = &facts.call {
                        rpc.call(
                            "execution.get",
                            json!({"session_id":kernel_id,"call_ref":call}),
                        )
                        .await?
                    } else {
                        Value::Null
                    };
                    let value = json!({"backend":"kernel","kernelSessionId":kernel_id,"executionId":facts.call,"runId":facts.run,"cursor":facts.cursor,"execution":current,"status":facts.status});
                    let mut state = state.lock().unwrap();
                    if !state.attached {
                        let disposed = state.disposed;
                        *state = facts;
                        state.disposed = disposed;
                    }
                    value
                }
                "read_session_messages" | "export_session_transcript" => {
                    let history = rpc
                        .lock()
                        .await
                        .call(
                            "execution.history.snapshot",
                            json!({"session_id":kernel_id,"context_ref":context_ref}),
                        )
                        .await?;
                    json!({"messages":history["history"],"revision":history["revision"],"backend":"kernel","kernelSessionId":kernel_id})
                }
                "restore_session_transcript" => {
                    let transfer = &params["transcript"];
                    let hash = transfer["sha256"]
                        .as_str()
                        .ok_or_else(|| error("Verified import hash missing."))?;
                    let status = rpc
                        .lock()
                        .await
                        .call(
                            "session.get",
                            json!({"session_id":kernel_id,"view":"status"}),
                        )
                        .await?;
                    let generation = status["data"]["generation"].as_u64().ok_or_else(|| {
                        error("Kernel session generation is unavailable for import.")
                    })?;
                    let receipt=rpc.lock().await.call("execution.history.import",json!({"session_id":kernel_id,"generation":generation,"context_ref":context_ref,"expected_revision":0,"idempotency_key":format!("pd-import:{hash}"),"history":transfer["messages"]})).await?;
                    json!({"sessionKey":params["sessionKey"],"kernelSessionId":kernel_id,"sha256":hash,"receipt":receipt,"imported":true})
                }
                "run_get" | "run_reattach" | "run_events" => {
                    let run = params["runId"]
                        .as_str()
                        .ok_or_else(|| error("runId is required."))?;
                    let mut rpc = rpc.lock().await;
                    let events = rpc
                        .call(
                            "execution.events",
                            json!({"session_id":kernel_id,"after":0}),
                        )
                        .await?;
                    let root = events
                        .as_array()
                        .and_then(|events| {
                            events.iter().find(|event| {
                                event["kind"] == "call.accepted"
                                    && event["correlation_id"] == run
                                    && event["payload"]["cause_execution_ref"].is_null()
                            })
                        })
                        .and_then(|event| event["call_ref"].as_str())
                        .ok_or_else(|| error("Original Kernel run identity was not found."))?;
                    rpc.call(
                        "execution.get",
                        json!({"session_id":kernel_id,"call_ref":root}),
                    )
                    .await?
                }
                "permission_list" => {
                    let mut rpc = rpc.lock().await;
                    let facts = recover(&mut rpc, &kernel_id).await?;
                    let mut requests = Vec::new();
                    for (request, call) in &facts.waits {
                        let view = rpc
                            .call(
                                "execution.get",
                                json!({"session_id":kernel_id,"call_ref":call}),
                            )
                            .await?;
                        requests.push(json!({"requestId":request,"executionId":call,"toolName":view["waiting"]["schema"]["tool_name"],"payload":view["waiting"]["schema"]}));
                    }
                    json!({"requests":requests})
                }
                "steer_turn" => {
                    let facts = recover(&mut *rpc.lock().await, &kernel_id).await?;
                    if facts.run.as_deref() != params["runId"].as_str() {
                        json!({"accepted":false,"reason":if facts.run.is_none(){"no_active_turn"}else{"turn_mismatch"}})
                    } else {
                        if params["attachments"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty())
                        {
                            return Err(error("Kernel steer attachments are not configured."));
                        }
                        let call = facts
                            .call
                            .ok_or_else(|| error("Original steer execution missing."))?;
                        let result=rpc.lock().await.call("execution.control",json!({"session_id":kernel_id,"call_ref":call,"kind":"steer","detail":params["message"].as_str().unwrap_or("")})).await?;
                        json!({"accepted":result=="accepted","outcome":result,"executionId":call})
                    }
                }
                "permission_decide" => {
                    let wait = params["requestId"]
                        .as_str()
                        .ok_or_else(|| error("Permission requestId is required."))?;
                    let call = state
                        .lock()
                        .unwrap()
                        .waits
                        .get(wait)
                        .cloned()
                        .ok_or_else(|| {
                            error("Original Kernel permission request is not pending.")
                        })?;
                    if params["executionId"].as_str().is_some_and(|id| id != call) {
                        return Err(error("Permission execution identity mismatch."));
                    }
                    let decision = match params["decision"].as_str().unwrap_or("reject_once") {
                        "allow" => "allow_once",
                        "deny" => "reject_once",
                        other => other,
                    };
                    rpc.lock().await.call("execution.respond", json!({"session_id":kernel_id,"call_ref":call,"wait_id":params["requestId"],"messages":[{"role":"user","text":decision}],"idempotency_key":format!("permission:{}",params["requestId"])})).await?;
                    state.lock().unwrap().waits.remove(wait);
                    if let Some(interactions) = &interactions {
                        interactions
                            .reconnect_port()
                            .set_external_pending(&session, false);
                    }
                    json!({"delivered":true})
                }
                "reconnect_interaction" => {
                    let events = rpc
                        .lock()
                        .await
                        .call(
                            "execution.events",
                            json!({"session_id":kernel_id,"after":0}),
                        )
                        .await?;
                    let facts = recover(&mut *rpc.lock().await, &kernel_id).await?;
                    let requests=facts.waits.iter().map(|(id,call)|json!({"requestId":id,"executionId":call,"kind":"permission"})).collect::<Vec<_>>();
                    {
                        let mut state = state.lock().unwrap();
                        state.waits = facts.waits;
                        if !state.attached {
                            state.call = facts.call;
                            state.run = facts.run;
                            state.status = facts.status;
                        }
                    }
                    json!({"outcome":"reconnected","kernelSessionId":kernel_id,"executionId":call,"events":events,"requests":requests})
                }
                _ => {
                    return Err(error(format!(
                        "Kernel backend operation {method} is unavailable."
                    )));
                }
            };
            Ok(Some(js(&value)?))
        })
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        let call = {
            let mut state = self.state.lock().unwrap();
            state.cancelled = Some(text(reason)?);
            if state.run.as_ref() == state.call_run.as_ref() {
                state.call.clone()
            } else {
                None
            }
        };
        if let Some(call) = call {
            let rpc = self.rpc.clone();
            let sid = self.kernel_id.clone();
            let reason = text(reason)?;
            tokio::spawn(async move {
                // A cancellation request is not evidence that an effect was undone.
                let _ = rpc
                    .lock()
                    .await
                    .call(
                        "execution.control",
                        json!({"session_id":sid,"call_ref":call,"kind":"cancel","detail":reason}),
                    )
                    .await;
            });
        }
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        self.state.lock().unwrap().disposed = true;
        Box::pin(async { Ok(()) })
    }
}
struct KernelTurn {
    rpc: Arc<tokio::sync::Mutex<Rpc>>,
    kernel_id: String,
    session: String,
    state: Arc<Mutex<State>>,
    started: Arc<AtomicBool>,
    input: Option<Value>,
    run: String,
    queue: VecDeque<JsValue>,
    done: bool,
    final_event: Option<Value>,
    failure_event: bool,
    accepted: bool,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
}
impl LiveTurn for KernelTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            loop {
                if let Some(event) = self.queue.pop_front() {
                    return Ok(Some(event));
                }
                if self.done {
                    return Ok(None);
                }
                if let Some(input) = self.input.take() {
                    let original = recover(&mut *self.rpc.lock().await, &self.kernel_id).await?;
                    if original.run.as_deref().is_some_and(|run| run != self.run) {
                        let mut state = self.state.lock().unwrap();
                        let disposed = state.disposed;
                        *state = original;
                        state.disposed = disposed;
                        return Err(error(
                            "Kernel has a different active execution; query or cancel that original execution before submitting a new turn.",
                        ));
                    }
                    let accepted=self.rpc.lock().await.call("execution.input",json!({"session_id":self.kernel_id,"idempotency_key":self.run,"input":input})).await?;
                    let call = accepted["call_ref"]
                        .as_str()
                        .ok_or_else(|| error("Kernel input omitted execution identity."))?
                        .to_string();
                    self.accepted = true;
                    {
                        let mut state = self.state.lock().unwrap();
                        state.call = Some(call.clone());
                        state.call_run = Some(self.run.clone());
                    }
                    // A duplicate idempotency key observes its original journal
                    // range; it never starts a replacement provider execution.
                    let journal = self
                        .rpc
                        .lock()
                        .await
                        .call(
                            "execution.events",
                            json!({"session_id":self.kernel_id,"after":0}),
                        )
                        .await?;
                    if let Some(cursor) = journal
                        .as_array()
                        .and_then(|events| {
                            events.iter().find(|event| {
                                event["kind"] == "call.accepted" && event["call_ref"] == call
                            })
                        })
                        .and_then(|event| event["cursor"].as_u64())
                    {
                        self.state.lock().unwrap().cursor = cursor.saturating_sub(1);
                    }
                    let cancelled = self.state.lock().unwrap().cancelled.clone();
                    if let Some(reason) = cancelled {
                        self.rpc.lock().await.call("execution.control",json!({"session_id":self.kernel_id,"call_ref":call,"kind":"cancel","detail":reason})).await?;
                    }
                    self.started.store(true, Ordering::SeqCst);
                    for kind in ["turn_started", "input_accepted", "user_prompt_submitted"] {
                        self.queue.push_back(js(&json!({"type":kind,"sessionId":self.session,"turnId":self.run,"input":input}))?);
                    }
                }
                let after = self.state.lock().unwrap().cursor;
                let events = self
                    .rpc
                    .lock()
                    .await
                    .call(
                        "execution.events",
                        json!({"session_id":self.kernel_id,"after":after}),
                    )
                    .await?;
                for event in events
                    .as_array()
                    .ok_or_else(|| error("Kernel events must be an array."))?
                {
                    let cursor = event["cursor"]
                        .as_u64()
                        .ok_or_else(|| error("Kernel event cursor missing."))?;
                    if cursor <= self.state.lock().unwrap().cursor {
                        continue;
                    }
                    self.state.lock().unwrap().cursor = cursor;
                    if event["correlation_id"] != self.run {
                        continue;
                    }
                    if event["state"] == "needs_human" {
                        if let (Some(wait), Some(call)) = (
                            event["payload"]["waiting"]["id"].as_str(),
                            event["call_ref"].as_str(),
                        ) {
                            let fresh = self
                                .state
                                .lock()
                                .unwrap()
                                .waits
                                .insert(wait.into(), call.into())
                                .is_none();
                            if fresh {
                                if let Some(interactions) = &self.interactions {
                                    interactions.reconnect_port().set_external_pending(
                                        &JsString::from(self.session.clone()),
                                        true,
                                    );
                                }
                                self.queue.push_back(js(&json!({"type":"permission_request","sessionId":self.session,"turnId":self.run,"requestId":wait,"executionId":call,"toolName":event["payload"]["waiting"]["schema"]["tool_name"],"payload":event["payload"]["waiting"]["schema"]}))?);
                            }
                        }
                    }
                    let root = self.state.lock().unwrap().call.clone();
                    if event["call_ref"].as_str() != root.as_deref() {
                        continue;
                    }
                    if let Some(agent_event) = event["payload"]["content"].get("agent_event") {
                        let mut agent_event = agent_event.clone();
                        agent_event["sessionId"] = json!(self.session);
                        agent_event["turnId"] = json!(self.run);
                        if agent_event["type"] == "turn_completed" {
                            self.final_event = Some(agent_event);
                        } else {
                            if agent_event["type"] == "turn_failed" {
                                self.failure_event = true;
                            }
                            self.queue.push_back(js(&agent_event)?);
                        }
                    }
                    if event["kind"] == "response" {
                        if let Some(content) = event["payload"]["content"].as_str() {
                            self.queue.push_back(js(&json!({"type":"model_event","sessionId":self.session,"event":{"type":"text_delta","text":content}}))?);
                        }
                    }
                }
                // The controller may complete while its loop/children still run;
                // only a terminal publication ends the product turn.
                let root = self.state.lock().unwrap().call.clone();
                let terminal = events.as_array().unwrap().iter().rev().find(|e| {
                    matches!(e["kind"].as_str(), Some("response"))
                        && e["correlation_id"] == self.run
                        && e["call_ref"].as_str() == root.as_deref()
                        && matches!(
                            e["payload"]["outcome"].as_str(),
                            Some("completed" | "failed" | "cancelled" | "unknown")
                        )
                });
                if let Some(event) = terminal.filter(|_| !self.done) {
                    let outcome = event["payload"]["outcome"].as_str().unwrap();
                    {
                        let mut state = self.state.lock().unwrap();
                        state.status = outcome.into();
                        state.run = None;
                    }
                    if let Some(interactions) = &self.interactions {
                        interactions
                            .reconnect_port()
                            .set_external_pending(&JsString::from(self.session.clone()), false);
                    }
                    if outcome == "failed" && !self.failure_event {
                        let original = &event["payload"]["error"];
                        let message = original["params"]["detail"]
                            .as_str()
                            .or_else(|| original["message"].as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| original.to_string());
                        self.queue.push_back(js(&json!({"type":"turn_failed","sessionId":self.session,"turnId":self.run,"error":{"code":original["code"].as_str().unwrap_or("KERNEL_EXECUTION_FAILED"),"message":message,"details":{"kernelSessionId":self.kernel_id,"executionId":root,"kernelError":original},"userHint":"Inspect the original Kernel execution and error before retrying."}}))?);
                    }
                    if outcome == "unknown" {
                        self.queue.push_back(js(&json!({"type":"turn_failed","sessionId":self.session,"turnId":self.run,"error":{"code":"KERNEL_OUTCOME_UNKNOWN","message":"Execution stopped waiting; an external effect may still have happened. Inspect the original execution before retrying.","details":{"kernelSessionId":self.kernel_id,"executionId":root,"outcome":"unknown"}}}))?);
                    }
                    let mut final_event = self.final_event.take().unwrap_or_else(|| {
                        json!({"type":"turn_completed","sessionId":self.session,"turnId":self.run,"result":{"type":if outcome=="completed"{"success"}else if outcome=="cancelled"{"aborted"}else{"error"},"stopReason":if outcome=="completed"{"completed"}else if outcome=="unknown"{"kernel_outcome_unknown"}else{outcome},"usage":{},"permissionDenials":[],"turns":0}})
                    });
                    if outcome == "unknown" {
                        final_event["result"]["type"] = json!("error");
                        final_event["result"]["stopReason"] = json!("kernel_outcome_unknown");
                    }
                    final_event["status"] = json!(outcome);
                    final_event["kernelSessionId"] = json!(self.kernel_id);
                    self.queue.push_back(js(&final_event)?);
                    self.state.lock().unwrap().attached = false;
                    self.done = true;
                }
                if self.queue.is_empty() && !self.done {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        })
    }
}
impl Drop for KernelTurn {
    fn drop(&mut self) {
        if !self.done {
            let mut state = self.state.lock().unwrap();
            if self.accepted {
                state.status = "unknown".into();
                state.run = None;
            } else if state.run.as_deref() == Some(&self.run) {
                state.run = None;
                state.status = "idle".into();
            }
            state.attached = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn event(cursor: u64, kind: &str, state: Option<&str>, payload: Value) -> Value {
        json!({"id":format!("e{cursor}"),"session_ref":"kernel-s","call_ref":"original-loop","correlation_id":"turn-1","kind":kind,"state":state,"payload":payload,"cursor":cursor})
    }
    fn session(socket: PathBuf) -> KernelSession {
        KernelSession {
            rpc: Arc::new(tokio::sync::Mutex::new(Rpc {
                socket,
                stream: None,
                next: 0,
                node: "test-node".into(),
                lease: None,
            })),
            kernel_id: "kernel-s".into(),
            session: "pd-s".into(),
            state: Arc::new(Mutex::new(State::default())),
            started: Arc::new(AtomicBool::new(false)),
            context_ref: "default".into(),
            interactions: None,
            agent_config: JsObject::new(),
            permission_mode_source: None,
        }
    }
    async fn next(turn: &mut dyn LiveTurn) -> Value {
        json_value(
            &tokio::time::timeout(std::time::Duration::from_secs(2), turn.next())
                .await
                .expect("bounded event")
                .expect("event result")
                .expect("event"),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn native_text_permission_and_terminal_use_original_scheduler_execution() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut answered = false;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => {
                        assert_eq!(req["params"]["abi_version"], 2);
                        json!({})
                    }
                    "execution.input" => {
                        assert_eq!(req["params"]["session_id"], "kernel-s");
                        assert_eq!(req["params"]["idempotency_key"], "turn-1");
                        json!({"call_ref":"original-loop"})
                    }
                    "execution.events" if !answered => json!([event(
                        1,
                        "call.result",
                        Some("needs_human"),
                        json!({"waiting":{"id":"original-approval","question":"allow tool?","schema":{"toolName":"write_file"}}})
                    )]),
                    "execution.respond" => {
                        assert_eq!(req["params"]["call_ref"], "original-loop");
                        assert_eq!(req["params"]["wait_id"], "original-approval");
                        assert_eq!(req["params"]["messages"][0]["text"], "allow_once");
                        answered = true;
                        Value::Null
                    }
                    "execution.events" => json!([
                        event(
                            2,
                            "progress",
                            Some("running"),
                            json!({"content":{"agent_event":{"type":"model_event","sessionId":"kernel-s","event":{"type":"text_delta","text":"done"}}}})
                        ),
                        event(
                            3,
                            "progress",
                            Some("running"),
                            json!({"content":{"agent_event":{"type":"turn_completed","sessionId":"kernel-s","result":{"status":"success"}}}})
                        ),
                        event(
                            4,
                            "response",
                            Some("completed"),
                            json!({"content":{"text":"done"},"outcome":"completed"})
                        )
                    ]),
                    other => panic!("Unexpected native request {other}"),
                };
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":req["id"],"result":result})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let session = session(socket.clone());
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"write something"})).unwrap(),
                js(&json!({"turnId":"turn-1"})).unwrap(),
            )
            .unwrap();
        for kind in ["turn_started", "input_accepted", "user_prompt_submitted"] {
            assert_eq!(next(turn.as_mut()).await["type"], kind);
        }
        let permission = next(turn.as_mut()).await;
        assert_eq!(permission["type"], "permission_request");
        assert_eq!(permission["requestId"], "original-approval");
        let response = session
            .backend_request(
                "permission_decide".into(),
                js(&json!({"requestId":"original-approval","decision":"allow_once"})).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.get("delivered"), Some(&JsValue::Bool(true)));
        let text = next(turn.as_mut()).await;
        assert_eq!(text["sessionId"], "pd-s");
        assert_eq!(text["turnId"], "turn-1");
        assert_eq!(text["event"]["text"], "done");
        let completed = next(turn.as_mut()).await;
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["result"]["status"], "success");
        assert!(turn.next().await.unwrap().is_none());
        drop(turn);
        drop(session);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
    #[tokio::test]
    async fn transport_loss_retains_original_identity_and_never_replays_input() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for response in [json!({}), json!([]), json!({"call_ref":"original-loop"})] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":req["id"],"result":response})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let session = session(socket.clone());
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"effect"})).unwrap(),
                js(&json!({"turnId":"turn-1"})).unwrap(),
            )
            .unwrap();
        assert!(turn.next().await.is_err());
        drop(turn);
        assert_eq!(
            session.snapshot().get("executionId"),
            Some(&JsValue::from("original-loop"))
        );
        assert_eq!(
            session.snapshot().get("status"),
            Some(&JsValue::from("unknown"))
        );
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}

#[cfg(test)]
mod abi2_wire_tests {
    use super::*;
    use tokio::net::UnixListener;
    #[tokio::test]
    async fn abi2_bare_session_envelope_is_a_reply_and_notification_is_not() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for method in ["initialize", "session.create", "session.lease_acquire"] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                let reply = match method {
                    "initialize" => {
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"abi_version":2}})
                    }
                    "session.create" => {
                        json!({"data":{"session_id":"local-real-abi2","generation":1},"effect":"advance","epoch":0,"status":"done"})
                    }
                    _ => {
                        json!({"data":{"epoch":1,"expires_at_ms":500000},"effect":"advance","epoch":1,"status":"done"})
                    }
                };
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n{}\n",
                            json!({"jsonrpc":"2.0","method":"session/events","params":{}}),
                            reply
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let mut rpc = Rpc {
            socket: socket.clone(),
            stream: None,
            next: 0,
            node: "test".into(),
            lease: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rpc.call("session.create", json!({})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["data"]["session_id"], "local-real-abi2");
        let lease = rpc
            .call(
                "session.lease_acquire",
                json!({"session_id":"local-real-abi2"}),
            )
            .await
            .unwrap();
        assert_eq!(lease["data"]["epoch"], 1);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
    #[tokio::test]
    async fn cancelled_request_discards_its_connection_before_reconnecting() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                for step in 0..2 {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    if attempt == 0 && step == 1 {
                        let mut tail = String::new();
                        assert_eq!(stream.read_line(&mut tail).await.unwrap(), 0);
                        break;
                    }
                    let reply =
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"attempt":attempt}});
                    stream
                        .get_mut()
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let mut rpc = Rpc {
            socket: socket.clone(),
            stream: None,
            next: 0,
            node: "test".into(),
            lease: None,
        };
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                rpc.call("execution.get", json!({}))
            )
            .await
            .is_err()
        );
        assert!(rpc.stream.is_none());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rpc.call("execution.get", json!({})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["attempt"], 1);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}

#[cfg(test)]
mod recovery_contract_tests {
    use super::*;
    use tokio::net::UnixListener;
    fn session(socket: PathBuf) -> KernelSession {
        KernelSession {
            rpc: Arc::new(tokio::sync::Mutex::new(Rpc {
                socket,
                stream: None,
                next: 0,
                node: "test".into(),
                lease: None,
            })),
            kernel_id: "native-session".into(),
            session: "pd-session".into(),
            state: Arc::new(Mutex::new(State::default())),
            started: Arc::new(AtomicBool::new(false)),
            context_ref: "default".into(),
            interactions: None,
            agent_config: JsObject::new(),
            permission_mode_source: None,
        }
    }
    #[tokio::test]
    async fn import_reads_and_sends_original_session_generation_not_lease_epoch() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for method in ["initialize", "session.get", "execution.history.import"] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], method);
                let reply = if method == "session.get" {
                    json!({"status":"done","effect":"observe","data":{"generation":7,"epoch":99}})
                } else {
                    if method == "execution.history.import" {
                        assert_eq!(req["params"]["generation"], 7);
                        assert_eq!(req["params"]["context_ref"], "default");
                        assert_eq!(req["params"]["idempotency_key"], "pd-import:verified-hash");
                        assert_eq!(req["params"]["expected_revision"], 0);
                    }
                    json!({"jsonrpc":"2.0","id":req["id"],"result":{"revision":1,"receipt":"original-receipt"}})
                };
                stream
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let session = session(socket.clone());
        let result=session.backend_request("restore_session_transcript".into(),js(&json!({"sessionKey":"new","transcript":{"sha256":"verified-hash","messages":[{"role":"user","content":[{"type":"text","text":"old"}]}]}})).unwrap()).await.unwrap().unwrap();
        assert_eq!(result.get("imported"), Some(&JsValue::Bool(true)));
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
    #[tokio::test]
    async fn reopened_actual_active_execution_rejects_distinct_new_input_without_replay() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for method in ["initialize", "execution.events", "execution.get"] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], method);
                let result = match method {
                    "execution.events" => {
                        json!([{"kind":"call.accepted","call_ref":"original-active","correlation_id":"old-run","cursor":1,"payload":{"cause_execution_ref":null}}])
                    }
                    "execution.get" => {
                        json!({"state":"running","invocation":{"correlation_id":"old-run"}})
                    }
                    _ => json!({}),
                };
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":req["id"],"result":result})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            let mut line = String::new();
            assert_eq!(
                stream.read_line(&mut line).await.unwrap(),
                0,
                "no second execution.input may be sent"
            );
        });
        let session = session(socket.clone());
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"new"})).unwrap(),
                js(&json!({"turnId":"new-run"})).unwrap(),
            )
            .unwrap();
        assert!(
            turn.next()
                .await
                .unwrap_err()
                .message
                .contains("different active execution")
        );
        drop(turn);
        assert_eq!(
            session.snapshot().get("executionId"),
            Some(&JsValue::from("original-active"))
        );
        assert_eq!(
            session.snapshot().get("status"),
            Some(&JsValue::from("running"))
        );
        drop(session);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
    #[tokio::test]
    async fn native_turn_preferences_reach_wire_without_overriding_kernel_model_route() {
        let socket = std::env::temp_dir().join(format!("pd-kernel-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut accepted = false;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "execution.events" if !accepted => json!([]),
                    "execution.input" => {
                        let agent = &req["params"]["input"]["agent"];
                        assert_eq!(agent["permissionMode"], "bypassPermissions");
                        assert_eq!(
                            agent["thinking"],
                            json!({"enabled":true,"budgetTokens":1024})
                        );
                        assert_eq!(agent["appendSystemPrompt"], "write concise answers");
                        assert!(agent.get("provider").is_none());
                        assert!(agent.get("model").is_none());
                        assert_eq!(req["params"]["input"]["runMode"], "plan");
                        accepted = true;
                        json!({"call_ref":"original"})
                    }
                    "execution.events" => {
                        json!([{"kind":"response","call_ref":"original","correlation_id":"run","cursor":1,"payload":{"outcome":"completed","content":"done"}}])
                    }
                    other => panic!("unexpected {other}"),
                };
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":req["id"],"result":result})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let mut session = session(socket.clone());
        session.agent_config=js(&json!({"provider":"must-not-override","model":"must-not-override","thinking":{"enabled":true,"budgetTokens":1024},"appendSystemPrompt":"write concise answers"})).unwrap().as_object().unwrap().clone();
        session.permission_mode_source = Some(Arc::new(|| Some("bypassPermissions".into())));
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"hello"})).unwrap(),
                js(&json!({"turnId":"run","runMode":"plan"})).unwrap(),
            )
            .unwrap();
        while turn.next().await.unwrap().is_some() {}
        drop(turn);
        drop(session);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
