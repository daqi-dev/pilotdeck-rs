//! Production model module port: the host module traits over `RouterRuntime`.
//!
//! This is the adapter the sidecar's model requests land on once composition
//! supplies a router (`AgentSessionRuntimeBundle.ts:184-207` builds the same
//! chain in TS):
//!
//! - `routerModelInvokerAdapter.ts:21-74` — `prepare` routes once (or honors
//!   `context.modelOverride`) and materializes the request; `stream` replays
//!   the prepared decision through `router.execute` without routing again.
//! - `durableModelInvokerPort.ts:4-22` — the model request is recorded before
//!   the stream body starts and every event before it is yielded.
//! - `sidecarHostModulePorts.ts:265-314` — [`compose`] is the per-turn
//!   `bindTurn`: sticky routing state is consumed once per turn, and the
//!   metadata/materializer ports ride along unwrapped.
//! - `ProjectSessionRuntimeBundle.ts:468-497` — metadata lookups read the
//!   provider config's capabilities; an unknown route answers `None`, like
//!   the `try { ... } catch { return undefined }` lookups.
//!
//! Stream events are pull-shaped at the module boundary but push-shaped in
//! `RouterRuntime::execute`, so one stream owns a lazily started body future
//! whose sink feeds an unbounded queue. The body starts on the first `next`
//! (a TS async generator's body never runs before that, so an unstarted
//! stream never touches the provider), advances only at its await points and
//! suspends at every consumer-visible event behind a [`YieldGate`] — the TS
//! yield boundary — so provider reads and terminal side effects stay behind
//! the pull that consumes the last delivered event. The turn's abort flag
//! (`dispatcher:81,441`, `adapter:68`) is forwarded into the per-stream
//! child [`AbortFlag`], so a turn cancel interrupts a `next()` waiting on the
//! provider without waiting for the stream mutex. Closing the handle aborts
//! and drops the body — a TS `return()` never runs the code after the
//! suspension it returns at — and closing also drops undelivered queued
//! events, so a closed stream answers `done` like an exhausted generator.
//!
//! Out of scope by design (composition gaps, not silently accepted here):
//! per-generation provider registry/leases (`ModelInvocationProviderRegistry`),
//! invocation audit staging / `PILOTDECK_DUMP_REQUEST` / proxy dispatchers,
//! and plugin custom routers — those stay with `NoopCustomRouters` and the
//! plain HTTP transport until their own packages land.
#![allow(clippy::result_large_err)]

use super::model::{
    self, ModelExecution, ModelFuture, ModelMetadata, ModelStream, PrepareInput,
    PreparedInvocation, RequestMaterializer, StickyRoute, StreamFuture, StreamInput, TurnContext,
    with_sticky,
};
use crate::host::ModuleError;
use crate::runtime::model::cpu;
use crate::runtime::model::http::YieldGate;
use crate::runtime::model::router::{
    AbortFlag, ManagedModelPolicy, RouterDecisionInput, RouterExecuteContext, RouterExecuteError,
    RouterModelRef, RouterRuntime, RouterRuntimeError,
};
use crate::storage::recorder::{Recorder, RecorderError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc, task::Poll};
use tokio::sync::mpsc;

