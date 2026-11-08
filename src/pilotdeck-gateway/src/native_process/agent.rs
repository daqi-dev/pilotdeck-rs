//! Native full forks reuse the application's one ProjectSessionFactory and sidecar.
use crate::{
    config::permission::RuleSetHandle,
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            capability::{
                RuntimeContext, ToolCall, ToolCatalog, ToolDefinition, ToolExecution,
                ToolInvocationContext,
            },
            tool_context::{HostToolCheckpoint, OneShotSubagent, SeedState, SharedFileState},
        },
    },
    runtime::{
        hooks::Cancellation,
        project::{
            RuntimeError, interaction_hooks::TurnHooks, interaction_permission::TurnPermission,
        },
        session::{
            LiveSessionHandle, SessionContext, SessionFactory, factory::ProjectSessionFactory,
        },
        tools::{
            ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext,
            ToolRuntime,
        },
        turn::interaction::TurnInteractionAuthority,
    },
    storage::{TranscriptKey, TranscriptStore, recorder::Recorder},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Semaphore, mpsc, watch};
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
fn object<const N: usize>(items: [(&str, JsValue); N]) -> JsValue {
    let mut value = JsObject::new();
    for (k, v) in items {
        value.insert(k.into(), v);
    }
    JsValue::Object(value)
}
fn string(value: &JsObject, key: &str) -> Option<JsString> {
    value.get(key).and_then(JsValue::as_string).cloned()
}
fn error(message: impl Into<JsString>) -> ToolError {
    ToolError::failed(message)
}
fn fork_error(message: impl Into<JsString>, code: &str) -> ToolError {
    ToolError {
        code: "tool_execution_failed".into(),
        message: message.into(),
        details: Some(
            object([("errorCode", JsValue::String(code.into()))])
                .as_object()
                .unwrap()
                .clone(),
        ),
    }
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|p| p.units().iter().copied())
            .collect(),
    )
}

pub(super) struct NativeForkRegistry {
    factory: OnceLock<Weak<ProjectSessionFactory>>,
    scopes: Mutex<HashMap<JsString, Arc<ForkScope>>>,
    slots: Arc<Semaphore>,
}
impl NativeForkRegistry {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            factory: OnceLock::new(),
            scopes: Mutex::default(),
            slots: Arc::new(Semaphore::new(8)),
        })
    }
    pub(super) fn install_factory(&self, factory: &Arc<ProjectSessionFactory>) {
        assert!(
            self.factory.set(Arc::downgrade(factory)).is_ok(),
            "fork factory is installed once"
        );
    }
    pub(super) fn scope(&self, context: &SessionContext) -> Option<Arc<ForkScope>> {
        lock(&self.scopes)
            .get(&context.session_key)
            .filter(|scope| scope.channel == context.channel_key)
            .cloned()
    }
}
#[derive(Clone)]
pub(super) struct ForkScope {
    pub config: JsObject,
    pub permission_rules: RuleSetHandle,
    pub permission_mode: JsString,
    pub seed: SeedState,
    pub prompt_session: JsString,
    pub store: Arc<dyn TranscriptStore>,
    pub key: TranscriptKey,
    channel: JsString,
}
impl ForkScope {
    pub(super) fn seed_checkpoint(
        &self,
        checkpoint: &HostToolCheckpoint,
    ) -> Result<(), RuntimeError> {
        let validated = HostToolCheckpoint::new(Some(&self.seed), &[])
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        *lock(&checkpoint.read_file_state) = lock(&validated.read_file_state).clone();
        *lock(&checkpoint.write_snapshots) = lock(&validated.write_snapshots).clone();
        Ok(())
    }
    pub(super) fn compose(&self) -> JsObject {
        self.config.clone()
    }
}
struct ScopeGuard {
    registry: Arc<NativeForkRegistry>,
    key: JsString,
}
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        lock(&self.registry.scopes).remove(&self.key);
    }
}

