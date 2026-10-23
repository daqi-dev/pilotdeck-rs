//! Model module adapter. A turn owns prepared invocations and open event streams.
//!
//! `ModuleError` is intentionally returned by value: it carries canonical and
//! retry details, and ports fail rarely enough that boxing every boundary would
//! add allocation without simplifying a hot path.
#![allow(clippy::result_large_err)]
//!
//! The execution/metadata ports are trait objects: A6.4-pr1 defines the boundary
//! and exercises it with fakes; provider HTTP and routing materialization arrive
//! in later A6.4 packages. Port implementations must be cooperative async work;
//! synchronous blocking would stall the host's callback shutdown path (#265).
use super::wire;
use crate::host::{HostError, HostFuture, JsObject, JsString, JsValue, ModuleError};
use crate::runtime::model::router::AbortFlag;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

/// Future returned by a model port. The adapter never polls a port inline.
pub type ModelFuture<T> = Pin<Box<dyn Future<Output = Result<T, ModuleError>> + Send + 'static>>;
/// Future returned by one stream step; borrowing keeps the stream object owned.
pub type StreamFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModuleError>> + Send + 'a>>;

/// Result of `prepare`, mirroring `PreparedModelInvocation` boundary fields.
/// `max_*` stay boundary values because the dispatcher serializes them by
/// JavaScript truthiness without validating them (`dispatcher:92-105`). The
/// route fields are boundary values too: an explicit override route reaches
/// `prepare` with `provider`/`model` as-is (`routerModelInvokerAdapter.ts:51-52`),
/// so either can be `undefined` or a non-string, and it serializes away like
/// the TS prepared invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedInvocation {
    pub request: JsValue,
    pub provider: JsValue,
    pub model: JsValue,
    pub max_context_tokens: Option<JsValue>,
    pub max_output_tokens: Option<JsValue>,
    /// `PreparedModelInvocation.opaque`: host-local routing state the preparing
    /// adapter reuses to execute without routing again. Like TS, it never
    /// serializes over the sidecar wire (`serialize_prepared` omits it).
    pub opaque: Option<JsValue>,
}
impl PreparedInvocation {
    pub fn new(request: JsValue, provider: JsValue, model: JsValue) -> Self {
        Self {
            request,
            provider,
            model,
            max_context_tokens: None,
            max_output_tokens: None,
            opaque: None,
        }
    }
}

#[derive(Debug)]
pub struct PrepareInput {
    pub request: JsValue,
    pub context: JsObject,
}
#[derive(Debug)]
pub struct StreamInput {
    pub prepared: PreparedInvocation,
    pub context: JsObject,
    /// `context.abortSignal` (`dispatcher:81,441`): the turn-level
    /// cancellation forwarded into the provider request. It rides alongside
    /// the JS context because it is host state, not serializable payload.
    pub abort: Option<AbortFlag>,
}

/// Host model execution port (`ModelInvokerPort` in the fixed TS baseline).
/// Implementations must not block the executor thread and must answer every
/// future promptly enough for the host's cooperative shutdown.
pub trait ModelExecution: Send + Sync {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation>;
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>>;
}

/// Reapply an already selected provider route without invoking routing again.
pub trait RequestMaterializer: Send + Sync {
    fn materialize(
        &self,
        prepared: PreparedInvocation,
        candidate: JsObject,
    ) -> ModelFuture<JsValue>;
}

/// Push-style view of one canonical model event iterator. `next` returns
/// `Ok(None)` exactly when the TS iterator reports `done`.
pub trait ModelStream: Send {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>>;
    /// Iterator `return()`. `close_stream` surfaces the error; `dispose`
    /// intentionally ignores it, like TS `Promise.allSettled`.
    fn close(&mut self) -> StreamFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Metadata lookup fields present on a host port. The fixed TS composition
/// only installs metadata when at least one lookup is defined, and the
/// dispatcher calls only the defined lookups (`dispatcher:445-453`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataField {
    MaxContextTokens,
    MaxOutputTokens,
    TokenLimits,
    Protocol,
    SupportsPromptCache,
}

