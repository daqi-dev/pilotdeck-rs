//! Context-facing EdgeClaw provider (`EdgeClawMemoryProvider.ts`, cc7c4e6).
//!
//! A retrieval owns its continuation: dropping the caller's wait (builder
//! timeout / parent abort) does not cancel the service or its pending write.
//! Pending is keyed by session and overwritten in completion order. The
//! deliberately preserved late-result behavior is covered by the M20 oracle.
use super::{
    file_memory::FileMemoryClock,
    retriever::RetrievalOptions,
    service::{CaptureTurnInput, EdgeClawMemoryService, ServiceError},
    text,
};
use crate::runtime::context::compaction::js_truthy;
use crate::{
    host::Cancellation,
    runtime::{
        context::memory::{
            CanonicalMessagesToMemoryMessagesOptions, ContextMemoryMessage, MemoryCaptureFuture,
            MemoryCaptureTurnInput, MemoryDiagnostic, MemoryFuture, MemoryResolver,
            MemoryRetrieveInput, MemoryRetrieveResult, canonical_messages_to_memory_messages,
        },
        memory::{MemoryProviderError, js_trim_units, maintenance::MaintenanceTelemetry},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    thread::ThreadId,
};
use tokio::runtime::Handle;

pub struct RetrieveContextInput {
    pub query: JsString,
    pub recent_messages: Vec<ContextMemoryMessage>,
    pub workspace_hint: JsString,
    pub retrieval_mode: JsString,
    pub signal: Cancellation,
}
pub type ServiceRetrieveFuture =
    Pin<Box<dyn Future<Output = Result<JsObject, MemoryProviderError>> + Send + 'static>>;
pub type ProviderRetrieveFuture = Pin<Box<dyn Future<Output = JsValue> + Send + 'static>>;

/// The TS service-like contract. Capture and trace persistence are synchronous;
/// implementations must return errors rather than panic for service failures.
pub trait EdgeClawMemoryServiceLike: Send + Sync {
    fn retrieve_context(self: Arc<Self>, input: RetrieveContextInput) -> ServiceRetrieveFuture;
    fn capture_turn(
        &self,
        messages: &[ContextMemoryMessage],
        input: CaptureTurnInput,
    ) -> Result<(), MemoryProviderError>;
    fn supports_case_trace(&self) -> bool {
        false
    }
    fn save_case_trace(&self, _record: &JsObject) -> Result<(), MemoryProviderError> {
        Ok(())
    }
}

impl EdgeClawMemoryServiceLike for EdgeClawMemoryService {
    fn retrieve_context(self: Arc<Self>, input: RetrieveContextInput) -> ServiceRetrieveFuture {
        Box::pin(async move {
            // The fixed TS service does not consume the forwarded signal.
            // Its extractor owns a separate timeout / abort controller.
            let options = RetrievalOptions {
                retrieval_mode: Some(input.retrieval_mode),
                recent_messages: input
                    .recent_messages
                    .iter()
                    .map(|m| {
                        let mut out = JsObject::new();
                        out.insert("msgId".into(), JsValue::String(m.msg_id.clone()));
                        out.insert("role".into(), JsValue::String(m.role.clone()));
                        out.insert("content".into(), JsValue::String(m.content.clone()));
                        out
                    })
                    .collect(),
                workspace_hint: input.workspace_hint,
            };
            EdgeClawMemoryService::retrieve_context(&self, &input.query, &options)
                .await
                .map_err(service_error)
        })
    }
    fn capture_turn(
        &self,
        messages: &[ContextMemoryMessage],
        input: CaptureTurnInput,
    ) -> Result<(), MemoryProviderError> {
        EdgeClawMemoryService::capture_turn(
            self,
            &messages
                .iter()
                .map(ContextMemoryMessage::to_js)
                .collect::<Vec<_>>(),
            input,
        )
        .map(|_| ())
        .map_err(service_error)
    }
    fn supports_case_trace(&self) -> bool {
        true
    }
    fn save_case_trace(&self, record: &JsObject) -> Result<(), MemoryProviderError> {
        EdgeClawMemoryService::save_case_trace(self, record).map_err(service_error)
    }
}
fn service_error(error: ServiceError) -> MemoryProviderError {
    error.into()
}

pub struct EdgeClawMemoryProviderOptions {
    pub service: Arc<dyn EdgeClawMemoryServiceLike>,
    pub retrieval_mode: Option<JsString>,
    pub source: Option<String>,
    pub now: Option<FileMemoryClock>,
    pub telemetry: Option<Arc<dyn MaintenanceTelemetry>>,
}
impl EdgeClawMemoryProviderOptions {
    pub fn new(service: Arc<dyn EdgeClawMemoryServiceLike>) -> Self {
        Self {
            service,
            retrieval_mode: None,
            source: None,
            now: None,
            telemetry: None,
        }
    }
}
#[derive(Clone)]
struct PendingRetrieval {
    query: String,
    started_at: String,
    result: JsObject,
}
struct Inner {
    options: EdgeClawMemoryProviderOptions,
    now: FileMemoryClock,
    pending: Mutex<HashMap<String, PendingRetrieval>>,
    turn: SyncTurn,
}

/// Preserve JS synchronous slices on a multithreaded executor. The state
/// mutex is never held over user callbacks; the lease allows same-thread
/// reentrancy, and its !Send guard cannot cross an await or change threads.
#[derive(Default)]
struct SyncTurn {
    owner: Mutex<Option<(ThreadId, usize)>>,
    released: Condvar,
    #[cfg(test)]
    contended: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}
struct SyncTurnGuard<'a> {
    turn: &'a SyncTurn,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl SyncTurn {
    fn enter(&self) -> SyncTurnGuard<'_> {
        let thread = std::thread::current().id();
        let mut owner = self.owner.lock().expect("provider turn mutex poisoned");
        loop {
            match owner.as_mut() {
                None => {
                    *owner = Some((thread, 1));
                    break;
                }
                Some((current, depth))
                    if *current == thread && !crate::mutants::on("provider-nonreentrant") =>
                {
                    *depth += 1;
                    break;
                }
                _ => {
                    #[cfg(test)]
                    if let Some(sender) = self
                        .contended
                        .lock()
                        .expect("#777 contention observer lock")
                        .take()
                    {
                        let _ = sender.send(());
                    }
                    owner = self
                        .released
                        .wait(owner)
                        .expect("provider turn mutex poisoned");
                }
            }
        }
        SyncTurnGuard {
            turn: self,
            _thread: std::marker::PhantomData,
        }
    }
}
impl Drop for SyncTurnGuard<'_> {
    fn drop(&mut self) {
        let mut owner = self
            .turn
            .owner
            .lock()
            .expect("provider turn mutex poisoned");
        let (_, depth) = owner.as_mut().expect("provider synchronous turn owner");
        *depth -= 1;
        if *depth == 0 {
            *owner = None;
            self.turn.released.notify_all();
        }
    }
}
/// Disposal belongs to the owning project bundle (service.close), not this
/// provider. Retained providers and in-flight calls keep their TS behavior.
#[derive(Clone)]
pub struct EdgeClawMemoryProvider {
    inner: Arc<Inner>,
    executor: Handle,
}
impl EdgeClawMemoryProvider {
    /// Constructor options before retrieval's runtime defaults are applied.
    #[doc(hidden)]
    pub fn factory_configuration(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert(
            "source".into(),
            self.inner
                .options
                .source
                .as_ref()
                .map(|s| JsValue::from(s.as_str()))
                .unwrap_or(JsValue::Undefined),
        );
        out.insert(
            "retrievalMode".into(),
            self.inner
                .options
                .retrieval_mode
                .as_ref()
                .map(|s| JsValue::String(s.clone()))
                .unwrap_or(JsValue::Undefined),
        );
        JsValue::Object(out)
    }

