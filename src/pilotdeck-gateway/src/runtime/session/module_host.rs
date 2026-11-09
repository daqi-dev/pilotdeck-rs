//! Production Module 2.0 host for one sidecar turn.
//!
//! The sidecar asks the gateway to run model/context/tool/permission callbacks;
//! this composes the already-ported engines so `submit_turn` never reaches a
//! stub. Capability metadata and provider credentials come from the same
//! configuration snapshot the rest of the gateway uses.
use super::routes::live::LiveSessionState;
use crate::host::modules::{
    ModuleRouter, budget, capability, context, event, lifecycle, model, permission,
    tool_context::{
        CapabilityContext, ContextRequestIdentity, HostToolCheckpoint, PermissionRequestContext,
        SteerAuthorizations, ToolContextBuilder, ToolContextPorts, TurnBinding, TurnEnvironment,
    },
    turn,
};
use crate::host::{HostError, HostFuture, ModuleError};
use crate::runtime::model::{
    fallback::HttpRouterInvocation,
    http::{self, HyperModelTransport, ModelHttpTransport, ModelStreamError, StreamOptions},
    router::{
        AbortFlag, RouterConfig, RouterDecisionInput, RouterDeps, RouterFuture, RouterInvokeError,
        RouterModelInvocation, RouterRuntime, RouterSharedState,
    },
};
use crate::runtime::tools::{SchedulerMode, ToolRegistry, ToolRuntime};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    cell::RefCell,
    collections::HashMap,
    future::poll_fn,
    path::PathBuf,
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::sync::mpsc;

thread_local! {
    static SUBAGENT_PROVIDERS: RefCell<
        Option<Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>>,
    > = const { RefCell::new(None) };
    static MCP_OWNER: RefCell<Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>> = const { RefCell::new(None) };
    static USER_DIALOGS: RefCell<Option<Arc<crate::user_dialog::GatewayUserDialogs>>> = const { RefCell::new(None) };
    static LIVE_STATE: RefCell<Option<Arc<LiveSessionState>>> = const { RefCell::new(None) };
    static TOOL_REGISTRY: RefCell<Option<ToolRegistry>> = const { RefCell::new(None) };
    static LIFECYCLE: RefCell<Option<crate::runtime::hooks::LifecycleRuntime>> = const { RefCell::new(None) };
    static FILE_UPDATE_HOOK: RefCell<Option<crate::runtime::hooks::session::FileUpdateHook>> = const { RefCell::new(None) };
    static CONTEXT_RESOURCES: RefCell<Option<Arc<super::context_resources::ContextResources>>> = const { RefCell::new(None) };
}

type ContinuationPort =
    Arc<dyn crate::runtime::tools::builtin::subagent_continuation::SubagentContinuationPort>;
type WeakContinuationPort = std::sync::Weak<
    dyn crate::runtime::tools::builtin::subagent_continuation::SubagentContinuationPort,
>;

fn continuation_ports() -> &'static Mutex<HashMap<JsString, WeakContinuationPort>> {
    static PORTS: std::sync::OnceLock<Mutex<HashMap<JsString, WeakContinuationPort>>> =
        std::sync::OnceLock::new();
    PORTS.get_or_init(Default::default)
}

/// Weak session-local tool binding keyed by the exact published live identity.
pub(crate) struct LiveContinuationToolBinding {
    identity: JsString,
    port: WeakContinuationPort,
}

pub(crate) fn bind_live_continuation_tools(
    identity: JsString,
    port: ContinuationPort,
) -> LiveContinuationToolBinding {
    let port = Arc::downgrade(&port);
    continuation_ports()
        .lock()
        .unwrap()
        .insert(identity.clone(), port.clone());
    LiveContinuationToolBinding { identity, port }
}

impl Drop for LiveContinuationToolBinding {
    fn drop(&mut self) {
        let mut ports = continuation_ports().lock().unwrap();
        if ports
            .get(&self.identity)
            .is_some_and(|current| current.ptr_eq(&self.port))
        {
            ports.remove(&self.identity);
        }
    }
}

fn continuation_port(identity: &JsString) -> Option<ContinuationPort> {
    let mut ports = continuation_ports().lock().unwrap();
    let port = ports.get(identity)?.upgrade();
    if port.is_none() {
        ports.remove(identity);
    }
    port
}

fn live_continuation_port() -> Option<ContinuationPort> {
    continuation_port(&live_state()?.identity)
}

fn install_continuation_tools(
    registry: &ToolRegistry,
    port: ContinuationPort,
) -> std::io::Result<()> {
    for tool in
        crate::runtime::tools::builtin::subagent_continuation::subagent_continuation_tools(port)
    {
        registry
            .register(tool)
            .map_err(|error| std::io::Error::other(error.message))?;
    }
    Ok(())
}

fn install_live_continuation_tools(registry: &ToolRegistry) -> std::io::Result<()> {
    match live_continuation_port() {
        Some(port) => install_continuation_tools(registry, port),
        None => Ok(()),
    }
}

pub(crate) fn with_context_resources<T>(
    resources: Arc<super::context_resources::ContextResources>,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Arc<super::context_resources::ContextResources>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONTEXT_RESOURCES.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(CONTEXT_RESOURCES.with(|slot| slot.replace(Some(resources))));
    build()
}

pub(crate) fn current_context_resources() -> Option<Arc<super::context_resources::ContextResources>>
{
    CONTEXT_RESOURCES.with(|slot| slot.borrow().clone())
}

pub(crate) fn with_file_update_hook<T>(
    notifier: Option<crate::runtime::hooks::session::FileUpdateHook>,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<crate::runtime::hooks::session::FileUpdateHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FILE_UPDATE_HOOK.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(FILE_UPDATE_HOOK.with(|slot| slot.replace(notifier)));
    build()
}

pub(crate) fn with_lifecycle<T>(
    runtime: crate::runtime::hooks::LifecycleRuntime,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<crate::runtime::hooks::LifecycleRuntime>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIFECYCLE.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(LIFECYCLE.with(|slot| slot.replace(Some(runtime))));
    build()
}