/// Metadata lookup port. The route arrives raw — `dispatcher:88` passes
/// `prepared.provider` / `prepared.model` into `serializeModelMetadata`
/// (`:445-450`) which hands them to every defined lookup unchanged, so an
/// implementation converts at its own config index exactly like its TS
/// counterpart. Values stay boundary values because the fixed TS dispatcher
/// accepts only safe positive integers/protocol identifiers and reports
/// invalid shapes as host errors (`dispatcher:451-470`).
pub trait ModelMetadata: Send + Sync {
    /// Whether this host port defines the lookup. Defaults to `true`, which is
    /// safe because absent values are represented by `Ok(None)`.
    fn provides(&self, _field: MetadataField) -> bool {
        true
    }
    fn get_model_max_context_tokens(
        &self,
        _provider: &JsValue,
        _model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(None)
    }
    fn get_model_max_output_tokens(
        &self,
        _provider: &JsValue,
        _model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(None)
    }
    fn get_model_token_limits(
        &self,
        _provider: &JsValue,
        _model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(None)
    }
    fn get_model_protocol(&self, _provider: &JsValue) -> Result<Option<JsValue>, ModuleError> {
        Ok(None)
    }
    fn get_model_supports_prompt_cache(
        &self,
        _provider: &JsValue,
        _model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(None)
    }
}

/// One turn's sticky routing state, consumed once by `bindTurn`
/// (`sidecarHostModulePorts.ts:287-291`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StickyRoute {
    pub previous_tier: Option<JsString>,
    pub previous_provider: Option<JsString>,
    pub previous_model: Option<JsString>,
    pub orchestrating: bool,
}
impl StickyRoute {
    fn metadata(&self, previous_tier: Option<&JsString>) -> JsObject {
        let mut metadata = JsObject::new();
        if let Some(tier) = previous_tier.filter(|value| !value.is_empty()) {
            metadata.insert("previousTier".into(), JsValue::String(tier.clone()));
        }
        if let Some(provider) = self
            .previous_provider
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            metadata.insert("previousProvider".into(), JsValue::String(provider.clone()));
        }
        if let Some(model) = self
            .previous_model
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            metadata.insert("previousModel".into(), JsValue::String(model.clone()));
        }
        metadata
    }
}

/// Turn identity used to build the model execution context
/// (`dispatcher:458`). Values that TS takes from `AgentLoopInput` are explicit
/// here so composition decides them once per turn.
#[derive(Clone, Debug, Default)]
pub struct TurnContext {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub operation_deadline: Option<JsString>,
    pub workspace_id: Option<JsString>,
    pub storage_config_version: Option<JsString>,
    /// `input.abortSignal` / `input.signal`: the turn-level cancellation the
    /// driver arms (`agentLoopSidecarServer.ts:248-253` deadline included).
    /// Every stream the turn opens forwards it to its provider request, so a
    /// turn cancel interrupts a `stream_next` waiting on the provider without
    /// going through `close_stream`'s stream mutex.
    pub abort: Option<AbortFlag>,
}

#[derive(Default)]
struct State {
    preparations: Vec<(JsString, PreparedInvocation)>,
    streams: Vec<(JsString, Arc<StreamEntry>)>,
    // Detached from lookup, but still owned by the turn until close settles.
    closing: Vec<Arc<StreamEntry>>,
}
impl State {
    fn preparation(&self, id: &JsString) -> Option<PreparedInvocation> {
        self.preparations
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value.clone())
    }
    fn insert_preparation(&mut self, id: JsString, value: PreparedInvocation) {
        if let Some((_, slot)) = self.preparations.iter_mut().find(|(key, _)| *key == id) {
            *slot = value;
        } else {
            self.preparations.push((id, value));
        }
    }
    fn stream(&self, id: &JsString) -> Option<Arc<StreamEntry>> {
        self.streams
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value.clone())
    }
    fn remove_stream(&mut self, id: &JsString, entry: &Arc<StreamEntry>, preparation: bool) {
        // An old callback must not remove a replacement created after close.
        if let Some(index) = self
            .streams
            .iter()
            .position(|(key, value)| key == id && Arc::ptr_eq(value, entry))
        {
            self.streams.remove(index);
            if preparation {
                self.preparations.retain(|(key, _)| key != id);
            }
        }
    }
    fn finish_close(&mut self, entry: &Arc<StreamEntry>) {
        self.closing.retain(|value| !Arc::ptr_eq(value, entry));
    }
}

