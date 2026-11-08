//! Internal to live: shares its exact stream validator and ordered send chain.
use super::*;
use super::super::{loop_control::{SopFuture, SopNodeExecutor, SopNodePlan, SopNodeContext, SopNodeOutcome}, sop_executor::NodeTemplate, interaction::InteractionTurn};
use crate::runtime::session::factory::{FactoryStorage, SopStorage};

pub(super) struct Prepared {
    pub template: Arc<NodeTemplate>,
    pub parent: Arc<InteractionTurn>,
}
pub(super) struct NodeScope {
    context: SopNodeContext, identity: ExecutionIdentity,
    runner: TurnRunner, storage: SopStorage, last: SopNodeOutcome,
}
pub(super) struct Executor {
    pub node: tokio::sync::Mutex<Option<NodeScope>>,
    pub prepared: Prepared,
    pub stream: Arc<StreamHandler>,
    pub recorder: Arc<Recorder>,
    pub lease: Arc<RuntimeLease>,
    pub request: ExecuteRequest,
    pub input: AgentInput,
    pub signal: Cancellation,
    pub close_timeout: Duration,
}
impl Executor {
    async fn publish(&self, mut event: JsValue, wire: bool, module: Option<JsString>) -> Result<(), HostError> {
        let parent = &self.request.identity;
        if let JsValue::Object(fields) = &mut event {
            fields.insert("sessionId".into(), JsValue::String(parent.session_id.clone()));
            fields.insert("turnId".into(), JsValue::String(parent.turn_id.clone()));
        }
        let mut delivery = self.stream.forwarding.lock().unwrap().reserve();
        delivery.wait().await;
        if let Some(module) = &module {
            *self.stream.integrity.module_id.lock().unwrap() = Some(module.clone());
        } else { self.stream.integrity.module_id.lock().unwrap().take(); }
        let timeline = self.stream.forwarding.lock().unwrap().timeline.clone();
        if let (JsValue::Object(fields), Some(timeline)) = (&mut event, timeline) {
            super::super::timeline::project(&mut timeline.lock().unwrap(), fields, module.as_ref());
        }
        let (send, recorded) = oneshot::channel();
        self.stream.events.send((event, wire, Some(send))).await.map_err(|_| HostError::new(HostErrorKind::Callback, "SOP parent event receiver closed."))?;
        recorded.await.map_err(|_| HostError::new(HostErrorKind::Callback, "SOP parent event callback rejected."))?;
        drop(delivery);
        Ok(())
    }
    async fn isolated_storage(&self, sop: &JsString, node: &JsString) -> Result<(SopStorage, JsString, Vec<JsValue>), HostError> {
        let encode = |value: &JsString| -> Result<String, HostError> {
            value.to_utf8().map(|text| crate::channel::http_client::encode_uri_component(&text)).map_err(|_| HostError::new(HostErrorKind::Session, "URI malformed"))
        };
        let suffix = format!(":sop-node:{}:{}", encode(sop)?, encode(node)?);
        let session = JsString::from_utf16(self.request.identity.session_id.units().iter().copied().chain(suffix.encode_utf16()).collect());
        let pair = pilotdeck_protocol::stringify_json(&JsValue::Array(vec![JsValue::String(sop.clone()), JsValue::String(node.clone())])).map_err(|e| HostError::new(HostErrorKind::Session, e.to_string()))?.ok_or_else(|| HostError::new(HostErrorKind::Session, "SOP node identity is not JSON serializable."))?;
        let sidechain: JsString = format!("sop-node-{}", encode(&pair.as_str().into())?).as_str().into();
        let (store, mut key, _) = self.recorder.writer().sidechain_storage(&sidechain);
        key.session_id = session.clone();
        let storage = FactoryStorage::new(store, key).open_sop(self.close_timeout, self.lease.clone()).await.map_err(storage_error)?;
        let snapshot = storage.storage.projection.snapshot().map_err(storage_error)?;
        match snapshot.metadata.get("parentSessionId") {
            value if value.is_none_or(|value| !super::super::submit::truthy(value)) => {
                let mut metadata = JsObject::new(); metadata.insert("parentSessionId".into(), JsValue::String(self.request.identity.session_id.clone()));
                storage.storage.append_metadata("sop-node-created".into(), JsValue::Object(metadata)).await.map_err(storage_error)?;
            }
            Some(JsValue::String(parent)) if parent == &self.request.identity.session_id => {},
            _ => return Err(HostError::new(HostErrorKind::Session, "SOP_NODE_PARENT_MISMATCH")),
        }
        Ok((storage, session, snapshot.messages))
    }
    async fn run_node(&self, mut plan: SopNodePlan) -> Result<SopNodeOutcome, HostError> {
        if self.signal.is_cancelled() { return Err(HostError::new(HostErrorKind::Session, "SOP node was cancelled before execution.")); }
        let mut node = self.node.lock().await;
        if node.is_some() && !plan.continuation { return Err(HostError::new(HostErrorKind::Session, "SOP node must finish before another node starts.")); }
        if plan.continuation && matches!(plan.context, SopNodeContext::Isolated { .. }) && node.as_ref().is_none_or(|scope| scope.context != plan.context || scope.identity.turn_id != self.request.identity.turn_id) {
            return Err(HostError::new(HostErrorKind::Session, "SOP correction has no matching active isolated node."));
        }
        let mut isolated = None;
        let mut node_runner = None;
        let mut identity = self.request.identity.clone();
        // Each actual loop is a separate operation. This identity is allocated
        // once here and retained by HostClient across its own reconnects.
        identity.request_id = format!("sop-node:{}", uuid::Uuid::new_v4()).as_str().into();
        identity.operation_id = plan.request.identity.operation_id.clone();
        let recorder = match &plan.context {
            SopNodeContext::Inherit => self.recorder.clone(),
            SopNodeContext::Isolated { .. } if plan.continuation => {
                let scope = node.take().expect("checked active isolated node");
                identity.session_id = scope.identity.session_id;
                plan.request.payload.insert("messages".into(), JsValue::Array(scope.last.messages));
                let recorder = scope.storage.storage.recorder.clone(); node_runner = Some(scope.runner); isolated = Some(scope.storage); recorder
            }
            SopNodeContext::Isolated { sop, node } => {
                let (storage, session, history) = self.isolated_storage(sop, node).await?;
                identity.session_id = session;
                let recorder = storage.storage.recorder.clone();
                let turn_input = TurnInput { input: self.input.clone(), history, synthetic: Vec::new(), metadata: Some(crate::metadata::object([("parentSessionId", JsValue::String(self.request.identity.session_id.clone()))]).as_object().unwrap().clone()) };
                let runner = TurnRunner::new(recorder.clone(), self.lease.clone(), identity.session_id.clone(), identity.turn_id.clone());
                let (runner, _) = runner.start().await.map_err(recorder_error)?;
                let messages = turn_input.loop_messages();
                let (runner, _) = runner.accept(turn_input).await.map_err(recorder_error)?;
                if runner.is_complete() { return Err(HostError::new(HostErrorKind::Session, "SOP node accepted input could not be persisted.")); }
                node_runner = Some(runner);
                plan.request.payload.insert("messages".into(), JsValue::Array(messages));
                isolated = Some(storage);
                recorder
            }
        };
        plan.request.identity = identity.clone();
        if let Some(node_route) = plan.model_override.take() {
            let mut merged = plan.request.payload.get("modelOverride").and_then(JsValue::as_object).cloned().unwrap_or_default();
            for (key, value) in node_route.entries() { merged.insert(key.clone(), value.clone()); }
            plan.request.payload.insert("modelOverride".into(), JsValue::Object(merged.clone())); plan.model_override = Some(merged);
        }
        let scope_context = plan.context.clone();
        let operation = async {
        let prepared = self.prepared.template.prepare(self.prepared.parent.clone(), &identity, recorder, &plan).await?;
        for (key, value) in prepared.manifest.entries() { plan.request.payload.insert(key.clone(), value.clone()); }
        let (events, mut receive) = mpsc::channel(1);
        let integrity = Arc::new(StreamIntegrity::new());
        let handler = Arc::new(StreamHandler { inner: prepared.handler.clone(), events, integrity: integrity.clone(), forwarding: Arc::default(), callback_cancel: Some(self.signal.clone()) });
        let mut pending = Some(crate::host::TurnScope::new(prepared.handler.clone(), self.close_timeout));
        let client = tokio::select! { biased;
            _ = self.signal.cancelled() => return Err(HostError::new(HostErrorKind::Session, "SOP node was cancelled before connecting.")),
            client = prepared.client => client?,
        };
        let module = client.binding().module_id.clone(); *integrity.module_id.lock().unwrap() = Some(module.clone());
        if let Some(scope) = pending.take() { scope.handoff(); }
        let abort_messages = match plan.request.payload.get("messages") { Some(JsValue::Array(messages)) => messages.clone(), _ => Vec::new() };
        let execution = handler.with_pump(client.execute_cancellable(plan.request, handler.clone(), self.signal.clone()), identity.clone());
        tokio::pin!(execution);
        let delivered = StreamIntegrity::new();
        let mut result = None;
        let delivery = async {
        loop {
            if result.is_some() && receive.is_empty() { break; }
            // Prefer already accepted events to final completion. Application
            // acknowledgements below gate both wire ACK and the next model step.
            tokio::select! { biased;
                event = receive.recv(), if result.is_none() || !receive.is_empty() => {
                    let Some((mut event, wire, receipt)) = event else { if result.is_some() { break; } else { continue; } };
                    delivered.observe_source(&event, wire).map_err(runtime_error)?;
                    let message = event.get("type").and_then(JsValue::as_string).is_some_and(|kind| kind.equals_str("assistant_message") || kind.equals_str("tool_results_projected"));
                    if message && let JsValue::Object(fields) = &mut event && let Some(mut value) = fields.get("message").cloned() {
                        mark_module_message(&mut value, &module).map_err(runtime_error)?; fields.insert("message".into(), value);
                    }
                    self.publish(event.clone(), wire && isolated.is_none(), Some(module.clone())).await?;
                    if let Some(runner) = node_runner.take() {
                        let transition = if message && wire { runner.durable_message(event.get("message").cloned().unwrap_or(JsValue::Undefined)).await }
                            else if wire && event.get("type") == Some(&"agent_status".into()) { runner.loop_status(event).await }
                            else { Ok((runner, Vec::new())) };
                        let (runner, _) = transition.map_err(recorder_error)?;
                        if let Some(error) = runner.callback_failure().map(str::to_owned) { return Err(HostError::new(HostErrorKind::Session, error)); }
                        node_runner = Some(runner);
                    }
                    if let Some(receipt) = receipt { let _ = receipt.send(()); }
                }
                completed = &mut execution, if result.is_none() => { result = Some(completed); if receive.is_empty() { break; } }
            }
        }
        Ok::<(), HostError>(())
        }.await;
        if let Err(error) = delivery {
            self.signal.cancel(None);
            receive.close(); while receive.try_recv().is_ok() {}
            if result.is_none() { let _ = execution.await; }
            return Err(error);
        }
        let result = result.ok_or_else(|| HostError::new(HostErrorKind::Protocol, "SOP node stream ended without a result."))??;
        validate_node_terminal(&integrity, &delivered, &result)?;
        let terminal = match result {
            ExecuteResult::Terminal(value) => value.get("payload").cloned().ok_or_else(|| HostError::new(HostErrorKind::Protocol, "SOP node final payload is missing."))?,
            ExecuteResult::Rejected(error) => return Err(HostError::new(HostErrorKind::Protocol, format!("SOP node execution rejected: {error:?}"))),
            ExecuteResult::Aborted { terminal: Some(terminal) } => terminal,
            ExecuteResult::Aborted { terminal: None } | ExecuteResult::AbortedCompaction => {
                let now = SystemExecutionClock.now_iso();
                let result = crate::metadata::object([("type", "aborted".into()), ("sessionId", JsValue::String(identity.session_id.clone())), ("turnId", JsValue::String(identity.turn_id.clone())), ("stopReason", "aborted_streaming".into()), ("usage", JsValue::Object(JsObject::new())), ("permissionDenials", JsValue::Array(Vec::new())), ("turns", JsValue::Number(0.0)), ("startedAt", JsValue::String(now.clone())), ("completedAt", JsValue::String(now))]);
                crate::metadata::object([("result", result), ("messages", JsValue::Array(abort_messages))])
            }
        };
        let mut result = terminal.get("result").cloned().unwrap_or(JsValue::Undefined);
        let messages = match terminal.get("messages") { Some(JsValue::Array(messages)) => messages.clone(), _ => return Err(HostError::new(HostErrorKind::Protocol, "SOP node final messages are missing.")) };
        if let JsValue::Object(fields) = &mut result { fields.insert("sessionId".into(), JsValue::String(self.request.identity.session_id.clone())); }
        drop(prepared.lease);
        Ok(SopNodeOutcome { result, messages })
        }.await;
        // Raw host/transport errors still finish the isolated durable turn.
        // Cleanup runs for every Result path, without replacing the primary
        // failure with a successful close or silently abandoning its writer.
        let mut operation = operation;
        if let Ok(outcome) = &operation && let Some(storage) = isolated.take() {
            let runner = node_runner.take().ok_or_else(|| HostError::new(HostErrorKind::Session, "SOP isolated node lost its recording owner."))?;
            *node = Some(NodeScope { context: scope_context, identity, runner, storage, last: outcome.clone() });
        }
        if let Some(runner) = node_runner.take() {
            let error = operation.as_ref().err().map(|error| error.message.clone()).unwrap_or_else(|| "SOP node ended without completing its durable turn.".into());
            if let Err(record) = runner.fail(crate::metadata::object([("code", "agent_host_error".into()), ("message", error.as_str().into())])).await {
                operation = Err(HostError::new(HostErrorKind::Session, format!("{error}; SOP node failure persistence: {record}")));
            }
        }
        if let Some(storage) = isolated && let Err(error) = storage.close().await {
            let prefix = operation.as_ref().err().map(|error| format!("{}; ", error.message)).unwrap_or_default();
            operation = Err(HostError::new(HostErrorKind::Session, format!("{prefix}SOP node storage close: {}", error.message)));
        }
        operation
    }
}
impl SopNodeExecutor for Executor {
    fn tool_execution(&self) -> Option<Arc<dyn crate::host::modules::capability::ToolExecution>> { Some(self.prepared.template.tool_execution()) }
    fn finish_node<'a>(&'a self, outcome: Option<&'a SopNodeOutcome>) -> SopFuture<'a, Result<(), HostError>> {
        Box::pin(async move {
            if outcome.is_some_and(|outcome| !matches!(&outcome.result, JsValue::Object(_))) { return Err(HostError::new(HostErrorKind::Protocol, "SOP node completion result must be an object.")); }
            if let Some(outcome) = outcome && (outcome.result.get("sessionId") != Some(&JsValue::String(self.request.identity.session_id.clone())) || outcome.result.get("turnId") != Some(&JsValue::String(self.request.identity.turn_id.clone()))) { return Err(HostError::new(HostErrorKind::Protocol, "SOP node completion identity mismatch.")); }
            let mut node = self.node.lock().await;
            if let (Some(scope), Some(outcome)) = (node.as_ref(), outcome) && outcome.messages != scope.last.messages { return Err(HostError::new(HostErrorKind::Protocol, "SOP node completion does not match the active node messages.")); }
            let Some(scope) = node.take() else { return Ok(()); };
            let mut result = outcome.map(|value| value.result.clone()).unwrap_or_else(|| scope.last.result.clone());
            let fields = result.as_object().cloned().ok_or_else(|| HostError::new(HostErrorKind::Protocol, "SOP node completion result must be an object."));
            let completed = match fields {
                Err(error) => Err(error),
                Ok(mut fields) => {
                    fields.insert("sessionId".into(), JsValue::String(scope.identity.session_id.clone()));
                    fields.insert("turnId".into(), JsValue::String(scope.identity.turn_id.clone()));
                    if outcome.is_none() {
                        fields.insert("type".into(), if self.signal.is_cancelled() { "aborted".into() } else { "error".into() });
                        fields.insert("stopReason".into(), if self.signal.is_cancelled() { "aborted_streaming".into() } else { "model_error".into() });
                        if !self.signal.is_cancelled() { fields.insert("errors".into(), JsValue::Array(vec![crate::metadata::object([("code", "sop_node_scope_closed".into()), ("message", "SOP node scope closed before logical completion.".into())])])); }
                    }
                    result = JsValue::Object(fields); let expected = result.clone();
                    match scope.runner.complete(result).await.map_err(recorder_error) {
                        Err(error) => Err(error),
                        Ok((_, events)) if events.last().and_then(|event| event.get("result")) == Some(&expected) => Ok(()),
                        Ok((_, events)) => Err(HostError::new(HostErrorKind::Session, format!("SOP node completion persistence failed: {events:?}"))),
                    }
                }
            };
            let closed = scope.storage.close().await.map_err(storage_error);
            match (completed, closed) { (Err(error), _) | (_, Err(error)) => Err(error), (Ok(()), Ok(())) => Ok(()) }
        })
    }
    fn model_validator(&self) -> Result<super::super::loop_control::SopModelValidator, HostError> { self.prepared.template.model_validator(self.prepared.parent.clone()) }

