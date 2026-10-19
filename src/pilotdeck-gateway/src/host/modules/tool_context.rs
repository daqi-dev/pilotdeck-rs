//! Turn-bound tool authority. Wire values never supply host service handles.
use super::{capability, permission, wire};
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use capability::{RuntimeContext, ToolInvocationContext};
use pilotdeck_protocol::JsString;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// Owned, detached projection. Absent maps stay absent until a checkpoint is made.
#[derive(Clone, Debug, Default)]
pub struct SeedState {
    pub allowed_read_files: Option<Vec<JsString>>,
    pub read_file_state: Option<JsObject>,
    pub write_snapshots: Option<JsObject>,
}

pub fn parse_seed_state(value: &JsValue) -> Result<Option<SeedState>, HostError> {
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        return Ok(None);
    }
    let source = plain_record(value)
        .ok_or_else(|| ModuleError::new("Invalid sidecar seedState: expected an object."))?;
    let mut result = SeedState::default();
    if let Some(value) = defined(source.get("allowedReadFiles")) {
        let JsValue::Array(paths) = value else {
            return Err(ModuleError::new("Invalid sidecar seedState.allowedReadFiles.").into());
        };
        result.allowed_read_files = Some(
            paths
                .iter()
                .map(|path| {
                    path.as_string().cloned().ok_or_else(|| {
                        ModuleError::new("Invalid sidecar seedState.allowedReadFiles.").into()
                    })
                })
                .collect::<Result<_, HostError>>()?,
        );
    }
    if let Some(value) = defined(source.get("readFileState")) {
        result.read_file_state = Some(parse_file_map(value, false)?);
    }
    if let Some(value) = defined(source.get("writeSnapshots")) {
        result.write_snapshots = Some(parse_file_map(value, true)?);
    }
    Ok(Some(result))
}

pub fn serialize_seed_state(seed: &SeedState) -> JsObject {
    let mut result = JsObject::new();
    if let Some(paths) = &seed.allowed_read_files {
        result.insert("allowedReadFiles".into(), strings(paths));
    }
    if let Some(state) = &seed.read_file_state {
        result.insert(
            "readFileState".into(),
            JsValue::Object(plain_entries(state)),
        );
    }
    if let Some(state) = &seed.write_snapshots {
        result.insert(
            "writeSnapshots".into(),
            JsValue::Object(plain_entries(state)),
        );
    }
    result
}

fn plain_entries(source: &JsObject) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in source.entries() {
        result.insert(key.clone(), value.clone());
    }
    result
}

fn parse_file_map(value: &JsValue, write: bool) -> Result<JsObject, HostError> {
    let label = if write {
        "writeSnapshots"
    } else {
        "readFileState"
    };
    // A native-tagged object represents a host Map with string keys. Ordinary
    // wire records use Object.entries order, which sorts array-index keys.
    let source = value
        .as_object()
        .filter(|v| v.native_tag() == Some("Map"))
        .or_else(|| plain_record(value))
        .ok_or_else(|| ModuleError::new(format!("Invalid sidecar seedState.{label}.")))?;
    let mut entries: Vec<_> = source.entries().iter().collect();
    if source.native_tag() != Some("Map") {
        entries.sort_by_key(|(key, _)| key.array_index().map_or((1, 0), |index| (0, index)));
    }
    let mut result = JsObject::new();
    for (path, value) in entries {
        let entry = plain_record(value);
        let valid = entry.is_some_and(|entry| {
            entry.get("mtimeMs").and_then(JsValue::as_number).is_some()
                && if write {
                    ["absolutePath", "contentHash"]
                        .iter()
                        .all(|key| entry.get(key).and_then(JsValue::as_string).is_some())
                } else {
                    entry
                        .get("kind")
                        .and_then(JsValue::as_string)
                        .is_some_and(|kind| {
                            ["text", "image", "pdf", "notebook"]
                                .iter()
                                .any(|s| kind.equals_str(s))
                        })
                }
        });
        if !valid {
            return Err(wire::named_error(
                &format!("Invalid {label} entry: "),
                path,
                ".",
            ));
        }
        let entry = entry.unwrap();
        let mut clean = JsObject::new();
        let required: &[&str] = if write {
            &["absolutePath", "mtimeMs", "contentHash"]
        } else {
            &["mtimeMs", "kind"]
        };
        for key in required {
            clean.insert((*key).into(), entry.get(key).unwrap().clone());
        }
        for key in ["offset", "limit"] {
            if let Some(value @ JsValue::Number(_)) = entry.get(key) {
                clean.insert(key.into(), value.clone());
            }
        }
        if !write && let Some(value @ JsValue::String(_)) = entry.get("pages") {
            clean.insert("pages".into(), value.clone());
        }
        result.insert(path.clone(), JsValue::Object(clean));
    }
    Ok(result)
}