/// `RouterModelInvokerAdapterOptions`. `workspaceId`/`storageConfigVersion`
/// only feed the invocation audit sink (out of scope), so they are absent.
#[derive(Clone)]
pub struct RouterModelPortOptions {
    /// `options.isMainAgent ?? true`.
    pub is_main_agent: bool,
    /// `options.projectPath`, persisted on router usage records.
    pub project_path: Option<JsString>,
    /// `options.fallbackModels`: SDK-resolved candidates tried before the
    /// primary attempt emits user-visible content.
    pub fallback_models: Vec<RouterModelRef>,
    /// `options.managedModelPolicy`, evaluated in addition to host policy.
    pub managed_model_policy: Option<ManagedModelPolicy>,
    /// `agent.model` + `agent.maxContextTokens` from the session snapshot:
    /// `resolveRoutedModelMaxContextTokens` answers the override instead of
    /// the capability when the route matches the configured agent model.
    pub agent_model: Option<(JsString, JsString)>,
    pub agent_max_context_tokens: Option<JsValue>,
}
impl Default for RouterModelPortOptions {
    /// `isMainAgent` defaults to true, like the TS adapter's
    /// `options.isMainAgent ?? true` (`routerModelInvokerAdapter.ts:33,41`)
    /// when the caller omits it.
    fn default() -> Self {
        Self {
            is_main_agent: true,
            project_path: None,
            fallback_models: Vec::new(),
            managed_model_policy: None,
            agent_model: None,
            agent_max_context_tokens: None,
        }
    }
}
impl RouterModelPortOptions {
    /// `isMainAgent` defaults to true, like every TS call site that omits it.
    pub fn main_agent() -> Self {
        Self {
            is_main_agent: true,
            ..Self::default()
        }
    }
}

/// `createDurableModelInvokerPort`'s recorder binding.
pub struct DurableRecorder {
    pub recorder: Arc<Recorder>,
    /// TS `recordingSessionId`; `None` records under the context session.
    pub recording_session_id: Option<JsString>,
}

/// The turn-composed model ports: `createSidecarModuleComposition().model`
/// plus one `bindTurn`, for drivers that call the ports directly instead of
/// the module wire.
pub struct ComposedPorts {
    /// `bindTurn(...).execution`: the sticky-wrapped durable execution.
    pub execution: Arc<dyn ModelExecution>,
    /// The metadata lookups; `None` matches a composition that installs none.
    pub metadata: Option<Arc<dyn ModelMetadata>>,
    /// `routing.materializeRequest`.
    pub materializer: Option<Arc<dyn RequestMaterializer>>,
}

pub fn ports(
    router: Arc<RouterRuntime>,
    model_config: JsValue,
    recorder: Option<DurableRecorder>,
    options: RouterModelPortOptions,
    session_id: &JsString,
) -> ComposedPorts {
    let port = Arc::new(RouterModelPort {
        router: router.clone(),
        model_config,
        options,
    });
    let execution: Arc<dyn ModelExecution> = recorder
        .map(|durable| {
            Arc::new(DurableModelExecution {
                inner: port.clone(),
                recorder: durable.recorder,
                recording_session_id: durable.recording_session_id,
            }) as Arc<dyn ModelExecution>
        })
        .unwrap_or_else(|| port.clone());
    ComposedPorts {
        execution: with_sticky(execution, sticky_route(router.as_ref(), session_id)),
        metadata: Some(port.clone()),
        materializer: Some(port),
    }
}

/// One turn's model handler over the router runtime: the `bindTurn`
/// equivalent of `sidecarHostModulePorts.ts:279-312`.
///
/// `model_config` is the provider config snapshot the router's invocation
/// provider was built from; only the metadata lookups read it here.
pub fn compose(
    router: Arc<RouterRuntime>,
    model_config: JsValue,
    recorder: Option<DurableRecorder>,
    options: RouterModelPortOptions,
    turn: TurnContext,
) -> model::Handler {
    let ComposedPorts {
        execution,
        metadata,
        materializer,
    } = ports(router, model_config, recorder, options, &turn.session_id);
    let mut handler = model::Handler::with_ports(execution, metadata, turn);
    handler.materializer = materializer;
    handler
}

/// `bindTurn`'s `routing.invalidateSticky(sessionId)`, projected onto the
/// turn-scoped [`StickyRoute`].
fn sticky_route(router: &RouterRuntime, session_id: &JsString) -> Option<StickyRoute> {
    let invalidated = router.invalidate_sticky(session_id);
    let sticky = invalidated.as_object()?;
    Some(StickyRoute {
        previous_tier: field_string(sticky, "previousTier"),
        previous_provider: field_string(sticky, "previousProvider"),
        previous_model: field_string(sticky, "previousModel"),
        orchestrating: sticky
            .get("orchestrating")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
    })
}

