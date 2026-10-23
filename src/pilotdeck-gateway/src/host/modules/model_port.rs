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
//! `RouterRuntime::execute`, so one stream owns a lazily started body task
//! whose sink feeds an unbounded queue. The body starts on the first `next`
//! (a TS async generator's body never runs before that, so an unstarted
//! stream never touches the provider), advances only at its await points and
//! suspends at every consumer-visible event behind a [`YieldGate`] — the TS
//! yield boundary — so provider reads and terminal side effects stay behind
//! the pull that consumes the last delivered event. The turn's abort flag
//! (`dispatcher:81,441`, `adapter:68`) reaches the per-stream child
//! [`AbortFlag`] through a dedicated task the moment it fires, like the abort
//! listener TS registers directly on the response body
//! (`streamModel.ts`'s `cancelReader`), so a turn cancel interrupts the
//! provider request without waiting for the stream mutex, for a consumer
//! pull, or for a recorder write the outer durable stream is awaiting. Closing
//! the handle aborts the body task — a TS `return()` never runs the code after
//! the suspension it returns at — and closing also drops undelivered queued
//! events, so a closed stream answers `done` like an exhausted generator.
//!
//! Out of scope by design (composition gaps, not silently accepted here):
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
use tokio::sync::{mpsc, oneshot};

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
                Some(over) => explicit_decision(
                    &override_field(&over, "provider"),
                    &override_field(&over, "model"),
                    &options,
                ),
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
            // `routerModelInvokerAdapter.ts:51-52`: `provider`/`model` are
            // `decision.provider` / `decision.model` — raw route values, an
            // override may leave either `undefined`.
            let route = |key: &str| {
                decision
                    .as_object()
                    .and_then(|object| object.get(key))
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            };
            Ok(PreparedInvocation {
                request,
                provider: route("provider"),
                model: route("model"),
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
                forwarder: None,
                events: None,
                outcome: None,
                awaiting_pull: false,
                abort,
                gate,
                turn_abort: input.abort,
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
    /// `ProjectSessionRuntimeBundle.ts:468-497`: the route arrives raw (the
    /// dispatcher passes it through, `sidecarDefaultModuleDispatcher.ts:88`),
    /// and every capability lookup wraps its read in
    /// `try { ... } catch { return undefined }` — so a route whose config-key
    /// conversion throws answers `None` here.
    fn get_model_max_context_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        // `modelContextWindow.ts:15-21`: the agent-level override wins when the
        // raw route strictly equals the configured agent model route. The
        // configured side is a string, so `===` is a string-value comparison.
        if let (Some(override_value), Some((agent_provider, agent_model))) = (
            self.options.agent_max_context_tokens.clone(),
            self.options.agent_model.clone(),
        ) && provider
            .as_string()
            .is_some_and(|route| route == &agent_provider)
            && model.as_string().is_some_and(|route| route == &agent_model)
        {
            return Ok(Some(override_value));
        }
        let (Some(provider), Some(model)) = (caught_route_key(provider), caught_route_key(model))
        else {
            return Ok(None);
        };
        Ok(capability(
            &self.model_config,
            &provider,
            &model,
            "maxContextTokens",
        ))
    }
    fn get_model_max_output_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (caught_route_key(provider), caught_route_key(model))
        else {
            return Ok(None);
        };
        Ok(capability(
            &self.model_config,
            &provider,
            &model,
            "maxOutputTokens",
        ))
    }
    fn get_model_token_limits(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (caught_route_key(provider), caught_route_key(model))
        else {
            return Ok(None);
        };
        // `getModelTokenLimits` always builds the object once capabilities
        // resolve; absent fields stay undefined like the TS record.
        let Some(capabilities) = model_capabilities(&self.model_config, &provider, &model) else {
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
    fn get_model_protocol(&self, provider: &JsValue) -> Result<Option<JsValue>, ModuleError> {
        // `runtime.model.getProviderProtocol(provider)`
        // (`ProjectSessionRuntimeBundle.ts:490`, no try/catch):
        // `config.providers[providerId]?.protocol` (`ModelRuntime.ts:49`) —
        // the property-key coercion itself throws for a route whose
        // `String()` fails, and that raw error rejects the call.
        let provider = crate::js_value::property_key(provider)
            .map_err(|error| ModuleError::new(error.message()))?;
        Ok(self
            .model_config
            .get("providers")
            .and_then(JsValue::as_object)
            .and_then(|providers| providers.get_key(&provider))
            .and_then(|provider| provider.get("protocol"))
            .and_then(JsValue::as_string)
            .map(|protocol| JsValue::String(protocol.clone())))
    }
    fn get_model_supports_prompt_cache(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let (Some(provider), Some(model)) = (caught_route_key(provider), caught_route_key(model))
        else {
            return Ok(None);
        };
        Ok(capability(
            &self.model_config,
            &provider,
            &model,
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
                request_record: RequestRecord::Unstarted,
                held: None,
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
    /// The request record of the first pull. TS `recordModelRequest` runs
    /// synchronously inside that pull, so the spawned record must complete
    /// (and be observed) before any event is pulled — no matter how many
    /// times the pull is cancelled and retried in between.
    request_record: RequestRecord,
    /// An event already pulled from the inner stream, held until its record
    /// has been observed complete. Keeping it in the stream (not in a local
    /// of one `next()` future) is what preserves it across a pull cancelled
    /// while the record was in flight: the next pull first awaits that same
    /// record, then delivers the same event — never skipping it, never
    /// re-recording it, exactly once like the TS wrapper's synchronous write.
    held: Option<HeldEvent>,
}

/// Where the request record of [`DurableModelStream`] stands.
enum RequestRecord {
    /// The first pull has not spawned it yet.
    Unstarted,
    /// Spawned; awaited across pulls until its completion is observed.
    InFlight(RecordCompletion),
    /// Completed and observed (a record failure is also terminal here).
    Done,
}

struct HeldEvent {
    event: JsValue,
    record: RecordCompletion,
}

/// A spawned record's result channel. Polled in place — never consumed by a
/// cancelled pull — so one completion observation serves any number of
/// retried pulls. `Receiver` is `Unpin`, so [`Future::poll`] works on a
/// mutable reference.
type RecordCompletion = oneshot::Receiver<Result<(), ModuleError>>;

/// Poll a [`RecordCompletion`] in place.
fn poll_record(
    record: &mut RecordCompletion,
    cx: &mut std::task::Context<'_>,
) -> Poll<Result<Result<(), ModuleError>, oneshot::error::RecvError>> {
    Future::poll(Pin::new(record), cx)
}

impl ModelStream for DurableModelStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            std::future::poll_fn(|cx| {
                loop {
                    if matches!(self.request_record, RequestRecord::Unstarted) {
                        self.request_record = RequestRecord::InFlight(spawn_record(
                            self.recorder.clone(),
                            self.session_id.clone(),
                            self.turn_id.clone(),
                            RecordCall::Request(self.request.clone()),
                        ));
                    }
                    if let RequestRecord::InFlight(record) = &mut self.request_record
                        && let Poll::Ready(result) = poll_record(record, cx)
                    {
                        self.request_record = RequestRecord::Done;
                        if let Err(error) = record_result(result) {
                            return Poll::Ready(Err(error));
                        }
                    } else if matches!(self.request_record, RequestRecord::InFlight(_)) {
                        return Poll::Pending;
                    }
                    if let Some(held) = self.held.as_mut() {
                        return match poll_record(&mut held.record, cx) {
                            Poll::Pending => Poll::Pending,
                            Poll::Ready(result) => {
                                let HeldEvent { event, .. } = self.held.take().expect("held event");
                                match record_result(result) {
                                    Ok(()) => Poll::Ready(Ok(Some(event))),
                                    Err(error) => Poll::Ready(Err(error)),
                                }
                            }
                        };
                    }
                    let polled = {
                        let mut inner = self.inner.next();
                        inner.as_mut().poll(cx)
                    };
                    match polled {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                        Poll::Ready(Ok(None)) => return Poll::Ready(Ok(None)),
                        Poll::Ready(Ok(Some(event))) => {
                            self.held = Some(HeldEvent {
                                record: spawn_record(
                                    self.recorder.clone(),
                                    self.session_id.clone(),
                                    self.turn_id.clone(),
                                    RecordCall::Event(event.clone()),
                                ),
                                event,
                            });
                        }
                    }
                }
            })
            .await
        })
    }
    fn close(&mut self) -> StreamFuture<'_, ()> {
        // `return()` on the durable wrapper records nothing new, but a record
        // already in flight still lands first: TS writes it synchronously
        // inside the pull it belonged to, so close settles after that write
        // and the event it gated is discarded — recorded, never yielded.
        Box::pin(async move {
            let records = [
                match std::mem::replace(&mut self.request_record, RequestRecord::Done) {
                    RequestRecord::InFlight(record) => Some(record),
                    _ => None,
                },
                self.held.take().map(|held| held.record),
            ];
            for record in records.into_iter().flatten() {
                let _ = record.await;
            }
            self.inner.close().await
        })
    }
}