pub type SharedFileState = Arc<Mutex<JsObject>>;

#[derive(Clone, Debug)]
pub struct ToolFileState {
    pub read_file_state: SharedFileState,
    pub write_snapshots: SharedFileState,
    /// A fresh array on every context; later grants do not change old contexts.
    pub allowed_read_files: Vec<JsString>,
}

#[derive(Clone, Debug, Default)]
pub struct HostToolCheckpoint {
    pub read_file_state: SharedFileState,
    pub write_snapshots: SharedFileState,
    allowed_read_files: Arc<Mutex<Vec<JsString>>>,
}
impl HostToolCheckpoint {
    pub fn new(seed: Option<&SeedState>, allowed: &[JsString]) -> Result<Self, HostError> {
        let seed = seed
            .map(|seed| parse_seed_state(&JsValue::Object(serialize_seed_state(seed))))
            .transpose()?
            .flatten()
            .unwrap_or_default();
        let checkpoint = Self {
            read_file_state: Arc::new(Mutex::new(seed.read_file_state.unwrap_or_default())),
            write_snapshots: Arc::new(Mutex::new(seed.write_snapshots.unwrap_or_default())),
            allowed_read_files: Arc::default(),
        };
        checkpoint.allow_read_files(seed.allowed_read_files.as_deref().unwrap_or_default());
        checkpoint.allow_read_files(allowed);
        Ok(checkpoint)
    }
    pub fn allow_read_files(&self, paths: &[JsString]) {
        let mut allowed = self.allowed_read_files.lock().unwrap();
        for path in paths {
            if !allowed.contains(path) {
                allowed.push(path.clone());
            }
        }
    }
    pub fn tool_context_state(&self) -> ToolFileState {
        ToolFileState {
            read_file_state: self.read_file_state.clone(),
            write_snapshots: self.write_snapshots.clone(),
            allowed_read_files: self.allowed_read_files.lock().unwrap().clone(),
        }
    }
    pub fn snapshot(&self) -> Result<SeedState, HostError> {
        let seed = SeedState {
            allowed_read_files: Some(self.allowed_read_files.lock().unwrap().clone()),
            read_file_state: Some(self.read_file_state.lock().unwrap().clone()),
            write_snapshots: Some(self.write_snapshots.lock().unwrap().clone()),
        };
        Ok(parse_seed_state(&JsValue::Object(serialize_seed_state(&seed)))?.unwrap())
    }
}