/// `createRouterModelInvokerPort` + the metadata lookups of
/// `ProjectSessionRuntimeBundle.ts:468-497`.
struct RouterModelPort {
    router: Arc<RouterRuntime>,
    model_config: JsValue,
    options: RouterModelPortOptions,
}

impl ModelExecution for RouterModelPort {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        let router = self.router.clone();
        let options = self.options.clone();
        Box::pin(async move {
            let decision = match model_override(&input.context) {
                // `routerModelInvokerAdapter.ts:28-37`: an override routes
                // explicitly, without consulting the router at all.
                Some((provider, model)) => explicit_decision(&provider, &model, &options),
                None => router
                    .decide(&RouterDecisionInput {
                        request: input.request.clone(),
                        session_id: context_string(&input.context, "sessionId").unwrap_or_default(),
                        is_main_agent: options.is_main_agent,
                        abort: None,
                        metadata: input
                            .context
                            .get("metadata")
                            .and_then(JsValue::as_object)
                            .cloned(),
                    })
                    .await
                    .map_err(router_error)?,
            };
            let request = router.materialize_request(&decision, &input.request);
            Ok(PreparedInvocation {
                request,
                provider: field_string(decision.as_object().expect("decision object"), "provider")
                    .unwrap_or_default(),
                model: field_string(decision.as_object().expect("decision object"), "model")
                    .unwrap_or_default(),
                max_context_tokens: None,
                max_output_tokens: None,
                opaque: Some(decision),
            })
        })
    }

    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        let router = self.router.clone();
        let session_id = context_string(&input.context, "sessionId").unwrap_or_default();
        let turn_id = context_string(&input.context, "turnId").unwrap_or_default();
        // A child flag per stream: closing or dropping the stream handle
        // interrupts this provider request without touching turn state. The
        // turn's flag forwards into it, like `context.abortSignal` flowing
        // through `router.execute` into the fetch signal.
        let abort = AbortFlag::new();
        let gate = YieldGate::new();
        let ctx = RouterExecuteContext {
            session_id,
            turn_id,
            fallback_models: self.options.fallback_models.clone(),
            managed_model_policy: self.options.managed_model_policy.clone(),
            project_path: self.options.project_path.clone(),
            stream_options: crate::runtime::model::http::StreamOptions {
                abort: Some(abort.clone()),
                yield_gate: Some(gate.clone()),
                ..Default::default()
            },
        };
        Box::pin(async move {
            // `routerModelInvokerAdapter.ts:57-58`: only the adapter that
            // prepared the invocation may stream it.
            let Some(decision) = input.prepared.opaque.clone() else {
                return Err(ModuleError::new(
                    "Router model adapter received an unprepared invocation.",
                ));
            };
            Ok(Box::new(RouterModelStream {
                router,
                pending: Some((decision, input.prepared.request, ctx)),
                body: None,
                events: None,
                failure: None,
                abort,
                gate,
                turn_abort: input.abort,
                turn_wait: None,
            }) as Box<dyn ModelStream>)
        })
    }
}

impl RequestMaterializer for RouterModelPort {
    fn materialize(
        &self,
        prepared: PreparedInvocation,
        candidate: JsObject,
    ) -> ModelFuture<JsValue> {
        let router = self.router.clone();
        Box::pin(async move {
            let Some(decision) = prepared.opaque else {
                return Err(ModuleError::new(
                    "Router model adapter received an unprepared invocation.",
                ));
            };
            Ok(router.materialize_request(&decision, &JsValue::Object(candidate)))
        })
    }
}