/// A record's completion as observed by the stream. `Err` is a record failure
/// (the TS recorder throws); a dropped channel means the record task ended
/// without completing, which it never does by design.
fn record_result(
    result: Result<Result<(), ModuleError>, oneshot::error::RecvError>,
) -> Result<(), ModuleError> {
    match result {
        Ok(result) => result,
        Err(_) => Err(ModuleError::new(
            "Model transcript record ended without completing.",
        )),
    }
}

/// Spawn one recorder call as its own task. The write completes regardless of
/// whether anyone still awaits it — the caller-cancellable `cpu::run` future
/// stays inside the task — and the result comes back through a oneshot the
/// stream re-awaits across cancelled pulls, so an abandoned pull neither
/// aborts the write nor duplicates it.
fn spawn_record(
    recorder: Arc<Recorder>,
    session_id: JsString,
    turn_id: JsString,
    call: RecordCall,
) -> RecordCompletion {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let _ = sender.send(record_off_thread(recorder, session_id, turn_id, call).await);
    });
    receiver
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

/// The `RouterRuntime::execute` body of one stream, as its own executor task:
/// the executor polls it, so the turn abort can interrupt the provider request
/// without a consumer pull (`next` only reads the queue the body feeds).
type BodyTask = tokio::task::JoinHandle<()>;
/// The turn abort's forwarder: one wait, fired the moment the turn cancels.
type Forwarder = tokio::task::JoinHandle<()>;