/// A6.7-pr1 shares this turn-owned table with its drain adapter. Remembering is
/// not granting: only the durable steer acknowledgement may call apply().
#[derive(Clone)]
pub struct SteerAuthorizations {
    checkpoint: HostToolCheckpoint,
    pending: Arc<Mutex<HashMap<JsString, Vec<JsString>>>>,
}
impl SteerAuthorizations {
    pub fn new(checkpoint: HostToolCheckpoint) -> Self {
        Self {
            checkpoint,
            pending: Arc::default(),
        }
    }
    /// The turn adapter supplies the host mailbox's item ID and filtered paths.
    /// An empty list intentionally leaves an existing grant unchanged.
    pub fn remember(&self, item_id: JsString, paths: Vec<JsString>) {
        if !paths.is_empty() {
            self.pending.lock().unwrap().insert(item_id, paths);
        }
    }
    pub fn apply(&self, item_id: &JsString) {
        // Delete before making the grant visible, including repeated callbacks.
        let mut pending = self.pending.lock().unwrap();
        let paths = pending.remove(item_id).unwrap_or_default();
        self.checkpoint.allow_read_files(&paths);
    }
}

/// Host service ports retain identity; they are not serializable wire values.
pub trait TurnModel: Send + Sync {
    fn for_turn(&self, identity: JsObject) -> Option<RuntimeContext>;
    fn unbound(&self) -> RuntimeContext;
}
pub trait SessionService: Send + Sync {
    fn for_session(&self, session_id: &JsString) -> Option<RuntimeContext>;
}
pub trait OneShotSubagent: Send + Sync {
    fn create_fork(
        &self,
        identity: JsObject,
        read: SharedFileState,
        write: SharedFileState,
    ) -> RuntimeContext;
}
pub trait PlanFiles: Send + Sync {
    fn directory(&self) -> Option<JsString>;
    fn resolve(&self, file_path: JsString, cwd: JsString) -> HostFuture<JsValue>;
    fn read(&self, file_path: JsString, cwd: JsString) -> HostFuture<JsValue>;
}
pub trait TurnEnvironment: Send + Sync {
    /// BuildTurnEnvironment belongs to the host: resolve cwd, NFKC-normalize path
    /// segments, and overlay the three PILOTDECK_* variables. No sidecar input.
    fn build(
        &self,
        base: Option<&JsValue>,
        cwd: &JsString,
        session: &JsString,
        turn: &JsString,
    ) -> Result<JsObject, HostError>;
}
pub type EventSink = Arc<dyn Fn(JsObject) + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> JsValue + Send + Sync>;

#[derive(Clone, Default)]
pub struct ToolContextPorts {
    pub model: Option<Arc<dyn TurnModel>>,
    pub elicitation: Option<RuntimeContext>,
    pub user_dialog: Option<RuntimeContext>,
    pub audit_recorder: Option<RuntimeContext>,
    pub file_history: Option<RuntimeContext>,
    pub file_update_notifier: Option<RuntimeContext>,
    pub clock: Option<Clock>,
    pub event_emitter: Option<EventSink>,
    pub subagent: Option<Arc<dyn OneShotSubagent>>,
    pub goal: Option<Arc<dyn SessionService>>,
    pub plan_files: Option<Arc<dyn PlanFiles>>,
}

#[derive(Clone)]
pub struct TurnBinding {
    /// Trusted host configuration and admission input; never callback payloads.
    pub config: JsObject,
    pub input: JsObject,
    pub checkpoint: HostToolCheckpoint,
    pub abort_signal: Option<RuntimeContext>,
    pub environment: Arc<dyn TurnEnvironment>,
}

pub struct PlanDirectory {
    pub path: JsString,
    cwd: JsString,
    files: Arc<dyn PlanFiles>,
}
impl PlanDirectory {
    pub fn resolve(&self, path: JsString) -> HostFuture<JsValue> {
        self.files.resolve(path, self.cwd.clone())
    }
    pub fn read(&self, path: JsString) -> HostFuture<JsValue> {
        self.files.read(path, self.cwd.clone())
    }
}