impl ModelMetadata for RouterModelPort {
    /// `ProjectSessionRuntimeBundle.ts:469-497`: every lookup catches a
    /// failed capability read and answers `undefined`.
    fn get_model_max_context_tokens(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<Option<JsValue>, ModuleError> {
        // `modelContextWindow.ts:12-28`: the agent-level override wins when the
        // route is the configured agent model.
        if let (Some(override_value), Some((agent_provider, agent_model))) = (
            self.options.agent_max_context_tokens.clone(),
            self.options.agent_model.clone(),
        ) && &agent_provider == provider
            && &agent_model == model
        {
            return Ok(Some(override_value));
        }
        Ok(capability(
            &self.model_config,
            provider,
            model,
            "maxContextTokens",
        ))
    }
    fn get_model_max_output_tokens(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(capability(
            &self.model_config,
            provider,
            model,
            "maxOutputTokens",
        ))
    }
    fn get_model_token_limits(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<Option<JsValue>, ModuleError> {
        // `getModelTokenLimits` always builds the object once capabilities
        // resolve; absent fields stay undefined like the TS record.
        let Some(capabilities) = model_capabilities(&self.model_config, provider, model) else {
            return Ok(None);
        };
        let mut limits = JsObject::new();
        for field in ["maxContextTokens", "maxOutputTokens"] {
            limits.insert(
                field.into(),
                capabilities
                    .get(field)
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
        }
        Ok(Some(JsValue::Object(limits)))
    }
    fn get_model_protocol(&self, provider: &JsString) -> Result<Option<JsValue>, ModuleError> {
        // `runtime.model.getProviderProtocol(provider)`:
        // `config.providers[providerId]?.protocol`, `undefined` when absent.
        Ok(self
            .model_config
            .get("providers")
            .and_then(JsValue::as_object)
            .and_then(|providers| providers.get_key(provider))
            .and_then(|provider| provider.get("protocol"))
            .and_then(JsValue::as_string)
            .map(|protocol| JsValue::String(protocol.clone())))
    }
    fn get_model_supports_prompt_cache(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(capability(
            &self.model_config,
            provider,
            model,
            "supportsPromptCache",
        ))
    }
}

/// `createDurableModelInvokerPort`: record the request before the stream body
/// starts and every event before it is yielded.
struct DurableModelExecution {
    inner: Arc<dyn ModelExecution>,
    recorder: Arc<Recorder>,
    recording_session_id: Option<JsString>,
}

impl ModelExecution for DurableModelExecution {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        self.inner.prepare(input)
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        let inner = self.inner.clone();
        let recorder = self.recorder.clone();
        // `recordingSessionId ?? input.context.sessionId`.
        let session_id = self
            .recording_session_id
            .clone()
            .or_else(|| context_string(&input.context, "sessionId"))
            .unwrap_or_default();
        let turn_id = context_string(&input.context, "turnId").unwrap_or_default();
        let request = input.prepared.request.clone();
        Box::pin(async move {
            Ok(Box::new(DurableModelStream {
                inner: inner.stream(input).await?,
                recorder,
                session_id,
                turn_id,
                request,
                // The durable generator records the request on its first pull,
                // so an unstarted stream records nothing.
                started: false,
            }) as Box<dyn ModelStream>)
        })
    }
}

struct DurableModelStream {
    inner: Box<dyn ModelStream>,
    recorder: Arc<Recorder>,
    session_id: JsString,
    turn_id: JsString,
    request: JsValue,
    started: bool,
}

impl ModelStream for DurableModelStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            if !self.started {
                self.started = true;
                record_off_thread(
                    self.recorder.clone(),
                    self.session_id.clone(),
                    self.turn_id.clone(),
                    RecordCall::Request(self.request.clone()),
                )
                .await?;
            }
            let event = self.inner.next().await?;
            if let Some(event) = &event {
                record_off_thread(
                    self.recorder.clone(),
                    self.session_id.clone(),
                    self.turn_id.clone(),
                    RecordCall::Event(event.clone()),
                )
                .await?;
            }
            Ok(event)
        })
    }
    fn close(&mut self) -> StreamFuture<'_, ()> {
        // `return()` on the durable wrapper records nothing; recording state
        // only advances while events are yielded.
        Box::pin(async move { self.inner.close().await })
    }
}

/// Which recorder call a [`record_off_thread`] run performs.
enum RecordCall {
    Request(JsValue),
    Event(JsValue),
}