struct Job {
    cancel: Cancellation,
    done: watch::Receiver<Option<Result<ToolOutput, ToolError>>>,
}
pub(super) struct ForkTurn {
    registry: Arc<NativeForkRegistry>,
    parent: ExecutionIdentity,
    config: JsObject,
    recorder: Arc<Recorder>,
    rules: RuleSetHandle,
    authority: Option<TurnInteractionAuthority>,
    prompt_route: Option<JsString>,
    tools: ToolRegistry,
    #[cfg(unix)]
    sandbox: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
    events: mpsc::Sender<JsObject>,
    events_rx: Mutex<mpsc::Receiver<JsObject>>,
    jobs: Mutex<HashMap<u64, Job>>,
    closed: Mutex<bool>,
    next: AtomicU64,
    cleanup_errors: Mutex<Vec<String>>,
    close_done: watch::Sender<Option<Result<(), HostError>>>,
}
impl ForkTurn {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        registry: Arc<NativeForkRegistry>,
        parent: ExecutionIdentity,
        config: JsObject,
        recorder: Arc<Recorder>,
        rules: RuleSetHandle,
        authority: Option<TurnInteractionAuthority>,
        scope: Option<&ForkScope>,
        tools: ToolRegistry,
        #[cfg(unix)] sandbox: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
    ) -> Arc<Self> {
        let (events, events_rx) = mpsc::channel(16);
        let (close_done, _) = watch::channel(None);
        Arc::new(Self {
            registry,
            parent,
            config,
            recorder,
            rules,
            authority,
            prompt_route: scope.map(|scope| scope.prompt_session.clone()),
            tools,
            #[cfg(unix)]
            sandbox,
            events,
            events_rx: Mutex::new(events_rx),
            jobs: Mutex::default(),
            closed: Mutex::new(false),
            next: AtomicU64::new(1),
            cleanup_errors: Mutex::default(),
            close_done,
        })
    }
    async fn emit(&self, mut value: JsObject) -> Result<(), ToolError> {
        value.insert(
            "sessionId".into(),
            JsValue::String(self.parent.session_id.clone()),
        );
        value.insert(
            "turnId".into(),
            JsValue::String(self.parent.turn_id.clone()),
        );
        if *lock(&self.closed) {
            return Err(ToolError::aborted());
        }
        self.events
            .send(value)
            .await
            .map_err(|_| ToolError::aborted())
    }
    pub(super) fn register(&self, tools: &ToolRegistry) {
        tools
            .register(Arc::new(AgentTool::new()))
            .expect("native agent name is unique");
    }
    fn close(self: Arc<Self>) -> HostFuture<()> {
        let mut completion = self.close_done.subscribe();
        let first = {
            let mut closed = lock(&self.closed);
            let first = !*closed;
            *closed = true;
            first
        };
        if first {
            {
                let mut receiver = lock(&self.events_rx);
                receiver.close();
                while receiver.try_recv().is_ok() {}
            }
            let jobs: Vec<_> = lock(&self.jobs)
                .values()
                .map(|job| {
                    job.cancel.cancel();
                    job.done.clone()
                })
                .collect();
            let done = self.close_done.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(executor) => crate::runtime::cleanup_task::spawn(&executor, done, async move {
                    // No deadline can prove blocked factory I/O quiescent. Keep
                    // owning every scope/child until it settles; the enclosing
                    // session/project drain reports its own timeout honestly.
                    for mut job in jobs {
                        if job.wait_for(Option::is_some).await.is_err() {
                            lock(&self.cleanup_errors)
                                .push("Subagent cleanup stopped before completion.".into());
                        }
                    }
                    let errors = lock(&self.cleanup_errors).clone();
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(ModuleError::new(errors.join("; ")).into())
                    }
                }),
                Err(_) => {
                    done.send_replace(Some(Err(ModuleError::new(
                        "Subagent cleanup requires its owned Tokio executor.",
                    )
                    .into())));
                }
            }
        }
        Box::pin(async move {
            loop {
                if let Some(result) = completion.borrow_and_update().clone() {
                    return result;
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| ModuleError::new("Subagent cleanup completion lost."))?;
            }
        })
    }
}
pub(super) struct ForkPort(pub Arc<ForkTurn>);
impl OneShotSubagent for ForkPort {
    fn create_fork(
        &self,
        _identity: JsObject,
        read: SharedFileState,
        write: SharedFileState,
    ) -> RuntimeContext {
        Arc::new(ForkApi {
            turn: self.0.clone(),
            read,
            write,
        })
    }
}
struct ForkApi {
    turn: Arc<ForkTurn>,
    read: SharedFileState,
    write: SharedFileState,
}
struct JobWait {
    cancel: Cancellation,
    done: watch::Receiver<Option<Result<ToolOutput, ToolError>>>,
}
impl Drop for JobWait {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct JobGuard {
    turn: Arc<ForkTurn>,
    id: u64,
    result: watch::Sender<Option<Result<ToolOutput, ToolError>>>,
}
impl Drop for JobGuard {
    fn drop(&mut self) {
        let missing = self.result.borrow().is_none();
        if missing {
            lock(&self.turn.cleanup_errors)
                .push("Subagent job stopped before completing its cleanup.".into());
            self.result.send_replace(Some(Err(error(
                "Subagent job stopped before completing its cleanup.",
            ))));
        }
        lock(&self.turn.jobs).remove(&self.id);
    }
}
struct ChildGuard {
    child: Option<LiveSessionHandle>,
    turn: Arc<ForkTurn>,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            lock(&self.turn.cleanup_errors)
                .push("Subagent handle fell back to asynchronous cleanup.".into());
            let _ = child.abort(&"parent_tool_dropped".into());
            drop(child.dispose("parent_tool_dropped".into()));
        }
    }
}