pub struct ToolRuntimeContext {
    pub fields: JsObject,
    pub files: ToolFileState,
    pub abort_signal: Option<RuntimeContext>,
    pub model: Option<RuntimeContext>,
    pub elicitation: Option<RuntimeContext>,
    pub user_dialog: Option<RuntimeContext>,
    pub audit_recorder: Option<RuntimeContext>,
    pub file_history: Option<RuntimeContext>,
    pub file_update_notifier: Option<RuntimeContext>,
    pub now: Option<Clock>,
    pub progress: Option<EventSink>,
    pub subagent: Option<RuntimeContext>,
    pub plan_todo: Option<RuntimeContext>,
    pub goal: Option<RuntimeContext>,
    pub plan_directory: Option<PlanDirectory>,
}
pub struct ExecutionContext {
    pub fields: JsObject,
    pub abort_signal: Option<RuntimeContext>,
}
pub struct ContextIdentity {
    pub fields: JsObject,
    /// When set, overrides any remote abortSignal in fields.
    pub abort_signal: Option<RuntimeContext>,
}

#[derive(Clone)]
pub struct ToolContextBuilder {
    binding: TurnBinding,
    ports: ToolContextPorts,
    auxiliary_model: Option<RuntimeContext>,
}
impl ToolContextBuilder {
    pub fn new(binding: TurnBinding, ports: ToolContextPorts, include_auxiliary: bool) -> Self {
        let auxiliary_model = if include_auxiliary {
            ports.model.as_ref().map(|model| {
                let mut identity = host_identity(&binding);
                identity.insert("projectPath".into(), get(&binding.config, "cwd"));
                model.for_turn(identity).unwrap_or_else(|| model.unbound())
            })
        } else {
            None
        };
        Self {
            binding,
            ports,
            auxiliary_model,
        }
    }
    pub fn permission_context(&self) -> JsObject {
        let config = &self.binding.config;
        let base = config.get("permissionContext").and_then(JsValue::as_object);
        let mut result = base.cloned().unwrap_or_default();
        result.insert("cwd".into(), get(config, "cwd"));
        result.insert("mode".into(), get(config, "permissionMode"));
        result.insert(
            "canPrompt".into(),
            self.binding
                .input
                .get("canPrompt")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .cloned()
                .unwrap_or_else(|| {
                    base.map(|b| get(b, "canPrompt"))
                        .unwrap_or(JsValue::Undefined)
                }),
        );
        result.insert(
            "rules".into(),
            base.map(|b| get(b, "rules")).unwrap_or(JsValue::Undefined),
        );
        result
    }
    pub fn tool_runtime_context(
        &self,
        remote: &JsValue,
        plan_todo: Option<RuntimeContext>,
        include_one_shot: bool,
    ) -> Result<ToolRuntimeContext, HostError> {
        let binding = &self.binding;
        let config = &binding.config;
        let session = required_host_string(&binding.input, "sessionId")?;
        let turn = required_host_string(&binding.input, "turnId")?;
        let cwd = required_host_string(config, "cwd")?;
        let directory = self
            .ports
            .plan_files
            .as_ref()
            .and_then(|p| p.directory())
            .filter(|p| !p.is_empty());
        let files = binding.checkpoint.tool_context_state();
        let mut fields = host_identity(binding);
        fields.insert("messageId".into(), JsValue::String(turn.clone()));
        fields.insert("cwd".into(), JsValue::String(cwd.clone()));
        let remote = remote.as_object();
        if let Some(value @ JsValue::String(_)) =
            remote.and_then(|r| r.get_property("currentToolCallId"))
        {
            fields.insert("currentToolCallId".into(), value.clone());
        }
        if let Some(value) = defined(remote.and_then(|r| r.get_property("outputTruncated"))) {
            if !matches!(value, JsValue::Bool(_)) {
                return Err(ModuleError::new(
                    "Sidecar tool context outputTruncated must be a boolean.",
                )
                .into());
            }
            fields.insert("outputTruncated".into(), value.clone());
        }
        for key in [
            "subagentTimeoutMs",
            "toolAliases",
            "maxResultBytes",
            "modelMultimodal",
            "maxOutputTokens",
        ] {
            if let Some(value) = config.get(key).filter(|v| truthy(v)) {
                fields.insert(key.into(), value.clone());
            }
        }
        fields.insert("permissionMode".into(), get(config, "permissionMode"));
        let mut permission = self.permission_context();
        if let Some(path) = &directory {
            permission.insert("planDirectoryPath".into(), JsValue::String(path.clone()));
        }
        fields.insert("permissionContext".into(), JsValue::Object(permission));
        fields.insert("runMode".into(), run_mode(config));
        fields.insert(
            "canElicit".into(),
            JsValue::Bool(
                binding.input.get("canElicit").and_then(JsValue::as_bool) == Some(true)
                    && self.ports.elicitation.is_some(),
            ),
        );
        fields.insert(
            "env".into(),
            JsValue::Object(
                binding
                    .environment
                    .build(config.get("env"), &cwd, &session, &turn)?,
            ),
        );
        if let Some(value) = defined(config.get("subagentDepth")) {
            fields.insert("subagentDepth".into(), value.clone());
        }
        let progress = self
            .ports
            .event_emitter
            .as_ref()
            .filter(|_| config.get("includeToolProgress").and_then(JsValue::as_bool) == Some(true))
            .map(|sink| {
                let sink = sink.clone();
                let identity = host_identity(binding);
                Arc::new(move |event: JsObject| {
                    let mut emitted = JsObject::new();
                    emitted.insert("type".into(), "tool_progress".into());
                    for (key, value) in identity.entries() {
                        emitted.insert(key.clone(), value.clone());
                    }
                    for key in ["toolCallId", "toolName", "message"] {
                        emitted.insert(key.into(), get(&event, key));
                    }
                    if let Some(value) = event.get("metadata").filter(|v| truthy(v)) {
                        emitted.insert("metadata".into(), value.clone());
                    }
                    emitted.insert("createdAt".into(), get(&event, "createdAt"));
                    sink(emitted);
                }) as EventSink
            });
        let subagent = if include_one_shot {
            self.ports.subagent.as_ref().map(|port| {
                port.create_fork(
                    host_identity(binding),
                    files.read_file_state.clone(),
                    files.write_snapshots.clone(),
                )
            })
        } else {
            None
        };
        let goal = self
            .ports
            .goal
            .as_ref()
            .and_then(|goal| goal.for_session(&session));
        Ok(ToolRuntimeContext {
            fields,
            files,
            abort_signal: binding.abort_signal.clone(),
            model: self.auxiliary_model.clone(),
            elicitation: self.ports.elicitation.clone(),
            user_dialog: self.ports.user_dialog.clone(),
            audit_recorder: self.ports.audit_recorder.clone(),
            file_history: self.ports.file_history.clone(),
            file_update_notifier: self.ports.file_update_notifier.clone(),
            now: self.ports.clock.clone(),
            progress,
            subagent,
            plan_todo,
            goal,
            plan_directory: directory.map(|path| PlanDirectory {
                path,
                cwd,
                files: self.ports.plan_files.as_ref().unwrap().clone(),
            }),
        })
    }
    pub fn execution_context(&self, call: &JsValue) -> ExecutionContext {
        let mut fields = host_identity(&self.binding);
        for key in ["runId", "operationId"] {
            fields.insert(
                key.into(),
                call.get(key).cloned().unwrap_or(JsValue::Undefined),
            );
        }
        if let Some(value) = call.get("idempotencyKey").filter(|v| truthy(v)) {
            fields.insert("idempotencyKey".into(), value.clone());
        }
        if let Some(value) = self
            .binding
            .input
            .get("execution")
            .and_then(|v| v.get("operationDeadline"))
            .filter(|v| truthy(v))
        {
            fields.insert("operationDeadline".into(), value.clone());
        }
        ExecutionContext {
            fields,
            abort_signal: self.binding.abort_signal.clone(),
        }
    }
    pub fn context_identity(&self, source: Option<&JsObject>) -> ContextIdentity {
        let mut fields = JsObject::new();
        if let Some(source) = source {
            for (key, value) in source.entries() {
                if self.binding.abort_signal.is_none() || !key.equals_str("abortSignal") {
                    fields.insert(key.clone(), value.clone());
                }
            }
        }
        for (key, value) in host_identity(&self.binding).entries() {
            fields.insert(key.clone(), value.clone());
        }
        for key in ["cwd", "permissionMode"] {
            fields.insert(key.into(), get(&self.binding.config, key));
        }
        fields.insert("runMode".into(), run_mode(&self.binding.config));
        ContextIdentity {
            fields,
            abort_signal: self.binding.abort_signal.clone(),
        }
    }
}