    fn run<'a>(&'a self, plan: SopNodePlan) -> SopFuture<'a, Result<SopNodeOutcome, HostError>> { Box::pin(self.run_node(plan)) }
    fn append_parent_reply<'a>(&'a self, message: JsValue) -> SopFuture<'a, Result<(), HostError>> {
        let recorder = self.recorder.clone(); let lease = self.lease.clone(); let identity = self.request.identity.clone();
        Box::pin(async move { let kind = super::super::runner::classify_durable_message(&message).map_err(|e| HostError::new(HostErrorKind::Session, e))?;
            tokio::task::spawn_blocking(move || { let _lease = lease; let mut draft = JsObject::new(); draft.insert("type".into(), kind.into()); draft.insert("message".into(), message); recorder.writer().record_session_event_checked(&identity.session_id, &identity.turn_id, draft).map_err(|e| HostError::new(HostErrorKind::Session, e.to_string())) }).await.map_err(|e| HostError::new(HostErrorKind::Session, e.to_string()))?
        })
    }
    fn emit_parent<'a>(&'a self, event: JsValue) -> SopFuture<'a, Result<(), HostError>> { Box::pin(self.publish(event, false, None)) }
}
fn runtime_error(error: RuntimeError) -> HostError { HostError::new(HostErrorKind::Protocol, error.message) }
fn recorder_error(error: crate::storage::recorder::RecorderError) -> HostError { HostError::new(HostErrorKind::Session, error.to_string()) }
fn storage_error(error: RuntimeError) -> HostError { HostError::new(HostErrorKind::Session, error.message) }
fn validate_node_terminal(received: &StreamIntegrity, delivered: &StreamIntegrity, result: &ExecuteResult) -> Result<(), HostError> {
    match result {
        ExecuteResult::Terminal(terminal) => {
            let outcome = terminal.get("payload").and_then(|p| p.get("result")).and_then(|r| r.get("type")).and_then(JsValue::as_string);
            if !outcome.is_some_and(|kind| kind.equals_str("aborted")) && *received.wire_events.lock().unwrap() != *delivered.wire_events.lock().unwrap() {
                return Err(HostError::new(HostErrorKind::Protocol, "SOP node final hid undelivered loop events."));
            }
            delivered.observe_terminal(outcome).map_err(runtime_error)?; received.observe_terminal(outcome).map_err(runtime_error)?;
            if outcome.is_some_and(|kind| kind.equals_str("success")) && !*received.loop_completed.lock().unwrap() {
                return Err(HostError::new(HostErrorKind::Protocol, "Sidecar success final arrived before loop completion."));
            }
            Ok(())
        }
        ExecuteResult::Aborted { .. } | ExecuteResult::AbortedCompaction => { received.observe_aborted().map_err(runtime_error)?; delivered.observe_aborted().map_err(runtime_error) }
        ExecuteResult::Rejected(_) => Ok(()),
    }
}