pub(super) struct ForkExecution {
    inner: Arc<ToolRuntime>,
    turn: Arc<ForkTurn>,
}
impl ForkExecution {
    pub(super) fn new(inner: Arc<ToolRuntime>, turn: Arc<ForkTurn>) -> Self {
        Self { inner, turn }
    }
}
impl ToolCatalog for ForkExecution {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        self.inner.list()
    }
}
impl ToolExecution for ForkExecution {
    fn applies_mode_effects(&self) -> bool {
        self.inner.applies_mode_effects()
    }
    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        let mut receiver = lock(&self.turn.events_rx);
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        Ok(events)
    }
    fn bind_turn_permission(&self, permission: TurnPermission) -> Result<(), HostError> {
        if let Some(session) = &self.turn.prompt_route {
            permission.bind_prompt_session(session.clone())?;
        }
        self.inner.bind_turn_permission(permission)
    }
    fn bind_turn_hooks(&self, hooks: TurnHooks) -> Result<(), HostError> {
        self.inner.bind_turn_hooks(hooks)
    }
    fn refresh(&self) -> HostFuture<()> {
        self.inner.refresh()
    }
    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        self.inner.execute_all(calls, context)
    }
    fn dispose(&self) -> HostFuture<()> {
        self.turn.clone().close()
    }
    fn wait_closed(&self) -> HostFuture<()> {
        self.turn.clone().close()
    }
}