/// Owns only capability context construction, not the execution port itself.
pub struct CapabilityContext {
    pub builder: ToolContextBuilder,
    pub plan_todo: Option<Arc<dyn SessionService>>,
}
impl capability::ToolContext for CapabilityContext {
    fn prepare(&self, call: JsValue) -> HostFuture<ToolInvocationContext> {
        let builder = self.builder.clone();
        let plans = self.plan_todo.clone();
        Box::pin(async move {
            let plan = plans.and_then(|p| {
                builder
                    .binding
                    .input
                    .get("sessionId")
                    .and_then(JsValue::as_string)
                    .and_then(|session| p.for_session(session))
            });
            let missing = JsValue::Undefined;
            let value = call
                .get("payload")
                .and_then(|v| v.get("context"))
                .unwrap_or(&missing);
            let runtime = builder.tool_runtime_context(value, plan, true)?;
            Ok(ToolInvocationContext {
                runtime: Arc::new(runtime),
                execution: Arc::new(builder.execution_context(&call)),
            })
        })
    }
}
/// Its private builder cannot enable auxiliary model or one-shot subagents.
pub struct PermissionRequestContext(ToolContextBuilder);
impl PermissionRequestContext {
    pub fn new(binding: TurnBinding, ports: ToolContextPorts) -> Self {
        Self(ToolContextBuilder::new(binding, ports, false))
    }
}
impl permission::PermissionContext for PermissionRequestContext {
    fn prepare(&self, value: JsValue) -> HostFuture<RuntimeContext> {
        let builder = self.0.clone();
        Box::pin(async move {
            Ok(Arc::new(builder.tool_runtime_context(&value, None, false)?) as RuntimeContext)
        })
    }
}
/// Only identity canonicalization is exposed to context callbacks.
pub struct ContextRequestIdentity(ToolContextBuilder);
impl ContextRequestIdentity {
    pub fn new(binding: TurnBinding, ports: ToolContextPorts) -> Self {
        Self(ToolContextBuilder::new(binding, ports, false))
    }
    pub fn context_identity(&self, source: Option<&JsObject>) -> ContextIdentity {
        self.0.context_identity(source)
    }
}

fn host_identity(binding: &TurnBinding) -> JsObject {
    let mut result = JsObject::new();
    for key in ["sessionId", "turnId"] {
        result.insert(key.into(), get(&binding.input, key));
    }
    result
}
fn required_host_string(object: &JsObject, field: &str) -> Result<JsString, HostError> {
    object
        .get(field)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| {
            ModuleError::new(format!("Host context field {field} must be a string.")).into()
        })
}
fn get(object: &JsObject, key: &str) -> JsValue {
    object.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn defined(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|v| !v.is_undefined())
}
fn plain_record(value: &JsValue) -> Option<&JsObject> {
    value
        .as_object()
        .filter(|v| matches!(v.prototype(), None | Some(JsValue::Null)) && v.native_tag().is_none())
}
fn strings(paths: &[JsString]) -> JsValue {
    JsValue::Array(paths.iter().cloned().map(JsValue::String).collect())
}
fn run_mode(config: &JsObject) -> JsValue {
    config
        .get("runMode")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .cloned()
        .unwrap_or_else(|| "agent".into())
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