pub(crate) fn with_tool_registry<T>(registry: ToolRegistry, build: impl FnOnce() -> T) -> T {
    struct Restore(Option<ToolRegistry>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TOOL_REGISTRY.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(TOOL_REGISTRY.with(|slot| slot.replace(Some(registry))));
    build()
}

pub(crate) fn current_mcp_owner()
-> Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>> {
    MCP_OWNER.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_subagent_providers()
-> Option<Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>> {
    SUBAGENT_PROVIDERS.with(|slot| slot.borrow().clone())
}

pub(crate) fn with_subagent_providers<T>(
    providers: Option<
        Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>,
    >,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(
        Option<
            Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>,
        >,
    );
    impl Drop for Restore {
        fn drop(&mut self) {
            SUBAGENT_PROVIDERS.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(SUBAGENT_PROVIDERS.with(|slot| slot.replace(providers)));
    build()
}

/// Keeps the subagent provider registry visible for an entire async turn worker.
pub(crate) struct SubagentProvidersScope {
    _restore: SubagentProvidersRestore,
}

struct SubagentProvidersRestore(
    Option<Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>>,
);

impl Drop for SubagentProvidersRestore {
    fn drop(&mut self) {
        SUBAGENT_PROVIDERS.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}

impl SubagentProvidersScope {
    pub fn enter(
        providers: Option<
            Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>,
        >,
    ) -> Self {
        let previous = SUBAGENT_PROVIDERS.with(|slot| slot.replace(providers));
        Self {
            _restore: SubagentProvidersRestore(previous),
        }
    }
}

/// SidecarSession::submit builds its module router synchronously, before
/// spawning. Capture real ports there; never hold this scope across an await.
/// Nested calls and unwind restore the previous scope, including on error.
pub fn with_live_state<T>(state: Arc<LiveSessionState>, submit: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<LiveSessionState>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIVE_STATE.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(LIVE_STATE.with(|slot| slot.replace(Some(state))));
    submit()
}

pub(crate) fn live_state() -> Option<Arc<LiveSessionState>> {
    LIVE_STATE.with(|slot| slot.borrow().clone())
}

pub fn with_mcp_owner<T>(
    owner: Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            MCP_OWNER.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(MCP_OWNER.with(|slot| slot.replace(owner)));
    build()
}

pub fn with_user_dialogs<T>(
    owner: Option<Arc<crate::user_dialog::GatewayUserDialogs>>,
    build: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Arc<crate::user_dialog::GatewayUserDialogs>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            USER_DIALOGS.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(USER_DIALOGS.with(|slot| slot.replace(owner)));
    build()
}

fn checkpoint(
    input: &JsValue,
    live: Option<&LiveSessionState>,
) -> std::io::Result<HostToolCheckpoint> {
    // Validate the wire seed even when non-serializable ports are injected.
    let seed = input
        .get("seedState")
        .map(crate::host::modules::tool_context::parse_seed_state)
        .transpose()
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .flatten();
    match live {
        Some(live) => Ok(live.checkpoint.clone()),
        None => HostToolCheckpoint::new(seed.as_ref(), &[])
            .map_err(|error| std::io::Error::other(error.to_string())),
    }
}

/// Events the sidecar emitted for this turn, in arrival order.
pub type EventDelivery = Arc<dyn Fn(JsValue) -> Result<(), HostError> + Send + Sync>;

#[derive(Default)]
pub struct EventBuffer {
    live: Mutex<Option<Arc<LiveSessionState>>>,
    buffered: Mutex<Vec<JsValue>>,
    delivery: Option<EventDelivery>,
}

impl EventBuffer {
    pub fn drain(&self) -> Vec<JsValue> {
        std::mem::take(&mut *self.buffered.lock().unwrap())
    }

    pub fn streaming(delivery: EventDelivery) -> Self {
        Self {
            buffered: Mutex::default(),
            delivery: Some(delivery),
            live: Mutex::default(),
        }
    }
}

impl event::EventSink for EventBuffer {
    fn emit(&self, event: JsObject) -> Result<(), HostError> {
        let live = self
            .live
            .lock()
            .map_err(|e| ModuleError::new(e.to_string()))?
            .clone();
        if let Some(state) = live {
            event::EventSink::emit(state.as_ref(), event.clone())?;
        }
        if let Some(delivery) = &self.delivery {
            return delivery(JsValue::Object(event));
        }
        self.buffered.lock().unwrap().push(JsValue::Object(event));
        Ok(())
    }
}

/// Ambient fields `buildTurnEnvironment` needs; never taken from payload.
struct Environment;
impl TurnEnvironment for Environment {
    fn build(
        &self,
        base: Option<&JsValue>,
        cwd: &JsString,
        session: &JsString,
        turn: &JsString,
    ) -> Result<JsObject, HostError> {
        lifecycle::build_turn_environment(base.and_then(JsValue::as_object), cwd, session, turn)
    }
}

/// `ModelExecution` over the ported provider HTTP path.
struct ProviderExecution {
    router: Arc<RouterRuntime>,
    cancel: crate::runtime::network::Cancellation,
    auto_route: bool,
    is_main_agent: bool,
    fallback_models: Vec<crate::runtime::model::router::RouterModelRef>,
    managed_model_policy: Option<crate::runtime::model::router::ManagedModelPolicy>,
    invocation: Option<crate::runtime::model::invocation::ModelInvocation>,
}

impl model::ModelExecution for ProviderExecution {
    fn prepare(&self, input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        let router = self.router.clone();
        let cancel = self.cancel.clone();
        let auto_route = self.auto_route;
        let is_main_agent = self.is_main_agent;
        let managed_model_policy = self.managed_model_policy.clone();
        Box::pin(async move {
            let decision = if auto_route {
                Some(
                    router
                        .decide(&RouterDecisionInput {
                            request: input.request.clone(),
                            session_id: input
                                .context
                                .get("sessionId")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                            is_main_agent,
                            abort: Some(cancel),
                            metadata: input
                                .context
                                .get("metadata")
                                .and_then(JsValue::as_object)
                                .cloned(),
                        })
                        .await
                        .map_err(|error| ModuleError {
                            code: Some(error.code.into()),
                            ..ModuleError::new(error.message)
                        })?,
                )
            } else {
                None
            };
            let request = decision.as_ref().map_or_else(
                || input.request.clone(),
                |decision| router.materialize_request(decision, &input.request),
            );
            let provider = request
                .get("provider")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "default".into());
            let name = request
                .get("model")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "default".into());
            router
                .validate_model_selection(&provider, &name, managed_model_policy.as_ref())
                .map_err(|error| ModuleError {
                    code: Some(error.code.into()),
                    ..ModuleError::new(error.message)
                })?;
            let mut prepared = model::PreparedInvocation::new(
                request,
                JsValue::String(provider),
                JsValue::String(name),
            );
            // Host-local routing state rides the existing opaque slot; it never
            // crosses the sidecar wire (`serialize_prepared` omits it).
            prepared.opaque = decision;
            Ok(prepared)
        })
    }

    fn stream(&self, input: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        let decision = input
            .prepared
            .opaque
            .clone()
            .unwrap_or_else(|| decision_for(&input.prepared));
        let request = input.prepared.request;
        let cancel = self.cancel.clone();
        let router = self.router.clone();
        let fallback_models = self.fallback_models.clone();
        let managed_model_policy = self.managed_model_policy.clone();
        let mut invocation = self.invocation.clone();
        if let Some(invocation) = &mut invocation {
            invocation.context.insert(
                "logicalCallId".into(),
                uuid::Uuid::new_v4().to_string().as_str().into(),
            );
        }
        let session_id = input
            .context
            .get("sessionId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let turn_id = input
            .context
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        Box::pin(async move {
            let (sender, events) = mpsc::unbounded_channel();
            let ctx = crate::runtime::model::router::RouterExecuteContext {
                session_id,
                turn_id,
                fallback_models,
                managed_model_policy,
                stream_options: crate::runtime::model::http::StreamOptions {
                    abort: Some(cancel),
                    ..Default::default()
                },
                ..Default::default()
            };
            let execution = Box::pin(async move {
                let mut sink = |event: JsValue| {
                    let _ = sender.send(event);
                };
                router
                    .execute(&decision, &request, &ctx, &mut sink)
                    .await
                    .map_err(|error| ModuleError::new(format!("Model execution failed: {error:?}")))
            });
            Ok(Box::new(ProviderStream {
                events,
                execution: Some(execution),
                error: None,
            }) as Box<dyn model::ModelStream>)
        })
    }
}

/// The canonical decision shape the router consumes for an already-prepared request.
fn decision_for(prepared: &model::PreparedInvocation) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert("provider".into(), prepared.provider.clone());
    decision.insert("model".into(), prepared.model.clone());
    decision.insert("scenarioType".into(), "explicit".into());
    JsValue::Object(decision)
}

fn router_fallback_models(input: &JsValue) -> Vec<crate::runtime::model::router::RouterModelRef> {
    let values = input.get("fallbackModels").or_else(|| {
        input
            .get("agent")
            .and_then(|agent| agent.get("fallbackModels"))
    });
    match values {
        Some(JsValue::Array(values)) => values
            .iter()
            .filter_map(|value| {
                let provider = value.get("provider")?.as_string()?.clone();
                let model = value.get("model")?.as_string()?.clone();
                Some(crate::runtime::model::router::RouterModelRef::new(
                    provider, model,
                ))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn router_managed_model_policy(
    input: &JsValue,
) -> Option<crate::runtime::model::router::ManagedModelPolicy> {
    let value = input.get("managedModelPolicy").or_else(|| {
        input
            .get("agent")
            .and_then(|agent| agent.get("managedModelPolicy"))
    })?;
    let strings = |key: &str| match value.get(key) {
        Some(JsValue::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_string().cloned())
            .collect(),
        _ => Vec::new(),
    };
    Some(crate::runtime::model::router::ManagedModelPolicy {
        allow: strings("allow"),
        deny: strings("deny"),
    })
}

struct ProviderStream {
    events: mpsc::UnboundedReceiver<JsValue>,
    // Owned by the iterator: dropping or closing it drops the HTTP exchange.
    execution: Option<model::ModelFuture<()>>,
    error: Option<ModuleError>,
}

/// Durable ports write before model/tool work becomes visible to the sidecar.
struct RecordedModel {
    inner: Arc<dyn model::ModelExecution>,
    state: Arc<LiveSessionState>,
    session: JsString,
    turn: JsString,
}
impl model::ModelExecution for RecordedModel {
    fn prepare(&self, input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        self.inner.prepare(input)
    }
    fn stream(&self, input: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        let inner = self.inner.clone();
        let state = self.state.clone();
        let session = self.session.clone();
        let turn = self.turn.clone();
        Box::pin(async move {
            state
                .recorder
                .record_model_request(&session, &turn, input.prepared.request.clone())
                .map_err(|e| ModuleError::new(e.to_string()))?;
            let stream = inner.stream(input).await?;
            Ok(Box::new(RecordedStream {
                inner: stream,
                recorder: state.recorder.clone(),
                session,
                turn,
                pending: None,
            }) as Box<dyn model::ModelStream>)
        })
    }
}
struct RecordedStream {
    inner: Box<dyn model::ModelStream>,
    recorder: Arc<crate::storage::recorder::Recorder>,
    session: JsString,
    turn: JsString,
    // Keep the event and its write across cancelled pulls, like model_port.
    pending: Option<(JsValue, tokio::task::JoinHandle<Result<(), HostError>>)>,
}
impl model::ModelStream for RecordedStream {
    fn next(&mut self) -> model::StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            if self.pending.is_none() {
                let Some(event) = self.inner.next().await? else {
                    return Ok(None);
                };
                let recorder = self.recorder.clone();
                let session = self.session.clone();
                let turn = self.turn.clone();
                let recorded = event.clone();
                let record = tokio::spawn(crate::runtime::model::cpu::run(move || {
                    recorder
                        .record_model_event(&session, &turn, &recorded)
                        .map_err(|e| ModuleError::new(e.to_string()).into())
                }));
                self.pending = Some((event, record));
            }
            let result = (&mut self.pending.as_mut().unwrap().1).await;
            let (event, _) = self.pending.take().unwrap();
            result
                .map_err(|e| ModuleError::new(e.to_string()))?
                .map_err(|e| ModuleError::new(e.message))?;
            Ok(Some(event))
        })
    }
    fn close(&mut self) -> model::StreamFuture<'_, ()> {
        Box::pin(async move {
            if let Some((_, record)) = &mut self.pending {
                let _ = record.await;
            }
            self.pending = None;
            self.inner.close().await
        })
    }
}
struct RecordedTools {
    inner: Arc<dyn capability::ToolExecution>,
    state: Arc<LiveSessionState>,
    session: JsString,
    turn: JsString,
}
impl capability::ToolCatalog for RecordedTools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        capability::ToolCatalog::list(self.inner.as_ref())
    }
}
impl capability::ToolExecution for RecordedTools {
    fn refresh(&self) -> HostFuture<()> {
        capability::ToolExecution::refresh(self.inner.as_ref())
    }
    fn execute_all(
        &self,
        calls: Vec<capability::ToolCall>,
        context: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let inner = self.inner.clone();
        let state = self.state.clone();
        let session = self.session.clone();
        let turn = self.turn.clone();
        Box::pin(async move {
            let encoded: Vec<_> = calls
                .iter()
                .map(|call| {
                    let mut value = JsObject::new();
                    value.insert("id".into(), JsValue::String(call.id.clone()));
                    value.insert("name".into(), JsValue::String(call.name.clone()));
                    value.insert("input".into(), call.input.clone());
                    JsValue::Object(value)
                })
                .collect();
            state
                .recorder
                .record_tool_calls(&session, &turn, &encoded)
                .map_err(|e| ModuleError::new(e.to_string()))?;
            let results =
                capability::ToolExecution::execute_all(inner.as_ref(), calls, context).await?;
            state
                .recorder
                .record_tool_results(
                    &session,
                    &turn,
                    &results
                        .iter()
                        .cloned()
                        .map(JsValue::Object)
                        .collect::<Vec<_>>(),
                )
                .map_err(|e| ModuleError::new(e.to_string()))?;
            Ok(results)
        })
    }
}

impl model::ModelStream for ProviderStream {
    fn next(&mut self) -> model::StreamFuture<'_, Option<JsValue>> {
        Box::pin(poll_fn(move |cx| {
            if let Some(execution) = &mut self.execution
                && let Poll::Ready(result) = execution.as_mut().poll(cx)
            {
                self.execution = None;
                self.error = result.err();
            }
            if let Poll::Ready(Some(event)) = self.events.poll_recv(cx) {
                return Poll::Ready(Ok(Some(event)));
            }
            if self.execution.is_none() {
                return Poll::Ready(self.error.take().map_or(Ok(None), Err));
            }
            Poll::Pending
        }))
    }

    fn close(&mut self) -> model::StreamFuture<'_, ()> {
        self.execution = None;
        self.error = None;
        self.events.close();
        while self.events.try_recv().is_ok() {}
        Box::pin(async { Ok(()) })
    }
}

/// Capability lookups from the configuration snapshot.
struct SnapshotMetadata(JsValue);

impl SnapshotMetadata {
    fn model(&self, provider: &JsString, model: &JsString) -> Option<JsValue> {
        self.0
            .get("config")?
            .get("model")?
            .get("providers")?
            .as_object()?
            .get_key(provider)?
            .get("models")?
            .as_object()?
            .get_key(model)
            .cloned()
    }
}

impl model::ModelMetadata for SnapshotMetadata {
    fn get_model_max_context_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (provider.as_string(), model.as_string()) else {
            return Ok(None);
        };
        Ok(self
            .model(provider, model)
            .and_then(|value| value.get("capabilities")?.get("maxContextTokens").cloned()))
    }
    fn get_model_max_output_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (provider.as_string(), model.as_string()) else {
            return Ok(None);
        };
        Ok(self
            .model(provider, model)
            .and_then(|value| value.get("capabilities")?.get("maxOutputTokens").cloned()))
    }
    fn get_model_token_limits(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (provider.as_string(), model.as_string()) else {
            return Ok(None);
        };
        let Some(capabilities) = self
            .model(provider, model)
            .and_then(|value| value.get("capabilities").cloned())
        else {
            return Ok(None);
        };
        let mut limits = JsObject::new();
        for key in ["maxContextTokens", "maxOutputTokens"] {
            if let Some(value) = capabilities.get(key) {
                limits.insert(key.into(), value.clone());
            }
        }
        Ok(Some(JsValue::Object(limits)))
    }
    fn get_model_protocol(&self, provider: &JsValue) -> Result<Option<JsValue>, ModuleError> {
        let Some(provider) = provider.as_string() else {
            return Ok(None);
        };
        Ok(self
            .0
            .get("config")
            .and_then(|config| config.get("model"))
            .and_then(|model| model.get("providers"))
            .and_then(|providers| providers.as_object()?.get_key(provider))
            .and_then(|provider| provider.get("protocol"))
            .cloned())
    }
    fn get_model_supports_prompt_cache(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (provider.as_string(), model.as_string()) else {
            return Ok(None);
        };
        Ok(self.model(provider, model).and_then(|value| {
            value
                .get("capabilities")?
                .get("supportsPromptCache")
                .cloned()
        }))
    }
}

/// Adapter from the catalog definition to the permission engine's input.
struct PermissionTool(Arc<dyn capability::ToolDefinition>);
impl crate::runtime::permission::PermissionTool for PermissionTool {
    fn name(&self) -> &JsString {
        self.0
            .fields()
            .get("name")
            .and_then(JsValue::as_string)
            .expect("catalog tool name")
    }
    fn kind(&self) -> Option<&JsString> {
        self.0.fields().get("kind").and_then(JsValue::as_string)
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.0.is_read_only(input)
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        context: &'a crate::runtime::permission::PermissionContext,
    ) -> crate::runtime::permission::PermissionFuture<'a, Option<JsValue>> {
        let builtin_agent = self.0.fields().get("name") == Some(&JsValue::from("agent"))
            && self
                .0
                .required_runtime_capabilities()
                .is_some_and(|v| v.contains(&"subagent_fork".into()));
        Box::pin(async move {
            if let Some(decision) = self.0.check_permissions(input, context).await? {
                return Ok(Some(decision));
            }
            Ok(builtin_agent.then(|| {
                crate::metadata::object([
                    ("type", "allow".into()),
                    (
                        "reason",
                        crate::metadata::object([
                            ("type", "tool".into()),
                            ("toolName", "agent".into()),
                            (
                                "message",
                                "Subagent invocation is allowed without prompting.".into(),
                            ),
                        ]),
                    ),
                ])
            }))
        })
    }
}