struct StreamEntry {
    phase: tokio::sync::Mutex<StreamPhase>,
}

// Futures belong to the turn, not to whichever callback happens to poll them.
// Awaiting by reference preserves creation/close progress if that callback is
// cancelled. No detached provider task can outlive the host's dispose timeout.
enum StreamPhase {
    Creating(ModelFuture<Box<dyn ModelStream>>),
    Open(Box<dyn ModelStream>),
    Ended(Box<dyn ModelStream>),
    CreateFailed(ModuleError),
    Done,
    Closing(ModelFuture<()>),
    Closed(Result<(), ModuleError>),
}
impl StreamPhase {
    async fn initialize(&mut self) -> Result<(), ModuleError> {
        if let Self::Creating(future) = self {
            *self = match future.await {
                Ok(stream) => Self::Open(stream),
                Err(error) => Self::CreateFailed(error),
            };
        }
        match self {
            Self::CreateFailed(error) => Err(error.clone()),
            _ => Ok(()),
        }
    }
    async fn next(&mut self) -> Result<Option<JsValue>, ModuleError> {
        self.initialize().await?;
        let Self::Open(stream) = self else {
            return Ok(None);
        };
        let next = stream.next().await;
        if !matches!(next, Ok(Some(_))) {
            let Self::Open(stream) = std::mem::replace(self, Self::Done) else {
                unreachable!()
            };
            // A close already waiting for next still owns iterator.return(),
            // even if next reaches EOF or rejects before releasing the lock.
            *self = Self::Ended(stream);
        }
        next
    }
    async fn close(&mut self) -> Result<(), ModuleError> {
        // Creation failure has no iterator to return. A successful late create
        // is closed here without ever publishing it back to the lookup table.
        let _ = self.initialize().await;
        if matches!(self, Self::Open(_) | Self::Ended(_)) {
            let (Self::Open(mut stream) | Self::Ended(mut stream)) =
                std::mem::replace(self, Self::Done)
            else {
                unreachable!()
            };
            *self = Self::Closing(Box::pin(async move { stream.close().await }));
        }
        if let Self::Closing(future) = self {
            *self = Self::Closed(future.await);
        }
        match self {
            Self::Closed(result) => result.clone(),
            _ => Ok(()),
        }
    }
}

/// Turn-owned model handler. The router constructs one instance per turn, so
/// `preparations`/`streams` are naturally scoped to that turn.
#[derive(Clone, Default)]
pub struct Handler {
    execution: Option<Arc<dyn ModelExecution>>,
    metadata: Option<Arc<dyn ModelMetadata>>,
    turn: TurnContext,
    state: Arc<Mutex<State>>,
    /// Optional host materializer captured by `materialize_prepared_request`
    /// and by the context module's compaction candidate request.
    pub materializer: Option<Arc<dyn RequestMaterializer>>,
}
impl Handler {
    /// `metadata` stays `None` when composition installs no lookup at all,
    /// matching TS `hasModelMetadata` (`sidecarHostModulePorts.ts:320-322`).
    pub fn with_ports(
        execution: Arc<dyn ModelExecution>,
        metadata: Option<Arc<dyn ModelMetadata>>,
        turn: TurnContext,
    ) -> Self {
        Self {
            execution: Some(execution),
            metadata,
            turn,
            state: Arc::new(Mutex::new(State::default())),
            materializer: None,
        }
    }

    pub(super) fn preparation(&self, id: &JsString) -> Option<PreparedInvocation> {
        self.state.lock().unwrap().preparation(id)
    }

    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        // Clone the turn-owned ports and state so the future is `'static`.
        let execution_port = self.execution.clone();
        let metadata_port = self.metadata.clone();
        let turn = self.turn.clone();
        let turn_abort = turn.abort.clone();
        let state = self.state.clone();
        let materializer = self.materializer.clone();
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?.clone();
            if operation.equals_str("get_metadata") {
                // The fixed dispatcher checks metadata availability before
                // reading provider/model (`dispatcher:69-77`).
                let Some(metadata) = metadata_port.as_ref() else {
                    return Err(
                        ModuleError::new("Host model metadata capability is unavailable.").into(),
                    );
                };
                let provider = wire::string_field(payload, "provider")?.clone();
                let model = wire::string_field(payload, "model")?.clone();
                return get_metadata(metadata.as_ref(), &provider, &model);
            }
            let preparation_id = wire::string_field(payload, "preparationId")?.clone();
            let context = execution_context(&turn, &call, payload.and_then(|p| p.get("context")));