struct AgentTool {
    fields: JsObject,
}
impl AgentTool {
    fn new() -> Self {
        let mut fields=object([
            ("name","agent".into()),("aliases",JsValue::Array(vec!["Agent".into(),"Task".into()])),
            ("description","Delegate a bounded task to a real child agent loop. Supply a short description and a complete directive. Built-in types: general-purpose, explore, plan, verify. Children inherit the parent's tool restrictions; nested delegation obeys maxSubagentDepth.".into()),
            ("kind","agent".into()),("isOpenWorld",JsValue::Bool(true)),("maxResultBytes",JsValue::Number(200_000.0)),
            ("requiredRuntimeCapabilities",JsValue::Array(vec!["subagent_fork".into()])),
            ("inputSchema",pilotdeck_protocol::parse_json(r#"{"type":"object","required":["description","prompt"],"additionalProperties":false,"properties":{"description":{"type":"string"},"prompt":{"type":"string"},"subagent_type":{"type":"string"},"subagentType":{"type":"string"}}}"#).expect("static agent schema")),
        ]).as_object().unwrap().clone();
        fields.insert("requiresUserInteraction".into(), JsValue::Bool(false));
        Self { fields }
    }
}
impl ToolDefinition for AgentTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["subagent_fork".into()])
    }
    fn check_permissions<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a crate::runtime::permission::PermissionContext,
    ) -> crate::runtime::permission::PermissionFuture<'a, Option<JsValue>> {
        Box::pin(async {
            Ok(Some(object([
                ("type", "allow".into()),
                (
                    "reason",
                    object([
                        ("type", "tool".into()),
                        ("toolName", "agent".into()),
                        (
                            "message",
                            "Subagent invocation is allowed without prompting.".into(),
                        ),
                    ]),
                ),
            ])))
        })
    }
}
impl ExecutableTool for AgentTool {
    #[cfg(unix)]
    fn enforces_native_sandbox(
        &self,
        context: &ToolRunContext,
        provider: &Arc<crate::runtime::tools::sandbox::NativeSandbox>,
    ) -> bool {
        context
            .host
            .subagent
            .as_ref()
            .and_then(|port| port.downcast_ref::<ForkApi>())
            .and_then(|api| api.turn.sandbox.as_ref())
            .is_some_and(|sandbox| Arc::ptr_eq(sandbox, provider))
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            if context.is_aborted() {
                return Err(ToolError::new(
                    "tool_aborted",
                    "agent subagent aborted before completion.",
                ));
            }
            let api = context
                .host
                .subagent
                .clone()
                .ok_or_else(|| error("Native full-fork service is unavailable."))?
                .downcast::<ForkApi>()
                .map_err(|_| error("Native full-fork service has the wrong type."))?;
            api.start(input, context).await
        })
    }
}
impl ForkApi {
    async fn start(
        self: Arc<Self>,
        input: JsValue,
        context: ToolRunContext,
    ) -> Result<ToolOutput, ToolError> {
        let description = input
            .get("description")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| ToolError::new("invalid_tool_input", "description must be a string."))?;
        let directive = input
            .get("prompt")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| ToolError::new("invalid_tool_input", "prompt must be a string."))?;
        let requested = input
            .get("subagent_type")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .or_else(|| input.get("subagentType"))
            .and_then(JsValue::as_string);
        let normalized = normalize_type(requested);
        let mut definition = normalized.to_utf8().unwrap_or_default();
        let parent_mode = context
            .fields
            .get("permissionContext")
            .and_then(|v| v.get("mode"))
            .and_then(JsValue::as_string)
            .cloned()
            .or_else(|| string(&self.turn.config, "permissionMode"))
            .unwrap_or_else(|| "default".into());
        let parent_ask = context
            .fields
            .get("runMode")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("ask"));
        if definition == "general-purpose" && (parent_mode.equals_str("plan") || parent_ask) {
            definition = "explore".into();
        }
        if !["general-purpose", "explore", "plan", "verify"].contains(&definition.as_str()) {
            return Err(ToolError::new(
                "invalid_tool_input",
                concat(&[
                    &"Unknown subagent_type \"".into(),
                    &normalized,
                    &"\". Available: general-purpose, explore, plan, verify.".into(),
                ]),
            ));
        }
        let depth = self
            .turn
            .config
            .get("subagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let max = self
            .turn
            .config
            .get("maxSubagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0);
        if depth >= max {
            return Err(fork_error(
                format!(
                    "subagent_depth_exceeded (depth={depth}, max={max}); nested fork rejected."
                ),
                "subagent_depth_exceeded",
            ));
        }
        let permit = self
            .turn
            .registry
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("Native subagent capacity is full."))?;
        let factory = self
            .turn
            .registry
            .factory
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("Native session factory is unavailable."))?;
        let mut wait = {
            let closed = lock(&self.turn.closed);
            if *closed {
                return Err(ToolError::aborted());
            }
            let id = self.turn.next.fetch_add(1, Ordering::SeqCst);
            let cancel = Cancellation::new();
            let (done, receiver) = watch::channel(None);
            lock(&self.turn.jobs).insert(
                id,
                Job {
                    cancel: cancel.clone(),
                    done: receiver.clone(),
                },
            );
            let guard = JobGuard {
                turn: self.turn.clone(),
                id,
                result: done,
            };
            let api = self.clone();
            let run_cancel = cancel.clone();
            let timeout = context
                .fields
                .get("subagentTimeoutMs")
                .and_then(JsValue::as_number)
                .unwrap_or(3_600_000.0);
            let deadline = tokio::time::Instant::now()
                + crate::runtime::tools::builtin::bash::runner::timer_delay(timeout);
            tokio::spawn(async move {
                let _permit = permit;
                let guard = guard;
                let result = api
                    .run(
                        factory,
                        definition,
                        description,
                        directive,
                        parent_mode,
                        parent_ask,
                        depth,
                        max,
                        context,
                        run_cancel,
                        deadline,
                    )
                    .await
                    .map_err(|failure| {
                        if failure.code == "tool_aborted" {
                            ToolError::new(
                                "tool_aborted",
                                "agent subagent aborted before completion.",
                            )
                        } else {
                            fork_error(
                                concat(&[&"agent subagent failed: ".into(), &failure.message]),
                                "subagent_execution_failed",
                            )
                        }
                    });
                guard.result.send_replace(Some(result));
            });
            drop(closed);
            JobWait {
                cancel,
                done: receiver,
            }
        };
        wait.done
            .wait_for(Option::is_some)
            .await
            .map_err(|_| error("Subagent result delivery stopped."))?
            .as_ref()
            .unwrap()
            .clone()
    }
    #[allow(clippy::too_many_arguments)]
    async fn run(
        self: Arc<Self>,
        factory: Arc<ProjectSessionFactory>,
        definition: String,
        description: JsString,
        directive: JsString,
        parent_mode: JsString,
        parent_ask: bool,
        depth: f64,
        max: f64,
        context: ToolRunContext,
        cancel: Cancellation,
        deadline: tokio::time::Instant,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let subagent: JsString = uuid::Uuid::new_v4().to_string().into();
        let cwd = string(&self.turn.config, "cwd")
            .ok_or_else(|| error("Subagent project root is missing."))?;
        let session = concat(&[&cwd, &"::sub::".into(), &subagent]);
        let turn = concat(&[&subagent, &"-t0".into()]);
        let channel: JsString = format!("native-fork:{}", uuid::Uuid::new_v4()).into();
        let readonly =
            definition != "general-purpose" || parent_ask || parent_mode.equals_str("plan");
        let allowed: Vec<_> =
            self.turn
                .tools
                .list_all()
                .map_err(|e| error(e.to_string()))?
                .into_iter()
                .filter_map(|tool| {
                    let name = tool
                        .fields()
                        .get("name")
                        .and_then(JsValue::as_string)?
                        .clone();
                    if definition == "plan"
                        && !["read_file", "grep", "glob"]
                            .iter()
                            .any(|v| name.equals_str(v))
                    {
                        return None;
                    }
                    if (definition == "explore" || definition == "verify")
                        && !["read_file", "grep", "glob", "bash"]
                            .iter()
                            .any(|v| name.equals_str(v))
                    {
                        return None;
                    }
                    let mut capabilities = tool.required_runtime_capabilities().unwrap_or_default();
                    if let Some(JsValue::Array(values)) =
                        tool.fields().get("requiredRuntimeCapabilities")
                    {
                        for value in values {
                            if let Some(value) = value.as_string() {
                                capabilities.push(value.clone());
                            }
                        }
                    }
                    if capabilities.iter().any(|capability| {
                        !capability.equals_str("subagent_fork") || depth + 1.0 >= max
                    }) {
                        return None;
                    }
                    if tool
                        .requires_user_interaction(&JsValue::Object(Default::default()))
                        .ok()
                        == Some(JsValue::Bool(true))
                    {
                        return None;
                    }
                    Some(name)
                })
                .collect();
        let (store, key, relative) = self.turn.recorder.writer().sidechain_storage(&subagent);
        let seed = SeedState {
            allowed_read_files: None,
            read_file_state: Some(lock(&self.read).clone()),
            write_snapshots: Some(lock(&self.write).clone()),
        };
        let descriptor = object([
            ("version", JsValue::Number(1.0)),
            ("mode", "one-shot".into()),
            ("provider", "pilotdeck-native".into()),
            ("definitionId", JsValue::String(definition.as_str().into())),
        ])
        .as_object()
        .unwrap()
        .clone();
        let mut config = self.turn.config.clone();
        config.insert(
            "permissionMode".into(),
            JsValue::String(parent_mode.clone()),
        );
        config.insert("isSubagent".into(), JsValue::Bool(true));
        config.insert("subagentDepth".into(), JsValue::Number(depth + 1.0));
        config.insert("maxSubagentDepth".into(), JsValue::Number(max));
        config.insert("stopOnStructuredOutput".into(), JsValue::Bool(false));
        config.insert(
            "subagentDescriptor".into(),
            JsValue::Object(descriptor.clone()),
        );
        if readonly {
            config.insert("runMode".into(), "ask".into());
        }
        if let Some(model) = config
            .get("subagentModel")
            .and_then(JsValue::as_object)
            .cloned()
        {
            for key in ["provider", "model"] {
                if let Some(value) = model.get(key) {
                    config.insert(key.into(), value.clone());
                }
            }
            config.insert("maxContextTokens".into(), JsValue::Undefined);
            config.insert("maxOutputTokens".into(), JsValue::Undefined);
        }
        let prompt = child_prompt(&definition, string(&config, "systemPrompt").as_ref());
        config.insert("systemPrompt".into(), JsValue::String(prompt));
        let prompt_session = self
            .turn
            .prompt_route
            .clone()
            .unwrap_or_else(|| self.turn.parent.session_id.clone());
        let scope = Arc::new(ForkScope {
            config,
            permission_rules: self.turn.rules.clone(),
            permission_mode: parent_mode,
            prompt_session,
            seed,
            store,
            key,
            channel: channel.clone(),
        });
        lock(&self.turn.registry.scopes).insert(session.clone(), scope);
        let _scope = ScopeGuard {
            registry: self.turn.registry.clone(),
            key: session.clone(),
        };
        let (prompt_preview, truncated) = preview(&directive, 1024);
        self.write_parent(object([
            ("type", "subagent_started".into()),
            ("subagentId", JsValue::String(subagent.clone())),
            ("subagentType", JsValue::String(definition.as_str().into())),
            ("promptPreview", JsValue::String(prompt_preview)),
            ("promptTruncated", JsValue::Bool(truncated)),
            (
                "transcriptRelativePath",
                JsValue::String(relative.as_str().into()),
            ),
            ("subagentSessionId", JsValue::String(session.clone())),
        ]))
        .await?;
        let _ = self
            .turn
            .emit(
                object([
                    ("type", "subagent_started".into()),
                    ("subagentId", JsValue::String(subagent.clone())),
                    ("subagentType", JsValue::String(definition.as_str().into())),
                    (
                        "toolCallId",
                        context
                            .fields
                            .get("currentToolCallId")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                ])
                .as_object()
                .unwrap()
                .clone(),
            )
            .await;
        let child_context = SessionContext {
            session_key: session.clone(),
            project_key: None,
            channel_key: channel,
            allowed_tools: Some(allowed),
            disallowed_tools: None,
        };
        let result = self
            .execute_child(
                factory,
                child_context,
                &turn,
                &subagent,
                &definition,
                &directive,
                &context,
                cancel,
                deadline,
            )
            .await;
        let elapsed = started.elapsed().as_millis() as f64;
        let (summary, usage, turns, errored) = match &result {
            Ok(report) => (
                report.text.clone(),
                report.usage.clone(),
                report.turns.clone(),
                false,
            ),
            Err(error) => (
                error.message.clone(),
                JsValue::Undefined,
                JsValue::Number(0.0),
                true,
            ),
        };
        let (summary, truncated) = preview(&summary, 4096);
        self.write_parent(object([
            ("type", "subagent_completed".into()),
            ("subagentId", JsValue::String(subagent.clone())),
            ("subagentType", JsValue::String(definition.as_str().into())),
            ("summaryPreview", JsValue::String(summary)),
            ("summaryTruncated", JsValue::Bool(truncated)),
            ("usage", usage),
            ("turns", turns),
            (
                "durationMs",
                JsValue::Number(if errored { 0.0 } else { elapsed }),
            ),
            ("errored", JsValue::Bool(errored)),
        ]))
        .await?;
        let _ = self
            .turn
            .emit(
                object([
                    ("type", "subagent_completed".into()),
                    ("subagentId", JsValue::String(subagent.clone())),
                    ("subagentType", JsValue::String(definition.as_str().into())),
                    ("success", JsValue::Bool(!errored)),
                    ("durationMs", JsValue::Number(elapsed)),
                ])
                .as_object()
                .unwrap()
                .clone(),
            )
            .await;
        let report = result?;
        Ok(agent_output(
            &definition,
            &description,
            &subagent,
            &session,
            &relative,
            report,
            elapsed,
        ))
    }
    async fn write_parent(&self, value: JsValue) -> Result<(), ToolError> {
        let recorder = self.turn.recorder.clone();
        let session = self.turn.parent.session_id.clone();
        let turn = self.turn.parent.turn_id.clone();
        tokio::task::spawn_blocking(move || {
            recorder.writer().record_session_event_checked(
                &session,
                &turn,
                value.as_object().unwrap().clone(),
            )
        })
        .await
        .map_err(|e| error(e.to_string()))?
        .map_err(|e| error(e.to_string()))
    }
}