/// The recorder's transcript append is synchronous file I/O (`open` +
/// `write_all` per event). It must not run on the executor thread
/// (`model.rs`: ports are cooperative async work), so it goes through the
/// same bound blocking pool as the tokenizer batches (`cpu::run`). The
/// record still completes before the event is yielded, preserving the
/// durable wrapper's on-disk order.
async fn record_off_thread(
    recorder: Arc<Recorder>,
    session_id: JsString,
    turn_id: JsString,
    call: RecordCall,
) -> Result<(), ModuleError> {
    cpu::run(move || {
        let recorded = match &call {
            RecordCall::Request(request) => recorder
                .record_model_request(&session_id, &turn_id, request.clone())
                .map(|_| ()),
            RecordCall::Event(event) => recorder.record_model_event(&session_id, &turn_id, event),
        };
        recorded.map_err(|error| crate::host::HostError::from(recorder_error(error)))
    })
    .await
    .map_err(|error| ModuleError::new(error.message.clone()))
}

/// The lazily started `RouterRuntime::execute` body of one stream.
type ExecuteBody = Pin<Box<dyn Future<Output = Result<(), RouterExecuteError>> + Send>>;
/// The turn abort's `cancelled()` wait, boxed so it can live in the stream.
type TurnWait = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The router-backed stream: a lazily started `execute` body feeding a queue.
struct RouterModelStream {
    router: Arc<RouterRuntime>,
    /// `(decision, request, ctx)` until the first `next` starts the body.
    pending: Option<(JsValue, JsValue, RouterExecuteContext)>,
    body: Option<ExecuteBody>,
    events: Option<mpsc::UnboundedReceiver<JsValue>>,
    failure: Option<ModuleError>,
    abort: AbortFlag,
    gate: YieldGate,
    /// The turn-level abort forwarded into [`Self::abort`].
    turn_abort: Option<AbortFlag>,
    /// `turn_abort.cancelled()` once the body has started.
    turn_wait: Option<TurnWait>,
}

impl RouterModelStream {
    fn start(&mut self) {
        let Some((decision, request, ctx)) = self.pending.take() else {
            return;
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let router = self.router.clone();
        let gate = self.gate.clone();
        self.events = Some(receiver);
        self.turn_wait = self.turn_abort.as_ref().map(|turn| {
            let turn = turn.clone();
            Box::pin(async move { turn.cancelled().await }) as TurnWait
        });
        self.body = Some(Box::pin(async move {
            let mut sink = move |event: JsValue| {
                gate.deliver();
                let _ = sender.send(event);
            };
            router.execute(&decision, &request, &ctx, &mut sink).await
        }));
    }
}

impl ModelStream for RouterModelStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            self.start();
            std::future::poll_fn(|cx| {
                loop {
                    if let Some(events) = self.events.as_mut() {
                        let received = events.poll_recv(cx);
                        match received {
                            // Events ahead of the body's completion are yielded
                            // first; only a drained closed channel falls through.
                            // Taking one completes a TS yield: the suspended body
                            // may advance to its next provider read.
                            Poll::Ready(Some(event)) => {
                                self.gate.pull();
                                return Poll::Ready(Ok(Some(event)));
                            }
                            Poll::Ready(None) => self.events = None,
                            Poll::Pending => {}
                        }
                    }
                    // Forward the turn abort into the child flag, then let the
                    // body observe it at its own await points. A pre-aborted
                    // turn therefore finishes the body with no provider work,
                    // like the attempts-loop check in `RouterRuntime.ts:685`.
                    if let Some(wait) = self.turn_wait.as_mut()
                        && wait.as_mut().poll(cx).is_ready()
                    {
                        self.turn_wait = None;
                        self.abort.abort();
                    }
                    let polled = match self.body.as_mut() {
                        Some(body) => body.as_mut().poll(cx),
                        // The body finished and the queue is drained: a buffered
                        // failure surfaces after the last queued event, like a
                        // throw from a TS generator that already yielded.
                        None => {
                            return match self.failure.take() {
                                Some(failure) => Poll::Ready(Err(failure)),
                                None => Poll::Ready(Ok(None)),
                            };
                        }
                    };
                    match polled {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(())) => self.body = None,
                        Poll::Ready(Err(error)) => {
                            self.failure = Some(module_error(error));
                            self.body = None;
                        }
                    }
                }
            })
            .await
        })
    }

    fn close(&mut self) -> StreamFuture<'_, ()> {
        Box::pin(async move {
            // `return()` on a generator that never entered its body must not
            // create (or interrupt) the underlying stream.
            if self.pending.is_some() {
                self.pending = None;
                return Ok(());
            }
            // Interrupt the provider request, then drop the body: a TS
            // `return()` never runs the code after the suspension it returns
            // at, so outcome handling (health, `execute_failed`, terminal
            // error events) must not fire after a partial pull. Dropping the
            // body future also drops the provider connection, like undici
            // losing its consumer. The interrupted body's error is discarded:
            // `close_stream` is `await iterator.return()`, and a TS
            // generator's early return never rejects
            // (`sidecarDefaultModuleDispatcher.ts:153-158`). Undelivered
            // queued events go with it — a closed generator is `done`.
            self.abort.abort();
            self.body = None;
            self.events = None;
            self.failure = None;
            Ok(())
        })
    }
}