            if operation.equals_str("prepare") {
                let request = canonical_request(payload.and_then(|p| p.get("request")))?;
                let execution = execution_of(&execution_port)?;
                let prepared = execution.prepare(PrepareInput { request, context }).await?;
                // The fixed dispatcher stores the preparation before building
                // the response, so a metadata error does not lose it (:84-90).
                state
                    .lock()
                    .expect("model state")
                    .insert_preparation(preparation_id, prepared.clone());
                let mut response = JsObject::new();
                response.insert("prepared".into(), serialize_prepared(&prepared));
                if let Some(metadata) = &metadata_port {
                    let metadata =
                        serialize_metadata(metadata.as_ref(), &prepared.provider, &prepared.model)?;
                    response.insert("metadata".into(), JsValue::Object(metadata));
                }
                return Ok(response);
            }

            if operation.equals_str("materialize_prepared_request") {
                let prepared = state
                    .lock()
                    .expect("model state")
                    .preparation(&preparation_id)
                    .ok_or_else(|| unknown_preparation(&preparation_id))?;
                let candidate = canonical_request(payload.and_then(|p| p.get("request")))?;
                // With no injected materializer, preserve the fixed dispatcher
                // fallback: prepared routing plus candidate messages (:96-104).
                let request = if let Some(materializer) = &materializer {
                    let candidate = super::budget::canonical_request(Some(&candidate))?;
                    let result = materializer.materialize(prepared, candidate).await?;
                    super::budget::canonical_request(Some(&result))?
                } else {
                    materialize_fallback(&prepared, &candidate)
                };
                let request = canonical_request(Some(&JsValue::Object(request)))?;
                let mut response = JsObject::new();
                response.insert("request".into(), request);
                return Ok(response);
            }

            if operation.equals_str("stream") {
                let prepared = state
                    .lock()
                    .expect("model state")
                    .preparation(&preparation_id)
                    .ok_or_else(|| unknown_preparation(&preparation_id))?;
                let request = payload.and_then(|p| p.get("request"));
                let prepared = current_prepared(prepared, request, false)?;
                let execution = execution_of(&execution_port)?;
                let mut stream = execution
                    .stream(StreamInput {
                        prepared,
                        context,
                        abort: turn.abort.clone(),
                    })
                    .await?;
                let mut events = Vec::new();
                while let Some(event) = stream.next().await? {
                    events.push(event);
                }
                // `for await` does not call return() after a normal done or a
                // rejected next(); dropping owns only the local iterator.
                drop(stream);
                let mut response = JsObject::new();
                response.insert("events".into(), JsValue::Array(events));
                return Ok(response);
            }

