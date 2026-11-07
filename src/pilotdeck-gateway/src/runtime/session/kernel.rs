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
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
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
#[derive(Clone, Debug)]
pub struct KernelBackendFactory {
    pub socket: PathBuf,
    pub loop_version: String,
    pub owner_path: PathBuf,
    pub project: PathBuf,
    pub session: JsString,
}
pub enum BackendOwner {
    Legacy,
    Kernel(KernelBackendFactory),
}
impl KernelBackendFactory {
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
        if mode == "legacy" || !binding.projection.snapshot()?.messages.is_empty() {
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
}
impl Rpc {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            if self.stream.is_none() {
                self.stream = Some(BufReader::new(
                    UnixStream::connect(&self.socket).await.map_err(error)?,
                ));
                self.exchange(
                    "initialize",
                    json!({"abi_version":2,"api_version":"0.1.0","features":[]}),
                )
                .await?;
            }
            self.exchange(method, params).await
        })
        .await
        .map_err(|_| {
            error("Kernel request timed out; query the original execution before retrying effects.")
        })?
    }
    async fn exchange(&mut self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        self.next += 1;
        let id = self.next;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| error("Kernel connection unavailable."))?;
        stream
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(error)?;
        loop {
            let mut bytes = Vec::new();
            let n = (&mut *stream)
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
                continue;
            }
            if reply["id"] != id {
                return Err(error("Kernel response identity mismatch."));
            }
            if let Some(e) = reply.get("error") {
                return Err(error(format!("Kernel request failed: {e}")));
            }
            return reply
                .get("result")
                .cloned()
                .ok_or_else(|| error("Kernel result missing."));
        }
    }
}
#[derive(Default)]
struct State {
    disposed: bool,
    run: Option<String>,
    call: Option<String>,
    status: String,
    cursor: u64,
    waits: BTreeMap<String, String>,
    cancelled: Option<String>,
}
struct KernelSession {
    rpc: Arc<tokio::sync::Mutex<Rpc>>,
    kernel_id: String,
    session: String,
    state: Arc<Mutex<State>>,
    started: Arc<AtomicBool>,
    context_ref: String,
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
            };
            let saved = std::fs::read(&self.owner_path)
                .ok()
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(error))
                .transpose()?;
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
            if saved.is_none() {
                write_owner(
                    &self.owner_path,
                    &json!({"schema":1,"backend":"kernel","project":self.project,"session":text(&self.session)?,"socket":self.socket,"loop_version":opened["loop_revision"],"kernel_session_id":kernel_id,"binding":binding,"context_ref":context_ref}),
                )?;
            }
            Ok(Box::new(KernelSession {
                rpc: Arc::new(tokio::sync::Mutex::new(rpc)),
                kernel_id,
                session: text(&self.session)?,
                state: Arc::new(Mutex::new(State::default())),
                started: Arc::new(AtomicBool::new(false)),
                context_ref,
            }) as Box<dyn SessionBackend>)
        })
    }
}
impl SessionBackend for KernelSession {
    fn permission_mode(&self) -> JsString {
        "default".into()
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
        let mut state = self.state.lock().unwrap();
        if state.disposed || state.run.is_some() {
            return Err(error("Kernel session is disposed or already running."));
        }
        state.run = Some(run.clone());
        state.status = "running".into();
        state.cancelled = None;
        Ok(Box::new(KernelTurn {
            rpc: self.rpc.clone(),
            kernel_id: self.kernel_id.clone(),
            session: self.session.clone(),
            state: self.state.clone(),
            started: self.started.clone(),
            input: Some(json_value(&input)?),
            run,
            queue: VecDeque::new(),
            done: false,
            final_event: None,
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
        Box::pin(async move {
            let params = json_value(&params)?;
            let (call, cursor) = {
                let s = state.lock().unwrap();
                (s.call.clone(), s.cursor)
            };
            let value = match method.as_str() {
                "active_turn_snapshot" => {
                    json!({"backend":"kernel","kernelSessionId":kernel_id,"executionId":call,"cursor":cursor})
                }
                "read_session_messages" | "export_session_transcript" => {
                    rpc.lock()
                        .await
                        .call(
                            "execution.history.snapshot",
                            json!({"session_id":kernel_id,"context_ref":context_ref}),
                        )
                        .await?
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
                    json!({"outcome":"reconnected","kernelSessionId":kernel_id,"executionId":call,"events":events})
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
            state.call.clone()
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
                    let accepted=self.rpc.lock().await.call("execution.input",json!({"session_id":self.kernel_id,"idempotency_key":self.run,"input":input})).await?;
                    let call = accepted["call_ref"]
                        .as_str()
                        .ok_or_else(|| error("Kernel input omitted execution identity."))?
                        .to_string();
                    self.state.lock().unwrap().call = Some(call.clone());
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
                            self.state
                                .lock()
                                .unwrap()
                                .waits
                                .insert(wait.into(), call.into());
                            self.queue.push_back(js(&json!({"type":"permission_request","sessionId":self.session,"turnId":self.run,"requestId":wait,"executionId":call,"payload":event["payload"]["waiting"]["schema"]}))?);
                        }
                    }
                    if let Some(agent_event) = event["payload"]["content"].get("agent_event") {
                        let mut agent_event = agent_event.clone();
                        agent_event["sessionId"] = json!(self.session);
                        agent_event["turnId"] = json!(self.run);
                        if agent_event["type"] == "turn_completed" {
                            self.final_event = Some(agent_event);
                        } else {
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
                let terminal = events.as_array().unwrap().iter().rev().find(|e| {
                    matches!(e["kind"].as_str(), Some("response"))
                        && e["correlation_id"] == self.run
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
                    let mut final_event=self.final_event.take().unwrap_or_else(|| json!({"type":"turn_completed","sessionId":self.session,"turnId":self.run}));
                    final_event["status"] = json!(outcome);
                    final_event["kernelSessionId"] = json!(self.kernel_id);
                    self.queue.push_back(js(&final_event)?);
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
            let mut s = self.state.lock().unwrap();
            s.status = "unknown".into();
            s.run = None;
        }
    }
}