struct ProductionPermission {
    mode: Arc<
        Mutex<(
            crate::config::permission::PermissionMode,
            Option<crate::config::permission::PermissionMode>,
        )>,
    >,
    mcp_owner: Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    events: Arc<EventBuffer>,
    session: JsString,
    turn: JsString,
    cancel: crate::runtime::network::Cancellation,
    timeout: Option<std::time::Duration>,
}
impl crate::host::modules::permission::PermissionDecision for ProductionPermission {
    fn decide(
        &self,
        tool: Arc<dyn capability::ToolDefinition>,
        input: JsValue,
        context: capability::RuntimeContext,
        tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        let mcp_owner = self.mcp_owner.clone();
        let interactions = self.interactions.clone();
        let events = self.events.clone();
        let session = self.session.clone();
        let turn = self.turn.clone();
        let cancel = self.cancel.clone();
        let timeout = self.timeout;
        let mode = self.mode.clone();
        Box::pin(async move {
            let fields = if let Some(context) =
                context.downcast_ref::<crate::runtime::tools::ToolRunContext>()
            {
                &context.fields
            } else if let Some(context) =
                context.downcast_ref::<crate::host::modules::tool_context::ToolRuntimeContext>()
            {
                &context.fields
            } else {
                return Err(ModuleError::new("Permission requires a host tool context.").into());
            };
            let mut parsed = parse_permission_context(fields)?;
            parsed.mode = mode.lock().unwrap_or_else(|e| e.into_inner()).0.clone();
            let session = fields
                .get("permissionSessionId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or(session);
            if let Some(owner) = mcp_owner {
                let rules = owner.rules().snapshot();
                parsed.rules.allow.extend(rules.allow);
                parsed.rules.deny.extend(rules.deny);
                parsed.rules.ask.extend(rules.ask);
            }
            if let Some(interactions) = &interactions {
                parsed
                    .rules
                    .allow
                    .extend(interactions.session_allow_rules(&session).snapshot());
            }
            let tool = PermissionTool(tool);
            let decision =
                crate::runtime::permission::decide(&parsed, &tool, &input, &tool_call_id).await?;
            if decision.get("type") != Some(&JsValue::from("ask")) {
                return Ok(decision);
            }
            let Some(interactions) = interactions else {
                return Ok(decision);
            };
            let request_id: JsString = uuid::Uuid::new_v4().to_string().into();
            let mut payload = decision
                .get("request")
                .and_then(JsValue::as_object)
                .cloned()
                .ok_or_else(|| ModuleError::new("Permission ask request is missing."))?;
            let (send, receive) = tokio::sync::oneshot::channel();
            let send = Arc::new(Mutex::new(Some(send)));
            let resolved = send.clone();
            let entry = crate::runtime::interaction::PendingEntry {
                request_id: request_id.clone(),
                tool_call_id,
                tool_name: tool
                    .0
                    .fields()
                    .get("name")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                resolve: Arc::new(move |answer| {
                    if let Some(send) = resolved.lock().unwrap().take() {
                        let _ = send.send(Ok(answer));
                    }
                }),
                reject: Arc::new(move |reason| {
                    if let Some(send) = send.lock().unwrap().take() {
                        let _ = send.send(Err(reason));
                    }
                }),
            };
            let registration = interactions
                .permission_bus()
                .register(&session, entry, Some(JsValue::Object(payload.clone())))
                .map_err(ModuleError::new)?;
            struct Cleanup(crate::runtime::interaction::Registration);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    self.0.dispose();
                }
            }
            let _cleanup = Cleanup(registration);
            payload.insert("type".into(), "permission_request".into());
            payload.insert("requestId".into(), JsValue::String(request_id.clone()));
            payload.insert("sessionId".into(), JsValue::String(session.clone()));
            payload.insert("turnId".into(), JsValue::String(turn));
            use crate::host::modules::event::EventSink;
            events.emit(payload)?;
            let timeout_answer = async {
                match timeout {
                    Some(timeout) => tokio::time::sleep(timeout).await,
                    None => std::future::pending().await,
                }
                let _entry =
                    match interactions
                        .permission_bus()
                        .consume(&session, &request_id, None)
                    {
                        Some(entry) => entry,
                        None => std::future::pending().await,
                    };
                crate::metadata::object([
                    ("requestId", JsValue::String(request_id.clone())),
                    ("decision", "deny".into()),
                    ("reason", "Permission prompt timed out.".into()),
                    ("outcome", "timeout".into()),
                ])
            };
            let answer = tokio::select! {
                answer = receive => answer.ok().and_then(Result::ok),
                _ = cancel.cancelled() => None,
                answer = timeout_answer => Some(answer),
            };
            let kind = answer
                .as_ref()
                .and_then(|a| a.get("decision"))
                .and_then(JsValue::as_string);
            let decision_type = match kind {
                Some(v) if v.equals_str("allow") || v.equals_str("allow_once") => "allow",
                Some(v) if v.equals_str("deny") => "deny",
                _ => "cancel",
            };
            let mut result = JsObject::new();
            result.insert("type".into(), decision_type.into());
            if let Some(answer) = answer {
                for key in ["reason", "outcome"] {
                    if let Some(value) = answer.get(key) {
                        result.insert(key.into(), value.clone());
                    }
                }
                if decision_type == "deny"
                    && let Some(message) = answer.get("reason").and_then(JsValue::as_string)
                {
                    result.insert("message".into(), JsValue::String(message.clone()));
                }
            }
            Ok(JsValue::Object(result))
        })
    }
}

fn parse_permission_context(
    fields: &JsObject,
) -> Result<crate::runtime::permission::PermissionContext, HostError> {
    let context = fields.get("permissionContext").and_then(JsValue::as_object);
    let rules = context
        .and_then(|value| value.get("rules"))
        .and_then(JsValue::as_object)
        .map(|rules| crate::config::permission::PermissionRuleSet {
            allow: parse_rules(rules.get("allow")),
            deny: parse_rules(rules.get("deny")),
            ask: parse_rules(rules.get("ask")),
        })
        .unwrap_or_default();
    let mode = fields
        .get("permissionMode")
        .and_then(JsValue::as_string)
        .cloned()
        .map(crate::config::permission::PermissionMode::new)
        .unwrap_or_else(|| crate::config::permission::PermissionMode::from("default"));
    Ok(
        crate::runtime::permission::create_default_permission_context(
            crate::runtime::permission::DefaultPermissionContextOptions {
                cwd: fields
                    .get("cwd")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                mode: Some(mode),
                can_prompt: context
                    .and_then(|value| value.get("canPrompt"))
                    .and_then(JsValue::as_bool),
                bypass_available: context
                    .and_then(|value| value.get("bypassAvailable"))
                    .and_then(JsValue::as_bool),
                rules,
                ..Default::default()
            },
        ),
    )
}

fn parse_rules(value: Option<&JsValue>) -> Vec<crate::config::permission::PermissionRule> {
    match value {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(crate::config::permission::PermissionRule::from_value)
            .collect(),
        _ => Vec::new(),
    }
}

impl capability::CapabilityResultObserver for ProductionPermission {
    fn on_capability_results(&self, results: Vec<JsObject>) -> HostFuture<()> {
        let mut state = self.mode.lock().unwrap_or_else(|e| e.into_inner());
        for result in results {
            if result.get("type") != Some(&"success".into()) {
                continue;
            }
            let Some(mode) = result
                .get("data")
                .and_then(|value| value.get("requestedMode"))
                .filter(|value| crate::runtime::permission::is_permission_mode(value))
                .and_then(JsValue::as_string)
            else {
                continue;
            };
            let mut mode = crate::config::permission::PermissionMode::from(mode.clone());
            if mode.as_js_string().equals_str("plan") && !state.0.as_js_string().equals_str("plan")
            {
                state.1 = Some(state.0.clone());
            } else if state.0.as_js_string().equals_str("plan")
                && !mode.as_js_string().equals_str("plan")
                && let Some(previous) = state.1.take()
            {
                mode = previous;
            }
            state.0 = mode;
        }
        Box::pin(async { Ok(()) })
    }
}

/// Give plan tools the same current mode that permission decisions observe.
struct ModeCapabilityContext {
    inner: CapabilityContext,
    permission: Arc<ProductionPermission>,
}
impl capability::ToolContext for ModeCapabilityContext {
    fn prepare(&self, call: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        let prepared = self.inner.prepare(call);
        let permission = self.permission.clone();
        Box::pin(async move {
            let mut context = prepared.await?;
            let runtime = Arc::get_mut(&mut context.runtime)
                .and_then(|value| {
                    value.downcast_mut::<crate::host::modules::tool_context::ToolRuntimeContext>()
                })
                .ok_or_else(|| ModuleError::new("Expected owned capability tool context."))?;
            let mode = permission
                .mode
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .0
                .clone();
            runtime.fields.insert(
                "permissionMode".into(),
                JsValue::String(mode.as_js_string().clone()),
            );
            let mut permission_context = runtime
                .fields
                .get("permissionContext")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            permission_context.insert("mode".into(), JsValue::String(mode.as_js_string().clone()));
            runtime.fields.insert(
                "permissionContext".into(),
                JsValue::Object(permission_context),
            );
            Ok(context)
        })
    }
}