            if operation.equals_str("stream_next") {
                let entry = {
                    let mut state = state.lock().expect("model state");
                    if let Some(entry) = state.stream(&preparation_id) {
                        entry
                    } else {
                        let prepared = state
                            .preparation(&preparation_id)
                            .ok_or_else(|| unknown_preparation(&preparation_id))?;
                        let request = payload.and_then(|p| p.get("request"));
                        let prepared = current_prepared(prepared, request, true)?;
                        let execution = execution_of(&execution_port)?;
                        // Register before the first await (and before polling
                        // the provider), so contenders share one creation.
                        let entry = Arc::new(StreamEntry {
                            phase: tokio::sync::Mutex::new(StreamPhase::Creating(Box::pin(
                                async move {
                                    execution
                                        .stream(StreamInput {
                                            prepared,
                                            context,
                                            abort: turn_abort,
                                        })
                                        .await
                                },
                            ))),
                        });
                        state.streams.push((preparation_id.clone(), entry.clone()));
                        entry
                    }
                };
                let mut phase = entry.phase.lock().await;
                if let Err(error) = phase.initialize().await {
                    // Raw TS stream creation happens outside its next() try.
                    // Sticky creation is lazy and instead fails inside next().
                    state.lock().expect("model state").remove_stream(
                        &preparation_id,
                        &entry,
                        false,
                    );
                    return Err(error.into());
                }
                // Lookup accepted this next before close removed the entry.
                // Keep its place ahead of return(), including while creation
                // awaits; close errors belong to the queued close callback.
                let next = phase.next().await;
                if !matches!(next, Ok(Some(_))) {
                    state
                        .lock()
                        .expect("model state")
                        .remove_stream(&preparation_id, &entry, true);
                }
                // Already accepted calls retain terminal iterator semantics;
                // only a new lookup after removal reports unknown preparation.
                let event = next?;
                let mut response = JsObject::new();
                let done = event.is_none();
                response.insert("events".into(), JsValue::Array(event.into_iter().collect()));
                response.insert("done".into(), JsValue::Bool(done));
                Ok(response)
            } else if operation.equals_str("close_stream") {
                let entry = {
                    let mut state = state.lock().expect("model state");
                    state.preparations.retain(|(key, _)| key != &preparation_id);
                    let entry = state
                        .streams
                        .iter()
                        .position(|(key, _)| key == &preparation_id)
                        .map(|index| state.streams.remove(index).1);
                    if let Some(entry) = &entry {
                        state.closing.push(entry.clone());
                    }
                    entry
                };
                if let Some(entry) = entry {
                    let result = entry.phase.lock().await.close().await;
                    state.lock().expect("model state").finish_close(&entry);
                    result?;
                }
                let mut response = JsObject::new();
                response.insert("closed".into(), JsValue::Bool(true));
                Ok(response)
            } else {
                Err(wire::named_error(
                    "Unsupported sidecar model operation: ",
                    &operation,
                    "",
                ))
            }
        })
    }

    pub fn dispose(&self) -> HostFuture<()> {
        let streams = {
            let mut state = self.state.lock().expect("model state");
            state.preparations.clear();
            let mut streams = std::mem::take(&mut state.closing);
            streams.extend(
                std::mem::take(&mut state.streams)
                    .into_iter()
                    .map(|(_, entry)| entry),
            );
            streams
        };
        Box::pin(async move {
            let mut closes: Vec<Option<ModelFuture<()>>> = streams
                .into_iter()
                .map(|entry| {
                    Some(
                        Box::pin(async move { entry.phase.lock().await.close().await })
                            as ModelFuture<()>,
                    )
                })
                .collect();
            // Like Promise.allSettled: poll every close even if an earlier
            // iterator waits forever. These futures remain scoped to dispose.
            poll_fn(|cx| {
                let mut pending = false;
                for close in &mut closes {
                    if let Some(future) = close {
                        if future.as_mut().poll(cx).is_ready() {
                            *close = None;
                        } else {
                            pending = true;
                        }
                    }
                }
                if pending {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
            Ok(())
        })
    }
}

fn execution_of(
    port: &Option<Arc<dyn ModelExecution>>,
) -> Result<Arc<dyn ModelExecution>, HostError> {
    port.clone()
        .ok_or_else(|| ModuleError::new("Host model execution capability is unavailable.").into())
}

fn get_metadata(
    metadata: &dyn ModelMetadata,
    provider: &JsString,
    model: &JsString,
) -> Result<JsObject, HostError> {
    let serialized = serialize_metadata(
        metadata,
        &JsValue::String(provider.clone()),
        &JsValue::String(model.clone()),
    )?;
    let mut response = JsObject::new();
    response.insert("metadata".into(), JsValue::Object(serialized));
    Ok(response)
}

fn execution_context(turn: &TurnContext, call: &JsValue, remote: Option<&JsValue>) -> JsObject {
    let remote = remote.and_then(JsValue::as_object);
    let mut context = JsObject::new();
    context.insert("sessionId".into(), JsValue::String(turn.session_id.clone()));
    context.insert("turnId".into(), JsValue::String(turn.turn_id.clone()));
    for field in ["runId", "operationId"] {
        context.insert(
            field.into(),
            call.get(field).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    if let Some(key) = call.get("idempotencyKey").filter(|value| js_truthy(value)) {
        context.insert("idempotencyKey".into(), key.clone());
    }
    if let Some(deadline) = turn
        .operation_deadline
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        context.insert(
            "operationDeadline".into(),
            JsValue::String(deadline.clone()),
        );
    }
    if let Some(workspace) = turn.workspace_id.as_ref().filter(|value| !value.is_empty()) {
        context.insert("workspaceId".into(), JsValue::String(workspace.clone()));
    }
    if let Some(version) = turn
        .storage_config_version
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        context.insert(
            "storageConfigVersion".into(),
            JsValue::String(version.clone()),
        );
    }
    if let Some(remote) = remote {
        // `sidecarDefaultModuleDispatcher.ts:441,473`: the wire entry sanitizes
        // `modelOverride` — `asRecord` plus both `provider`/`model` strings,
        // only those two fields copied — and drops it otherwise, so a
        // non-conforming override falls back to router routing here. The
        // truthiness semantics of fix round 4 apply one layer down
        // (`model_port.rs`), where the context is already in-process.
        if let Some(over) = remote.get("modelOverride").and_then(JsValue::as_object)
            && let (Some(provider), Some(model)) = (
                over.get("provider").and_then(JsValue::as_string),
                over.get("model").and_then(JsValue::as_string),
            )
        {
            let mut object = JsObject::new();
            object.insert("provider".into(), JsValue::String(provider.clone()));
            object.insert("model".into(), JsValue::String(model.clone()));
            context.insert("modelOverride".into(), JsValue::Object(object));
        }
        if let Some(metadata) = remote.get("metadata").and_then(JsValue::as_object) {
            context.insert("metadata".into(), JsValue::Object(metadata.clone()));
        }
    }
    context
}

/// Wrap an execution port with the turn's sticky routing metadata
/// (`sidecarHostModulePorts.ts:252-314`). The previous tier is consumed by the
/// first stream that completes, errors, or is closed; `orchestrating` sticky
/// state keeps it for the next turn. Provider/model hints survive the turn.
pub fn with_sticky(
    execution: Arc<dyn ModelExecution>,
    route: Option<StickyRoute>,
) -> Arc<dyn ModelExecution> {
    Arc::new(StickyExecution {
        previous_tier: Arc::new(Mutex::new(
            route.as_ref().and_then(|route| route.previous_tier.clone()),
        )),
        inner: execution,
        route,
    })
}

struct StickyExecution {
    inner: Arc<dyn ModelExecution>,
    route: Option<StickyRoute>,
    previous_tier: Arc<Mutex<Option<JsString>>>,
}
impl StickyExecution {
    fn route_metadata(&self) -> Option<JsObject> {
        let route = self.route.as_ref()?;
        let tier = self.previous_tier.lock().expect("sticky tier");
        Some(route.metadata((*tier).as_ref()))
    }
    fn orchestrating(&self) -> bool {
        self.route.as_ref().is_some_and(|route| route.orchestrating)
    }
}
impl ModelExecution for StickyExecution {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        let inner = self.inner.clone();
        let route = self.route_metadata();
        let mut context = input.context;
        if let Some(route) = route {
            let mut metadata = context
                .get("metadata")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in route.entries() {
                metadata.insert(key.clone(), value.clone());
            }
            context.insert("metadata".into(), JsValue::Object(metadata));
        }
        let request = input.request;
        Box::pin(async move { inner.prepare(PrepareInput { request, context }).await })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        let inner = self.inner.clone();
        let previous_tier = self.previous_tier.clone();
        let orchestrating = self.orchestrating();
        Box::pin(async move {
            Ok(Box::new(StickyStream {
                phase: StreamPhase::Creating(Box::pin(async move { inner.stream(input).await })),
                started: false,
                previous_tier,
                orchestrating,
            }) as Box<dyn ModelStream>)
        })
    }
}

struct StickyStream {
    phase: StreamPhase,
    started: bool,
    previous_tier: Arc<Mutex<Option<JsString>>>,
    orchestrating: bool,
}
impl StickyStream {
    fn clear_tier(&self) {
        if self.started && !self.orchestrating {
            *self.previous_tier.lock().expect("sticky tier") = None;
        }
    }
}
impl ModelStream for StickyStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            if !self.started && matches!(self.phase, StreamPhase::Done) {
                return Ok(None);
            }
            self.started = true;
            let next = self.phase.next().await;
            if !matches!(next, Ok(Some(_))) {
                self.phase = StreamPhase::Done;
                self.clear_tier();
            }
            next
        })
    }
    fn close(&mut self) -> StreamFuture<'_, ()> {
        Box::pin(async move {
            // Returning an unstarted TS async generator never enters its body
            // (including finally), and must not create the underlying stream.
            if !self.started {
                self.phase = StreamPhase::Done;
                return Ok(());
            }
            let result = self.phase.close().await;
            self.clear_tier();
            result
        })
    }
}
impl Drop for StickyStream {
    fn drop(&mut self) {
        self.clear_tier();
    }
}