    pub fn new(options: EdgeClawMemoryProviderOptions, executor: Handle) -> Self {
        let now = options
            .now
            .clone()
            .unwrap_or_else(|| Arc::new(text::now_iso));
        Self {
            inner: Arc::new(Inner {
                options,
                now,
                pending: Mutex::new(HashMap::new()),
                turn: SyncTurn::default(),
            }),
            executor,
        }
    }

    /// Full TS result including metadata. The existing context resolver only
    /// consumes context and diagnostics; its adapter below projects those keys.
    pub fn retrieve(&self, input: MemoryRetrieveInput) -> ProviderRetrieveFuture {
        let _turn = self.inner.turn.enter();
        let started_at = (self.inner.now)();
        self.inner
            .stage(&input.session_id, "retrieve", "loop_start", "success", None);
        let normalized =
            canonical_messages_to_memory_messages(&input.recent_messages, Default::default());
        let service_future = normalized.map(|recent_messages| {
            self.inner
                .options
                .service
                .clone()
                .retrieve_context(RetrieveContextInput {
                    query: input.query.clone().into(),
                    recent_messages,
                    workspace_hint: if crate::mutants::on("provider-workspace") {
                        "".into()
                    } else {
                        input.project_root.into()
                    },
                    retrieval_mode: self
                        .inner
                        .options
                        .retrieval_mode
                        .clone()
                        .unwrap_or_else(|| "auto".into()),
                    signal: if crate::mutants::on("provider-signal") {
                        Cancellation::default()
                    } else {
                        input.signal.clone()
                    },
                })
        });
        // JS async functions execute through their first await before returning.
        // Poll only the service prefix here; the provider's await continuation
        // still runs on the executor. A later first poll installs its own waker.
        let service_future = service_future.map(|mut future| {
            if !crate::mutants::on("provider-lazy-service") {
                let _runtime = self.executor.enter();
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                if let std::task::Poll::Ready(result) = future.as_mut().poll(&mut context) {
                    return Box::pin(std::future::ready(result)) as ServiceRetrieveFuture;
                }
            }
            future
        });
        let inner = self.inner.clone();
        // Spawn before handing back the waiter, just as invoking the TS async
        // function starts it immediately. A dropped JoinHandle detaches it.
        let work = async move {
            let retrieved = match service_future {
                Ok(future) => future.await,
                Err(error) => Err(MemoryProviderError::new(error)),
            };
            let _turn = inner.turn.enter();
            let result = match retrieved {
                Ok(result) => {
                    if !(crate::mutants::on("provider-drop-late") && input.signal.is_cancelled()) {
                        let mut pending = inner
                            .pending
                            .lock()
                            .expect("provider pending mutex poisoned");
                        if !crate::mutants::on("provider-first-write")
                            || !pending.contains_key(&input.session_id)
                        {
                            pending.insert(
                                input.session_id.clone(),
                                PendingRetrieval {
                                    query: input.query,
                                    started_at,
                                    result: result.clone(),
                                },
                            );
                        }
                    }
                    context_preview(&result, false).map(|context| {
                        let injected = !context.is_empty();
                        inner.stage(
                            &input.session_id,
                            "retrieve",
                            "loop_end",
                            "success",
                            Some(injected),
                        );
                        let mut out = JsObject::new();
                        if injected {
                            out.insert("systemContext".into(), JsValue::String(context));
                        }
                        let diagnostics =
                            if injected || crate::mutants::on("provider-empty-diagnostic") {
                                vec![]
                            } else {
                                vec![diagnostic(
                                    "memory_context_empty",
                                    "info",
                                    "EdgeClaw memory returned no relevant context.".into(),
                                )]
                            };
                        out.insert("diagnostics".into(), JsValue::Array(diagnostics));
                        out.insert(
                            "metadata".into(),
                            object([
                                ("trace", field(&result, "trace")),
                                ("debug", field(&result, "debug")),
                            ]),
                        );
                        JsValue::Object(out)
                    })
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(result) => result,
                Err(error) => {
                    if crate::mutants::on("provider-reject-clears") {
                        inner
                            .pending
                            .lock()
                            .expect("provider pending mutex poisoned")
                            .remove(&input.session_id);
                    }
                    if let Some(telemetry) = &inner.options.telemetry {
                        let mut event = event(&input.session_id, "retrieve", "loop_end");
                        event.insert("errorCategory".into(), "loop_error".into());
                        event.insert("sessionId".into(), input.session_id.as_str().into());
                        telemetry.track_error(&error, JsValue::Object(event));
                    }
                    error_result(error.message)
                }
            }
        };
        if crate::mutants::on("provider-owned-continuation") {
            return Box::pin(work);
        }
        let task = self.executor.spawn(work);
        Box::pin(async move {
            task.await.unwrap_or_else(|error| {
                error_result(format!("EdgeClaw retrieval task failed: {error}").into())
            })
        })
    }

    pub fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        let _turn = (!crate::mutants::on("provider-sync-capture")).then(|| self.inner.turn.enter());
        // TS executes the entire capture body before its async return, without
        // yielding. Release the pending mutex before invoking user callbacks.
        let outcome = (|| {
            let messages = canonical_messages_to_memory_messages(
                &input.messages,
                CanonicalMessagesToMemoryMessagesOptions {
                    include_fork_carryover: Some(crate::mutants::on("provider-capture-fork")),
                },
            )?;
            self.inner
                .stage(&input.session_id, "capture", "loop_start", "success", None);
            let captured = self.inner.options.service.capture_turn(
                &messages,
                CaptureTurnInput {
                    session_key: input.session_id.clone(),
                    timestamp: Some((self.inner.now)()),
                    source: Some(
                        self.inner
                            .options
                            .source
                            .clone()
                            .unwrap_or_else(|| "pilotdeck".into()),
                    ),
                },
            );
            self.inner.stage(
                &input.session_id,
                "capture",
                "loop_end",
                if captured.is_ok() {
                    "success"
                } else {
                    "failed"
                },
                None,
            );
            if crate::mutants::on("provider-capture-error-skips-trace") && captured.is_err() {
                return Ok(());
            }
            if !self.inner.options.service.supports_case_trace() {
                if crate::mutants::on("provider-missing-save-drops") {
                    self.inner
                        .pending
                        .lock()
                        .expect("provider pending mutex poisoned")
                        .remove(&input.session_id);
                }
                return Ok(());
            }
            let pending = {
                let mut pending = self
                    .inner
                    .pending
                    .lock()
                    .expect("provider pending mutex poisoned");
                if crate::mutants::on("provider-keep-pending") {
                    pending.get(&input.session_id).cloned()
                } else {
                    pending.remove(&input.session_id)
                }
            };
            let Some(pending) = pending else {
                return Ok(());
            };
            let preview = context_preview(&pending.result, true);
            let context = if crate::mutants::on("provider-capture-preview-swallow") {
                preview.unwrap_or_default()
            } else {
                preview.map_err(|e| String::from_utf16_lossy(e.message.units()))?
            };
            let debug = field(&pending.result, "debug");
            let intent = [
                field(&pending.result, "intent"),
                debug.get("route").cloned().unwrap_or(JsValue::Undefined),
                debug.get("mode").cloned().unwrap_or(JsValue::Undefined),
            ]
            .into_iter()
            .find(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .unwrap_or_else(|| "none".into());
            let reply = messages
                .iter()
                .rev()
                .find(|m| {
                    m.role.equals_str("assistant") && !js_trim_units(m.content.units()).is_empty()
                })
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let errored = js_truthy(&input.errored);
            let errored = if crate::mutants::on("provider-errored-status") {
                !errored
            } else {
                errored
            };
            let trace = field(&pending.result, "trace");
            let record = object([
                ("sessionKey", JsValue::String(input.session_id.into())),
                ("query", JsValue::String(pending.query.into())),
                ("startedAt", JsValue::String(pending.started_at.into())),
                ("finishedAt", JsValue::String((self.inner.now)().into())),
                ("status", if errored { "error" } else { "completed" }.into()),
                (
                    "retrieval",
                    object([
                        ("intent", intent),
                        ("injected", JsValue::Bool(!context.is_empty())),
                        ("contextPreview", JsValue::String(context)),
                        (
                            "trace",
                            if matches!(trace, JsValue::Null | JsValue::Undefined) {
                                JsValue::Null
                            } else {
                                trace
                            },
                        ),
                    ]),
                ),
                ("toolEvents", JsValue::Array(vec![])),
                ("assistantReply", JsValue::String(reply)),
            ]);
            if let JsValue::Object(record) = &record {
                let saved = self.inner.options.service.save_case_trace(record);
                if crate::mutants::on("provider-save-error-propagates") {
                    saved.map_err(|e| String::from_utf16_lossy(e.message.units()))?;
                }
            }
            Ok(())
        })();
        Box::pin(std::future::ready(outcome))
    }
}
impl Inner {
    fn stage(
        &self,
        session: &str,
        phase: &str,
        stage: &str,
        outcome: &str,
        injected: Option<bool>,
    ) {
        if crate::mutants::on("provider-telemetry") {
            return;
        }
        if let Some(telemetry) = &self.options.telemetry {
            let mut event = event(session, phase, stage);
            event.insert("outcome".into(), outcome.into());
            if outcome == "failed" {
                event.insert("errorCategory".into(), "loop_error".into());
            }
            event.insert("sessionId".into(), session.into());
            if let Some(injected) = injected {
                event.insert(
                    "metadata".into(),
                    object([("injected", JsValue::Bool(injected))]),
                );
            }
            telemetry.track_feature_loop_stage(JsValue::Object(event));
        }
    }
}
impl MemoryResolver for EdgeClawMemoryProvider {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        let future = EdgeClawMemoryProvider::retrieve(self, input);
        Box::pin(async move {
            let result = future.await;
            let system_context = result
                .get("systemContext")
                .and_then(JsValue::as_string)
                .map(JsString::to_utf8)
                .transpose()
                .map_err(|_| "Memory context is not valid UTF-8.".to_string())?;
            let diagnostics = match result.get("diagnostics") {
                Some(JsValue::Array(items)) => items.as_slice(),
                _ => &[],
            }
            .iter()
            .map(|d| {
                let text = |key| {
                    d.get(key)
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default()
                };
                MemoryDiagnostic {
                    code: text("code"),
                    severity: text("severity"),
                    message: text("message"),
                }
            })
            .collect();
            Ok(MemoryRetrieveResult {
                system_context,
                diagnostics,
            })
        })
    }
    fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        EdgeClawMemoryProvider::capture_turn(self, input)
    }
}
fn field(object: &JsObject, key: &str) -> JsValue {
    object.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn context_preview(result: &JsObject, pending: bool) -> Result<JsString, MemoryProviderError> {
    let context = [field(result, "systemContext"), field(result, "context")]
        .into_iter()
        .find(|v| {
            !matches!(v, JsValue::Null | JsValue::Undefined)
                && (!crate::mutants::on("provider-empty-fallback") || js_truthy(v))
        })
        .unwrap_or_else(|| "".into());
    let text = context.as_string().ok_or_else(|| {
        let expression = if pending { "pending.result" } else { "result" };
        let mut error = MemoryProviderError::new(format!(
            "({expression}.systemContext ?? {expression}.context ?? \"\").trim is not a function"
        ));
        error.name = "TypeError".into();
        if crate::mutants::on("provider-context-error") {
            error.message = "wrong error".into();
        }
        error
    })?;
    Ok(JsString::from_utf16(js_trim_units(text.units()).to_vec()))
}
fn event(_session: &str, phase: &str, stage: &str) -> JsObject {
    let mut event = JsObject::new();
    for (key, value) in [
        ("module", "memory"),
        ("ownerModule", "memory"),
        ("executionKind", "memory"),
        ("phase", phase),
        ("loopStage", stage),
    ] {
        event.insert(key.into(), value.into());
    }
    event
}
fn object<const N: usize>(pairs: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn diagnostic(code: &str, severity: &str, message: JsString) -> JsValue {
    object([
        ("code", code.into()),
        ("severity", severity.into()),
        ("message", JsValue::String(message)),
    ])
}
fn error_result(message: JsString) -> JsValue {
    object([(
        "diagnostics",
        JsValue::Array(vec![diagnostic("memory_provider_error", "error", message)]),
    )])
}

#[cfg(test)]
#[path = "../../../../tests/edgeclaw_provider.rs"]
mod tests;

#[cfg(test)]
mod sync_turn_tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    use tokio::sync::oneshot;

    struct Service {
        resolve: Mutex<Option<oneshot::Sender<JsObject>>>,
        observed: Mutex<mpsc::Receiver<()>>,
        traces: Mutex<Vec<JsValue>>,
        reenter: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    impl EdgeClawMemoryServiceLike for Service {
        fn retrieve_context(
            self: Arc<Self>,
            _input: RetrieveContextInput,
        ) -> ServiceRetrieveFuture {
            let (send, recv) = oneshot::channel();
            *self.resolve.lock().expect("#777 sync resolve lock") = Some(send);
            Box::pin(async move { Ok(recv.await.expect("#777 sync retrieval release")) })
        }
        fn capture_turn(
            &self,
            _messages: &[ContextMemoryMessage],
            _input: CaptureTurnInput,
        ) -> Result<(), MemoryProviderError> {
            let reenter = self.reenter.lock().expect("#777 reentry lock").take();
            if let Some(reenter) = reenter {
                reenter();
            }
            let resolve = self.resolve.lock().expect("#777 sync resolve lock").take();
            if let Some(resolve) = resolve {
                let mut result = JsObject::new();
                result.insert("context".into(), "context".into());
                resolve
                    .send(result)
                    .expect("#777 sync retrieval remains alive");
                // Correct execution reports contention. The mutant reports
                // completion instead. Both paths terminate on an event.
                self.observed
                    .lock()
                    .expect("#777 sync observation lock")
                    .recv_timeout(Duration::from_secs(20))
                    .expect("#777 sync contention or completion event");
            }
            Ok(())
        }
        fn supports_case_trace(&self) -> bool {
            true
        }
        fn save_case_trace(&self, record: &JsObject) -> Result<(), MemoryProviderError> {
            self.traces
                .lock()
                .expect("#777 sync trace lock")
                .push(JsValue::Object(record.clone()));
            Ok(())
        }
    }
    struct Telemetry(mpsc::Sender<()>);
    impl MaintenanceTelemetry for Telemetry {
        fn track_feature_loop_stage(&self, event: JsValue) {
            if event.get("phase") == Some(&JsValue::from("retrieve"))
                && event.get("loopStage") == Some(&JsValue::from("loop_end"))
            {
                let _ = self.0.send(());
            }
        }
        fn track_error(&self, _error: &MemoryProviderError, _event: JsValue) {
            let _ = self.0.send(());
        }
    }
    fn capture_input() -> MemoryCaptureTurnInput {
        MemoryCaptureTurnInput {
            session_id: "s1".into(),
            project_root: "/workspace".into(),
            messages: vec![],
            errored: JsValue::Bool(false),
        }
    }
    fn setup() -> (EdgeClawMemoryProvider, Arc<Service>) {
        let (send, recv) = mpsc::channel();
        let service = Arc::new(Service {
            resolve: Mutex::new(None),
            observed: Mutex::new(recv),
            traces: Mutex::new(vec![]),
            reenter: Mutex::new(None),
        });
        let mut options = EdgeClawMemoryProviderOptions::new(service.clone());
        options.telemetry = Some(Arc::new(Telemetry(send.clone())));
        let provider = EdgeClawMemoryProvider::new(options, Handle::current());
        *provider
            .inner
            .turn
            .contended
            .lock()
            .expect("#777 install contention observer") = Some(send);
        (provider, service)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_capture_defers_completion() {
        let (provider, service) = setup();
        let retrieval = provider.retrieve(MemoryRetrieveInput {
            query: "q1".into(),
            session_id: "s1".into(),
            project_root: "/workspace".into(),
            recent_messages: vec![],
            signal: Cancellation::default(),
        });
        provider
            .capture_turn(capture_input())
            .await
            .expect("#777 first synchronous capture");
        assert!(
            service
                .traces
                .lock()
                .expect("#777 sync trace lock")
                .is_empty(),
            "#777 capture must finish before retrieval continuation"
        );
        tokio::time::timeout(Duration::from_secs(20), retrieval)
            .await
            .expect("#777 deferred retrieval completes");
        provider
            .capture_turn(capture_input())
            .await
            .expect("#777 next synchronous capture");
        let traces = service.traces.lock().expect("#777 sync trace lock");
        assert_eq!(traces.len(), 1, "#777 next capture persists late retrieval");
        assert_eq!(
            traces[0].get("query"),
            Some(&JsValue::from("q1")),
            "#777 next capture owns q1, as in none-resolve-in-capture TS cell"
        );
    }
    #[tokio::test]
    async fn capture_callback_can_reenter_provider() {
        let (provider, service) = setup();
        let nested = provider.clone();
        *service.reenter.lock().expect("#777 install reentry") = Some(Box::new(move || {
            // The entire capture body executes synchronously before return.
            drop(nested.capture_turn(capture_input()));
        }));
        // A separate thread bounds a regression to a non-reentrant mutex;
        // receiving the actual return is the only success condition.
        let (send, recv) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            drop(provider.capture_turn(capture_input()));
            send.send(()).expect("#777 publish reentrant completion");
        });
        recv.recv_timeout(Duration::from_secs(20))
            .expect("#777 same-thread capture callback can reenter provider");
        thread.join().expect("#777 reentrant thread completed");
    }
}