/// The router-backed stream: a lazily started `execute` body task feeding a
/// queue.
struct RouterModelStream {
    router: Arc<RouterRuntime>,
    /// `(decision, request, ctx)` until the first `next` starts the body.
    pending: Option<(JsValue, JsValue, RouterExecuteContext)>,
    /// The body task; `None` before the first `next` and once it finished.
    body: Option<BodyTask>,
    /// Forwards the turn abort into [`Self::abort`] without a consumer pull.
    forwarder: Option<Forwarder>,
    events: Option<mpsc::UnboundedReceiver<JsValue>>,
    /// The body task's outcome — sent before its events channel closes.
    outcome: Option<mpsc::UnboundedReceiver<Result<(), RouterExecuteError>>>,
    /// An event was taken from the queue; its [`YieldGate`] pull is deferred
    /// to the next `next()` call.
    awaiting_pull: bool,
    abort: AbortFlag,
    gate: YieldGate,
    /// The turn-level abort forwarded into [`Self::abort`].
    turn_abort: Option<AbortFlag>,
}

impl RouterModelStream {
    /// Stop the tasks a closed or dropped handle no longer needs. Aborting a
    /// task only takes effect at its current suspension, so nothing runs
    /// after it, and the provider connection goes with the abandoned body.
    fn stop_tasks(&mut self) {
        if let Some(forwarder) = self.forwarder.take() {
            forwarder.abort();
        }
        if let Some(body) = self.body.take() {
            body.abort();
        }
    }