struct Report {
    text: JsString,
    usage: JsValue,
    turns: JsValue,
    parsed: Option<JsObject>,
}
impl ForkApi {
    #[allow(clippy::too_many_arguments)]
    async fn execute_child(
        &self,
        factory: Arc<ProjectSessionFactory>,
        context: SessionContext,
        turn: &JsString,
        id: &JsString,
        definition: &str,
        directive: &JsString,
        parent_context: &ToolRunContext,
        cancel: Cancellation,
        deadline: tokio::time::Instant,
    ) -> Result<Report, ToolError> {
        let effective_mode = self
            .turn
            .registry
            .scope(&context)
            .map(|scope| scope.permission_mode.clone())
            .unwrap_or_else(|| "default".into());
        let creating = factory.create(context.clone(), None);
        tokio::pin!(creating);
        let child = tokio::select! {
            biased;
            result=&mut creating=>result.map_err(|e|error(e.to_string()))?,
            _=cancel.cancelled()=>{if let Ok(child)=creating.await{let _=child.abort(&"parent_cancelled".into());child.dispose("parent_cancelled".into()).await.map_err(|e|error(e.to_string()))?;}return Err(ToolError::aborted());},
            _=parent_context.cancelled()=>{if let Ok(child)=creating.await{let _=child.abort(&"parent_cancelled".into());child.dispose("parent_cancelled".into()).await.map_err(|e|error(e.to_string()))?;}return Err(ToolError::aborted());},
            _=tokio::time::sleep_until(deadline)=>{if let Ok(child)=creating.await{let _=child.abort(&"subagent_timeout".into());child.dispose("subagent_timeout".into()).await.map_err(|e|error(e.to_string()))?;}return Err(error("Subagent timed out before starting."));},
        };
        let mut guard = ChildGuard {
            child: Some(child.clone()),
            turn: self.turn.clone(),
        };
        let input = object([
            ("type", "text".into()),
            ("text", JsValue::String(directive.clone())),
        ]);
        let options = object([
            ("sessionId", JsValue::String(context.session_key.clone())),
            ("turnId", JsValue::String(turn.clone())),
            ("runId", JsValue::String(turn.clone())),
        ]);
        let submitted = match self.turn.authority.clone() {
            Some(mut authority) => {
                authority.permission_mode = Some(effective_mode);
                child.submit_with_authority(input, options, authority)
            }
            None => child.submit(input, options),
        };
        let result = match submitted {
            Err(e) => Err(error(e.to_string())),
            Ok(mut stream) => {
                let mut terminal = None;
                let mut terminal_finish = None;
                let mut last_text = JsString::default();
                let mut failure = None;
                loop {
                    let next = tokio::select! {
                        biased;
                        _=cancel.cancelled()=>{let _=child.abort(&"parent_cancelled".into());failure=Some(ToolError::aborted());break;},
                        _=parent_context.cancelled()=>{let _=child.abort(&"parent_cancelled".into());failure=Some(ToolError::aborted());break;},
                        _=tokio::time::sleep_until(deadline)=>{let _=child.abort(&"subagent_timeout".into());failure=Some(error("Subagent timed out before completion."));break;},
                        next=stream.next()=>next,
                    };
                    match next {
                        Err(e) => {
                            failure = Some(error(e.to_string()));
                            break;
                        }
                        Ok(None) => break,
                        Ok(Some(event)) => {
                            let kind = event.get("type").and_then(JsValue::as_string);
                            if kind.is_some_and(|kind| kind.equals_str("turn_completed")) {
                                terminal = event.get("result").cloned();
                                terminal_finish = event
                                    .get("result")
                                    .and_then(|result| result.get("stopReason"))
                                    .cloned();
                            }
                            if kind.is_some_and(|kind| kind.equals_str("assistant_message"))
                                && let Some(message) = event.get("message")
                                && let Some(text) = assistant_text(message)
                            {
                                last_text = text;
                            }
                            let forwarded = tokio::select! {
                                _=cancel.cancelled()=>Err(ToolError::aborted()),
                                _=parent_context.cancelled()=>Err(ToolError::aborted()),
                                _=tokio::time::sleep_until(deadline)=>Err(error("Subagent event delivery timed out.")),
                                result=self.forward_event(&event,id,definition)=>result,
                            };
                            if let Err(error) = forwarded {
                                let _ = child.abort(&"subagent_delivery_cancelled".into());
                                failure = Some(error);
                                break;
                            }
                        }
                    }
                }
                // Release activity before the idempotent owned handle drain.
                drop(stream);
                if let Some(failure) = failure {
                    Err(failure)
                } else if let Some(result) = terminal {
                    let kind = result.get("type").and_then(JsValue::as_string);
                    if !terminal_is_complete(terminal_finish.as_ref(), kind) {
                        Err(error(format!(
                            "Subagent turn failed: {}",
                            pilotdeck_protocol::stringify_json(&result)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "unknown result".into())
                        )))
                    } else {
                        // Actual durable projection is authoritative if the
                        // event stream did not carry the last assistant body.
                        if last_text.is_empty()
                            && let Some(JsValue::Array(messages)) = child.snapshot().get("messages")
                        {
                            for message in messages.iter().rev() {
                                if let Some(text) = assistant_text(message) {
                                    last_text = text;
                                    break;
                                }
                            }
                        }
                        let parsed = parse_report(&last_text);
                        Ok(Report {
                            text: last_text,
                            usage: result.get("usage").cloned().unwrap_or(JsValue::Undefined),
                            turns: result.get("turns").cloned().unwrap_or(JsValue::Number(0.0)),
                            parsed,
                        })
                    }
                } else {
                    Err(error("Subagent loop ended without a terminal result."))
                }
            }
        };
        let disposed = child.dispose("subagent_session_settled".into()).await;
        let _ = guard.child.take();
        disposed.map_err(|e| {
            let message = format!("Subagent storage/runtime disposal failed: {e}");
            lock(&self.turn.cleanup_errors).push(message.clone());
            error(message)
        })?;
        if parent_context.is_aborted() {
            return Err(ToolError::aborted());
        }
        result
    }
    async fn forward_event(
        &self,
        event: &JsValue,
        id: &JsString,
        definition: &str,
    ) -> Result<(), ToolError> {
        let Some(kind) = event.get("type").and_then(JsValue::as_string) else {
            return Ok(());
        };
        let mapped = if kind.equals_str("model_event") {
            Some((
                "subagent_model_event",
                vec!["event", "timeline", "blockId", "streamBoundary"],
            ))
        } else if kind.equals_str("tool_calls_detected") {
            Some(("subagent_tool_calls_detected", vec!["calls"]))
        } else if kind.equals_str("tool_result") {
            Some(("subagent_tool_result", vec!["result", "timeline"]))
        } else {
            None
        };
        if let Some((kind, fields)) = mapped {
            let mut output = object([
                ("type", JsValue::from(kind)),
                ("subagentId", JsValue::String(id.clone())),
                ("subagentType", JsValue::String(definition.into())),
            ])
            .as_object()
            .unwrap()
            .clone();
            for field in fields {
                if let Some(value) = event.get(field) {
                    output.insert(field.into(), value.clone());
                }
            }
            self.turn.emit(output).await?;
        }
        Ok(())
    }
}
fn terminal_is_complete(finish: Option<&JsValue>, kind: Option<&JsString>) -> bool {
    finish
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str("completed"))
        && !kind.is_some_and(|value| value.equals_str("error") || value.equals_str("aborted"))
}
fn normalize_type(value: Option<&JsString>) -> JsString {
    let Some(value) = value else {
        return "general-purpose".into();
    };
    let trimmed = trim(value);
    if trimmed.is_empty() {
        return "general-purpose".into();
    }
    let Ok(raw) = trimmed.to_utf8() else {
        return trimmed;
    };
    match raw.to_lowercase().as_str() {
        "general_purpose" | "general-purpose" | "general purpose" => "general-purpose".into(),
        "explore" | "explorer" => "explore".into(),
        "plan" => "plan".into(),
        "verify" => "verify".into(),
        _ => trimmed,
    }
}
fn child_prompt(definition: &str, parent: Option<&JsString>) -> JsString {
    let own = format!(
        "You are a focused PilotDeck child agent ({definition}). Follow only the delegated directive, use the available tools within the inherited policy, and finish with a concise report containing these five labels: Scope:, Result:, Key files:, Files changed:, Issues:. Do not ask follow-up questions. Do not create files unless the directive requests them."
    );
    let Some(parent) = parent else {
        return own.into();
    };
    let parent = if definition == "general-purpose" {
        parent.clone()
    } else {
        filter_parent_prompt(parent)
    };
    if parent.is_empty() {
        own.into()
    } else {
        concat(&[&own.into(), &"\n\n".into(), &parent])
    }
}
fn filter_parent_prompt(parent: &JsString) -> JsString {
    // Encode each UTF-16 unit separately, including lone surrogates, so the
    // markup filter cannot replacement-decode unrelated parent instructions.
    let mut text: String = parent
        .units()
        .iter()
        .map(|&unit| {
            char::from_u32(if (0xd800..=0xdfff).contains(&unit) {
                0xf0000 + u32::from(unit)
            } else {
                u32::from(unit)
            })
            .unwrap()
        })
        .collect();
    let ws = r"[\x09-\x0d\x20\u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
    for tag in [
        "agents-md",
        "project-instructions",
        "git-status",
        "gitStatus",
    ] {
        let regex = regex::Regex::new(&format!(
            r"(?s)(?:^|\n){ws}*<{tag}(?:{ws}[^>]*)?>.*?</{tag}>{ws}*"
        ))
        .expect("static subagent block filter");
        text = regex.replace_all(&text, "\n").into_owned();
    }
    let mut units: Vec<u16> = text
        .chars()
        .map(|ch| {
            let value = u32::from(ch);
            if value >= 0xf0000 {
                (value - 0xf0000) as u16
            } else {
                value as u16
            }
        })
        .collect();
    while units.last().is_some_and(|unit|matches!(*unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)){units.pop();}
    JsString::from_utf16(units)
}
fn assistant_text(message: &JsValue) -> Option<JsString> {
    if !message
        .get("role")
        .and_then(JsValue::as_string)
        .is_some_and(|role| role.equals_str("assistant"))
    {
        return None;
    }
    let Some(JsValue::Array(blocks)) = message.get("content") else {
        return None;
    };
    let parts: Vec<_> = blocks
        .iter()
        .filter(|block| block.get("type") == Some(&"text".into()))
        .filter_map(|block| block.get("text").and_then(JsValue::as_string))
        .collect();
    if parts.is_empty() {
        None
    } else {
        let mut units = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                units.push(10);
            }
            units.extend_from_slice(part.units());
        }
        Some(trim(&JsString::from_utf16(units)))
    }
}
fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(crate::runtime::memory::js_trim_units(value.units()).to_vec())
}
fn parse_report(text: &JsString) -> Option<JsObject> {
    let labels = ["Scope", "Result", "Key files", "Files changed", "Issues"];
    let lines: Vec<_> = text.units().split(|unit| *unit == 10).collect();
    let mut result = JsObject::new();
    for label in labels {
        let prefix: Vec<_> = format!("{label}:").encode_utf16().collect();
        let index = lines.iter().position(|line| line.starts_with(&prefix))?;
        let mut units = lines[index][prefix.len()..].to_vec();
        for line in &lines[index + 1..] {
            if labels.iter().any(|label| {
                line.starts_with(&format!("{label}:").encode_utf16().collect::<Vec<_>>())
            }) {
                break;
            }
            units.push(10);
            units.extend_from_slice(line);
        }
        result.insert(
            label.into(),
            JsValue::String(trim(&JsString::from_utf16(units))),
        );
    }
    Some(result)
}
fn preview(value: &JsString, cap: usize) -> (JsString, bool) {
    let mut used = 0;
    let mut end = 0;
    let units = value.units();
    while end < units.len() {
        let high = (0xd800..=0xdbff).contains(&units[end]);
        let paired = high && end + 1 < units.len() && (0xdc00..=0xdfff).contains(&units[end + 1]);
        let width = if paired {
            4
        } else if units[end] < 0x80 {
            1
        } else if units[end] < 0x800 {
            2
        } else {
            3
        };
        if used + width > cap {
            break;
        }
        used += width;
        end += if paired { 2 } else { 1 };
    }
    (
        JsString::from_utf16(units[..end].to_vec()),
        end < units.len(),
    )
}
fn agent_output(
    definition: &str,
    description: &JsString,
    id: &JsString,
    session: &JsString,
    relative: &str,
    report: Report,
    elapsed: f64,
) -> ToolOutput {
    let mut data = object([
        ("subagentType", JsValue::String(definition.into())),
        ("description", JsValue::String(description.clone())),
        ("text", JsValue::String(report.text.clone())),
        ("usage", report.usage),
        ("turns", report.turns.clone()),
        ("durationMs", JsValue::Number(elapsed)),
    ])
    .as_object()
    .unwrap()
    .clone();
    if let Some(parsed) = report.parsed {
        data.insert("parsed".into(), JsValue::Object(parsed));
    }
    data.insert("subagentSessionId".into(), JsValue::String(session.clone()));
    data.insert(
        "transcriptRelativePath".into(),
        JsValue::String(relative.into()),
    );
    let title: JsString = format!("[{definition}] ").into();
    let text = concat(&[&title, description, &"\n\n".into(), &report.text]);
    ToolOutput {
        content: vec![
            object([("type", "text".into()), ("text", JsValue::String(text))]),
            object([
                ("type", "json".into()),
                ("value", JsValue::Object(data.clone())),
            ]),
        ],
        data: Some(JsValue::Object(data)),
        metadata: Some(
            object([
                ("subagent", JsValue::String(definition.into())),
                ("subagentId", JsValue::String(id.clone())),
                ("forkMode", "full".into()),
                ("turns", report.turns),
                ("durationMs", JsValue::Number(elapsed)),
                ("subagentSessionId", JsValue::String(session.clone())),
                ("transcriptRelativePath", JsValue::String(relative.into())),
            ])
            .as_object()
            .unwrap()
            .clone(),
        ),
        ..Default::default()
    }
}

pub(super) fn apply_subagent_settings(config: &mut JsObject) {
    if let Some(settings) = config
        .get("subagents")
        .and_then(JsValue::as_object)
        .cloned()
    {
        for (source, target) in [
            ("maxDepth", "maxSubagentDepth"),
            ("timeoutMs", "subagentTimeoutMs"),
            ("default", "subagentModel"),
        ] {
            if let Some(value) = settings
                .get(source)
                .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            {
                config.insert(target.into(), value.clone());
            }
        }
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
