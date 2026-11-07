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
    collections::{BTreeMap, HashSet, VecDeque},
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
fn transport_error(e: impl std::fmt::Display) -> RuntimeError {
    let message = e.to_string();
    error(&message).with_js_error(crate::runtime::project::RuntimeJsError {
        name: "KernelTransportError".into(),
        code: Some("KERNEL_TRANSPORT_LOST".into()),
        message: message.into(),
        requires_host: false,
    })
}
fn js(value: &Value) -> Result<JsValue, RuntimeError> {
    parse_json(&value.to_string()).map_err(error)
}
pub(super) fn json_value(value: &JsValue) -> Result<Value, RuntimeError> {
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
pub struct KernelBackendFactory {
    preflight: Option<BoxFuture<crate::runtime::project::RuntimeResult>>,
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
    pub fn with_port_preflight(
        mut self,
        preflight: BoxFuture<crate::runtime::project::RuntimeResult>,
    ) -> Self {
        self.preflight = Some(preflight);
        self
    }
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
                    preflight: None,
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
            preflight: None,
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
#[derive(Clone)]
struct RpcOwner {
    session: String,
    binding: String,
    revision: Value,
    protocol: Value,
    permissions: Value,
    features: Vec<Value>,
}
impl RpcOwner {
    fn validate(&self, opened: &Value) -> Result<(), RuntimeError> {
        if !opened["input_controller_ref"].is_null()
            || opened["default_loop_ref"] != self.binding
            || opened["loop_revision"] != self.revision
            || opened["protocol"] != self.protocol
            || (self.permissions.is_object() && opened["loop_permissions"] != self.permissions)
        {
            return Err(error(
                "Kernel reconnected session does not match its persisted binding/revision/protocol/permissions owner.",
            ));
        }
        if self.features.iter().any(|feature| {
            !opened["features"]
                .as_array()
                .is_some_and(|features| features.contains(feature))
        }) {
            return Err(error(
                "Kernel reconnected host omitted an already-used session capability.",
            ));
        }
        Ok(())
    }
}
struct Rpc {
    owner: Option<RpcOwner>,
    socket: PathBuf,
    stream: Option<BufReader<UnixStream>>,
    next: u64,
    node: String,
    lease: Option<(String, u64, std::time::Instant)>,
}
impl Rpc {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let connected = self.stream.is_some();
        let result = self.call_guarded(method, params.clone(), None).await;
        let observation = matches!(
            method,
            "execution.get"
                | "execution.events"
                | "execution.history.snapshot"
                | "execution.history.archive"
                | "session.get"
                | "session.lease_status"
        );
        let transport_lost = result.as_ref().err().is_some_and(|error| {
            error.causes.iter().any(|cause| {
                cause.js_error().and_then(|error| error.code.as_deref())
                    == Some("KERNEL_TRANSPORT_LOST")
            })
        });
        let result = if connected && self.owner.is_some() && observation && transport_lost {
            self.call_guarded(method, params, None).await
        } else {
            result
        };
        result?.ok_or_else(|| error("Kernel RPC omitted its reply."))
    }
    async fn call_guarded(
        &mut self,
        method: &str,
        params: Value,
        admission: Option<(&Mutex<State>, &str)>,
    ) -> Result<Option<Value>, RuntimeError> {
        let mut phase = format!("{method}:connect");
        let timed=tokio::time::timeout(std::time::Duration::from_secs(30), async {
            if self.stream.is_none() {
                self.stream=Some(BufReader::new(UnixStream::connect(&self.socket).await.map_err(error)?));
                phase="initialize".into();
                self.exchange("initialize",json!({"abi_version":2,"api_version":"0.1.0","features":["model-session/1.0","model-response-metadata/1.0"],"node_id":self.node})).await?;
                if let Some(owner)=self.owner.clone() {
                    phase="execution.open:restore_owned_session".into();
                    let opened=self.exchange("execution.open",json!({"session_id":owner.session})).await?;
                    owner.validate(&opened)?;
                    // A new resident process must supply its actual lease state;
                    // an old local timestamp is never evidence of its authority.
                    self.lease=None;
                }
            }
            if matches!(method,"execution.input"|"execution.respond"|"execution.history.import") {
                let session=params["session_id"].as_str().ok_or_else(||error("Effect session identity is missing."))?.to_string();
                if self.lease.as_ref().is_none_or(|(held,_,since)|held!=&session||since.elapsed()>std::time::Duration::from_secs(60)) {
                    phase="session.lease_status".into();
                    let status=self.exchange("session.lease_status",json!({"session_id":session})).await?;
                    let data=&status["data"];
                    let active=data["active"].as_bool().ok_or_else(||error("Kernel lease activity is missing."))?;
                    let lease_method=if active {
                        if data["holder_node"].as_str()!=Some(self.node.as_str()){return Err(error("Kernel session has an active lease held by another node; observation remains available."));}
                        "session.lease_renew"
                    }else{"session.lease_acquire"};
                    phase=lease_method.into();
                    let grant=self.exchange(lease_method,json!({"session_id":session,"epoch":data["epoch"],"ttl_ms":300000})).await?;
                    let epoch=grant["data"]["epoch"].as_u64().filter(|epoch|*epoch>0).ok_or_else(||error("Kernel lease epoch is missing."))?;
                    self.lease=Some((session,epoch,std::time::Instant::now()));
                }
            }
            phase=method.into();
            if let Some((state,run))=admission {
                let state=state.lock().unwrap();
                let original=state.call.is_some() && state.call_run.as_deref()==Some(run);
                if state.cancelled.is_some() && !original { return Ok(None); }
            }
            self.exchange(method,params).await.map(Some)
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
            let mut failure =
                error(message.clone()).with_js_error(crate::runtime::project::RuntimeJsError {
                    name: "KernelBackendError".into(),
                    code: Some("KERNEL_EXECUTION_UNAVAILABLE".into()),
                    message: message.into(),
                    requires_host: false,
                });
            failure.causes.push(cause);
            failure
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
            .map_err(transport_error)?;
        loop {
            let mut bytes = Vec::new();
            let n = (&mut stream)
                .take(16 * 1024 * 1024)
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(transport_error)?;
            if n == 0 {
                return Err(transport_error(
                    "Kernel connection ended; execution outcome is unknown.",
                ));
            }
            if bytes.last() != Some(&b'\n') {
                return Err(error(
                    "Kernel frame exceeded 16 MiB; execution outcome is unknown.",
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
                        | "session.lease_status"
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
    input: Option<Value>,
    accepted_at_ms: Option<u64>,
    status: String,
    cursor: u64,
    waits: BTreeMap<String, String>,
    seen_waits: HashSet<String>,
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
        state.accepted_at_ms = root["payload"]["accepted_at_ms"]
            .as_u64()
            .filter(|value| *value <= 9_007_199_254_740_991);
        state.input = Some(view["invocation"]["payload"].clone());
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
        // Child waits retain their original execution identity; observing a
        // parent never grants or fabricates a second approval for its child.
        let mut candidates = HashSet::new();
        for event in events
            .iter()
            .filter(|event| event["correlation_id"].as_str() == state.call_run.as_deref())
        {
            if let Some(wait) = event["payload"]["waiting"]["id"].as_str() {
                state.seen_waits.insert(wait.into());
            }
            if event["state"] == "needs_human" {
                if let Some(child) = event["call_ref"].as_str() {
                    if child != call {
                        candidates.insert(child.to_string());
                    }
                }
            }
        }
        if state.run.is_some() {
            for child in candidates {
                let view = rpc
                    .call(
                        "execution.get",
                        json!({"session_id":session,"call_ref":child}),
                    )
                    .await?;
                if view["state"] == "needs_human" {
                    if let Some(wait) = view["waiting"]["id"].as_str() {
                        state.waits.insert(wait.into(), child);
                    }
                }
            }
        }
    } else {
        state.status = "idle".into();
    }
    Ok(state)
}
/// Every display row comes from a validated, committed native history version.
/// A snapshot pins the target revision; newer entries on later pages wait for
/// the next read rather than being mistaken for an atomic multi-page snapshot.
async fn projection_history(
    rpc: &mut Rpc,
    kernel: &str,
    context: &str,
    projection: Option<&Arc<super::kernel_projection::KernelProjection>>,
) -> Result<Value, RuntimeError> {
    let mut snapshot = rpc
        .call(
            "execution.history.snapshot",
            json!({"session_id":kernel,"context_ref":context}),
        )
        .await?;
    let Some(projection) =
        projection.filter(|projection| projection.archive_enabled.load(Ordering::SeqCst))
    else {
        return Ok(snapshot);
    };
    let target = snapshot["revision"]
        .as_u64()
        .ok_or_else(|| error("Kernel history revision is missing."))?;
    let mut archive = projection.archive.lock().await;
    if archive.revision > target {
        return Err(error("Kernel archive revision moved backwards."));
    }
    if target == 0 {
        archive.seed(&snapshot["history"])?;
    }
    while archive.revision < target {
        let page = rpc.call("execution.history.archive",json!({"session_id":kernel,"context_ref":context,"after_revision":archive.revision,"limit":16,"max_bytes":4*1024*1024})).await?;
        let before = archive.revision;
        for entry in page["entries"]
            .as_array()
            .ok_or_else(|| error("Kernel archive page omitted entries."))?
        {
            if entry["revision"]
                .as_u64()
                .is_some_and(|revision| revision > target)
            {
                break;
            }
            archive.apply(entry)?;
        }
        if archive.revision == before {
            return Err(error(
                "Kernel archive omitted the pinned committed history revision.",
            ));
        }
    }
    snapshot["displayHistory"] = archive.display();
    snapshot["archiveRevision"] = json!(archive.revision);
    Ok(snapshot)
}

fn with_index(mut history: Value, facts: &State) -> Value {
    if !history.is_object() {
        return history;
    }
    history["kernelIndex"] = match (
        facts.call.as_ref(),
        facts.call_run.as_ref(),
        facts.input.as_ref(),
    ) {
        (Some(call), Some(run), Some(input))
            if input["type"] == "text" && input["text"].is_string() =>
        {
            json!({"executionId":call,"runId":run,"status":facts.status,"prompt":input["text"],"active":facts.run.is_some(),"acceptedAt":facts.accepted_at_ms.map(|ms|pilotdeck_transport::system_clock::format_epoch_iso(ms as i64))})
        }
        _ => Value::Null,
    };
    history
}
#[derive(Default)]
struct NativePortCheck {
    pending: Option<BoxFuture<crate::runtime::project::RuntimeResult>>,
    result: Option<crate::runtime::project::RuntimeResult>,
}
impl NativePortCheck {
    async fn check(&mut self) -> crate::runtime::project::RuntimeResult {
        if let Some(pending) = self.pending.take() {
            // Cancellation cannot consume the check and turn a later retry
            // into permission to bypass it. Reprepare explicitly if interrupted.
            self.result = Some(Err(super::kernel_config::unavailable(
                "interrupted product port preflight",
            )));
            self.result = Some(pending.await);
        }
        self.result.clone().unwrap_or(Ok(()))
    }
}
struct KernelSession {
    compact_supported: bool,
    input_limits_supported: bool,
    port_check: Arc<tokio::sync::Mutex<NativePortCheck>>,
    projection: Option<Arc<super::kernel_projection::KernelProjection>>,
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
impl KernelSession {
    fn prepare_turn(
        &self,
        input: JsValue,
        options: JsValue,
        native_command: bool,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        let options = json_value(&options)?;
        let limits = self.input_limits(&options)?;
        let run = options["turnId"]
            .as_str()
            .or_else(|| options["runId"].as_str())
            .ok_or_else(|| error("Kernel turn requires turnId or runId."))?
            .to_string();
        let mut payload = json_value(&input)?;
        if !payload.is_object() {
            return Err(error("Native input must be an object."));
        }
        if payload.get("nativeCommand").is_some() && !native_command {
            return Err(error(
                "nativeCommand is reserved for the installed backend command port.",
            ));
        }
        if payload["type"] == "blocks" {
            return Err(error(
                "Native block/attachment ingress is awaiting its canonical binding; no input was accepted.",
            ));
        }

        super::kernel_config::preflight(&payload)?;
        super::kernel_config::preflight(&options)?;
        for container in [
            &payload,
            &payload["agent"],
            &payload["contextOverride"],
            &options,
            &options["agent"],
        ] {
            if container.get("modelPricingEntries").is_some() {
                return Err(super::kernel_config::unavailable(
                    "browser modelPricingEntries override; pricing is owned by the leased product router",
                ));
            }
        }
        let config = json_value(&JsValue::Object(self.agent_config.clone()))?;
        super::kernel_config::preflight(&config)?;
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
            "modelPricingEntries",
        ] {
            if let Some(value) = config.get(key) {
                agent.insert(key.into(), value.clone());
            }
        }
        agent.insert(
            "permissionMode".into(),
            json!(text(&self.turn_permission_mode(&options)?)?),
        );
        payload["agent"] = Value::Object(agent);
        for key in ["runMode", "allowPlanModeTools", "basePermissionMode"] {
            if let Some(value) = options.get(key) {
                payload[key] = value.clone();
            }
        }
        if payload
            .get("basePermissionMode")
            .is_some_and(|value| !matches!(value.as_str(), Some("default" | "bypassPermissions")))
        {
            return Err(error("Native basePermissionMode is invalid."));
        }
        let mut state = self.state.lock().unwrap();
        if state.disposed || state.run.is_some() {
            return Err(error(if native_command {
                "Kernel session is not idle for manual compaction."
            } else {
                "Kernel session is disposed or already running."
            }));
        }
        state.run = Some(run.clone());
        state.status = "running".into();
        state.cancelled = None;
        state.attached = true;
        self.started.store(false, Ordering::SeqCst);
        Ok(Box::new(KernelTurn {
            limits,
            port_check: self.port_check.clone(),
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
            projection: self.projection.clone(),
            context_ref: self.context_ref.clone(),
        }))
    }
    fn input_limits(&self, options: &Value) -> Result<Option<Value>, RuntimeError> {
        let Some(deadline) = options["execution"]
            .get("operationDeadline")
            .filter(|value| !value.is_null())
        else {
            return Ok(None);
        };
        if !self.input_limits_supported {
            return Err(super::kernel_config::unavailable(
                "scheduler.input-limits.v1",
            ));
        }
        let deadline = deadline
            .as_str()
            .ok_or_else(|| error("Native operationDeadline must be an ISO date string."))?;
        let millis =
            pilotdeck_transport::system_clock::parse_node22_date(&JsString::from(deadline))
                .map_err(|cause| error(String::from_utf16_lossy(cause.units())))?
                .filter(|value| {
                    value.is_finite()
                        && *value >= 0.0
                        && value.fract() == 0.0
                        && *value <= 9_007_199_254_740_991.0
                })
                .ok_or_else(|| error("Native operationDeadline is invalid."))?;
        Ok(Some(json!({"deadline_ms":millis as u64})))
    }
    fn turn_permission_mode(&self, options: &Value) -> Result<JsString, RuntimeError> {
        let requested = options.get("mode").filter(|value| !value.is_null());
        if requested.is_some_and(|value| {
            !matches!(
                value.as_str(),
                Some("default" | "plan" | "bypassPermissions")
            )
        }) {
            return Err(error("Native per-turn mode is invalid."));
        }
        Ok(crate::runtime::permission::effective_permission_mode(
            &self.agent_config,
            requested
                .and_then(Value::as_str)
                .map(JsString::from)
                .or_else(|| Some(self.permission_mode())),
        ))
    }
}
impl SessionBackendFactory for KernelBackendFactory {
    fn build(
        mut self: Box<Self>,
        context: BackendContext,
    ) -> BoxFuture<Result<Box<dyn SessionBackend>, RuntimeError>> {
        Box::pin(async move {
            let mut rpc = Rpc {
                owner: None,
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
            if opened["loop_revision"].as_str().is_none() {
                return Err(error("Kernel did not expose the immutable loop revision."));
            }
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
            // Reopening observation never acquires execution authority. The
            // first effect renews this same owner's live lease or acquires an
            // inactive lease from authoritative public status.
            if saved.is_none() {
                write_owner(
                    &self.owner_path,
                    &json!({"schema":1,"backend":"kernel","project":self.project,"session":text(&self.session)?,"socket":self.socket,"loop_version":opened["loop_revision"],"kernel_session_id":kernel_id,"binding":binding,"context_ref":context_ref,"node_id":rpc.node}),
                )?;
            }
            rpc.owner = Some(RpcOwner {
                session: kernel_id.clone(),
                binding: binding.into(),
                revision: opened["loop_revision"].clone(),
                protocol: opened["protocol"].clone(),
                permissions: opened["loop_permissions"].clone(),
                features: opened["features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|feature| {
                        matches!(
                            feature.as_str(),
                            Some("scheduler.input-limits.v1" | "scheduler.history-archive.v1")
                        )
                    })
                    .cloned()
                    .collect(),
            });
            let recovered = recover(&mut rpc, &kernel_id).await?;
            let projection = Arc::new(super::kernel_projection::KernelProjection::new(
                context.recorder,
                context.reload_source.projection,
                text(&self.session)?,
                kernel_id.clone(),
                context_ref.clone(),
            ));
            projection.enable_archive(opened["features"].as_array().is_some_and(|features| {
                features
                    .iter()
                    .any(|feature| feature == "scheduler.history-archive.v1")
            }));
            let history =
                projection_history(&mut rpc, &kernel_id, &context_ref, Some(&projection)).await?;
            projection.sync(&with_index(history, &recovered))?;

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
                compact_supported: opened["features"].as_array().is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature == "scheduler.history-archive.v1")
                }) && opened["loop_permissions"]["resources"]
                    .as_array()
                    .is_some_and(|resources| {
                        resources
                            .iter()
                            .any(|resource| resource == "native.command.compact.v1")
                    }),
                input_limits_supported: opened["features"].as_array().is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature == "scheduler.input-limits.v1")
                }),
                port_check: Arc::new(tokio::sync::Mutex::new(NativePortCheck {
                    pending: self.preflight.take(),
                    result: None,
                })),
                projection: Some(projection),
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
        self.prepare_turn(input, options, false)
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
        if method == "kernel_manual_compact_v1" {
            if !self.compact_supported {
                return Box::pin(async {
                    Err(super::kernel_config::unavailable(
                        "native.command.compact.v1 with scheduler.history-archive.v1",
                    ))
                });
            }
            let prepared = (|| {
                let params = json_value(&params)?;
                self.prepare_turn(
                    js(
                        &json!({"nativeCommand":{"kind":"compact","version":1,"manualForce":true}}),
                    )?,
                    js(&json!({"turnId":params["turnId"]}))?,
                    true,
                )
            })();
            return Box::pin(async move {
                let mut turn = prepared?;
                let mut result = None;
                while let Some(event) = turn.next().await? {
                    let event = json_value(&event)?;
                    if event["type"] == "turn_completed" {
                        if event["kernelAccepted"] == false && event["result"]["type"] == "aborted"
                        {
                            result = Some(js(&event["result"])?);
                            continue;
                        }
                        if event["status"] == "cancelled" {
                            result = Some(js(
                                &json!({"type":"aborted","kernelSessionId":event["kernelSessionId"],"executionId":event["executionId"],"nativeStatus":"cancelled"}),
                            )?);
                            continue;
                        }
                        if event["status"] == "failed"
                            && event["nativeResult"]["command"]["kind"] == "compact"
                            && event["nativeResult"]["command"]["version"] == 1
                            && event["nativeResult"]["command"]["result"]["type"] == "failed"
                        {
                            result = Some(js(&event["nativeResult"]["command"]["result"])?);
                            continue;
                        }
                        if event["status"] != "completed" {
                            let message = format!(
                                "Kernel compact original execution outcome {}: {}",
                                event["status"], event
                            );
                            let failure = error(&message);
                            return Err(if event["status"] == "unknown" {
                                failure.with_js_error(crate::runtime::project::RuntimeJsError {
                                    name: "KernelOutcomeUnknown".into(),
                                    code: Some("result_unknown".into()),
                                    message: message.into(),
                                    requires_host: false,
                                })
                            } else {
                                failure
                            });
                        }
                        let command = &event["nativeResult"]["command"];
                        if command["kind"] != "compact"
                            || command["version"] != 1
                            || !command["result"].is_object()
                        {
                            return Err(error(
                                "Kernel compact terminal omitted its installed command result.",
                            ));
                        }
                        result = Some(js(&command["result"])?);
                    }
                }
                result.map(Some).ok_or_else(|| {
                    error("Kernel compact ended without a committed command terminal.")
                })
            });
        }
        let rpc = self.rpc.clone();
        let projection = self.projection.clone();
        let config = self.agent_config.clone();
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
                    let journal = rpc
                        .call(
                            "execution.events",
                            json!({"session_id":kernel_id,"after":0}),
                        )
                        .await?;
                    if let Some(projection) = &projection {
                        let history = projection_history(
                            &mut rpc,
                            &kernel_id,
                            &context_ref,
                            Some(&projection),
                        )
                        .await?;
                        projection.sync(&with_index(history, &facts))?;
                    }
                    let attached = state.lock().unwrap().attached;
                    let value = json!({"backend":"kernel","kernelSessionId":kernel_id,"executionId":facts.call,"runId":facts.call_run,"cursor":facts.cursor,"execution":current,"status":facts.status,"journal":journal,"attached":attached});
                    let mut state = state.lock().unwrap();
                    if !state.attached {
                        let disposed = state.disposed;
                        *state = facts;
                        state.disposed = disposed;
                    }
                    value
                }
                "session_model_get" => {
                    let config = json_value(&JsValue::Object(config))?;
                    json!({"projectKey":params["projectKey"],"sessionKey":text(&session)?,"saved":{"mode":"model","provider":config["provider"],"model":config["model"]},"effective":{"provider":config["provider"],"model":config["model"],"source":"kernel_assembly"},"immutable":true})
                }
                "read_session_messages" | "export_session_transcript" => {
                    let history = projection_history(
                        &mut *rpc.lock().await,
                        &kernel_id,
                        &context_ref,
                        projection.as_ref(),
                    )
                    .await?;
                    let facts = recover(&mut *rpc.lock().await, &kernel_id).await?;
                    let indexed = with_index(history.clone(), &facts);
                    if let Some(projection) = &projection {
                        projection.sync(&indexed)?;
                    }
                    let mut display = history
                        .get("displayHistory")
                        .unwrap_or(&history["history"])
                        .clone();
                    if display.as_array().is_some_and(|messages| {
                        messages
                            .iter()
                            .any(|message| !message["metadata"]["turnId"].is_string())
                    }) {
                        let mut rpc = rpc.lock().await;
                        let journal = rpc
                            .call(
                                "execution.events",
                                json!({"session_id":kernel_id,"after":0}),
                            )
                            .await?;
                        let mut roots = Vec::new();
                        for event in journal.as_array().into_iter().flatten().filter(|event| {
                            event["kind"] == "call.accepted"
                                && event["payload"]["cause_execution_ref"].is_null()
                        }) {
                            let root = rpc
                                .call(
                                    "execution.get",
                                    json!({"session_id":kernel_id,"call_ref":event["call_ref"]}),
                                )
                                .await?;
                            roots.push(root);
                        }
                        display = super::kernel_projection::reconcile_identity(
                            &display, &journal, &roots,
                        );
                    }
                    let created = projection
                        .as_ref()
                        .and_then(|projection| {
                            facts
                                .call_run
                                .as_deref()
                                .and_then(|run| projection.accepted_time(run))
                        })
                        .or_else(|| {
                            facts.accepted_at_ms.map(|ms| {
                                pilotdeck_transport::system_clock::format_epoch_iso(ms as i64)
                            })
                        })
                        .unwrap_or_else(|| {
                            String::from_utf16_lossy(
                                pilotdeck_transport::server::ExecutionClock::now_iso(
                                    &pilotdeck_transport::system_clock::SystemExecutionClock,
                                )
                                .units(),
                            )
                        });
                    display = super::kernel_projection::with_active_input(
                        &display,
                        &indexed["kernelIndex"],
                        &created,
                    );
                    let mut result = super::kernel_projection::web_messages(
                        &display,
                        &text(&session)?,
                        params["projectKey"].as_str(),
                        &params,
                    );
                    result["canonicalMessages"] = json!(
                        history
                            .get("displayHistory")
                            .unwrap_or(&history["history"])
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(
                                |message| message["metadata"]["kernelProjectionBoundary"] != true
                            )
                            .cloned()
                            .collect::<Vec<_>>()
                    );
                    result["contextMessages"] = history["history"].clone();
                    result["revision"] = history["revision"].clone();
                    result["kernelSessionId"] = json!(kernel_id);
                    result
                }
                "restore_session_transcript" | "history_import_receipt" => {
                    let receipt_only = method == "history_import_receipt";
                    if receipt_only {
                        let snapshot = rpc
                            .lock()
                            .await
                            .call(
                                "execution.history.snapshot",
                                json!({"session_id":kernel_id,"context_ref":context_ref}),
                            )
                            .await?;
                        let revision = snapshot["revision"].as_u64().ok_or_else(|| {
                            error("Kernel target import revision is unavailable.")
                        })?;
                        if revision == 0 {
                            return Ok(Some(js(&json!({"found":false}))?));
                        }
                    }
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
                    if let Some(projection) = &projection {
                        let history = projection_history(
                            &mut *rpc.lock().await,
                            &kernel_id,
                            &context_ref,
                            Some(projection),
                        )
                        .await?;
                        projection.sync(&history)?;
                    }
                    let imported = json!({"sessionKey":params["sessionKey"],"kernelSessionId":kernel_id,"sha256":hash,"receipt":receipt,"imported":true});
                    if receipt_only {
                        json!({"found":true,"import":imported})
                    } else {
                        imported
                    }
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
                        interactions.reconnect_port().set_external_pending(
                            &session,
                            !state.lock().unwrap().waits.is_empty(),
                        );
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
                    let mut requests = Vec::new();
                    for (id, call) in &facts.waits {
                        let view = rpc
                            .lock()
                            .await
                            .call(
                                "execution.get",
                                json!({"session_id":kernel_id,"call_ref":call}),
                            )
                            .await?;
                        requests.push(json!({"requestId":id,"executionId":call,"kind":"permission","toolName":view["waiting"]["schema"]["tool_name"],"payload":view["waiting"]["schema"]}));
                    }
                    if let Some(interactions) = &interactions {
                        interactions
                            .reconnect_port()
                            .set_external_pending(&session, !facts.waits.is_empty());
                    }
                    {
                        let mut state = state.lock().unwrap();
                        state.waits = facts.waits;
                        state.seen_waits.extend(facts.seen_waits);
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
    limits: Option<Value>,
    port_check: Arc<tokio::sync::Mutex<NativePortCheck>>,
    projection: Option<Arc<super::kernel_projection::KernelProjection>>,
    context_ref: String,
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
impl KernelTurn {
    fn finish_unaccepted_cancellation(&mut self) -> Result<bool, RuntimeError> {
        let reason = {
            let mut state = self.state.lock().unwrap();
            if state.call.is_some() && state.call_run.as_deref() == Some(self.run.as_str()) {
                return Ok(false);
            }
            let Some(reason) = state.cancelled.clone() else {
                return Ok(false);
            };
            state.run = None;
            state.attached = false;
            state.status = "idle".into();
            reason
        };
        self.done = true;
        self.queue.push_back(js(&json!({"type":"session_aborted","sessionId":self.session,"turnId":self.run,"reason":reason,"kernelAccepted":false}))?);
        self.queue.push_back(js(&json!({"type":"turn_completed","sessionId":self.session,"turnId":self.run,"kernelAccepted":false,"result":{"type":"aborted","stopReason":"cancelled_before_kernel_admission","usage":{},"permissionDenials":[],"turns":0}}))?);
        Ok(true)
    }
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
                    if self.finish_unaccepted_cancellation()? {
                        continue;
                    }
                    self.port_check.lock().await.check().await?;
                    if self.finish_unaccepted_cancellation()? {
                        continue;
                    }
                    let original = recover(&mut *self.rpc.lock().await, &self.kernel_id).await?;
                    if original.run.as_deref().is_some_and(|run| run != self.run) {
                        let mut state = self.state.lock().unwrap();
                        let disposed = state.disposed;
                        *state = original;
                        state.disposed = disposed;
                        return Err(error(
                            "Kernel has a different active execution; the session has an active turn. Query or cancel that original execution before submitting a new turn.",
                        ));
                    }
                    let rpc = self.rpc.clone();
                    let mut rpc = rpc.lock().await;
                    if self.finish_unaccepted_cancellation()? {
                        continue;
                    }
                    let mut params = json!({"session_id":self.kernel_id,"idempotency_key":self.run,"input":input});
                    if let Some(limits) = &self.limits {
                        params["limits"] = limits.clone();
                    }
                    let accepted = rpc
                        .call_guarded("execution.input", params, Some((&self.state, &self.run)))
                        .await?;
                    drop(rpc);
                    let Some(accepted) = accepted else {
                        if self.finish_unaccepted_cancellation()? {
                            continue;
                        }
                        return Err(error(
                            "Kernel input was not sent but cancellation identity changed.",
                        ));
                    };
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
                    if let Some(projection) = &self.projection {
                        // Publish the accepted input's directory hint now,
                        // before a user can refresh a pending approval page.
                        let mut rpc = self.rpc.lock().await;
                        let facts = recover(&mut rpc, &self.kernel_id).await?;
                        let history = projection_history(
                            &mut rpc,
                            &self.kernel_id,
                            &self.context_ref,
                            self.projection.as_ref(),
                        )
                        .await?;
                        projection.sync(&with_index(history, &facts))?;
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
                            let fresh = {
                                let mut state = self.state.lock().unwrap();
                                let fresh = state.seen_waits.insert(wait.into());
                                if fresh {
                                    state.waits.insert(wait.into(), call.into());
                                }
                                fresh
                            };
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
                        super::kernel_projection::normalize_display_timelines(
                            &mut agent_event,
                            &self.run,
                        );
                        if agent_event["type"] == "permission_request" {
                            if let (Some(wait), Some(call)) = (
                                agent_event["requestId"].as_str(),
                                agent_event["executionId"].as_str(),
                            ) {
                                let fresh = {
                                    let mut state = self.state.lock().unwrap();
                                    let fresh = state.seen_waits.insert(wait.into());
                                    if fresh {
                                        state.waits.insert(wait.into(), call.into());
                                    }
                                    fresh
                                };
                                if !fresh {
                                    continue;
                                }
                                if let Some(interactions) = &self.interactions {
                                    interactions
                                        .reconnect_port()
                                        .set_external_pending(&self.session.as_str().into(), true);
                                }
                            } else {
                                return Err(error(
                                    "Native permission event omitted original wait/execution identity.",
                                ));
                            }
                        }
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
                    // The observed Kernel terminal remains authoritative even if
                    // rebuilding its UI cache fails; never relabel it Unknown.
                    self.done = true;
                    self.state.lock().unwrap().attached = false;
                    if let Some(projection) = &self.projection {
                        let history = projection_history(
                            &mut *self.rpc.lock().await,
                            &self.kernel_id,
                            &self.context_ref,
                            self.projection.as_ref(),
                        )
                        .await?;
                        projection.sync(&history)?;
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
                        self.queue.push_back(js(&json!({"type":"turn_failed","sessionId":self.session,"turnId":self.run,"error":{"code":"result_unknown","nativeCode":"KERNEL_OUTCOME_UNKNOWN","message":"Execution stopped waiting; an external effect may still have happened. Inspect the original execution before retrying.","details":{"kernelSessionId":self.kernel_id,"executionId":root,"outcome":"unknown"}}}))?);
                    }
                    let mut final_event = self.final_event.take().unwrap_or_else(|| {
                        json!({"type":"turn_completed","sessionId":self.session,"turnId":self.run,"result":{"type":if outcome=="completed"{"success"}else if outcome=="cancelled"{"aborted"}else{"error"},"stopReason":if outcome=="completed"{"completed"}else if outcome=="unknown"{"kernel_outcome_unknown"}else{outcome},"usage":{},"permissionDenials":[],"turns":0}})
                    });
                    if outcome == "unknown" {
                        final_event["result"]["type"] = json!("error");
                        final_event["result"]["stopReason"] = json!("kernel_outcome_unknown");
                    }
                    final_event["nativeResult"] = event["payload"]["content"].clone();
                    final_event["executionId"] = json!(root);
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
            compact_supported: false,
            input_limits_supported: false,
            port_check: Arc::new(tokio::sync::Mutex::new(NativePortCheck::default())),
            projection: None,
            rpc: Arc::new(tokio::sync::Mutex::new(Rpc {
                owner: None,
                socket,
                stream: None,
                next: 0,
                node: "test-node".into(),
                lease: Some(("kernel-s".into(), 3, std::time::Instant::now())),
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
    async fn kernel_compact_uses_private_command_and_original_durable_terminal() {
        let socket = std::env::temp_dir().join(format!("pd-compact-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut admitted = false;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "execution.input" => {
                        assert_eq!(req["params"]["idempotency_key"], "turn-1");
                        let payload = &req["params"]["input"];
                        assert_eq!(
                            payload["nativeCommand"],
                            json!({"kind":"compact","version":1,"manualForce":true})
                        );
                        assert!(payload.get("text").is_none());
                        assert!(payload.get("originalAgentInput").is_none());
                        admitted = true;
                        json!({"call_ref":"original-loop"})
                    }
                    "execution.events" if !admitted => json!([]),
                    "execution.events" => json!([event(
                        1,
                        "response",
                        Some("completed"),
                        json!({"outcome":"completed","content":{"command":{"kind":"compact","version":1,"result":{"type":"compacted","messagesSummarized":7,"preTokens":900,"usage":{"inputTokens":2}}}}})
                    )]),
                    other => panic!("Unexpected compact request {other}"),
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
        assert!(
            session
                .backend_request(
                    "kernel_manual_compact_v1".into(),
                    js(&json!({"turnId":"turn-1"})).unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            session
                .submit(
                    js(&json!({"nativeCommand":{"kind":"compact","version":1,"manualForce":true}}))
                        .unwrap(),
                    js(&json!({"turnId":"turn-1"})).unwrap()
                )
                .is_err()
        );
        session.compact_supported = true;
        let result = session
            .backend_request(
                "kernel_manual_compact_v1".into(),
                js(&json!({"turnId":"turn-1"})).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        let result = json_value(&result).unwrap();
        assert_eq!(result["type"], "compacted");
        assert_eq!(result["messagesSummarized"], 7);
        assert_eq!(result["preTokens"], 900);
        drop(session);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[tokio::test]
    async fn kernel_compact_cancel_observes_original_unknown_without_replaying_summary() {
        let socket =
            std::path::PathBuf::from("/tmp").join(format!("pdc-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let ready = Arc::new(tokio::sync::Notify::new());
        let notify = ready.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut admitted = false;
            let mut cancelled = false;
            let mut inputs = 0;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "execution.input" => {
                        inputs += 1;
                        admitted = true;
                        json!({"call_ref":"original-loop"})
                    }
                    "execution.control" => {
                        assert_eq!(req["params"]["call_ref"], "original-loop");
                        assert_eq!(req["params"]["kind"], "cancel");
                        cancelled = true;
                        json!({"outcome":"accepted"})
                    }
                    "execution.events" if cancelled => json!([event(
                        1,
                        "response",
                        Some("unknown"),
                        json!({"outcome":"unknown","content":null})
                    )]),
                    "execution.events" => {
                        if admitted {
                            notify.notify_one();
                        }
                        json!([])
                    }
                    other => panic!("Unexpected compact cancellation request {other}"),
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
            assert_eq!(inputs, 1);
            assert!(cancelled);
        });
        let mut session = session(socket.clone());
        session.compact_supported = true;
        let operation = tokio::spawn(session.backend_request(
            "kernel_manual_compact_v1".into(),
            js(&json!({"turnId":"turn-1"})).unwrap(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), ready.notified())
            .await
            .unwrap();
        session.abort(&"timeout:turn-1".into()).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.js_error().unwrap().code.as_deref(),
            Some("result_unknown")
        );
        assert!(error.message.contains("original-loop"));
        assert!(error.message.contains("kernel-s"));
        drop(session);
        server.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[tokio::test]
    async fn kernel_abort_during_lease_handshake_does_not_send_execution_input() {
        let socket =
            std::path::PathBuf::from("/tmp").join(format!("pdl-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (seen, proceed) = (ready.clone(), release.clone());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut acquired = 0;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "execution.events" => json!([]),
                    "session.lease_status" => {
                        seen.notify_one();
                        proceed.notified().await;
                        json!({"data":{"active":false,"epoch":0}})
                    }
                    "session.lease_acquire" => {
                        acquired += 1;
                        json!({"data":{"epoch":1}})
                    }
                    other => panic!("Pre-admission cancellation must not send {other}"),
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
            assert_eq!(acquired, 1);
        });
        let session = session(socket.clone());
        session.rpc.lock().await.lease = None;
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"must never be admitted"})).unwrap(),
                js(&json!({"turnId":"turn-1"})).unwrap(),
            )
            .unwrap();
        let consumer = tokio::spawn(async move {
            let first = next(turn.as_mut()).await;
            let final_event = next(turn.as_mut()).await;
            assert!(turn.next().await.unwrap().is_none());
            (first, final_event)
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), ready.notified())
            .await
            .unwrap();
        session
            .abort(&"cancelled during lease handshake".into())
            .unwrap();
        release.notify_one();
        let (first, final_event) =
            tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(first["type"], "session_aborted");
        assert_eq!(first["kernelAccepted"], false);
        assert_eq!(final_event["result"]["type"], "aborted");
        assert!(!session.started.load(Ordering::SeqCst));
        assert!(session.state.lock().unwrap().call.is_none());
        drop(session);
        server.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[tokio::test]
    async fn kernel_tool_filter_rejection_prevents_any_execution_input_or_rpc_connection() {
        let socket =
            std::path::PathBuf::from("/tmp").join(format!("pdf-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let session = session(socket.clone());
        for input in [
            json!({"type":"text","text":"denied","allowedTools":[]}),
            json!({"type":"text","text":"denied","allowedTools":["web_fetch"]}),
            json!({"type":"text","text":"denied","disallowedTools":["web.fetch"]}),
        ] {
            assert!(
                session
                    .submit(
                        js(&input).unwrap(),
                        js(&json!({"turnId":"filtered"})).unwrap()
                    )
                    .is_err()
            );
            assert!(super::super::kernel_config::tool_filters_js(&js(&input).unwrap()).is_err());
        }
        assert!(
            session
                .submit(
                    js(&json!({"type":"text","text":"denied"})).unwrap(),
                    js(&json!({"turnId":"filtered","allowedTools":[]})).unwrap()
                )
                .is_err()
        );
        // The resolved programmatic SessionContext is checked in its lazy
        // effect preflight too, so bypassing public JSON cannot drop a scope.
        session.port_check.lock().await.pending = Some(Box::pin(async {
            super::super::kernel_config::session_tool_filters(Some(&[]), None)
        }));
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"denied"})).unwrap(),
                js(&json!({"turnId":"filtered"})).unwrap(),
            )
            .unwrap();
        assert!(turn.next().await.is_err());
        drop(turn);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        assert!(session.state.lock().unwrap().call.is_none());
        assert!(!session.started.load(Ordering::SeqCst));
        assert!(
            super::super::kernel_config::tool_filters_js(
                &js(&json!({"allowedTools":null,"disallowedTools":[]})).unwrap()
            )
            .is_ok()
        );
        assert!(super::super::kernel_config::session_tool_filters(None, Some(&[])).is_ok());
        // Metadata reads remain available despite a configured unsupported scope.
        assert!(
            session
                .backend_request("session_model_get".into(), JsValue::Null)
                .await
                .is_ok()
        );
        drop(session);
        drop(listener);
        std::fs::remove_file(socket).unwrap();
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
    async fn kernel_reopen_observes_without_lease_and_renews_only_original_holder_before_effect() {
        for (active, holder) in [(true, "saved-node"), (false, ""), (true, "different-node")] {
            let socket = std::env::temp_dir().join(format!("pdl-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&socket).unwrap();
            let holder = holder.to_string();
            let other = holder == "different-node";
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut seen = Vec::new();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let method = req["method"].as_str().unwrap().to_string();
                    seen.push(method.clone());
                    let result = match method.as_str() {
                        "initialize" => {
                            assert_eq!(req["params"]["node_id"], "saved-node");
                            json!({})
                        }
                        "execution.get" => json!({"state":"running"}),
                        "session.lease_status" => {
                            json!({"status":"done","effect":"none","data":{"epoch":7,"active":active,"holder_node":holder}})
                        }
                        "session.lease_renew" => {
                            assert!(active);
                            assert_eq!(req["params"]["epoch"], 7);
                            json!({"status":"done","effect":"advance","data":{"epoch":7}})
                        }
                        "session.lease_acquire" => {
                            assert!(!active);
                            json!({"status":"done","effect":"advance","data":{"epoch":8}})
                        }
                        "execution.input" => json!({"call_ref":"original"}),
                        other => panic!("unexpected {other}"),
                    };
                    let reply = if method.starts_with("session.") {
                        result
                    } else {
                        json!({"id":req["id"],"result":result})
                    };
                    stream
                        .get_mut()
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                }
                seen
            });
            let mut rpc = Rpc {
                owner: None,
                socket: socket.clone(),
                stream: None,
                next: 0,
                node: "saved-node".into(),
                lease: None,
            };
            assert_eq!(
                rpc.call(
                    "execution.get",
                    json!({"session_id":"same","call_ref":"original"})
                )
                .await
                .unwrap()["state"],
                "running"
            );
            let effect = rpc
                .call(
                    "execution.input",
                    json!({"session_id":"same","idempotency_key":"same-key","input":{}}),
                )
                .await;
            assert_eq!(effect.is_err(), other);
            drop(rpc);
            let seen = server.await.unwrap();
            assert_eq!(
                &seen[..3],
                ["initialize", "execution.get", "session.lease_status"]
            );
            if other {
                assert_eq!(seen.len(), 3);
            } else {
                assert_eq!(seen.last().unwrap(), "execution.input");
            }
            std::fs::remove_file(socket).unwrap();
        }
    }
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
            owner: None,
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
            owner: None,
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
            compact_supported: false,
            input_limits_supported: false,
            port_check: Arc::new(tokio::sync::Mutex::new(NativePortCheck::default())),
            projection: None,
            rpc: Arc::new(tokio::sync::Mutex::new(Rpc {
                owner: None,
                socket,
                stream: None,
                next: 0,
                node: "test".into(),
                lease: Some(("native-session".into(), 3, std::time::Instant::now())),
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
    async fn kernel_frozen_model_read_names_available_route_when_auto_is_unavailable() {
        let mut session = session(std::env::temp_dir().join("unused-kernel-model-read.sock"));
        session
            .agent_config
            .insert("provider".into(), "fixture".into());
        session.agent_config.insert("model".into(), "fixed".into());
        let value = session
            .backend_request(
                "session_model_get".into(),
                js(&json!({"projectKey":"/p"})).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        let value = json_value(&value).unwrap();
        assert_eq!(
            value["saved"],
            json!({"mode":"model","provider":"fixture","model":"fixed"})
        );
        assert_eq!(value["effective"]["source"], "kernel_assembly");
    }
    #[tokio::test]
    async fn kernel_import_receipt_probe_cannot_create_a_first_import() {
        for revision in [0, 1] {
            let socket =
                std::path::PathBuf::from("/tmp").join(format!("pdi-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut imports = 0;
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let result = match req["method"].as_str().unwrap() {
                        "initialize" => json!({}),
                        "execution.history.snapshot" => json!({"revision":revision,"history":[]}),
                        "session.get" if revision > 0 => json!({"data":{"generation":7}}),
                        "execution.history.import" if revision > 0 => {
                            imports += 1;
                            assert_eq!(req["params"]["idempotency_key"], "pd-import:original-hash");
                            assert_eq!(req["params"]["expected_revision"], 0);
                            assert_eq!(req["params"]["generation"], 7);
                            json!({"revision":1,"receipt":"original-receipt"})
                        }
                        other => panic!("Pristine receipt probe must not send {other}"),
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
                assert_eq!(imports, revision);
            });
            let session = session(socket.clone());
            let result=session.backend_request("history_import_receipt".into(),js(&json!({"sessionKey":"target","transcript":{"sha256":"original-hash","messages":[]}})).unwrap()).await.unwrap().unwrap();
            let result = json_value(&result).unwrap();
            assert_eq!(result["found"], revision > 0);
            if revision > 0 {
                assert_eq!(result["import"]["receipt"]["receipt"], "original-receipt");
            }
            drop(session);
            server.await.unwrap();
            std::fs::remove_file(socket).unwrap();
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
    #[test]
    fn kernel_deadline_requires_advertised_outer_limits_and_uses_exact_prepared_utc() {
        let mut session = session(std::env::temp_dir().join("no-limit-rpc.sock"));
        let options = json!({"execution":{"operationDeadline":"2030-01-01T00:00:00.000Z"}});
        assert!(session.input_limits(&options).is_err());
        session.input_limits_supported = true;
        assert_eq!(
            session.input_limits(&options).unwrap(),
            Some(json!({"deadline_ms":1893456000000_u64}))
        );
        assert_eq!(session.input_limits(&json!({"maxTurns":5})).unwrap(), None);
        assert!(
            session
                .input_limits(&json!({"execution":{"operationDeadline":"invalid"}}))
                .is_err()
        );
        let facts = State {
            call: Some("original".into()),
            call_run: Some("run".into()),
            input: Some(json!({"type":"text","text":"hi"})),
            accepted_at_ms: Some(1893456000000),
            ..Default::default()
        };
        assert_eq!(
            with_index(json!({"revision":0,"history":[]}), &facts)["kernelIndex"]["acceptedAt"],
            "2030-01-01T00:00:00.000Z"
        );
    }
    #[tokio::test]
    async fn kernel_abort_during_preflight_never_opens_rpc_or_admits_new_execution() {
        let mut session = session(std::env::temp_dir().join("no-kernel-rpc.sock"));
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        session.port_check = Arc::new(tokio::sync::Mutex::new(NativePortCheck {
            pending: Some(Box::pin(async move {
                let _ = started.send(());
                gate.notified().await;
                Ok(())
            })),
            result: None,
        }));
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"must not execute"})).unwrap(),
                js(&json!({"turnId":"cancelled-ingress"})).unwrap(),
            )
            .unwrap();
        let task = tokio::spawn(async move {
            let first = json_value(&turn.next().await.unwrap().unwrap()).unwrap();
            let final_event = json_value(&turn.next().await.unwrap().unwrap()).unwrap();
            assert!(turn.next().await.unwrap().is_none());
            (first, final_event)
        });
        ready.await.unwrap();
        session.abort(&"cancelled before admission".into()).unwrap();
        release.notify_one();
        let (first, final_event) = task.await.unwrap();
        assert_eq!(first["type"], "session_aborted");
        assert_eq!(final_event["kernelAccepted"], false);
        assert_eq!(final_event["result"]["type"], "aborted");
        assert!(!session.started.load(Ordering::SeqCst));
        assert!(session.rpc.lock().await.stream.is_none());
    }
    #[tokio::test]
    async fn kernel_configured_port_failure_remains_rejected_without_replay_or_blocking_reads() {
        let mut check = NativePortCheck {
            pending: Some(Box::pin(async {
                Err(super::super::kernel_config::unavailable("fixture"))
            })),
            result: None,
        };
        assert!(check.check().await.is_err());
        assert!(check.check().await.is_err());
        let mut interrupted = NativePortCheck {
            pending: Some(Box::pin(std::future::pending())),
            result: None,
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), interrupted.check())
                .await
                .is_err()
        );
        assert!(interrupted.check().await.is_err());
        let mut session = session(std::env::temp_dir().join("no-effect.sock"));
        session.agent_config = js(&json!({"managedPermissions":{"defaultMode":"default"}}))
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            session
                .turn_permission_mode(&json!({"mode":"bypassPermissions"}))
                .unwrap(),
            JsString::from("default")
        );
        assert!(
            session
                .turn_permission_mode(&json!({"mode":"bad"}))
                .is_err()
        );
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
                        assert_eq!(
                            req["params"]["limits"],
                            json!({"deadline_ms":1893456000000_u64})
                        );
                        assert!(req["params"]["input"].get("limits").is_none());
                        let agent = &req["params"]["input"]["agent"];
                        assert_eq!(agent["permissionMode"], "plan");
                        assert_eq!(
                            req["params"]["input"]["basePermissionMode"],
                            "bypassPermissions"
                        );
                        assert_eq!(agent["modelPricingEntries"][0]["key"], "z-first");
                        assert_eq!(agent["modelPricingEntries"][1]["key"], "a-second");
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
        session.agent_config=js(&json!({"provider":"must-not-override","model":"must-not-override","thinking":{"enabled":true,"budgetTokens":1024},"appendSystemPrompt":"write concise answers","modelPricingEntries":[{"key":"z-first","input":1},{"key":"a-second","input":2}]})).unwrap().as_object().unwrap().clone();
        session.permission_mode_source = Some(Arc::new(|| Some("bypassPermissions".into())));
        session.input_limits_supported = true;
        let mut turn = session
            .submit(
                js(&json!({"type":"text","text":"hello"})).unwrap(),
                js(&json!({"turnId":"run","runMode":"plan","mode":"plan","basePermissionMode":"bypassPermissions","execution":{"operationDeadline":"2030-01-01T00:00:00.000Z"}})).unwrap(),
            )
            .unwrap();
        while turn.next().await.unwrap().is_some() {}
        drop(turn);
        drop(session);
        task.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}

#[cfg(test)]
mod archive_wire_tests {
    use super::*;
    use crate::storage::{JsonlTranscriptStore, TranscriptKey, recorder::Recorder};
    use tokio::net::UnixListener;
    fn cache(root: &std::path::Path) -> Arc<super::super::kernel_projection::KernelProjection> {
        let cache = Arc::new(super::super::kernel_projection::KernelProjection::new(
            Arc::new(Recorder::new(
                Arc::new(JsonlTranscriptStore::new()),
                TranscriptKey {
                    project_root: root.into(),
                    pilot_home: root.into(),
                    session_id: "s".into(),
                    transcript_path: root.join("s.jsonl"),
                },
            )),
            Arc::new(super::super::factory::SessionProjection::default()),
            "s".into(),
            "kernel".into(),
            "default".into(),
        ));
        cache.enable_archive(true);
        cache
    }
    #[tokio::test]
    async fn kernel_archive_pages_pin_context_revision_and_rebuild_deleted_product_cache() {
        let root = std::path::PathBuf::from("/tmp").join(format!("pda-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("s.jsonl.backend.json"),
            json!({"backend":"kernel","kernel_session_id":"kernel"}).to_string(),
        )
        .unwrap();
        let socket = root.join("rpc.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut pages = 0;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let old = json!([{"role":"user","metadata":{"kernelMessageId":"u","turnId":"r"},"content":[{"type":"text","text":"full original prompt"}]},{"role":"assistant","metadata":{"kernelMessageId":"a","turnId":"r"},"content":[{"type":"text","text":"full original answer"}]}]);
                let compressed = json!([{"role":"user","metadata":{"synthetic":true,"kernelMessageId":"summary"},"content":[{"type":"text","text":"compressed model context"}]}]);
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "execution.history.snapshot" => json!({"revision":2,"history":compressed}),
                    "execution.history.archive" => {
                        pages += 1;
                        assert_eq!(req["params"]["context_ref"], "default");
                        assert_eq!(req["params"]["max_bytes"], 4 * 1024 * 1024);
                        match req["params"]["after_revision"].as_u64().unwrap() {
                            0 => {
                                json!({"entries":[{"revision":1,"history":old}],"latest_revision":2,"next_revision":1,"has_more":true})
                            }
                            1 => {
                                json!({"entries":[{"revision":2,"history":compressed,"checkpoint":{"compaction":{"compactMetadata":{"compactionId":"c","preTokens":100,"postTokens":20,"messagesSummarized":2}}}},{"revision":3,"history":[{"role":"user","content":[{"type":"text","text":"future commit"}]}]}],"latest_revision":3,"next_revision":3,"has_more":false})
                            }
                            other => panic!("Unexpected archive cursor {other}"),
                        }
                    }
                    other => panic!("Unexpected archive request {other}"),
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
            assert_eq!(pages, 4);
        });
        let mut rpc = Rpc {
            owner: None,
            socket,
            stream: None,
            next: 0,
            node: "observer".into(),
            lease: None,
        };
        let projection = cache(&root);
        let snapshot = projection_history(&mut rpc, "kernel", "default", Some(&projection))
            .await
            .unwrap();
        assert_eq!(snapshot["revision"], 2);
        assert_eq!(snapshot["archiveRevision"], 2);
        assert_eq!(
            snapshot["history"][0]["content"][0]["text"],
            "compressed model context"
        );
        assert_eq!(snapshot["displayHistory"].as_array().unwrap().len(), 3);
        assert!(
            !snapshot["displayHistory"]
                .to_string()
                .contains("future commit")
        );
        projection.sync(&snapshot).unwrap();
        let original = std::fs::read(root.join("s.jsonl")).unwrap();
        let unchanged = projection_history(&mut rpc, "kernel", "default", Some(&projection))
            .await
            .unwrap();
        assert_eq!(unchanged, snapshot);
        let rows = super::super::kernel_projection::web_messages(
            &snapshot["displayHistory"],
            "s",
            None,
            &json!({}),
        );
        assert_eq!(rows["messages"][0]["text"], "full original prompt");
        assert_eq!(rows["messages"][2]["kind"], "compact_boundary");
        drop(projection);
        std::fs::remove_file(root.join("s.jsonl")).unwrap();
        let rebuilt = cache(&root);
        let reread = projection_history(&mut rpc, "kernel", "default", Some(&rebuilt))
            .await
            .unwrap();
        assert_eq!(reread, snapshot);
        rebuilt.sync(&reread).unwrap();
        // Record IDs and source rows rebuild deterministically; cache-local clock
        // and sequence are deliberately not native execution authority.
        assert!(
            String::from_utf8(original)
                .unwrap()
                .contains("full original answer")
        );
        let raw = std::fs::read_to_string(root.join("s.jsonl")).unwrap();
        assert!(raw.contains("full original answer"));
        assert!(!raw.contains("\"type\":\"turn_result\""));
        drop(rpc);
        task.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod resident_restart_tests {
    use super::*;
    use tokio::net::UnixListener;
    fn opened(revision: &str) -> Value {
        json!({"protocol":"agentcpm-scheduler/1","input_controller_ref":null,"default_loop_ref":"saved-loop","loop_revision":revision,"loop_permissions":{"resources":["read"],"invoke":[]},"features":["scheduler.input-limits.v1","scheduler.history-archive.v1"]})
    }
    fn rpc(socket: PathBuf) -> Rpc {
        let open = opened("saved-v1");
        Rpc {
            socket,
            stream: None,
            next: 0,
            node: "saved-owner-node".into(),
            lease: None,
            owner: Some(RpcOwner {
                session: "saved-session".into(),
                binding: "saved-loop".into(),
                revision: open["loop_revision"].clone(),
                protocol: open["protocol"].clone(),
                permissions: open["loop_permissions"].clone(),
                features: open["features"].as_array().unwrap().clone(),
            }),
        }
    }
    async fn request(stream: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn reply(stream: &mut BufReader<UnixStream>, request: &Value, result: Value) {
        stream
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn kernel_resident_restart_reopens_original_pin_before_readonly_retry_and_refuses_changed_revision()
     {
        for changed in [false, true] {
            let socket =
                std::path::PathBuf::from("/tmp").join(format!("pdr-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                for connection in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut stream = BufReader::new(stream);
                    let init = request(&mut stream).await;
                    assert_eq!(init["method"], "initialize");
                    assert_eq!(init["params"]["node_id"], "saved-owner-node");
                    reply(&mut stream,&init,json!({"server_capabilities":{"scheduler":{"default_loop_ref":"new-global-default"}}})).await;
                    let open = request(&mut stream).await;
                    assert_eq!(open["method"], "execution.open");
                    assert_eq!(open["params"]["session_id"], "saved-session");
                    reply(
                        &mut stream,
                        &open,
                        opened(if connection == 1 && changed {
                            "changed-same-id-v2"
                        } else {
                            "saved-v1"
                        }),
                    )
                    .await;
                    if connection == 1 && changed {
                        let mut line = String::new();
                        assert_eq!(
                            stream.read_line(&mut line).await.unwrap(),
                            0,
                            "pin refusal must not query or submit against another revision"
                        );
                        continue;
                    }
                    let read = request(&mut stream).await;
                    assert_eq!(read["method"], "execution.history.snapshot");
                    reply(&mut stream, &read, json!({"revision":3,"history":[]})).await;
                    // Dropping this first connection models a resident restart.
                }
            });
            let mut rpc = rpc(socket.clone());
            let params = json!({"session_id":"saved-session","context_ref":"default"});
            assert_eq!(
                rpc.call("execution.history.snapshot", params.clone())
                    .await
                    .unwrap()["revision"],
                3
            );
            rpc.lease = Some(("saved-session".into(), 9, std::time::Instant::now()));
            let restored = rpc.call("execution.history.snapshot", params).await;
            if changed {
                assert!(
                    restored
                        .unwrap_err()
                        .message
                        .contains("persisted binding/revision")
                );
            } else {
                assert_eq!(restored.unwrap()["revision"], 3);
                assert!(rpc.lease.is_none());
            }
            drop(rpc);
            server.await.unwrap();
            std::fs::remove_file(socket).unwrap();
        }
    }
    #[tokio::test]
    async fn kernel_resident_restart_never_retries_an_effect_after_a_lost_reply() {
        let socket =
            std::path::PathBuf::from("/tmp").join(format!("pde-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for method in ["initialize", "execution.open", "execution.history.snapshot"] {
                let req = request(&mut stream).await;
                assert_eq!(req["method"], method);
                reply(
                    &mut stream,
                    &req,
                    match method {
                        "execution.open" => opened("saved-v1"),
                        "execution.history.snapshot" => json!({"revision":0,"history":[]}),
                        _ => json!({}),
                    },
                )
                .await;
            }
            let input = request(&mut stream).await;
            assert_eq!(input["method"], "execution.input");
            assert_eq!(input["params"]["idempotency_key"], "original-operation");
            drop(stream);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                    .await
                    .is_err(),
                "effect must not reconnect/replay automatically"
            );
        });
        let mut rpc = rpc(socket.clone());
        rpc.call(
            "execution.history.snapshot",
            json!({"session_id":"saved-session","context_ref":"default"}),
        )
        .await
        .unwrap();
        rpc.lease = Some(("saved-session".into(), 9, std::time::Instant::now()));
        assert!(rpc.call("execution.input",json!({"session_id":"saved-session","idempotency_key":"original-operation","input":{"type":"text","text":"one request"}})).await.is_err());
        drop(rpc);
        server.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