fn canonical_request(value: Option<&JsValue>) -> Result<JsValue, HostError> {
    let valid = value.and_then(JsValue::as_object).is_some_and(|request| {
        request
            .get("provider")
            .and_then(JsValue::as_string)
            .is_some()
            && request.get("model").and_then(JsValue::as_string).is_some()
            && matches!(request.get("messages"), Some(JsValue::Array(_)))
    });
    if !valid {
        return Err(
            ModuleError::new("Sidecar model call contains an invalid canonical request.").into(),
        );
    }
    Ok(value.expect("validated request").clone())
}

fn current_prepared(
    prepared: PreparedInvocation,
    request: Option<&JsValue>,
    required: bool,
) -> Result<PreparedInvocation, HostError> {
    if request.is_none_or(JsValue::is_undefined) {
        if required {
            return canonical_request(request).map(|request| PreparedInvocation {
                request,
                ..prepared
            });
        }
        return Ok(prepared);
    }
    let request = canonical_request(request)?;
    Ok(PreparedInvocation {
        request,
        ..prepared
    })
}

fn materialize_fallback(prepared: &PreparedInvocation, candidate: &JsValue) -> JsObject {
    let candidate = candidate.as_object().expect("canonical request");
    // JS object spread copies enumerable own keys: strings contribute UTF-16
    // code-unit indices, arrays contribute element indices (not length), and
    // null/undefined/booleans/numbers contribute nothing. Later fields win.
    let mut request = JsObject::new();
    match &prepared.request {
        JsValue::Object(object) => {
            for (key, value) in object.entries() {
                request.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                request.insert(index.to_string().into(), value.clone());
            }
        }
        JsValue::String(value) => {
            for (index, unit) in value.units().iter().enumerate() {
                request.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    let messages = candidate
        .get("messages")
        .cloned()
        .expect("canonical request messages");
    request.insert("messages".into(), messages);
    request.insert("provider".into(), prepared.provider.clone());
    request.insert("model".into(), prepared.model.clone());
    request
}

pub(crate) fn serialize_prepared(prepared: &PreparedInvocation) -> JsValue {
    let mut object = JsObject::new();
    object.insert("request".into(), prepared.request.clone());
    object.insert("provider".into(), prepared.provider.clone());
    object.insert("model".into(), prepared.model.clone());
    if let Some(value) = prepared
        .max_context_tokens
        .as_ref()
        .filter(|value| js_truthy(value))
    {
        object.insert("maxContextTokens".into(), value.clone());
    }
    if let Some(value) = prepared
        .max_output_tokens
        .as_ref()
        .filter(|value| js_truthy(value))
    {
        object.insert("maxOutputTokens".into(), value.clone());
    }
    JsValue::Object(object)
}

fn serialize_metadata(
    metadata: &dyn ModelMetadata,
    provider: &JsValue,
    model: &JsValue,
) -> Result<JsObject, ModuleError> {
    // `dispatcher:88` passes `prepared.provider` / `prepared.model` through
    // as-is, so an override route can be `undefined` — or any raw value —
    // here. `serializeModelMetadata` (`:445-450`) hands that raw route to
    // every defined lookup and only then validates the answers (`:451-470`);
    // there is no pre-conversion and no lookup is skipped when the route's
    // key conversion would throw. A route that cannot convert reaches the
    // lookups, and each implementation answers (or throws) exactly like its
    // TS counterpart: the baseline's catch-wrapped lookups
    // (`ProjectSessionRuntimeBundle.ts:476-497`) answer `undefined`, while
    // `getModelProtocol` (`:490`, no catch) lets the config index's coercion
    // `TypeError` reject the call.
    // TS calls every lookup before validating any result (:446-453).
    let max_context = if metadata.provides(MetadataField::MaxContextTokens) {
        defined(metadata.get_model_max_context_tokens(provider, model)?)
    } else {
        None
    };
    let max_output = if metadata.provides(MetadataField::MaxOutputTokens) {
        defined(metadata.get_model_max_output_tokens(provider, model)?)
    } else {
        None
    };
    let token_limits = if metadata.provides(MetadataField::TokenLimits) {
        defined(metadata.get_model_token_limits(provider, model)?).filter(js_truthy)
    } else {
        None
    };
    let protocol = if metadata.provides(MetadataField::Protocol) {
        defined(metadata.get_model_protocol(provider)?)
    } else {
        None
    };
    let prompt_cache = if metadata.provides(MetadataField::SupportsPromptCache) {
        defined(metadata.get_model_supports_prompt_cache(provider, model)?)
    } else {
        None
    };

    validate_metadata_number(max_context.as_ref(), "maxContextTokens")?;
    validate_metadata_number(max_output.as_ref(), "maxOutputTokens")?;
    if let Some(limits) = token_limits.as_ref().and_then(JsValue::as_object) {
        validate_metadata_number(
            limits.get("maxContextTokens"),
            "tokenLimits.maxContextTokens",
        )?;
        validate_metadata_number(limits.get("maxOutputTokens"), "tokenLimits.maxOutputTokens")?;
    }
    if let Some(protocol) = protocol.as_ref() {
        let valid = protocol.as_string().is_some_and(|protocol| {
            ["anthropic", "openai", "openai-responses", "google"]
                .iter()
                .any(|known| protocol.equals_str(known))
        });
        if !valid {
            return Err(ModuleError::new(
                "Host model metadata returned an invalid protocol.",
            ));
        }
    }

    let mut object = JsObject::new();
    object.insert("provider".into(), provider.clone());
    object.insert("model".into(), model.clone());
    if let Some(value) = max_context {
        object.insert("maxContextTokens".into(), value);
    }
    if let Some(value) = max_output {
        object.insert("maxOutputTokens".into(), value);
    }
    if let Some(value) = token_limits {
        object.insert("tokenLimits".into(), value);
    }
    if let Some(value) = protocol {
        object.insert("protocol".into(), value);
    }
    if let Some(value) = prompt_cache {
        object.insert("supportsPromptCache".into(), value);
    }
    Ok(object)
}

fn validate_metadata_number(value: Option<&JsValue>, field: &str) -> Result<(), ModuleError> {
    match value {
        None | Some(JsValue::Undefined) => Ok(()),
        Some(JsValue::Number(value))
            if value.is_finite()
                && value.fract() == 0.0
                && *value > 0.0
                && *value <= 9_007_199_254_740_991.0 =>
        {
            Ok(())
        }
        Some(_) => Err(ModuleError::new(format!(
            "Host model metadata returned an invalid {field}."
        ))),
    }
}

fn defined(value: Option<JsValue>) -> Option<JsValue> {
    value.filter(|value| !value.is_undefined())
}

pub(super) fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn unknown_preparation(id: &JsString) -> HostError {
    wire::named_error("Unknown sidecar model preparation: ", id, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_rejects_non_safe_numbers() {
        for value in [
            JsValue::Number(0.0),
            JsValue::Number(-1.0),
            JsValue::Number(1.5),
            JsValue::Number(f64::NAN),
            JsValue::Number(f64::INFINITY),
            JsValue::Number(9_007_199_254_740_992.0),
            JsValue::String("1".into()),
        ] {
            assert!(validate_metadata_number(Some(&value), "x").is_err());
        }
        assert!(validate_metadata_number(Some(&JsValue::Number(1.0)), "x").is_ok());
        assert!(validate_metadata_number(None, "x").is_ok());
    }

    #[test]
    fn js_truthiness_matches_serializable_prepared_fields() {
        assert!(!js_truthy(&JsValue::Number(0.0)));
        assert!(!js_truthy(&JsValue::Number(f64::NAN)));
        assert!(!js_truthy(&JsValue::String("".into())));
        assert!(js_truthy(&JsValue::Number(-1.0)));
        assert!(js_truthy(&JsValue::Array(Vec::new())));
        assert!(js_truthy(&JsValue::Object(JsObject::new())));
    }
}