/// `RouterRuntimeError` carries `code` on the wire like every TS call site
/// that constructs it; model stream errors use the host projection.
fn module_error(error: RouterExecuteError) -> ModuleError {
    match error {
        RouterExecuteError::Router(error) => router_error(error),
        RouterExecuteError::Model(error) => error.into(),
    }
}

fn router_error(error: RouterRuntimeError) -> ModuleError {
    ModuleError {
        message: error.message,
        code: Some(error.code.into()),
        canonical: None,
        retryable: None,
        retry_after_ms: None,
    }
}

/// A recorder failure crosses the module boundary as its message, like the
/// TS recorder's `Error`/validation throws.
fn recorder_error(error: RecorderError) -> ModuleError {
    ModuleError::new(error.to_string())
}

/// `routerModelInvokerAdapter.ts:29-37`.
fn explicit_decision(
    provider: &JsString,
    model: &JsString,
    options: &RouterModelPortOptions,
) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert("provider".into(), JsValue::String(provider.clone()));
    decision.insert("model".into(), JsValue::String(model.clone()));
    decision.insert("scenarioType".into(), "explicit".into());
    decision.insert("isSubagent".into(), JsValue::Bool(!options.is_main_agent));
    decision.insert("orchestrating".into(), JsValue::Bool(false));
    decision.insert("resolvedFrom".into(), "explicit".into());
    decision.insert("mutations".into(), JsValue::Object(JsObject::new()));
    JsValue::Object(decision)
}

/// `context.modelOverride`, present only with both route strings.
fn model_override(context: &JsObject) -> Option<(JsString, JsString)> {
    let over = context.get("modelOverride")?.as_object()?;
    Some((
        over.get("provider")?.as_string()?.clone(),
        over.get("model")?.as_string()?.clone(),
    ))
}

fn context_string(context: &JsObject, key: &str) -> Option<JsString> {
    context
        .get(key)
        .and_then(JsValue::as_string)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn field_string(object: &JsObject, key: &str) -> Option<JsString> {
    object.get(key).and_then(JsValue::as_string).cloned()
}

/// `config.providers[provider].models[model].capabilities`, or `None` when
/// the route or the capabilities record is absent (TS `getCapabilities`
/// throws and every metadata lookup catches).
fn model_capabilities<'a>(
    config: &'a JsValue,
    provider: &JsString,
    model: &JsString,
) -> Option<&'a JsObject> {
    config
        .get("providers")
        .and_then(JsValue::as_object)?
        .get_key(provider)?
        .get("models")
        .and_then(JsValue::as_object)?
        .get_key(model)?
        .get("capabilities")
        .and_then(JsValue::as_object)
}

fn capability(
    config: &JsValue,
    provider: &JsString,
    model: &JsString,
    field: &str,
) -> Option<JsValue> {
    model_capabilities(config, provider, model)?
        .get(field)
        .cloned()
}