/// Build the router for one turn. `snapshot` is the loaded configuration; the
/// `input` is the execute payload the sidecar received.
pub fn build(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
) -> std::io::Result<ModuleRouter> {
    let turn_id = input
        .get("turnId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let mailbox = crate::runtime::steer::SteerMailbox::new(None);
    mailbox.start(turn_id.clone());
    build_with_steers(
        snapshot,
        input,
        events,
        cancel,
        crate::runtime::steer::TurnSteers::new(mailbox, turn_id, Vec::new()),
    )
}

/// The live session and module dispatcher share this exact turn mailbox.
pub fn build_with_steers(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
    steers: crate::runtime::steer::TurnSteers,
) -> std::io::Result<ModuleRouter> {
    build_with_runtime(snapshot, input, events, cancel, steers, None)
}

pub fn build_with_runtime(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
    steers: crate::runtime::steer::TurnSteers,
    stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
) -> std::io::Result<ModuleRouter> {
    build_with_services_runtime(snapshot, input, events, cancel, steers, stats, None)
}

pub fn build_with_services_runtime(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
    steers: crate::runtime::steer::TurnSteers,
    stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
    services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
) -> std::io::Result<ModuleRouter> {
    build_with_services_interactions(
        snapshot, input, events, cancel, steers, stats, services, None,
    )
}

// Preserve the explicit assembly interface and existing callers.
#[allow(clippy::too_many_arguments)]
pub fn build_with_services_interactions(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
    steers: crate::runtime::steer::TurnSteers,
    stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
    services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
) -> std::io::Result<ModuleRouter> {
    build_with_router_state(
        snapshot,
        input,
        events,
        cancel,
        steers,
        stats,
        services,
        interactions,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_with_router_state(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: crate::runtime::network::Cancellation,
    steers: crate::runtime::steer::TurnSteers,
    stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
    services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    router_state: Option<Arc<RouterSharedState>>,
) -> std::io::Result<ModuleRouter> {
    let live = live_state();
    let checkpoint = checkpoint(input, live.as_deref())?;
    let transport: Arc<dyn ModelHttpTransport> =
        Arc::new(HyperModelTransport::from_snapshot(&snapshot));
    let model_config = snapshot
        .get("config")
        .and_then(|config| config.get("model"))
        .cloned()
        .unwrap_or(JsValue::Null);
    let audit = provider_audit(input);
    let delegate = Arc::new(SnapshotInvocation {
        snapshot: snapshot.clone(),
        transport: transport.clone(),
        model_config: model_config.clone(),
        invocation: audit.clone(),
    });
    let budget_config = model_config.as_object().cloned().unwrap_or_default();
    let token_accounting = Arc::new(
        crate::runtime::model::token_accounting::TokenAccountingRuntime::new_with_snapshot(
            budget_config,
            &snapshot,
        ),
    );
    let invocation = Arc::new(HttpRouterInvocation::new(delegate, transport, model_config));
    let mut deps = RouterDeps::new(invocation);
    deps.stats = stats.clone();
    if let Some(shared) = &router_state {
        deps.custom_routers = shared.custom_routers.clone();
        shared.apply_model_policy(&snapshot, &mut deps);
    }
    deps.shared = router_state;
    let router_events = events.clone();
    deps.events = Arc::new(move |event: JsValue| {
        if let Some(mut event) = event.as_object().cloned() {
            use crate::host::modules::event::EventSink;
            let _ = router_events.emit(std::mem::take(&mut event));
        }
    });
    let mut router_config = RouterConfig::from_snapshot(&snapshot);
    // createLocalGateway disables absent optional routers before composing them.
    if snapshot
        .get("config")
        .and_then(|config| config.get("router"))
        .is_none_or(|router| matches!(router, JsValue::Null | JsValue::Undefined))
    {
        router_config.enabled = false;
    }
    let router = RouterRuntime::with_deps(router_config, deps)
        .map_err(|error| std::io::Error::other(format!("{error}")))?;
    let execution: Arc<dyn model::ModelExecution> = Arc::new(ProviderExecution {
        router: Arc::new(router),
        cancel: cancel.clone(),
        // Only NativeSubmitHost's top-level intent can enable auto routing.
        auto_route: input
            .get("modelSelectionSource")
            .and_then(JsValue::as_string)
            .is_some_and(|source| source.equals_str("router")),
        is_main_agent: input
            .get("agent")
            .and_then(|agent| agent.get("isSubagent"))
            .and_then(JsValue::as_bool)
            != Some(true),
        fallback_models: router_fallback_models(input),
        managed_model_policy: router_managed_model_policy(input),
        invocation: audit,
    });
    let metadata: Arc<dyn model::ModelMetadata> = Arc::new(SnapshotMetadata(snapshot.clone()));

    let session_id = input
        .get("sessionId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let turn_id = input
        .get("turnId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let cwd = input
        .get("agent")
        .and_then(|agent| agent.get("cwd"))
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let execution: Arc<dyn model::ModelExecution> = match &live {
        Some(state) => Arc::new(RecordedModel {
            inner: execution,
            state: state.clone(),
            session: session_id.clone(),
            turn: turn_id.clone(),
        }),
        None => execution,
    };
    let mut turn_input = input.as_object().cloned().unwrap_or_default();
    let sop_sessions_root = snapshot
        .get("config")
        .and_then(|config| config.get("modules"))
        .and_then(|modules| modules.get("sop"))
        .and_then(|sop| {
            crate::runtime::sop::sessions_root_from_sop_config(
                cwd.to_utf8().unwrap_or_default().as_str(),
                sop,
            )
        });
    let sop_prepared = crate::runtime::sop::prepare_step::shared_prepared_step_store();
    let sop_step_knowledge =
        Arc::new(crate::runtime::sop::control_tool_port::SopStepKnowledgeRegistry::default());
    let authority_binding =
        crate::runtime::sop::authority_port::sop_capability_authority_binding(&snapshot, &cwd);
    let sop_authority: Option<Arc<dyn crate::runtime::sop::authority_port::SopAuthorityResolve>> =
        authority_binding
            .as_ref()
            .and_then(|binding| {
                crate::runtime::sop::authority_port::HttpSopCapabilityAuthorityPort::from_knowledge_binding(
                    binding,
                )
            })
            .map(|port| Arc::new(port) as Arc<dyn crate::runtime::sop::authority_port::SopAuthorityResolve>);
    if let Some(sessions_root) = sop_sessions_root.as_deref() {
        let project_key = cwd.to_utf8().unwrap_or_default();
        let _ = crate::runtime::sop::prepare_step::production_prepare_current_step_optional_knowledge_blocking(
            crate::runtime::sop::prepare_step::ProductionPrepareInput {
                sessions_root,
                session_id: &session_id,
                turn_id: &turn_id,
                project_key: &project_key,
                registry: &sop_step_knowledge,
                prepared: sop_prepared.as_ref(),
                turn: &mut turn_input,
                authority_binding: authority_binding.as_ref(),
                authority: sop_authority.clone(),
                knowledge_query_registered: crate::runtime::sop::prepare_step::knowledge_query_available(
                    &snapshot,
                ),
            },
        );
    }
    let binding = TurnBinding {
        config: input
            .get("agent")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default(),
        input: turn_input,
        checkpoint: checkpoint.clone(),
        abort_signal: Some(Arc::new(cancel.clone()) as capability::RuntimeContext),
        environment: Arc::new(Environment),
    };
    let mut ports = ToolContextPorts {
        file_history: live
            .as_ref()
            .map(|state| state.file_history.clone() as capability::RuntimeContext),
        ..Default::default()
    };
    if let Some(session_services) = services.as_ref() {
        ports.goal = Some(session_services.goal.clone());
        ports.plan_files = Some(Arc::new(NativePlanFiles::new(cwd.clone())));
    }
    if let Some(interactions) = interactions.clone()
        && binding.config.get("isSubagent") != Some(&JsValue::Bool(true))
    {
        let event_sink = events.clone();
        let event_session = session_id.clone();
        let event_turn = turn_id.clone();
        ports.elicitation = Some(Arc::new(
            crate::runtime::interaction::channel::QuestionChannel::new(
                interactions,
                session_id.clone(),
                Arc::new(move |mut event| {
                    use crate::host::modules::event::EventSink;
                    event.insert("sessionId".into(), JsValue::String(event_session.clone()));
                    event.insert("turnId".into(), JsValue::String(event_turn.clone()));
                    event_sink.emit(event)
                }),
            ),
        ));
    }
    if let Some(dialogs) = USER_DIALOGS.with(|slot| slot.borrow().clone()) {
        let sink = events.clone();
        let owner = session_id.clone();
        let event_turn = turn_id.clone();
        let mut options = crate::user_dialog::ChannelOptions::new(
            session_id.clone(),
            dialogs.bus.clone(),
            Arc::new(move |event| {
                use crate::host::modules::event::EventSink;
                let mut fields = event.as_object().cloned().ok_or_else(|| {
                    crate::user_dialog::DialogError::Invalid("Invalid dialog event".into())
                })?;
                fields.insert("sessionId".into(), JsValue::String(owner.clone()));
                fields.insert("turnId".into(), JsValue::String(event_turn.clone()));
                sink.emit(fields)
                    .map_err(|e| crate::user_dialog::DialogError::Invalid(e.message))
            }),
        );
        if let Some(persistence) = &dialogs.persistence {
            let authority = crate::metadata::object([
                ("sessionKey", JsValue::String(session_id.clone())),
                ("projectKey", JsValue::String(cwd.clone())),
            ]);
            if let Some(resolved) = (persistence.resolve_session)(&authority)
                .map_err(|e| std::io::Error::other(String::from_utf16_lossy(e.message.units())))?
            {
                options.journal = resolved
                    .journal
                    .map(|journal| journal as Arc<dyn crate::user_dialog::UserDialogJournal>);
                options.store_key = Some(resolved.key.clone());
                if let Some(crate::user_dialog::DialogStore::Native(store)) = &persistence.store {
                    options.store = Some(store.clone());
                    let key = resolved.key.clone();
                    let answer_store = store.clone();
                    options.take_stored_answer = Some(Arc::new(move |request| {
                        answer_store.take_live_answer(&key, request)
                    }));
                    let key = resolved.key.clone();
                    let claim_store = store.clone();
                    options.claim_stored_owner = Some(Arc::new(move |request, owner| {
                        Ok(matches!(
                            claim_store.claim_live_owner(&key, request, owner, 30_000.0)?,
                            crate::user_dialog::OwnerClaim::Owned { .. }
                        ))
                    }));
                    let key = resolved.key.clone();
                    let renew_store = store.clone();
                    options.renew_stored_owner = Some(Arc::new(move |request, owner| {
                        renew_store.renew_live_owner(&key, request, owner, 30_000.0)
                    }));
                    let key = resolved.key;
                    let release_store = store.clone();
                    options.release_stored_owner = Some(Arc::new(move |request, owner| {
                        release_store.release_live_owner(&key, request, owner)
                    }));
                }
            }
        }
        ports.user_dialog = Some(Arc::new(crate::user_dialog::GatewayUserDialogChannel::new(
            options,
        )));
    }
    let permission = Arc::new(ProductionPermission {
        mode: Arc::new(Mutex::new((
            binding
                .config
                .get("permissionMode")
                .and_then(JsValue::as_string)
                .cloned()
                .map(crate::config::permission::PermissionMode::from)
                .unwrap_or_else(|| crate::config::permission::PermissionMode::from("default")),
            None,
        ))),
        mcp_owner: MCP_OWNER.with(|slot| slot.borrow().clone()),
        interactions: interactions.clone(),
        events: events.clone(),
        session: session_id.clone(),
        turn: turn_id.clone(),
        cancel: cancel.clone(),
        timeout: interactions
            .as_ref()
            .and_then(|interactions| interactions.permission_timeout()),
    });
    let registry = build_registry(input, services.clone())?;
    let lifecycle_runtime = LIFECYCLE
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(|| {
            let missing = JsValue::Undefined;
            let hooks = snapshot
                .get("config")
                .and_then(|v| v.get("hooks"))
                .unwrap_or(&missing);
            crate::runtime::hooks::LifecycleRuntime::new(crate::runtime::hooks::HookRuntime::new(
                crate::hooks::config::parse_hooks_config(hooks).settings,
            ))
        });
    ports.file_update_notifier = FILE_UPDATE_HOOK
        .with(|slot| slot.borrow().clone())
        .map(|notifier| Arc::new(notifier) as crate::host::modules::capability::RuntimeContext);
    ports.subagent = Some(Arc::new(super::subagent::NativeSubagentPort::new(
        snapshot,
        input.clone(),
        registry.clone(),
        events.clone(),
        live.clone(),
        services.clone(),
        interactions.clone(),
        stats,
        lifecycle_runtime.clone(),
    )));
    let builder = ToolContextBuilder::new(binding.clone(), ports.clone(), true);
    let tools = ToolRuntime::new(
        registry.clone(),
        permission.clone(),
        SchedulerMode::Concurrent,
    );
    let tools: Arc<dyn capability::ToolExecution> = Arc::new(
        if std::env::var("PDRS_TOOL_TIMING_METADATA").as_deref() == Ok("1") {
            tools.with_timing_metadata()
        } else {
            tools
        },
    );
    let sop_tools = Arc::new(
        crate::runtime::sop::control_tool_port::SopKnowledgeToolExecution::new(
            tools.clone(),
            sop_step_knowledge,
            sop_prepared,
            crate::runtime::sop::control_tool_port::SopKnowledgeExecuteConfig {
                sessions_root: sop_sessions_root,
                authority: sop_authority,
            },
        ),
    );
    let execution_tools: Arc<dyn capability::ToolExecution> = match &live {
        Some(state) => Arc::new(RecordedTools {
            inner: sop_tools,
            state: state.clone(),
            session: session_id.clone(),
            turn: turn_id.clone(),
        }),
        None => sop_tools,
    };
    let context_config = binding.config.clone();
    let budget_manager =
        Arc::new(crate::runtime::model::token_accounting::TokenBudgetManager::default());
    let compaction = crate::runtime::context::compaction::engine::port::CompactionPort {
        budget: Some(budget_manager.clone()),
        policy: Some(Arc::new(
            crate::runtime::context::compaction::policy::AutoCompactionPolicy::new(budget_manager),
        )),
        ..Default::default()
    };
    let mut context = context::Handler::new(
        Arc::new(crate::runtime::context::DefaultContextRuntime::new(
            crate::runtime::context::Options {
                recorder: live.as_ref().map(|state| state.recorder.clone()),
                runtime_context_surface: match context_config
                    .get("runtimeContextSurface")
                    .and_then(JsValue::as_string)
                {
                    Some(value) if value.equals_str("user_message") => {
                        crate::runtime::context::RuntimeContextSurface::UserMessage
                    }
                    _ => crate::runtime::context::RuntimeContextSurface::SystemPrompt,
                },
                instruction_discovery: (context_config.get("omitProjectInstructions")
                    != Some(&JsValue::Bool(true)))
                .then(|| {
                    crate::runtime::context::InstructionDiscovery::new(
                        cwd.to_utf8().unwrap_or_default(),
                        cwd.to_utf8().unwrap_or_default(),
                        std::env::var_os("PILOT_HOME")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_default(),
                        None,
                    )
                }),
                compaction: Some(Arc::new(compaction)),
                extension: current_context_resources().map(|resources| resources.extension()),
                memory_resolver: current_context_resources()
                    .and_then(|resources| resources.memory_resolver.clone()),
                project_root: current_context_resources()
                    .map(|resources| resources.project_root.clone()),
                memory_retrieval_timeout_ms: current_context_resources()
                    .map(|resources| resources.memory_retrieval_timeout_ms)
                    .unwrap_or(30_000.0),
                max_context_tokens: context_config
                    .get("maxContextTokens")
                    .and_then(JsValue::as_number)
                    .unwrap_or(8192.0),
                ..Default::default()
            },
        )),
        ContextRequestIdentity::new(binding.clone(), ports.clone()),
        context_config,
        Some(cancel.clone()),
    );
    let event_sink: Arc<dyn event::EventSink> = if let Some(state) = live {
        context = context.with_recorder(state.recorder.clone());
        *events
            .live
            .lock()
            .map_err(|e| std::io::Error::other(e.to_string()))? = Some(state);
        events
    } else {
        events
    };
    Ok(ModuleRouter {
        model: model::Handler::with_ports(
            execution,
            Some(metadata),
            model::TurnContext {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                ..Default::default()
            },
        ),
        budget: budget::Handler {
            ports: Some(budget::Ports {
                evaluate_request_budget: Some(token_accounting),
                ..budget::Ports::local(None)
            }),
            signal: Some(cancel.clone()),
        },
        capability: capability::Handler::new(
            execution_tools,
            Arc::new(ModeCapabilityContext {
                inner: CapabilityContext {
                    builder: builder.clone(),
                    plan_todo: services.as_ref().map(|value| {
                        value.plan_todo.clone()
                            as Arc<dyn crate::host::modules::tool_context::SessionService>
                    }),
                },
                permission: permission.clone(),
            }),
            permission.clone(),
        ),
        permission: permission::Handler::new(
            tools,
            permission,
            Arc::new(PermissionRequestContext::new(
                binding.clone(),
                ports.clone(),
            )),
        ),
        context,
        turn: turn::Handler::new(
            turn::Ports::for_turn(steers),
            SteerAuthorizations::new(checkpoint),
        ),
        lifecycle: lifecycle::Handler::new(
            lifecycle_runtime,
            lifecycle::Binding {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                cwd,
                permission_mode: input
                    .get("agent")
                    .and_then(|a| a.get("permissionMode"))
                    .and_then(JsValue::as_string)
                    .cloned(),
                base_env: None,
                signal: Some(cancel),
            },
        ),
        event: event::Handler::new(event::TurnIdentity::new(session_id, turn_id), event_sink),
    })
}

#[cfg(test)]
#[path = "module_host_router_policy_tests.rs"]
mod router_policy_tests;

#[cfg(test)]
// Programmatic-router coverage builds its fixtures through `auto_route_tests`
// and the channel composition root; it re-enables with that track.
#[cfg(all(test, any()))]
#[path = "module_host_router_contribution_tests.rs"]
mod router_contribution_tests;

/// Operations installed by build_with_steers; optional domains use their own
/// capability views so the execute request never advertises a missing port.
pub fn host_manifest(router: &ModuleRouter) -> JsValue {
    use pilotdeck_protocol::module::{
        HOST_EVENT_METHODS, HOST_LIFECYCLE_METHODS, HOST_MODEL_METHODS, HOST_PERMISSION_METHODS,
    };
    let mut modules = JsObject::new();
    let mut insert = |name: &str, methods: Vec<&str>| {
        let mut module = JsObject::new();
        module.insert(
            "methods".into(),
            JsValue::Array(methods.into_iter().map(JsValue::from).collect()),
        );
        modules.insert(name.into(), JsValue::Object(module));
    };
    insert("model", HOST_MODEL_METHODS.to_vec());
    if let Some(methods) = router.budget.methods() {
        insert("budget", methods);
    }
    insert("capability", vec!["list_tools", "execute", "execute_batch"]);
    insert("permission", HOST_PERMISSION_METHODS.to_vec());
    insert("context", router.context.methods());
    if let Some(methods) = router.turn.methods() {
        insert("turn", methods);
    }
    insert("lifecycle", HOST_LIFECYCLE_METHODS.to_vec());
    insert("event", HOST_EVENT_METHODS.to_vec());
    JsValue::Object(modules)
}

/// `RouterRuntime.ts:1158-1173`: raw auditing requires both a configured sink
/// and the Gateway-resolved workspace identity. Never use a caller path as ID.
fn provider_audit(input: &JsValue) -> Option<crate::runtime::model::invocation::ModelInvocation> {
    use crate::runtime::model::invocation::{JsonlInvocationLogSink, ModelInvocation};
    let root = std::env::var("PILOTDECK_STORAGE_ROOT").ok()?;
    let root = crate::metadata::js_trim(&root);
    if root.is_empty() {
        return None;
    }
    let workspace = input.get("workspaceId")?.as_string()?.clone();
    if workspace.is_empty() {
        return None;
    }
    let mut context = JsObject::new();
    context.insert("workspaceId".into(), JsValue::String(workspace));
    for key in ["sessionId", "turnId"] {
        context.insert(
            key.into(),
            input.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    context.insert(
        "runId".into(),
        input
            .get("runId")
            .or_else(|| input.get("turnId"))
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    context.insert(
        "caller".into(),
        if input.get("agent").and_then(|agent| agent.get("isSubagent"))
            == Some(&JsValue::Bool(true))
        {
            "subagent".into()
        } else {
            "agent".into()
        },
    );
    if let Ok(version) = std::env::var("PILOTDECK_STORAGE_CONFIG_VERSION") {
        let version = crate::metadata::js_trim(&version);
        if !version.is_empty() {
            context.insert("storageConfigVersion".into(), version.into());
        }
    }
    Some(ModelInvocation {
        context,
        sink: Arc::new(JsonlInvocationLogSink::new(root)),
    })
}

/// Capability lookups the router uses when materializing provider requests.
struct SnapshotInvocation {
    snapshot: JsValue,
    transport: Arc<dyn ModelHttpTransport>,
    model_config: JsValue,
    invocation: Option<crate::runtime::model::invocation::ModelInvocation>,
}
impl SnapshotInvocation {
    fn model(&self, provider: &JsString, model: &JsString) -> Option<JsValue> {
        self.snapshot
            .get("config")?
            .get("model")?
            .get("providers")?
            .as_object()?
            .get_key(provider)?
            .get("models")?
            .as_object()?
            .get_key(model)
            .cloned()
    }
}
impl RouterModelInvocation for SnapshotInvocation {
    fn supports_complete(&self) -> bool {
        true
    }

    fn complete(&self, request: JsValue, abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        let transport = self.transport.clone();
        let config = self.model_config.clone();
        let mut invocation = self.invocation.clone();
        if let Some(invocation) = &mut invocation {
            invocation
                .context
                .insert("caller".into(), "router_judge".into());
            invocation.context.insert(
                "logicalCallId".into(),
                uuid::Uuid::new_v4().to_string().as_str().into(),
            );
        }
        Box::pin(async move {
            let options = StreamOptions {
                abort: abort.clone(),
                invocation,
                ..Default::default()
            };
            let execute = http::complete_model(transport.as_ref(), &request, &config, &options);
            if let Some(abort) = &abort {
                tokio::select! {
                    biased;
                    _ = abort.cancelled() => return Err(RouterInvokeError::other("This operation was aborted")),
                    result = execute => result.map_err(judge_error),
                }
            } else {
                execute.await.map_err(judge_error)
            }
        })
    }

    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        Ok(self
            .model(provider, model)
            .and_then(|value| value.get("capabilities").cloned())
            .unwrap_or_else(|| JsValue::Object(JsObject::new())))
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        Ok(self
            .model(provider, model)
            .and_then(|value| value.get("multimodal").cloned())
            .unwrap_or_else(|| JsValue::Object(JsObject::new())))
    }
    fn get_provider_protocol(&self, provider: &JsString) -> JsString {
        self.snapshot
            .get("config")
            .and_then(|config| config.get("model"))
            .and_then(|model| model.get("providers"))
            .and_then(|providers| providers.as_object()?.get_key(provider))
            .and_then(|provider| provider.get("protocol"))
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "openai".into())
    }
}

fn judge_provider_error(error: &JsValue) -> RouterInvokeError {
    RouterInvokeError::provider(
        error
            .get("code")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "provider_error".into()),
        error
            .get("message")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "Judge model request failed.".into()),
        error
            .get("retryable")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
    )
}

fn judge_error(error: ModelStreamError) -> RouterInvokeError {
    match error {
        ModelStreamError::Request { code, message, .. } => {
            RouterInvokeError::request(code, message)
        }
        ModelStreamError::Provider(error) => judge_provider_error(&JsValue::Object(error)),
        ModelStreamError::Thrown { message, .. } => RouterInvokeError::other(message),
    }
}

#[cfg(test)]
#[path = "auto_route_tests.rs"]
mod auto_route_tests;

#[cfg(test)]
#[path = "module_host_stream_tests.rs"]
mod module_host_stream_tests;

#[cfg(test)]
#[path = "routes/live/tests.rs"]
mod live_bridge_tests;

struct PhaseTool {
    fields: JsObject,
    tools: Arc<dyn crate::runtime::turn::services::PhaseTools>,
}

/// Phase tool arguments cross the JS boundary as JSON, like the channel-side
/// adapter does; an undefined value is rejected instead of becoming `null`.
fn phase_input(value: &JsValue) -> Result<serde_json::Value, String> {
    let text = pilotdeck_protocol::stringify_json(value)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Undefined native value.".to_owned())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Host-owned plan files. Plan mode may inspect only markdown files below the
/// current workspace's `.pilotdeck/plans` directory; this keeps the path
/// authority out of model-provided context values.
struct NativePlanFiles {
    root: PathBuf,
}

impl NativePlanFiles {
    fn new(cwd: JsString) -> Self {
        Self {
            root: PathBuf::from(cwd.to_utf8().unwrap_or_default())
                .join(".pilotdeck")
                .join("plans"),
        }
    }

    fn path(&self, file_path: &JsString) -> Result<PathBuf, HostError> {
        let raw = PathBuf::from(
            file_path
                .to_utf8()
                .map_err(|_| ModuleError::new("Plan file path must be valid UTF-16 text."))?,
        );
        let candidate = if raw.is_absolute() {
            raw
        } else {
            self.root.join(raw)
        };
        let candidate = candidate
            .components()
            .fold(PathBuf::new(), |mut path, part| {
                use std::path::Component;
                match part {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        path.pop();
                    }
                    other => path.push(other.as_os_str()),
                }
                path
            });
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let checked = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if !checked.starts_with(&root) {
            return Err(ModuleError::new(
                "Plan file path must stay under the current project's .pilotdeck/plans directory.",
            )
            .into());
        }
        Ok(candidate)
    }
}

impl crate::host::modules::tool_context::PlanFiles for NativePlanFiles {
    fn directory(&self) -> Option<JsString> {
        Some(self.root.to_string_lossy().to_string().into())
    }

    fn resolve(&self, file_path: JsString, _cwd: JsString) -> HostFuture<JsValue> {
        let this = self.root.clone();
        Box::pin(async move {
            let files = Self { root: this };
            let path = files.path(&file_path)?;
            Ok(JsValue::String(path.to_string_lossy().to_string().into()))
        })
    }

    fn read(&self, file_path: JsString, _cwd: JsString) -> HostFuture<JsValue> {
        let this = self.root.clone();
        Box::pin(async move {
            let files = Self { root: this };
            let path = files.path(&file_path)?;
            match std::fs::read_to_string(path) {
                Ok(value) => Ok(JsValue::String(value.into())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(JsValue::Undefined)
                }
                Err(error) => Err(HostError::new(
                    crate::host::HostErrorKind::Io,
                    error.to_string(),
                )),
            }
        })
    }
}

impl capability::ToolDefinition for PhaseTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        self.fields
            .get("readOnly")
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| Ok(JsValue::Bool(false)))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(self
            .fields
            .get("concurrencySafe")
            .cloned()
            .unwrap_or(JsValue::Bool(false)))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        self.fields
            .get("requiredRuntimeCapabilities")
            .and_then(|v| match v {
                JsValue::Array(items) => Some(
                    items
                        .iter()
                        .filter_map(JsValue::as_string)
                        .cloned()
                        .collect(),
                ),
                _ => None,
            })
    }
}
impl crate::runtime::tools::ExecutableTool for PhaseTool {
    fn execute(
        &self,
        input: JsValue,
        _context: crate::runtime::tools::ToolRunContext,
    ) -> crate::runtime::tools::ToolFuture<crate::runtime::tools::ToolOutput> {
        let name = self
            .fields
            .get("name")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_default();
        let tools = self.tools.clone();
        Box::pin(async move {
            use crate::runtime::tools::{ToolError, ToolOutput};
            let input =
                phase_input(&input).map_err(|message| ToolError::new("invalid_params", message))?;
            let value = tools.call(&name, &input).await.map_err(|e| {
                let code = e
                    .module_error
                    .as_ref()
                    .and_then(|m| m.code.clone())
                    .map(|code| String::from_utf16_lossy(code.units()).to_string())
                    .unwrap_or_else(|| "tool_execution_failed".to_owned());
                ToolError::new(code, e.message.as_str())
            })?;
            let value = pilotdeck_protocol::parse_json(&value.to_string())
                .map_err(|e| ToolError::new("serialization_failed", e.to_string()))?;
            Ok(ToolOutput {
                content: match value.get("content") {
                    Some(JsValue::Array(v)) => v.clone(),
                    _ => vec![],
                },
                data: value.get("data").cloned(),
                metadata: value.get("metadata").and_then(JsValue::as_object).cloned(),
                supplemental_messages: None,
            })
        })
    }
}

fn tool_selector(input: &JsValue, key: &str) -> Option<(Vec<String>, Vec<String>)> {
    let policy = input.get(key)?.as_object()?;
    let values = |field: &str| match policy.get(field) {
        Some(JsValue::Array(values)) => values
            .iter()
            .filter_map(JsValue::as_string)
            .filter_map(|value| value.to_utf8().ok())
            .collect(),
        _ => Vec::new(),
    };
    Some((values("allow"), values("deny")))
}

fn tool_pattern_matches(pattern: &str, name: &str) -> bool {
    pattern == "*"
        || pattern == name
        || pattern
            .strip_suffix('*')
            .is_some_and(|prefix| name.starts_with(prefix))
}

fn tool_selector_allows(policy: &Option<(Vec<String>, Vec<String>)>, name: &str) -> bool {
    let Some((allowed, denied)) = policy else {
        return true;
    };
    !denied
        .iter()
        .any(|pattern| tool_pattern_matches(pattern, name))
        && (allowed.is_empty()
            || allowed
                .iter()
                .any(|pattern| tool_pattern_matches(pattern, name)))
}

pub fn build_registry(
    input: &JsValue,
    services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
) -> std::io::Result<ToolRegistry> {
    let allowed = input.get("allowedTools").and_then(|value| match value {
        JsValue::Array(items) => Some(
            items
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });
    let denied = input
        .get("disallowedTools")
        .and_then(|value| match value {
            JsValue::Array(items) => Some(
                items
                    .iter()
                    .filter_map(JsValue::as_string)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let excluded = match input.get("excludedTools") {
        Some(JsValue::Array(v)) => v
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect::<Vec<_>>(),
        _ => vec![],
    };
    let organization_policy = tool_selector(input, "organizationToolPolicy");
    let managed_policy = tool_selector(input, "managedToolPolicy");
    let policy: crate::runtime::tools::registry::ToolPolicy = Arc::new(move |tool| {
        tool.fields()
            .get("name")
            .and_then(JsValue::as_string)
            .is_some_and(|name| {
                let utf8 = name.to_utf8().unwrap_or_default();
                allowed.as_ref().is_none_or(|names| names.contains(name))
                    && !denied.contains(name)
                    && !excluded.contains(name)
                    && tool_selector_allows(&organization_policy, &utf8)
                    && tool_selector_allows(&managed_policy, &utf8)
            })
    });
    if let Some(registry) = TOOL_REGISTRY.with(|slot| slot.borrow().clone()) {
        let registry = registry
            .scoped(policy)
            .map_err(|error| std::io::Error::other(error.message));
        let registry = registry?;
        install_live_continuation_tools(&registry)?;
        return Ok(registry);
    }
    let registry = ToolRegistry::new(None, Some(policy));
    crate::runtime::tools::builtin::register_read_only_tools(&registry)
        .map_err(|error| std::io::Error::other(error.message))?;
    if let Some(resources) = current_context_resources() {
        resources
            .install_read_skill(&registry)
            .map_err(|error| std::io::Error::other(error.message))?;
    }
    registry
        .register(Arc::new(
            crate::runtime::tools::builtin::agent::AgentTool::for_depth(
                input
                    .get("agent")
                    .and_then(|a| a.get("maxSubagentDepth"))
                    .and_then(JsValue::as_number)
                    .unwrap_or(1.0),
                input
                    .get("agent")
                    .and_then(|a| a.get("subagentDepth"))
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0),
            ),
        ))
        .map_err(|error| std::io::Error::other(error.message))?;
    crate::runtime::tools::builtin::register_write_tools(&registry)
        .map_err(|error| std::io::Error::other(error.message))?;
    for tool in [
        Arc::new(crate::runtime::tools::builtin::structured_output::StructuredOutputTool::new())
            as Arc<dyn crate::runtime::tools::ExecutableTool>,
        // B dc123d18 advertises the question tool from `read_only_tools`; daqi keeps
        // that family read-only for the isolated native entry, so the session
        // catalog registers it here instead. The tool itself resolves the host
        // elicitation channel at execute time.
        Arc::new(crate::runtime::tools::builtin::ask_user_question::AskUserQuestionTool::new()),
        Arc::new(crate::runtime::tools::builtin::send_attachment::SendAttachmentTool::new()),
        Arc::new(crate::runtime::tools::builtin::execute_code::ExecuteCodeTool::new()),
        Arc::new(crate::runtime::tools::builtin::plan_mode::PlanModeTool::enter()),
        Arc::new(crate::runtime::tools::builtin::plan_mode::PlanModeTool::exit()),
    ] {
        registry
            .register(tool)
            .map_err(|error| std::io::Error::other(error.message))?;
    }
    if services.is_some() {
        for tool in [
            Arc::new(crate::runtime::tools::builtin::goal::GoalTool::get())
                as Arc<dyn crate::runtime::tools::ExecutableTool>,
            Arc::new(crate::runtime::tools::builtin::goal::GoalTool::create()),
            Arc::new(crate::runtime::tools::builtin::goal::GoalTool::update()),
            Arc::new(crate::runtime::tools::builtin::todo_write::TodoWriteTool::new()),
        ] {
            registry
                .register(tool)
                .map_err(|error| std::io::Error::other(error.message))?;
        }
    }
    #[cfg(unix)]
    crate::runtime::tools::builtin::register_shell_tools(&registry)
        .map_err(|error| std::io::Error::other(error.message))?;
    if let Some(services) = services {
        if let Some(cron) = services.cron.as_ref() {
            cron.register_tools(&registry)?;
        }
        #[cfg(unix)]
        for tool in crate::runtime::tools::builtin::task::task_tools(Some(
            services.background.tasks.clone(),
        )) {
            registry
                .register(tool)
                .map_err(|e| std::io::Error::other(e.message))?;
        }
        if let Some(phase) = &services.tools {
            for schema in phase.schemas() {
                let fields = pilotdeck_protocol::parse_json(&schema.to_string())
                    .map_err(|e| std::io::Error::other(e.to_string()))?
                    .as_object()
                    .cloned()
                    .ok_or_else(|| std::io::Error::other("Invalid phase tool descriptor"))?;
                registry
                    .register(Arc::new(PhaseTool {
                        fields,
                        tools: phase.clone(),
                    }))
                    .map_err(|e| std::io::Error::other(e.message))?;
            }
        }
    }
    if let Some(owner) = MCP_OWNER.with(|slot| slot.borrow().clone()) {
        owner
            .install(&registry)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    if let Some(JsValue::Array(kinds)) = input.get("hostUserDialogKinds") {
        let kinds = kinds
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect::<Vec<_>>();
        crate::runtime::tools::builtin::register_user_dialog_tools(&registry, &kinds)
            .map_err(|e| std::io::Error::other(e.message))?;
    }
    crate::runtime::composition::register_knowledge_query_tool(
        &registry,
        input.get("configModules").unwrap_or(&JsValue::Null),
    )
    .map_err(|error| std::io::Error::other(error.message))?;
    install_live_continuation_tools(&registry)?;
    Ok(registry)
}
pub fn descriptors(registry: &ToolRegistry) -> std::io::Result<Vec<JsValue>> {
    registry
        .list()
        .map_err(|e| std::io::Error::other(e.message))?
        .iter()
        .map(|tool| {
            let fields = tool.fields();
            let string = |key: &str, default: &str| {
                fields
                    .get(key)
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_else(|| default.into())
            };
            let schema = fields
                .get("inputSchema")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            Ok(crate::runtime::turn::execute_payload::tool_descriptor(
                &string("name", ""),
                &string("description", ""),
                &string("kind", "custom"),
                &schema,
                tool.is_read_only(&JsValue::Null)
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                tool.is_concurrency_safe(&JsValue::Null)
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                tool.requires_user_interaction(&JsValue::Null)
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                tool.required_runtime_capabilities().as_deref(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostHandler;
    use crate::host::modules::event::EventSink;
    use crate::runtime::session::subagent_continuation::{
        ContinuationAdmission, ContinuationFuture,
    };
    use crate::runtime::tools::builtin::subagent_continuation::{
        SubagentFollowupPortRequest, SubagentStartPortRequest,
    };
    use pilotdeck_protocol::parse_json;

    fn permission_fixture(mode: &str) -> Arc<ProductionPermission> {
        Arc::new(ProductionPermission {
            mode: Arc::new(Mutex::new((mode.into(), None))),
            mcp_owner: None,
            interactions: None,
            events: Arc::new(EventBuffer::default()),
            session: "s".into(),
            turn: "t".into(),
            cancel: Default::default(),
            timeout: None,
        })
    }

    async fn observe(permission: &ProductionPermission, json: &str) {
        use capability::CapabilityResultObserver;
        let value = parse_json(json).unwrap();
        let JsValue::Array(results) = &value else {
            panic!()
        };
        permission
            .on_capability_results(
                results
                    .iter()
                    .map(|value| value.as_object().unwrap().clone())
                    .collect(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn capability_modes_restore_previous_and_ignore_invalid_results() {
        let permission = permission_fixture("bypassPermissions");
        observe(&permission, r#"[{"type":"success","data":{"requestedMode":"plan"}},{"type":"success","data":{"requestedMode":"plan"}}]"#).await;
        assert_eq!(
            *permission.mode.lock().unwrap(),
            ("plan".into(), Some("bypassPermissions".into()))
        );
        observe(&permission, r#"[{"type":"error","data":{"requestedMode":"default"}},{"type":"success","data":{"requestedMode":"PLAN"}},{"type":"success","data":{"requestedMode":7}},{"type":"success"}]"#).await;
        assert_eq!(
            *permission.mode.lock().unwrap(),
            ("plan".into(), Some("bypassPermissions".into()))
        );
        observe(
            &permission,
            r#"[{"type":"success","data":{"requestedMode":"default"}}]"#,
        )
        .await;
        assert_eq!(
            *permission.mode.lock().unwrap(),
            ("bypassPermissions".into(), None)
        );
        observe(&permission, r#"[{"type":"success","data":{"requestedMode":"default"}},{"type":"success","data":{"requestedMode":"plan"}},{"type":"success","data":{"requestedMode":"bypassPermissions"}}]"#).await;
        assert_eq!(*permission.mode.lock().unwrap(), ("default".into(), None));
        let permission = permission_fixture("plan");
        observe(
            &permission,
            r#"[{"type":"success","data":{"requestedMode":"default"}}]"#,
        )
        .await;
        assert_eq!(*permission.mode.lock().unwrap(), ("default".into(), None));
    }

    struct WriteDefinition(JsObject);
    impl capability::ToolDefinition for WriteDefinition {
        fn fields(&self) -> &JsObject {
            &self.0
        }
    }

    #[tokio::test]
    async fn capability_mode_changes_reach_permission_decisions_and_tool_context() {
        use capability::ToolContext;
        use permission::PermissionDecision;
        let permission = permission_fixture("bypassPermissions");
        let input = parse_json(r#"{"sessionId":"s","turnId":"t","agent":{"cwd":"/tmp","permissionMode":"bypassPermissions","permissionContext":{"bypassAvailable":true}}}"#).unwrap();
        let binding = TurnBinding {
            config: input.get("agent").unwrap().as_object().unwrap().clone(),
            input: input.as_object().unwrap().clone(),
            checkpoint: HostToolCheckpoint::new(None, &[]).unwrap(),
            abort_signal: None,
            environment: Arc::new(Environment),
        };
        let context = ModeCapabilityContext {
            inner: CapabilityContext {
                builder: ToolContextBuilder::new(binding, ToolContextPorts::default(), false),
                plan_todo: None,
            },
            permission: permission.clone(),
        };
        let tool: Arc<dyn capability::ToolDefinition> = Arc::new(WriteDefinition(
            parse_json(r#"{"name":"write_file","kind":"file"}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        ));
        let stale = context.prepare(JsValue::Null).await.unwrap().runtime;
        observe(
            &permission,
            r#"[{"type":"success","data":{"requestedMode":"plan"}}]"#,
        )
        .await;
        let current = context.prepare(JsValue::Null).await.unwrap().runtime;
        let fields = &current
            .downcast_ref::<crate::host::modules::tool_context::ToolRuntimeContext>()
            .unwrap()
            .fields;
        assert_eq!(fields.get("permissionMode"), Some(&"plan".into()));
        assert_eq!(
            fields.get("permissionContext").and_then(|v| v.get("mode")),
            Some(&"plan".into())
        );
        let decision = permission
            .decide(tool.clone(), JsValue::Null, stale.clone(), "call".into())
            .await
            .unwrap();
        assert_eq!(decision.get("type"), Some(&"deny".into()));
        observe(
            &permission,
            r#"[{"type":"success","data":{"requestedMode":"default"}}]"#,
        )
        .await;
        let decision = permission
            .decide(tool, JsValue::Null, stale, "call".into())
            .await
            .unwrap();
        assert_eq!(decision.get("type"), Some(&"allow".into()));
    }

    struct RecordingStore {
        entries: Mutex<Vec<JsValue>>,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        fail: std::sync::atomic::AtomicBool,
        executor: std::thread::ThreadId,
    }
    impl crate::storage::TranscriptStore for RecordingStore {
        fn append(
            &self,
            _: &crate::storage::TranscriptKey,
            entry: &JsValue,
        ) -> crate::storage::StoreResult<()> {
            if entry.get("type") == Some(&"model_stream_event".into()) {
                assert_ne!(std::thread::current().id(), self.executor);
                if let Some(started) = self.started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                if let Some(gate) = self.gate.lock().unwrap().take() {
                    gate.recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
                if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(crate::storage::LedgerError::Invalid(
                        "record fixture failed".into(),
                    ));
                }
            }
            self.entries.lock().unwrap().push(entry.clone());
            Ok(())
        }
        fn read(
            &self,
            _: &crate::storage::TranscriptKey,
            _: Option<u64>,
        ) -> crate::storage::StoreResult<crate::storage::TranscriptRead> {
            Ok(crate::storage::TranscriptRead {
                entries: self.entries.lock().unwrap().clone(),
                diagnostics: vec![],
            })
        }
    }
    struct EventStream(std::collections::VecDeque<JsValue>);
    impl model::ModelStream for EventStream {
        fn next(&mut self) -> model::StreamFuture<'_, Option<JsValue>> {
            Box::pin(async move { Ok(self.0.pop_front()) })
        }
        fn close(&mut self) -> model::StreamFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    fn recording_fixture() -> (RecordedStream, Arc<RecordingStore>) {
        let store = Arc::new(RecordingStore {
            entries: Mutex::default(),
            gate: Mutex::default(),
            started: Mutex::default(),
            fail: false.into(),
            executor: std::thread::current().id(),
        });
        let recorder = Arc::new(crate::storage::recorder::Recorder::new(
            store.clone(),
            crate::storage::TranscriptKey {
                project_root: PathBuf::new(),
                pilot_home: PathBuf::new(),
                session_id: "s".into(),
                transcript_path: PathBuf::new(),
            },
        ));
        recorder.start_turn(&"s".into(), &"t".into()).unwrap();
        recorder
            .record_model_request(&"s".into(), &"t".into(), JsValue::Object(JsObject::new()))
            .unwrap();
        (
            RecordedStream {
                inner: Box::new(EventStream(vec!["first".into(), "second".into()].into())),
                recorder,
                session: "s".into(),
                turn: "t".into(),
                pending: None,
            },
            store,
        )
    }

    #[tokio::test]
    async fn recorded_stream_preserves_order_across_cancelled_pull() {
        use model::ModelStream;
        let (mut stream, store) = recording_fixture();
        let (release, gate) = std::sync::mpsc::channel();
        let (started, waiting) = tokio::sync::oneshot::channel();
        *store.gate.lock().unwrap() = Some(gate);
        *store.started.lock().unwrap() = Some(started);
        let mut pull = stream.next();
        tokio::select! {
            _ = waiting => {},
            result = &mut pull => panic!("event yielded before recording: {result:?}"),
        }
        drop(pull);
        release.send(()).unwrap();
        assert_eq!(stream.next().await.unwrap(), Some("first".into()));
        assert_eq!(stream.next().await.unwrap(), Some("second".into()));
        assert_eq!(stream.next().await.unwrap(), None);
        let entries = store.entries.lock().unwrap();
        let events: Vec<_> = entries
            .iter()
            .filter(|e| e.get("type") == Some(&"model_stream_event".into()))
            .map(|e| e.get("event").unwrap().clone())
            .collect();
        assert_eq!(events, vec!["first".into(), "second".into()]);
    }

    #[tokio::test]
    async fn recorded_stream_returns_record_failure_before_yielding_event() {
        use model::ModelStream;
        let (mut stream, store) = recording_fixture();
        store.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            stream.next().await.unwrap_err().message,
            JsString::from("record fixture failed")
        );
        assert!(stream.pending.is_none());
        assert!(
            !store
                .entries
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.get("type") == Some(&"model_stream_event".into()))
        );
    }

    struct ContinuationPortFixture;

    impl crate::runtime::tools::builtin::subagent_continuation::SubagentContinuationPort
        for ContinuationPortFixture
    {
        fn start(
            &self,
            _request: SubagentStartPortRequest,
        ) -> ContinuationFuture<ContinuationAdmission> {
            Box::pin(async {
                Ok(ContinuationAdmission {
                    child_session_id: "child".into(),
                    item_id: "item".into(),
                    turn_id: "turn".into(),
                })
            })
        }

        fn followup(
            &self,
            _request: SubagentFollowupPortRequest,
        ) -> ContinuationFuture<ContinuationAdmission> {
            Box::pin(async {
                Ok(ContinuationAdmission {
                    child_session_id: "child".into(),
                    item_id: "item".into(),
                    turn_id: "turn".into(),
                })
            })
        }
    }

    #[test]
    fn continuation_registry_exposes_start_alias_and_followup() {
        let port: ContinuationPort = Arc::new(ContinuationPortFixture);
        let registry = ToolRegistry::default();
        install_continuation_tools(&registry, port).unwrap();
        assert!(registry.has(&"subagent".into()));
        assert!(registry.has(&"continuable_subagent".into()));
        assert!(registry.has(&"send_message".into()));
    }

    #[test]
    fn replacement_parent_port_is_bound_to_exact_live_identity() {
        let old_identity: JsString = "old-live-identity".into();
        let replacement_identity: JsString = "replacement-live-identity".into();
        let old_port: ContinuationPort = Arc::new(ContinuationPortFixture);
        let replacement_port: ContinuationPort = Arc::new(ContinuationPortFixture);
        let old = bind_live_continuation_tools(old_identity.clone(), old_port.clone());
        let replacement =
            bind_live_continuation_tools(replacement_identity.clone(), replacement_port.clone());

        drop(old);
        assert!(continuation_port(&old_identity).is_none());
        assert!(Arc::ptr_eq(
            &continuation_port(&replacement_identity).unwrap(),
            &replacement_port,
        ));

        drop(replacement);
        assert!(continuation_port(&replacement_identity).is_none());
    }

    #[tokio::test]
    async fn production_permission_waits_for_the_shared_bus_decision() {
        let interactions =
            Arc::new(crate::runtime::interaction::GatewayInteractionCoordinator::default());
        let events = Arc::new(EventBuffer::default());
        let input = parse_json(r#"{"sessionId":"s","turnId":"t","canPrompt":true,"agent":{"cwd":"/tmp","permissionMode":"default","permissionContext":{"canPrompt":true,"rules":{"ask":[{"source":"session","behavior":"ask","toolName":"bash"}]}}}}"#).unwrap();
        let configured = input
            .get("agent")
            .unwrap()
            .get("permissionContext")
            .unwrap()
            .get("rules")
            .unwrap()
            .get("ask")
            .unwrap();
        let parsed = parse_rules(Some(configured));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].tool_name, JsString::from("bash"));
        let mailbox = crate::runtime::steer::SteerMailbox::new(None);
        mailbox.start("t".into());
        let router = build_with_services_interactions(
            JsValue::Null,
            &input,
            events.clone(),
            crate::runtime::network::Cancellation::default(),
            crate::runtime::steer::TurnSteers::new(mailbox, "t".into(), vec![]),
            None,
            None,
            Some(interactions.clone()),
        )
        .unwrap();
        let mut task = tokio::spawn(router.module_call(parse_json(r#"{"module":"permission","payload":{"operation":"decide","tool":{"name":"bash"},"toolCallId":"call","input":{"command":"printf fixture"}}}"#).unwrap()));
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                for event in events.drain() {
                    if event.get("type") == Some(&"permission_request".into()) {
                        return event;
                    }
                }
                tokio::select! {
                    completed = &mut task => panic!("Permission decision completed before publishing a prompt: {completed:?}"),
                    _ = tokio::task::yield_now() => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        let request_id = request
            .get("requestId")
            .and_then(JsValue::as_string)
            .unwrap()
            .clone();
        assert!(interactions.decide_permission(
            &"s".into(),
            crate::runtime::interaction::PermissionDecision {
                request_id,
                decision: "deny".into(),
                remember: None,
                reason: None
            },
            None
        ));
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            result.get("decision").and_then(|d| d.get("type")),
            Some(&"deny".into())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn production_permission_timeout_denies_and_cleans_pending() {
        let interactions = Arc::new(
            crate::runtime::interaction::GatewayInteractionCoordinator::new(
                crate::runtime::interaction::InteractionOptions {
                    permission_timeout: Some(std::time::Duration::from_millis(17)),
                    ..Default::default()
                },
            ),
        );
        let events = Arc::new(EventBuffer::default());
        let input = parse_json(r#"{"sessionId":"s","turnId":"t","canPrompt":true,"agent":{"cwd":"/tmp","permissionMode":"default","permissionContext":{"canPrompt":true,"rules":{"ask":[{"source":"session","behavior":"ask","toolName":"bash"}]}}}}"#).unwrap();
        let mailbox = crate::runtime::steer::SteerMailbox::new(None);
        mailbox.start("t".into());
        let router = build_with_services_interactions(
            JsValue::Null,
            &input,
            events,
            crate::runtime::network::Cancellation::default(),
            crate::runtime::steer::TurnSteers::new(mailbox, "t".into(), vec![]),
            None,
            None,
            Some(interactions.clone()),
        )
        .unwrap();
        let task = tokio::spawn(router.module_call(parse_json(r#"{"module":"permission","payload":{"operation":"decide","tool":{"name":"bash"},"toolCallId":"call","input":{"command":"printf fixture"}}}"#).unwrap()));
        while interactions.permission_bus().pending_count(&"s".into()) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(std::time::Duration::from_millis(17)).await;
        let result = task.await.unwrap().unwrap();
        let decision = result.get("decision").unwrap();
        assert_eq!(decision.get("type"), Some(&"deny".into()));
        assert_eq!(
            decision.get("message"),
            Some(&"Permission prompt timed out.".into())
        );
        assert_eq!(decision.get("outcome"), Some(&"timeout".into()));
        assert_eq!(interactions.permission_bus().pending_count(&"s".into()), 0);
    }

    #[test]
    fn external_knowledge_binding_registers_knowledge_query() {
        let input = parse_json(
            r#"{"configModules":{"knowledge":{"enabled":true,"implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","endpoint":"http://127.0.0.1:9","manifestPath":"/api/v1/knowledge-module/module-manifest","callPath":"/api/v1/agents/a/knowledge-module/v2/module/call","methods":["query"],"credentialEnv":"STAFFDECK_SOP_MANAGEMENT_API_KEY"}}}"#,
        )
        .unwrap();
        let registry = build_registry(&input, None).unwrap();
        assert!(registry.has(&"knowledge_query".into()));
    }

    #[test]
    fn explicit_tool_restrictions_preserve_empty_allow_and_deny_precedence() {
        for (input, expected) in [
            (
                r#"{"allowedTools":["get_current_time"]}"#,
                vec!["get_current_time"],
            ),
            (r#"{"allowedTools":[]}"#, vec![]),
            (
                r#"{"allowedTools":["get_current_time"],"disallowedTools":["get_current_time"]}"#,
                vec![],
            ),
            (
                r#"{"allowedTools":["get_current_time"],"excludedTools":["get_current_time"]}"#,
                vec![],
            ),
        ] {
            let registry = build_registry(&parse_json(input).unwrap(), None).unwrap();
            let names = descriptors(&registry)
                .unwrap()
                .iter()
                .map(|tool| {
                    tool.get("name")
                        .and_then(JsValue::as_string)
                        .unwrap()
                        .to_utf8()
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(names, expected, "{input}");
        }
    }

    #[test]
    fn organization_and_managed_tool_selectors_intersect_and_support_prefixes() {
        let registry = build_registry(
            &parse_json(
                r#"{
                    "organizationToolPolicy":{"allow":["g*","agent"],"deny":["grep"]},
                    "managedToolPolicy":{"allow":["glob","grep","agent"],"deny":["agent"]}
                }"#,
            )
            .unwrap(),
            None,
        )
        .unwrap();
        let names = descriptors(&registry)
            .unwrap()
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["glob"]);
    }

    #[test]
    fn injected_registry_keeps_host_policy_for_late_tool_discovery() {
        let parent = build_registry(&JsValue::Null, None).unwrap();
        let glob = parent.get(&"glob".into()).unwrap();
        let scoped = with_tool_registry(parent.clone(), || {
            build_registry(
                &parse_json(
                    r#"{"organizationToolPolicy":{"allow":["g*"]},"managedToolPolicy":{"deny":["grep"]}}"#,
                )
                .unwrap(),
                None,
            )
            .unwrap()
        });
        assert!(scoped.has(&"glob".into()));
        assert!(!scoped.has(&"grep".into()));
        assert!(!scoped.has(&"read_file".into()));

        assert!(parent.unregister(&"glob".into()).unwrap());
        assert!(!scoped.has(&"glob".into()));
        parent.register(glob).unwrap();
        assert!(scoped.has(&"glob".into()));
    }

    #[test]
    fn sdk_input_registration_requires_explicit_host_kind() {
        let plain = build_registry(&JsValue::Null, None).unwrap();
        assert!(
            !descriptors(&plain)
                .unwrap()
                .iter()
                .any(|t| t.get("name") == Some(&"request_user_input".into()))
        );
        let opted = build_registry(
            &parse_json(r#"{"hostUserDialogKinds":["input"]}"#).unwrap(),
            None,
        )
        .unwrap();
        assert!(
            descriptors(&opted)
                .unwrap()
                .iter()
                .any(|t| t.get("name") == Some(&"request_user_input".into()))
        );
    }

    #[tokio::test]
    async fn production_question_uses_the_injected_shared_coordinator() {
        let interactions =
            Arc::new(crate::runtime::interaction::GatewayInteractionCoordinator::default());
        let events = Arc::new(EventBuffer::default());
        let input = parse_json(r#"{"sessionId":"s","turnId":"t","canPrompt":true,"canElicit":true,"agent":{"cwd":"/tmp","permissionMode":"bypassPermissions"}}"#).unwrap();
        let mailbox = crate::runtime::steer::SteerMailbox::new(None);
        mailbox.start("t".into());
        let router = build_with_services_interactions(
            JsValue::Null,
            &input,
            events.clone(),
            crate::runtime::network::Cancellation::default(),
            crate::runtime::steer::TurnSteers::new(mailbox, "t".into(), vec![]),
            None,
            None,
            Some(interactions.clone()),
        )
        .unwrap();
        let call = router.module_call(parse_json(r#"{"module":"capability","payload":{"operation":"execute","toolCallId":"q","name":"ask_user_question","arguments":{"questions":[{"question":"Pick one","header":"Choice","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}]}]}}}"#).unwrap());
        let task = tokio::spawn(call);
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                for event in events.drain() {
                    if event.get("type") == Some(&"elicitation_request".into()) {
                        return event;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let projected = crate::runtime::turn::projector::map_agent_event(
            &request,
            "t",
            crate::runtime::turn::projector::GatewayAgentEventProjectorOptions::default(),
        )
        .unwrap();
        assert_eq!(
            projected.len(),
            1,
            "pending question must reach the gateway stream"
        );
        assert_eq!(
            projected[0].get("type"),
            Some(&"elicitation_request".into())
        );
        assert_eq!(projected[0].get("requestId"), request.get("requestId"));
        assert_eq!(projected[0].get("runId"), Some(&"t".into()));
        assert!(
            !task.is_finished(),
            "question must await the shared bus answer"
        );
        let request_id = request
            .get("requestId")
            .and_then(JsValue::as_string)
            .unwrap();
        assert!(interactions.respond_elicitation(
            &"s".into(),
            request_id,
            parse_json(r#"{"type":"answered","answers":{"Pick one":"A"}}"#).unwrap(),
            None
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn production_context_honors_runtime_surface_without_rewriting_prompt() {
        for (surface, user_message) in [("user_message", true), ("system_prompt", false)] {
            let input = parse_json(&format!(r#"{{"sessionId":"surface-session","turnId":"surface-turn","agent":{{"cwd":"/tmp","runtimeContextSurface":"{surface}"}}}}"#)).unwrap();
            let router = build(
                JsValue::Null,
                &input,
                Arc::new(EventBuffer::default()),
                crate::runtime::network::Cancellation::default(),
            )
            .unwrap();
            let response = router.module_call(parse_json(r#"{"module":"context","payload":{"operation":"prepare_for_model","input":{"provider":"proof","model":"base","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"tools":[],"customSystemPrompt":"exact SDK prompt"}}}"#).unwrap()).await.unwrap();
            let result = response.get("result").unwrap();
            let system = result
                .get("systemPrompt")
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            assert!(system.contains("exact SDK prompt"));
            assert_eq!(system.contains("<user-context>"), !user_message);
            let messages = result
                .get("messages")
                .and_then(|value| match value {
                    JsValue::Array(items) => Some(items),
                    _ => None,
                })
                .unwrap();
            let encoded = pilotdeck_protocol::stringify_json(&JsValue::Array(messages.clone()))
                .unwrap()
                .unwrap();
            assert_eq!(encoded.contains("<runtime-context"), user_message);
            assert_eq!(encoded.contains("<user-context>"), user_message);
        }
    }

    #[tokio::test]
    async fn production_context_budget_uses_configured_limits_and_real_estimator() {
        let input = parse_json(
            r#"{"sessionId":"s","turnId":"t","agent":{"cwd":"/tmp","maxContextTokens":1234}}"#,
        )
        .unwrap();
        let router = build(
            JsValue::Null,
            &input,
            Arc::new(EventBuffer::default()),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap();
        let response = router.module_call(parse_json(r#"{"module":"context","payload":{"operation":"try_auto_compact","input":{"reservedOutputTokens":34,"messages":[{"role":"user","content":[{"type":"text","text":"Count this actual message."}]}]}}}"#).unwrap()).await.unwrap();
        let snapshot = response
            .get("result")
            .and_then(|v| v.get("snapshot"))
            .unwrap();
        assert!(snapshot.get("tokens").and_then(JsValue::as_number).unwrap() > 0.0);
        assert_eq!(
            snapshot.get("totalContextTokens"),
            Some(&JsValue::Number(1234.0))
        );
        assert_eq!(
            snapshot.get("effectiveContextTokens"),
            Some(&JsValue::Number(1200.0))
        );
        assert_eq!(
            snapshot.get("reservedOutputTokens"),
            Some(&JsValue::Number(34.0))
        );
    }

    #[test]
    fn streaming_events_are_delivered_immediately_and_never_replayed() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let output = delivered.clone();
        let events = EventBuffer::streaming(Arc::new(move |event| {
            output.lock().unwrap().push(event);
            Ok(())
        }));
        events
            .emit(
                parse_json(r#"{"type":"model_request_started"}"#)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(delivered.lock().unwrap().len(), 1);
        assert!(events.drain().is_empty());
        assert!(events.drain().is_empty());
    }

    /// The sidecar's first callback used to fail with "no module host wired".
    /// Exercise the real router so that regression cannot come back silently.
    #[tokio::test]
    async fn capability_list_tools_reaches_the_production_router() {
        let input = parse_json(
            r#"{"sessionId":"s","turnId":"t","agent":{"cwd":"/tmp","permissionMode":"default"}}"#,
        )
        .unwrap();
        let router = build(
            JsValue::Null,
            &input,
            Arc::new(EventBuffer::default()),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap();
        let response = router
            .module_call(
                parse_json(r#"{"module":"capability","payload":{"operation":"list_tools"}}"#)
                    .unwrap(),
            )
            .await
            .expect("capability module must be wired");
        let Some(JsValue::Array(tools)) = response.get("tools") else {
            panic!("tools must be an array");
        };
        assert!(!tools.is_empty(), "builtin tools must be advertised");
        let manifest = host_manifest(&router);
        assert_eq!(
            manifest
                .get("turn")
                .and_then(|module| module.get("methods")),
            Some(&parse_json(r#"["drain_steer","drain_or_close_steer"]"#).unwrap())
        );
        assert_eq!(
            manifest
                .get("model")
                .and_then(|module| module.get("methods")),
            Some(&JsValue::Array(
                pilotdeck_protocol::module::HOST_MODEL_METHODS
                    .iter()
                    .map(|method| JsValue::from(*method))
                    .collect()
            ))
        );
    }
}