    fn start(&mut self) {
        let Some((decision, request, ctx)) = self.pending.take() else {
            return;
        };
        // TS registers the abort listener on the response body when the
        // stream starts (`streamModel.ts`'s `cancelReader`): cancellation
        // must not depend on the consumer pulling. A turn already cancelled
        // when the body starts is forwarded inline so the body observes it on
        // its first poll, like the attempts-loop precheck
        // (`RouterRuntime.ts:685`); otherwise a dedicated task forwards it
        // the moment it fires — including while the consumer sits between
        // pulls or while the outer durable stream awaits a recorder write.
        if let Some(turn) = self.turn_abort.clone() {
            if turn.is_aborted() {
                self.abort.abort();
            } else {
                let child = self.abort.clone();
                self.forwarder = Some(tokio::spawn(async move {
                    turn.cancelled().await;
                    child.abort();
                }));
            }
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        let (outcome_sender, outcome) = mpsc::unbounded_channel();
        let router = self.router.clone();
        let gate = self.gate.clone();
        self.events = Some(receiver);
        self.outcome = Some(outcome);
        self.body = Some(tokio::spawn(async move {
            let mut sink = move |event: JsValue| {
                gate.deliver();
                let _ = sender.send(event);
            };
            let _ = outcome_sender.send(router.execute(&decision, &request, &ctx, &mut sink).await);
        }));
    }
}

impl Drop for RouterModelStream {
    /// Dropping the handle loses the consumer, like undici's connection when
    /// its consumer goes away: the provider request is interrupted and both
    /// tasks stop. Aborting a finished task is a no-op.
    fn drop(&mut self) {
        self.abort.abort();
        self.stop_tasks();
    }
}

impl ModelStream for RouterModelStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            self.start();
            std::future::poll_fn(|cx| {
                // Taking an event completes a TS yield, but the suspended
                // body only resumes on the consumer's next pull — which,
                // through the durable wrapper, is after the yielded event has
                // been recorded and delivered. Pulling at queue-take would
                // let the body's post-yield terminal handling (e.g. the
                // execute_failed event) race the record window and surface
                // before the yielded event, diverging from TS. So the pull
                // is deferred to the start of the next `next()` call.
                if self.awaiting_pull {
                    self.awaiting_pull = false;
                    self.gate.pull();
                }
                if let Some(events) = self.events.as_mut() {
                    match events.poll_recv(cx) {
                        // Events ahead of the body's completion are yielded
                        // first; only a drained closed channel falls through.
                        Poll::Ready(Some(event)) => {
                            self.awaiting_pull = true;
                            return Poll::Ready(Ok(Some(event)));
                        }
                        Poll::Ready(None) => self.events = None,
                        Poll::Pending => return Poll::Pending,
                    }
                }
                // The body task has finished and the queue is drained: its
                // outcome — sent before the channel closed — surfaces now,
                // like a throw from a TS generator that already yielded, and
                // exactly once; later pulls answer `done`.
                match self.outcome.as_mut() {
                    Some(outcome) => match outcome.poll_recv(cx) {
                        Poll::Ready(Some(Ok(()))) => {
                            self.outcome = None;
                            Poll::Ready(Ok(None))
                        }
                        Poll::Ready(Some(Err(error))) => {
                            self.outcome = None;
                            Poll::Ready(Err(module_error(error)))
                        }
                        Poll::Ready(None) => {
                            self.outcome = None;
                            Poll::Ready(Err(ModuleError::new(
                                "Model stream body ended without an outcome.",
                            )))
                        }
                        Poll::Pending => Poll::Pending,
                    },
                    None => Poll::Ready(Ok(None)),
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
            // Interrupt the provider request, then stop the tasks: a TS
            // `return()` never runs the code after the suspension it returns
            // at, so outcome handling (health, `execute_failed`, terminal
            // error events) must not fire after a partial pull. The aborted
            // body's error is discarded: `close_stream` is
            // `await iterator.return()`, and a TS generator's early return
            // never rejects (`sidecarDefaultModuleDispatcher.ts:153-158`).
            // Undelivered queued events go with it — a closed generator is
            // `done`. The forwarder stops too: there is nothing left to
            // cancel.
            self.abort.abort();
            self.stop_tasks();
            self.events = None;
            self.outcome = None;
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

/// `routerModelInvokerAdapter.ts:29-37`. The route fields are the raw
/// override reads: a missing or non-string field stays `undefined` (and a
/// non-string value passes through), exactly like the TS decision literal —
/// serialization drops `undefined`, it never invents a route.
fn explicit_decision(
    provider: &JsValue,
    model: &JsValue,
    options: &RouterModelPortOptions,
) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert("provider".into(), provider.clone());
    decision.insert("model".into(), model.clone());
    decision.insert("scenarioType".into(), "explicit".into());
    decision.insert("isSubagent".into(), JsValue::Bool(!options.is_main_agent));
    decision.insert("orchestrating".into(), JsValue::Bool(false));
    decision.insert("resolvedFrom".into(), "explicit".into());
    decision.insert("mutations".into(), JsValue::Object(JsObject::new()));
    JsValue::Object(decision)
}

/// `context.modelOverride` (`routerModelInvokerAdapter.ts:28`): any truthy
/// value routes explicitly — the judgment is JavaScript truthiness, not a
/// shape check. `null` and `undefined` (or `false`/`0`/`""`) fall through to
/// `router.decide()`.
fn model_override(context: &JsObject) -> Option<JsValue> {
    let over = context.get("modelOverride")?;
    super::model::js_truthy(over).then(|| over.clone())
}

/// `context.modelOverride.provider` / `.model` (`routerModelInvokerAdapter.ts:30-31`):
/// a property read off the override. A truthy non-object has neither, so both
/// read as `undefined`.
fn override_field(over: &JsValue, key: &str) -> JsValue {
    over.as_object()
        .and_then(|object| object.get(key))
        .cloned()
        .unwrap_or(JsValue::Undefined)
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

/// The raw route's config-index key, or `None` when its `String()` conversion
/// throws — the `try { ... } catch { return undefined }` wrappers of
/// `ProjectSessionRuntimeBundle.ts:475-497` answer `undefined` for it.
fn caught_route_key(value: &JsValue) -> Option<JsString> {
    crate::js_value::property_key(value).ok()
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
