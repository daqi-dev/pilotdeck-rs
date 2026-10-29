//! LiveSession production adapter: HostClient → AgentEvent stream.
//!
//! This connects the gateway's submit pipeline to a real sidecar process via
//! the host module transport. TurnRunner.ts:195-217 is the baseline: after
//! recordAcceptedInput, the loop.run() generator yields AgentEvents until
//! turn_completed.
//!
//! StreamIntegrity checks AgentLoop prefixes and terminal contradictions on
//! receipt and delivery. Host lifecycle is recorded before execute; it cannot
//! justify a sidecar final. HostClient owns sequence/identity and suppresses
//! non-final loop turn_completed. A received final cannot hide queued loop
//! events dropped during bounded cancellation; both observations must agree.

use super::runner::{Recording, RecordingAction, TurnInput, TurnRunner};
use crate::{
    host::{
        Cancellation, EventGuard, ExecuteRequest, ExecuteResult, HostClient, HostError,
        HostErrorKind, HostFuture, HostHandler, ReconnectObservation,
    },
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeLease, RuntimeResult},
        session::{LiveSession, LiveTurn, SessionNext},
    },
    storage::recorder::Recorder,
};
#[cfg(test)]
use pilotdeck_protocol::parse_json;
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, sleep_until};

type StatusReceipt = Option<oneshot::Sender<()>>;
type ForwardedEvent = (JsValue, bool, StatusReceipt);

type TurnHost = (
    HostFuture<HostClient>,
    Arc<dyn HostHandler>,
    Arc<RuntimeLease>,
);
type PreparedHost = (
    HostFuture<HostClient>,
    Arc<dyn HostHandler>,
    Arc<RuntimeLease>,
    Option<JsObject>,
    Option<crate::runtime::project::interaction_events::TurnEvents>,
    Option<super::runner::PromptHooks>,
    Option<super::live_session::SessionLifecycle>,
);
type TurnHostFactory = dyn Fn(
        ExecutionIdentity,
        Arc<Recorder>,
        crate::runtime::hooks::Cancellation,
    ) -> HostFuture<PreparedHost>
    + Send
    + Sync;

/// Host-acquired authority for one turn. Acquire the project lease with
/// ProjectRuntimeRegistry::acquire when this factory is invoked, not when the
/// LiveSessionAdapter is created. The sidecar cannot supply any of these ports.
/// Assemble a fresh router from that lease and the factory's ExecutionIdentity
/// every time: model/budget/context and capability's execution, context,
/// observer and plan/todo ports retain per-turn state. InteractionTurn::bind
/// only replaces the interaction handlers; it cannot sanitize these ports.
pub struct InteractionHost {
    pub client: HostFuture<HostClient>,
    pub lease: crate::runtime::project::RuntimeLease,
    pub router: crate::host::modules::ModuleRouter,
    pub input: super::interaction::InteractionTurnInput,
}

/// Production LiveSession backed by a fresh HostClient and handler for each turn.
/// HostClient closes its process and disposes its handler after each execution.
pub struct LiveSessionAdapter {
    host_factory: Arc<TurnHostFactory>,
    close_timeout: std::time::Duration,
    agent_config: Option<JsObject>,
    tail: Option<Arc<super::runner::tail::SessionTail>>,
    recorder: Arc<Recorder>,
    history: Arc<dyn Fn() -> Vec<JsValue> + Send + Sync>,
    pub(super) state: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
pub(super) struct SessionState {
    reload: Option<Arc<crate::runtime::session::factory::ReloadSource>>,
    reload_execution: Arc<std::sync::atomic::AtomicBool>,
    pub(super) disposed: bool,
    pub(super) active: Option<Cancellation>,
    pub(super) signal: crate::runtime::hooks::Cancellation,
    pub(super) status: super::live_session::SessionStatus,
    pub(super) current_turn_id: Option<JsString>,
}

pub(super) struct ActiveTurn(pub(super) Arc<Mutex<SessionState>>);

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.0.lock().unwrap().active = None;
    }
}

impl LiveSessionAdapter {
    /// The factory must prepare a lazy connection and turn-owned resources, including a fresh handler
    /// when its dispose implementation releases module resources.
    /// Pass the supplied recorder to model/tool durable ports; never construct
    /// a second writer. `history` reads the session's current projection.
    pub fn new(
        recorder: Arc<Recorder>,
        history: impl Fn() -> Vec<JsValue> + Send + Sync + 'static,
        host_factory: impl Fn(Arc<Recorder>) -> HostFuture<TurnHost> + Send + Sync + 'static,
    ) -> Self {
        Self {
            close_timeout: crate::host::Limits::default().close_timeout,
            agent_config: None,
            tail: None,
            recorder,
            history: Arc::new(history),
            host_factory: Arc::new(move |_, recorder, _| {
                let host = host_factory(recorder);
                Box::pin(async move {
                    let (client, handler, lease) = host.await?;
                    Ok((client, handler, lease, None, None, None, None))
                })
            }),
            state: Arc::default(),
        }
    }

    /// Install session-owned tail dependencies. With no installation, auxiliary
    /// model and file collection capabilities remain unavailable.
    pub fn with_tail(mut self, tail: Arc<super::runner::tail::SessionTail>) -> Self {
        self.tail = Some(tail);
        self
    }

    /// Session-owned serialized AgentRuntimeConfig for the real sidecar.
    pub fn with_agent_config(mut self, config: JsObject) -> Self {
        self.agent_config = Some(config);
        self
    }

    /// Use the same cleanup budget as the lazy HostClient's Limits.
    pub fn with_close_timeout(mut self, close_timeout: std::time::Duration) -> Self {
        self.close_timeout = close_timeout;
        self
    }

    /// Invoke the factory once when executing each submitted turn. It must satisfy
    /// InteractionHost's fresh-router contract, even across a project reload
    /// that admits the same session/turn identity again.
    pub fn with_interaction(
        recorder: Arc<Recorder>,
        history: impl Fn() -> Vec<JsValue> + Send + Sync + 'static,
        factory: impl Fn(ExecutionIdentity, Arc<Recorder>) -> HostFuture<InteractionHost>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            close_timeout: crate::host::Limits::default().close_timeout,
            agent_config: None,
            tail: None,
            recorder,
            history: Arc::new(history),
            host_factory: Arc::new(move |identity, recorder, signal| {
                let host = factory(identity.clone(), recorder.clone());
                Box::pin(async move {
                    let mut host = host.await?;
                    let cwd = host.input.permission.context.cwd.clone();
                    let transcript_path: JsString = recorder
                        .writer()
                        .path()
                        .to_string_lossy()
                        .into_owned()
                        .into();
                    let lease = Arc::new(host.lease);
                    let mut lifecycle = super::live_session::SessionLifecycle::bind(
                        lease.clone(),
                        &identity,
                        &mut host.input,
                        signal,
                        cwd.clone(),
                        transcript_path.clone(),
                    )?;
                    let handler = super::interaction::InteractionTurn::bind_owned(
                        lease.clone(),
                        &identity,
                        host.router,
                        host.input,
                        lifecycle.is_some(),
                    )
                    .await?;
                    let handler = Arc::new(handler);
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.retain_turn(handler.clone());
                    }
                    let manifest = handler.manifest().clone();
                    let events = handler.ports().events;
                    let prompt_hooks =
                        handler
                            .ports()
                            .hooks
                            .map(|port| super::runner::PromptHooks {
                                port: Arc::new(port),
                                cwd,
                                transcript_path,
                            });
                    Ok((
                        host.client,
                        handler as Arc<dyn HostHandler>,
                        lease,
                        Some(manifest),
                        events,
                        prompt_hooks,
                        lifecycle,
                    ))
                })
            }),
            state: Arc::default(),
        }
    }
}

impl LiveSession for LiveSessionAdapter {
    #[cfg(test)]
    fn reload_signal_for_test(&self) -> Option<crate::runtime::hooks::Cancellation> {
        Some(self.state.lock().unwrap().signal.clone())
    }
    fn reload_snapshot(
        &self,
    ) -> Result<crate::runtime::session::factory::ReloadSnapshot, RuntimeError> {
        let state = self.state.lock().unwrap();
        let source = state.reload_source()?;
        source.snapshot_consistent(&self.recorder)
    }

    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        // Invalid requests must not occupy the session's active turn slot.
        let (mut request, mut input) = parse_execute_request(&input, &options, vec![])?;
        if let Some(config) = &self.agent_config {
            request
                .payload
                .insert("agent".into(), JsValue::Object(config.clone()));
        }
        let cancellation = Cancellation::default();
        let signal = crate::runtime::hooks::Cancellation::new();
        {
            let mut state = self.state.lock().unwrap();
            if state.disposed {
                return Err(RuntimeError::new("Agent session disposed."));
            }
            if state.active.is_some() {
                return Err(RuntimeError::new(
                    "Agent session already has an active turn.",
                ));
            }
            state.active = Some(cancellation.clone());
            state.signal = signal.clone();
            state.status = super::live_session::SessionStatus::Running;
            state.current_turn_id = Some(request.identity.turn_id.clone());
            state.reload_execution = Arc::default();
        }
        let active = Arc::new(ActiveTurn(self.state.clone()));
        let reload_execution = self.reload_execution_started();
        // Read the projection only after admission owns the slot. Rejected
        // submissions must not observe history or race its current owner.
        input.history = (self.history)();
        let reload_source = self.state.lock().unwrap().reload.clone();
        if let Some(source) = reload_source {
            match source.projection.snapshot() {
                Ok(snapshot) => input.history = snapshot.messages,
                Err(error) => {
                    let mut state = self.state.lock().unwrap();
                    state.status = super::live_session::SessionStatus::Idle;
                    state.current_turn_id = None;
                    return Err(error);
                }
            }
        }
        request
            .payload
            .insert("messages".into(), JsValue::Array(input.loop_messages()));
        let owner = active.clone();
        let (recording_sender, recording_ready) = oneshot::channel();
        let (execute_ready, execute_wait) = oneshot::channel();
        let host_factory = self.host_factory.clone();
        let close_timeout = self.close_timeout;
        let recorder = self.recorder.clone();
        let identity = request.identity.clone();
        let tail = self.tail.clone();
        let tail_options = options.clone();
        // Shared between the host-side validation hook (every received event)
        // and finish()'s second, contradiction-only check; see StreamHandler.
        let integrity = Arc::new(StreamIntegrity::new());
        let hook_integrity = integrity.clone();
        // Keep at most one delivered event ahead of the submit consumer.
        let (events, receiver) = mpsc::channel(1);
        let (session_sender, session_ready) = oneshot::channel();
        let session_id = identity.session_id.clone();
        let session_owner = active.clone();
        let turn = LiveTurnStream {
            callback_cancel: Some(cancellation.clone()),
            execution: Some(Box::pin(async move {
                let (client, inner, lease, manifest, interaction_events, prompt_hooks, lifecycle) = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Ok(ExecuteResult::Aborted { terminal: None });
                    }
                    // select constructs every branch before polling; keep the
                    // factory call lazy so a pre-cancel never creates resources.
                    host = async { host_factory(request.identity.clone(), recorder.clone(), signal).await } => host?,
                };
                let session_steer = lifecycle
                    .as_ref()
                    .and_then(super::live_session::SessionLifecycle::steer);
                let _ = session_sender.send(lifecycle);
                let pending_host = crate::host::TurnScope::new(inner.clone(), close_timeout);
                let mut runner = TurnRunner::new(
                    recorder,
                    lease,
                    request.identity.session_id.clone(),
                    request.identity.turn_id.clone(),
                )
                .with_session_steer(session_steer)
                .with_interaction_events(interaction_events)
                .with_prompt_hooks(prompt_hooks);
                if let Some(tail) = tail {
                    runner = runner.with_tail(tail, cancellation.clone(), tail_options);
                }
                // Transfer the actual driver before any host event can arrive.
                let _ = recording_sender.send(Recording::new(runner, owner));
                if let Some(manifest) = manifest {
                    for (key, value) in manifest.entries() {
                        request.payload.insert(key.clone(), value.clone());
                    }
                }
                let handler = Arc::new(StreamHandler {
                    inner,
                    events,
                    integrity: hook_integrity,
                    forwarding: Arc::default(),
                    callback_cancel: Some(cancellation.clone()),
                });
                let client_integrity = handler.integrity.clone();
                let client: HostFuture<HostClient> = Box::pin(async move {
                    let client = client.await?;
                    *client_integrity.module_id.lock().unwrap() =
                        Some(client.binding().module_id.clone());
                    reload_execution.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(client)
                });
                // No connection or execute can start until both durable gates commit.
                let Ok(messages) = execute_wait.await else {
                    return Ok(ExecuteResult::Aborted { terminal: None });
                };
                request
                    .payload
                    .insert("messages".into(), JsValue::Array(messages));
                let client = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                    client = client => client?,
                };
                pending_host.handoff();
                let identity = request.identity.clone();
                handler
                    .with_pump(
                        client.execute_cancellable(request, handler.clone(), cancellation),
                        identity,
                    )
                    .await
            })),
            result: None,
            events: receiver,
            identity,
            input,
            integrity,
            delivered: StreamIntegrity::new(),
            active: Some(active),
            recording_ready,
            recording: None,
            pending_event: None,
            pending_durable: None,
            accepted: false,
            preflight: 0,
            execute_ready: Some(execute_ready),
            recorded: std::collections::VecDeque::new(),
        };
        Ok(Box::new(super::live_session::SessionStream::new(
            turn,
            session_ready,
            session_id,
            self.state.clone(),
            session_owner,
        )))
    }

    fn snapshot(&self) -> JsValue {
        let (status, current_turn_id) = {
            let state = self.state.lock().unwrap();
            (state.status, state.current_turn_id.clone())
        };
        let mut value = JsObject::new();
        value.insert("messages".into(), JsValue::Array((self.history)()));
        value.insert("status".into(), status.as_str().into());
        if let Some(turn) = &current_turn_id {
            value.insert("currentTurnId".into(), JsValue::String(turn.clone()));
        }
        JsValue::Object(value)
    }

    fn abort(&self, reason: &JsString) -> RuntimeResult {
        let mut state = self.state.lock().unwrap();
        state.signal.abort();
        state.status = super::live_session::SessionStatus::Aborted;
        if let Some(cancellation) = &state.active {
            cancellation.cancel(Some(reason.clone()));
        }
        Ok(())
    }

    fn dispose(&self) -> DisposeFuture<'_> {
        let mut state = self.state.lock().unwrap();
        state.disposed = true;
        if let Some(tail) = &self.tail {
            tail.dispose();
        }
        if state.status == super::live_session::SessionStatus::Running {
            state.status = super::live_session::SessionStatus::Aborted;
            state.signal.abort();
            if let Some(cancellation) = &state.active {
                cancellation.cancel(Some("session_closed".into()));
            }
        }
        self.recorder.writer().begin_close();
        let writer = self.recorder.writer().clone();
        let tail = self.tail.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || writer.close())
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            if let Some(tail) = tail {
                tail.close()
                    .await
                    .map_err(|e| RuntimeError::new(e.to_string()))?;
            }
            Ok(())
        })
    }
}

impl SessionState {
    fn reload_source(
        &self,
    ) -> Result<Arc<crate::runtime::session::factory::ReloadSource>, RuntimeError> {
        if self.disposed {
            return Err(RuntimeError::new("Session projection driver is disposed."));
        }
        self.reload
            .clone()
            .ok_or_else(|| RuntimeError::new("Agent session runtime reload is unavailable."))
    }
}

impl LiveSessionAdapter {
    /// Bind the same projection and session file-state owner used by the host.
    pub fn with_reload_source(
        self,
        source: crate::runtime::session::factory::ReloadSource,
    ) -> Self {
        self.state.lock().unwrap().reload = Some(Arc::new(source));
        self
    }

    pub(crate) fn reload_execution_started(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.state.lock().unwrap().reload_execution.clone()
    }
}

/// Streaming adapter for one turn execution.
pub(super) struct LiveTurnStream {
    // Owned here rather than spawned: dropping the stream drops execute and
    // lets HostClient clean up its child and outstanding callbacks.
    execution: Option<HostFuture<ExecuteResult>>,
    result: Option<Result<ExecuteResult, HostError>>,
    // The sidecar status callback applies only to wire events, not host drains.
    events: mpsc::Receiver<ForwardedEvent>,
    identity: ExecutionIdentity,
    input: TurnInput,
    // The instance the host-side receive hook observes into. finish() also
    // consults it so a contradiction received but dropped before delivery (for
    // example turn_failed followed by success) still fails the terminal.
    integrity: Arc<StreamIntegrity>,
    // Applied sidecar lifecycle consumed by this stream. Host preflight events
    // do not count as evidence that the sidecar applied its own lifecycle.
    delivered: StreamIntegrity,
    // The last stream/runner/blocking-write owner releases the real slot.
    active: Option<Arc<ActiveTurn>>,
    recording_ready: oneshot::Receiver<Recording>,
    recording: Option<Recording>,
    // A cancelled next drops only its borrow, never the transition or payload.
    pending_event: Option<(JsValue, bool, StatusReceipt)>,
    pending_durable: Option<(JsValue, StatusReceipt)>,
    callback_cancel: Option<Cancellation>,
    accepted: bool,
    preflight: u8,
    execute_ready: Option<oneshot::Sender<Vec<JsValue>>>,
    recorded: std::collections::VecDeque<JsValue>,
}

impl LiveTurn for LiveTurnStream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            loop {
                if self.pending_event.is_some() {
                    if let Some(event) = self.settle_recording().await? {
                        return Ok(Some(event));
                    }
                    continue;
                }
                if let Some((message, receipt)) = self.pending_durable.take() {
                    self.begin_recording(
                        RecordingAction::Durable(message),
                        JsValue::Null,
                        false,
                        receipt,
                    )
                    .await?;
                    continue;
                }
                if let Some(error) = self.recording.as_ref().and_then(Recording::callback_error) {
                    // A callback rejection ends the loop, including host drains.
                    // No queued successor may escape while execute joins cleanup.
                    if let Some(cancel) = &self.callback_cancel {
                        cancel.cancel(None);
                    }
                    self.events.close();
                    while self.events.try_recv().is_ok() {}
                    if let Some(execution) = self.execution.as_mut() {
                        let _ = execution.await;
                    }
                    self.execution = None;
                    self.result = None;
                    self.recording.as_mut().unwrap().take_callback_error();
                    let failure = crate::metadata::object([
                        ("code", "agent_invalid_state".into()),
                        ("message", error.as_str().into()),
                    ]);
                    return self
                        .begin_recording(RecordingAction::Fail(failure), JsValue::Null, true, None)
                        .await;
                }
                if let Some(event) = self.recorded.pop_front() {
                    return self.deliver_recorded(event);
                }
                if self.execute_ready.is_some() && self.recording.is_some() {
                    if self.preflight < 3 {
                        let (kind, action) = if self.preflight == 0 {
                            ("turn_started", RecordingAction::Start)
                        } else if self.preflight == 1 {
                            (
                                "input_accepted",
                                RecordingAction::Accept(self.input.clone()),
                            )
                        } else {
                            (
                                "user_prompt_submitted",
                                RecordingAction::SubmitPrompt(
                                    self.input.input.clone(),
                                    self.callback_cancel.clone().expect("host cancellation"),
                                ),
                            )
                        };
                        self.preflight += 1;
                        let event = crate::metadata::object([("type", kind.into())]);
                        return self.begin_recording(action, event, false, None).await;
                    }
                    let runner = self
                        .recording
                        .as_mut()
                        .unwrap()
                        .runner_mut()
                        .expect("settled preflight");
                    if let Some(error) = runner.prompt_block_error() {
                        return self
                            .begin_recording(
                                RecordingAction::Fail(error),
                                JsValue::Null,
                                true,
                                None,
                            )
                            .await;
                    }
                    runner.begin_tail_work();
                    let messages = runner.messages().to_vec();
                    let _ = self.execute_ready.take().unwrap().send(messages);
                }
                let mut delivered = None;
                if let Some(execution) = self.execution.as_mut() {
                    tokio::select! {
                        biased;
                        recording = &mut self.recording_ready, if self.execute_ready.is_some() && self.recording.is_none() => {
                            if let Ok(recording) = recording { self.recording = Some(recording); } else { self.execute_ready.take(); }
                            continue;
                        }
                        Some(event) = self.events.recv() => delivered = Some(event),
                        result = execution => { self.result = Some(result); self.execution = None; }
                    }
                }
                if delivered.is_none() {
                    delivered = self.events.try_recv().ok();
                }
                if let Some((event, wire, receipt)) = delivered {
                    // TurnRunner ignores loop artifact snapshots; its collector owns final artifacts.
                    if event.get("type") == Some(&"file_artifacts".into()) {
                        self.delivered.observe_source(&event, wire)?;
                        continue;
                    }
                    if let Some(event) = self.record_event(event, false, wire, receipt).await? {
                        return Ok(Some(event));
                    }
                    continue;
                }
                return match self.result.take() {
                    Some(result) => match self.finish(result) {
                        Ok(Some(event)) => self.record_event(event, true, false, None).await,
                        Err(error) if self.accepted => {
                            let failure = crate::metadata::object([
                                ("code", "agent_invalid_state".into()),
                                ("message", error.message.as_str().into()),
                            ]);
                            self.begin_recording(
                                RecordingAction::Fail(failure),
                                JsValue::Null,
                                true,
                                None,
                            )
                            .await
                        }
                        result => {
                            self.release();
                            result
                        }
                    },
                    None => Ok(None),
                };
            }
        })
    }
}

impl LiveTurnStream {
    pub(super) async fn prepare_session(
        &mut self,
        ready: &mut oneshot::Receiver<Option<super::live_session::SessionLifecycle>>,
    ) -> Result<Option<super::live_session::SessionLifecycle>, RuntimeError> {
        let Some(execution) = self.execution.as_mut() else {
            return Ok(None);
        };
        tokio::select! {
            biased;
            lifecycle = ready => Ok(lifecycle.unwrap_or(None)),
            result = execution => {
                self.execution = None;
                match result {
                    Err(error) if error.kind == HostErrorKind::Session => Err(RuntimeError::new(error.message)),
                    Err(error) => Err(map_host_error(error)),
                    result => { self.result = Some(result); Ok(None) }
                }
            }
        }
    }
    fn release(&mut self) {
        self.execute_ready.take();
        self.execution.take();
        self.result.take();
        self.events.close();
        while self.events.try_recv().is_ok() {}
        self.recording_ready.close();
        let _ = self.recording_ready.try_recv();
        self.recording.take();
        self.active.take();
    }

    async fn record_event(
        &mut self,
        mut event: JsValue,
        terminal: bool,
        wire: bool,
        mut receipt: StatusReceipt,
    ) -> Result<Option<JsValue>, RuntimeError> {
        if !terminal {
            self.delivered.observe_source(&event, wire)?;
        }
        if let Ok(recording) = self.recording_ready.try_recv() {
            self.recording = Some(recording);
        }
        if event.get("type") == Some(&"tool_result".into())
            && let Some(runner) = self.recording.as_mut().and_then(Recording::runner_mut)
        {
            runner.observe_tool_result(event.get("result").unwrap_or(&JsValue::Undefined));
        }
        let module_message = event
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| {
                kind.equals_str("assistant_message") || kind.equals_str("tool_results_projected")
            });
        let durable_wire = wire
            && self.integrity.module_id.lock().unwrap().is_some()
            && self.recording.is_some()
            && module_message;
        if durable_wire {
            let message = event.get("message").cloned().unwrap_or(JsValue::Undefined);
            if !crate::mutants::on("live-malformed-durable")
                && super::runner::classify_durable_message(&message).is_err()
            {
                // Never expose malformed payloads. The owned durable transition
                // records its callback error, then the next poll joins the host
                // and commits the ordinary failure terminal before releasing.
                self.pending_durable = Some((message, receipt.take()));
                return Ok(None);
            }
        }
        // TS markModuleOwnedEvent only interprets these two message families.
        // Other events may carry a human-readable `message` string.
        if (module_message || crate::mutants::on("live-message-field-classification"))
            && let (Some(module), JsValue::Object(fields)) = (
                self.integrity.module_id.lock().unwrap().as_ref(),
                &mut event,
            )
            && let Some(mut message) = fields.get("message").cloned()
        {
            mark_module_message(&mut message, module)?;
            fields.insert("message".into(), message);
        }
        if durable_wire {
            if crate::mutants::on("live-durable-before-yield") {
                return self
                    .begin_recording(
                        RecordingAction::Durable(
                            event.get("message").cloned().unwrap_or(JsValue::Undefined),
                        ),
                        event,
                        false,
                        receipt,
                    )
                    .await;
            }
            self.pending_durable = Some((
                event.get("message").cloned().unwrap_or(JsValue::Undefined),
                receipt.take(),
            ));
        }
        let action = match event.get("type").and_then(JsValue::as_string) {
            Some(kind) if kind.equals_str("turn_started") => Some(RecordingAction::Start),
            Some(kind) if kind.equals_str("input_accepted") => {
                Some(RecordingAction::Accept(self.input.clone()))
            }
            Some(kind) if kind.equals_str("turn_failed") => Some(RecordingAction::LoopFailed(
                event.get("error").cloned().unwrap_or(JsValue::Undefined),
            )),
            Some(kind) if wire && kind.equals_str("agent_status") => {
                Some(RecordingAction::Status(event.clone()))
            }
            // Pre-cancel without a runner retains the existing abort contract.
            _ if terminal && self.accepted => Some(RecordingAction::Complete(
                event.get("result").cloned().unwrap_or(JsValue::Undefined),
            )),
            _ => None,
        };
        if let (true, Some(action)) = (self.recording.is_some(), action) {
            return self.begin_recording(action, event, terminal, receipt).await;
        }
        if let Some(receipt) = receipt {
            let _ = receipt.send(());
        }
        if terminal {
            self.release();
        }
        Ok(Some(event))
    }

    async fn begin_recording(
        &mut self,
        action: RecordingAction,
        event: JsValue,
        terminal: bool,
        receipt: StatusReceipt,
    ) -> Result<Option<JsValue>, RuntimeError> {
        if let Err(error) = self
            .recording
            .as_mut()
            .expect("recording driver")
            .begin(action)
        {
            self.release();
            return Err(RuntimeError::new(error.to_string()));
        }
        self.pending_event = Some((event, terminal, receipt));
        self.settle_recording().await
    }

    async fn settle_recording(&mut self) -> Result<Option<JsValue>, RuntimeError> {
        let outcome = self
            .recording
            .as_mut()
            .expect("pending event owns recording")
            .settle()
            .await;
        if self
            .recording
            .as_ref()
            .expect("recording progress")
            .is_pending()
        {
            let mut events = outcome.map_err(|e| RuntimeError::new(e.to_string()))?;
            return self.deliver_recorded(events.remove(0));
        }
        let (event, terminal, receipt) = self.pending_event.take().expect("pending event");
        let mut events = match outcome {
            Ok(events) => events,
            Err(error) => {
                self.release();
                return Err(RuntimeError::new(error.to_string()));
            }
        };
        if self.recording.as_ref().unwrap().callback_error().is_some() {
            return Ok(None);
        }
        if let Some(receipt) = receipt {
            let _ = receipt.send(());
        }
        if events.is_empty() && event == JsValue::Null && !terminal {
            return Ok(None);
        }
        if event.get("type") == Some(&"turn_failed".into()) {
            // Preserve the loop event (including timeline/extensions), while the
            // runner contributes only its optional durable failure status.
            *events.last_mut().expect("loop failure") = event;
            self.recorded.extend(events);
            let event = self.recorded.pop_front().expect("loop failure");
            return self.deliver_recorded(event);
        }
        // Completion fallback is itself a committed failure. Publish the runner's
        // result, never the original success whose persistence failed.
        if self.execute_ready.is_some() || events.len() != 1 || event == JsValue::Null {
            self.recorded.extend(events);
            let event = self.recorded.pop_front().expect("transition events");
            return self.deliver_recorded(event);
        }
        if terminal {
            self.release();
        }
        Ok(Some(event))
    }

    fn deliver_recorded(&mut self, event: JsValue) -> Result<Option<JsValue>, RuntimeError> {
        if event.get("type") == Some(&"input_accepted".into()) {
            self.accepted = true;
        }
        if event.get("type") == Some(&"turn_completed".into()) {
            self.release();
        }
        Ok(Some(event))
    }

    fn finish(
        &self,
        result: Result<ExecuteResult, HostError>,
    ) -> Result<Option<JsValue>, RuntimeError> {
        match result {
            Ok(ExecuteResult::Terminal(terminal)) => {
                let outcome = terminal
                    .get("payload")
                    .and_then(|payload| payload.get("result"))
                    .and_then(|result| result.get("type"))
                    .and_then(JsValue::as_string);
                // No bounded cancel exit may hide an unapplied loop suffix.
                if !outcome.is_some_and(|kind| kind.equals_str("aborted"))
                    && *self.integrity.wire_events.lock().unwrap()
                        != *self.delivered.wire_events.lock().unwrap()
                {
                    return Err(RuntimeError::new(
                        "Stream integrity violation: terminal without proper lifecycle (undelivered loop events).",
                    ));
                }
                self.delivered.observe_terminal(outcome)?;
                // The received stream can still expose a contradiction the
                // consumer never got (for example a queued turn_failed before
                // a success final); keep this second, fail-closed gate so the
                // cancellation exit cannot hide it.
                self.integrity.observe_terminal(outcome)?;
                if outcome.is_some_and(|kind| kind.equals_str("success"))
                    && !*self.integrity.loop_completed.lock().unwrap()
                    && !crate::mutants::on("live-early-final-witness")
                {
                    return Err(RuntimeError::new(
                        "Sidecar success final arrived before loop completion.",
                    ));
                }
                Ok(Some(self.completed_event(
                    terminal.get("payload").unwrap_or(&JsValue::Undefined),
                )?))
            }
            Ok(ExecuteResult::Aborted { terminal }) => {
                self.integrity.observe_aborted()?;
                self.delivered.observe_aborted()?;
                let terminal = terminal.unwrap_or_else(|| self.aborted_result());
                Ok(Some(self.completed_event(&terminal)?))
            }
            Ok(ExecuteResult::Rejected(error)) => Err(RuntimeError {
                message: "Turn execution rejected.".into(),
                causes: vec![RuntimeError::new(format!("{:?}", error))],
                js_error: None,
            }),
            // A session-composition failure predates any host execution; the
            // composition boundary already carries the TS session-creation
            // message, so `Host execution failed:` must not wrap it (#885).
            Err(error) if error.kind == HostErrorKind::Session => {
                Err(RuntimeError::new(error.message))
            }
            Err(error) => Err(map_host_error(error)),
        }
    }

    fn completed_event(&self, terminal: &JsValue) -> Result<JsValue, RuntimeError> {
        let mut terminal = terminal.clone();
        if let (Some(module), JsValue::Object(fields)) = (
            self.integrity.module_id.lock().unwrap().as_ref(),
            &mut terminal,
        ) {
            if let Some(mut result) = fields.get("result").and_then(JsValue::as_object).cloned()
                && let Some(mut message) = result.get("finalMessage").cloned()
            {
                mark_module_message(&mut message, module)?;
                result.insert("finalMessage".into(), message);
                fields.insert("result".into(), JsValue::Object(result));
            }
            if let Some(JsValue::Array(messages)) = fields.get("messages") {
                let mut messages = messages.clone();
                for message in &mut messages {
                    mark_module_message(message, module)?;
                }
                fields.insert("messages".into(), JsValue::Array(messages));
            }
        }
        let mut event = JsObject::new();
        event.insert("type".into(), "turn_completed".into());
        event.insert(
            "sessionId".into(),
            JsValue::String(self.identity.session_id.clone()),
        );
        event.insert(
            "turnId".into(),
            JsValue::String(self.identity.turn_id.clone()),
        );
        event.insert(
            "result".into(),
            terminal
                .get("result")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        event.insert(
            "messages".into(),
            terminal
                .get("messages")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Object(event))
    }

    fn aborted_result(&self) -> JsValue {
        // agentLoopSidecarClient.ts:1434-1450: both timestamps share one now;
        // an incomplete compaction final must retain the original messages.
        let now = SystemExecutionClock.now_iso();
        let mut result = JsObject::new();
        for (key, value) in [
            ("type", "aborted".into()),
            (
                "sessionId",
                JsValue::String(self.identity.session_id.clone()),
            ),
            ("turnId", JsValue::String(self.identity.turn_id.clone())),
            ("stopReason", "aborted_streaming".into()),
            ("usage", JsValue::Object(JsObject::new())),
            ("permissionDenials", JsValue::Array(vec![])),
            ("turns", JsValue::Number(0.0)),
            ("startedAt", JsValue::String(now.clone())),
            ("completedAt", JsValue::String(now)),
        ] {
            result.insert(key.into(), value);
        }
        let mut terminal = JsObject::new();
        terminal.insert("result".into(), JsValue::Object(result));
        terminal.insert(
            "messages".into(),
            JsValue::Array(
                self.recording
                    .as_ref()
                    .and_then(Recording::runner)
                    .map(|runner| runner.messages().to_vec())
                    .unwrap_or_else(|| self.input.loop_messages()),
            ),
        );
        JsValue::Object(terminal)
    }
}

/// Preserve host module/lifecycle behavior while exposing applied AgentEvents.
///
/// Ordering boundary: `validate_event` observes each event's lifecycle on the
/// host's receive path, before the inner (module-side) handler applies it and
/// before the host ACKs it. A violating event therefore never reaches the
/// inner handler, and a misbehaving sidecar is cut off at its first violating
/// event. `event` awaits `applied` and only then forwards the payload, so the
/// consumer only ever publishes events that were applied successfully; the
/// session records that delivered prefix separately and `finish()` requires it
/// to justify the terminal. The receive-path state may be ahead of delivery:
/// events whose application callback never completed (including the whole
/// queue dropped by the host's bounded cancel exit) are still observed there,
/// but they never become terminal evidence — only a contradiction gate.
///
/// `validate_event` forwards to `inner.validate_event` after its own lifecycle
/// check: an inner handler that rejects a payload keeps exactly its rejection
/// semantics when wrapped (the wrapper never silently skips it), and either
/// rejection fails the receive path before application and ACK.
struct StreamHandler {
    inner: Arc<dyn HostHandler>,
    events: mpsc::Sender<ForwardedEvent>,
    integrity: Arc<StreamIntegrity>,
    forwarding: Arc<Mutex<Forwarding>>,
    // Embedded pump probes may omit execution cancellation; production binds it.
    callback_cancel: Option<Cancellation>,
}

/// A cancelled slot hands its unfinished predecessor to its successor, even
/// when that successor has already moved from the wire queue into a callback.
struct DeliveryWait(tokio::sync::oneshot::Receiver<Option<DeliveryWait>>);

impl Drop for DeliveryWait {
    fn drop(&mut self) {
        // Long runs of cancelled slots can nest completed channel values.
        // Close before draining to exclude concurrent sends, then dismantle
        // that chain iteratively rather than recursively dropping receivers.
        self.0.close();
        while let Ok(Some(mut previous)) = self.0.try_recv() {
            previous.0.close();
            std::mem::swap(&mut self.0, &mut previous.0);
        }
    }
}

struct Delivery {
    previous: Option<DeliveryWait>,
    done: Option<tokio::sync::oneshot::Sender<Option<DeliveryWait>>>,
}

impl Delivery {
    async fn wait(&mut self) {
        while let Some(previous) = self.previous.as_mut() {
            // Keep the pending receiver in self so cancellation while waiting
            // still transfers it through Drop to the next surviving callback.
            self.previous = (&mut previous.0).await.unwrap_or(None);
        }
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(self.previous.take());
        }
    }
}

#[derive(Default)]
struct Forwarding {
    tail: Option<DeliveryWait>,
    wire: std::collections::VecDeque<(Arc<()>, Delivery)>,
    activity: Option<Instant>,
    stopped: bool,
    parent: Option<ExecutionIdentity>,
    subagents: Vec<HostSubagent>,
}

struct HostSubagent {
    id: JsString,
    kind: Option<JsValue>,
    started: Instant,
    heartbeat: Instant,
    tool: Option<JsValue>,
    name: Option<JsValue>,
}

impl HostSubagent {
    fn status(
        &self,
        parent: &ExecutionIdentity,
        status: &str,
        now: Instant,
        success: Option<&JsValue>,
    ) -> JsObject {
        let mut event = JsObject::new();
        for (key, value) in [
            ("type", "subagent_status".into()),
            ("sessionId", JsValue::String(parent.session_id.clone())),
            ("turnId", JsValue::String(parent.turn_id.clone())),
            ("subagentId", JsValue::String(self.id.clone())),
        ] {
            event.insert(key.into(), value);
        }
        if let Some(kind) = &self.kind {
            event.insert("subagentType".into(), kind.clone());
        }
        event.insert("status".into(), status.into());
        for (key, value) in [("toolCallId", &self.tool), ("toolName", &self.name)] {
            if let Some(value) = value {
                event.insert(key.into(), value.clone());
            }
        }
        if let Some(success) = success {
            event.insert("success".into(), success.clone());
        }
        event.insert(
            "durationMs".into(),
            JsValue::Number(now.duration_since(self.started).as_millis() as f64),
        );
        event
    }
}

impl Forwarding {
    fn deadline(&self) -> Instant {
        self.activity.unwrap_or_else(Instant::now) + Duration::from_millis(500)
    }

    // Only host-buffer events update the active map, like TS drain(). Wire
    // events go through project(), which does not track host subagents.
    fn project_host(&mut self, events: Vec<JsObject>, heartbeat: bool) -> Vec<JsObject> {
        let now = Instant::now();
        let mut projected = Vec::new();
        let Some(parent) = &self.parent else {
            return events;
        };
        for event in events {
            let kind = event.get("type").and_then(JsValue::as_string);
            let lifecycle = kind.is_some_and(|k| {
                k.equals_str("subagent_started") || k.equals_str("subagent_completed")
            });
            let pre = kind.is_some_and(|k| k.equals_str("pre_tool_execute"));
            let post = kind.is_some_and(|k| k.equals_str("post_tool_execute"));
            let id = if lifecycle {
                event
                    .get("subagentId")
                    .and_then(JsValue::as_string)
                    .cloned()
            } else if pre || post {
                event.get("sessionId").and_then(JsValue::as_string).and_then(|session| {
                    let units = session.units();
                    let marker = [58, 58, 115, 117, 98, 58, 58];
                    let index = units.windows(marker.len()).rposition(|s| s == marker)?;
                    let rest = &units[index + marker.len()..];
                    let space = |u: &u16| matches!(*u, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
                    let start = rest.iter().position(|u| !space(u))?;
                    let end = rest.iter().rposition(|u| !space(u))? + 1;
                    Some(JsString::from_utf16(rest[start..end].to_vec()))
                })
            } else {
                None
            };
            let mut derived = None;
            if let Some(id) = id {
                let found = self.subagents.iter().position(|s| s.id == id);
                if kind.is_some_and(|k| k.equals_str("subagent_completed")) {
                    self.subagents.retain(|s| s.id != id);
                } else {
                    let fresh = HostSubagent {
                        id,
                        kind: if lifecycle {
                            event.get("subagentType").cloned()
                        } else {
                            None
                        },
                        started: now,
                        heartbeat: now,
                        tool: None,
                        name: None,
                    };
                    let index = found.unwrap_or_else(|| {
                        self.subagents.push(fresh);
                        self.subagents.len() - 1
                    });
                    let state = &mut self.subagents[index];
                    if lifecycle {
                        state.kind = event.get("subagentType").cloned();
                        state.started = now;
                        state.tool = None;
                        state.name = None;
                    }
                    state.heartbeat = now;
                    if pre || post {
                        state.tool = event.get("toolCallId").cloned();
                        state.name = event.get("toolName").cloned();
                        let status = state.status(
                            parent,
                            if pre {
                                "tool_started"
                            } else {
                                "tool_completed"
                            },
                            now,
                            if post { event.get("success") } else { None },
                        );
                        if post {
                            state.tool = None;
                            state.name = None;
                        }
                        derived = Some(status);
                    }
                }
            }
            projected.push(event);
            projected.extend(derived);
        }
        if heartbeat {
            for state in &mut self.subagents {
                if now.duration_since(state.heartbeat) < Duration::from_millis(2000) {
                    continue;
                }
                state.heartbeat = now;
                let running = state.name.as_ref().is_some_and(super::submit::truthy);
                projected.push(state.status(
                    parent,
                    if running { "running" } else { "waiting_model" },
                    now,
                    None,
                ));
            }
        }
        projected
    }
}
impl Forwarding {
    fn reserve(&mut self) -> Delivery {
        let (done, next) = tokio::sync::oneshot::channel();
        Delivery {
            previous: self.tail.replace(DeliveryWait(next)),
            done: Some(done),
        }
    }
}
#[cfg(test)]
thread_local! {
    /// #855 test hook: with_pump entries on this thread. Entry is the
    /// observable "the pump started": entering synchronously sets the
    /// activity timestamp and the first poll arms the 500 ms timer. A gate
    /// that rejects or is cancelled must never reach it, which no drain
    /// counter can prove — a parked or dropped pump never drains. The
    /// execution future is only ever polled by the consumer's next(), so
    /// under per-test threads (current_thread runtimes) this is race-free.
    static PUMP_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn pump_starts_for_test() -> usize {
    PUMP_STARTS.with(|starts| starts.get())
}

#[cfg(test)]
fn reset_pump_starts_for_test() {
    PUMP_STARTS.with(|starts| starts.set(0));
}

impl StreamHandler {
    async fn with_pump(
        &self,
        execution: impl std::future::Future<Output = Result<ExecuteResult, HostError>>,
        identity: ExecutionIdentity,
    ) -> Result<ExecuteResult, HostError> {
        #[cfg(test)]
        PUMP_STARTS.with(|starts| starts.set(starts.get() + 1));
        {
            let mut state = self.forwarding.lock().unwrap();
            state.parent = Some(identity);
            state.activity = Some(Instant::now());
        }
        tokio::pin!(execution);
        tokio::select! {
            biased;
            result = &mut execution => result,
            error = async {
                loop {
                    if self.forwarding.lock().unwrap().stopped { std::future::pending::<()>().await; }
                    let deadline = self.forwarding.lock().unwrap().deadline();
                    sleep_until(deadline).await;
                    let mut delivery = self.forwarding.lock().unwrap().reserve();
                    delivery.wait().await;
                    if self.forwarding.lock().unwrap().stopped || Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }
                    self.forwarding.lock().unwrap().activity = Some(Instant::now());
                    if let Err(error) = self.forward(Box::pin(async { Ok(()) }), None, Some(delivery), false, true).await { break error; }
                }
            } => {
                if let Some(cancel) = &self.callback_cancel {
                    cancel.cancel(None);
                    // Unlike dropping execute, awaiting it joins child/handler cleanup.
                    let _ = execution.await;
                }
                Err(error)
            },
        }
    }

    fn forward<T: Send + 'static>(
        &self,
        applied: HostFuture<T>,
        payload: Option<JsValue>,
        delivery: Option<Delivery>,
        module: bool,
        heartbeat: bool,
    ) -> HostFuture<T> {
        // Capture and validate synchronously at callback invocation. Even a
        // subsequently dropped callback must leave contradiction evidence.
        // Serialize capture/reservation with receive validation and closing
        // the inner port: dispose may run on another runtime thread.
        let mut state = self.forwarding.lock().unwrap();
        if heartbeat && state.stopped {
            return applied;
        }
        // A received wire payload owns the earlier slot. Leave later host
        // writes for the pump/module boundary instead of prepending them to it.
        let buffered = if payload.is_some() {
            Ok(Vec::new())
        } else {
            self.inner.drain_events()
        }
        .and_then(|events| {
            for event in &events {
                self.integrity
                    .observe_source(&JsValue::Object(event.clone()), false)
                    .map_err(|e| HostError::new(HostErrorKind::Protocol, e.message))?;
            }
            Ok(events)
        })
        .inspect_err(|error| {
            self.integrity
                .host_error
                .lock()
                .unwrap()
                .get_or_insert_with(|| error.message.clone());
        });
        if buffered.as_ref().is_ok_and(Vec::is_empty)
            && payload.is_none()
            && delivery.is_none()
            && !heartbeat
        {
            return applied; // pending model/tool calls must not stall wire events
        }
        // One send chain across module and wire callbacks: concurrent futures
        // cannot interleave batches or overtake an earlier callback's payload.
        let mut delivery = delivery.unwrap_or_else(|| state.reserve());
        let deadline = state.deadline();
        drop(state);
        let events = self.events.clone();
        let integrity = self.integrity.clone();
        let forwarding = self.forwarding.clone();
        let record_status = self.callback_cancel.is_some();
        Box::pin(async move {
            let buffered = buffered?;
            let sending = async move {
                delivery.wait().await;
                let buffered = forwarding.lock().unwrap().project_host(buffered, heartbeat);
                for (event, wire) in buffered
                    .into_iter()
                    .map(|e| (JsValue::Object(e), false))
                    .chain(payload.map(|e| (e, true)))
                {
                    let permit = events.reserve().await.map_err(|_| {
                        HostError::new(HostErrorKind::Callback, "LiveTurn event receiver closed.")
                    })?;
                    // A callback may have passed validation before a later host
                    // drain failed, then waited for ordering or channel capacity.
                    // Check and send atomically with respect to recording failure.
                    // TS awaits agent_status persistence before yield, but
                    // yields assistant/tool messages before the durable callback.
                    // All three await persistence before ACK; preserve the
                    // receipt across cancelled next() borrows (live-write-order).
                    let (receipt, recorded) = if record_status
                        && wire
                        && event
                            .get("type")
                            .and_then(JsValue::as_string)
                            .is_some_and(|kind| {
                                kind.equals_str("agent_status")
                                    || kind.equals_str("assistant_message")
                                    || kind.equals_str("tool_results_projected")
                            }) {
                        let (send, receive) = oneshot::channel();
                        (Some(send), Some(receive))
                    } else {
                        (None, None)
                    };
                    {
                        let error = integrity.host_error.lock().unwrap();
                        if let Some(error) = error.as_ref() {
                            return Err(HostError::new(HostErrorKind::Protocol, error));
                        }
                        permit.send((event, wire, receipt));
                    }
                    if let Some(recorded) = recorded {
                        recorded.await.map_err(|_| {
                            HostError::new(
                                HostErrorKind::Callback,
                                "LiveTurn status callback rejected.",
                            )
                        })?;
                    }
                    if !module {
                        forwarding.lock().unwrap().activity = Some(Instant::now());
                    }
                }
                drop(delivery); // wake the next sender only after the whole batch
                Ok(())
            };
            let mut applied = applied;
            if module {
                // A long module callback must not sequester its captured host
                // events. After the pump deadline, drive application and sends
                // concurrently; only the sending future owns the Delivery.
                tokio::select! {
                    biased;
                    result = &mut applied => { let result = result?; sending.await?; Ok(result) }
                    _ = sleep_until(deadline) => {
                        let (result, ()) = tokio::try_join!(applied, sending)?;
                        Ok(result)
                    }
                }
            } else {
                let result = applied.await?;
                sending.await?;
                Ok(result)
            }
        })
    }
}

impl HostHandler for StreamHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.forward(self.inner.module_call(call), None, None, true, false)
    }

    fn observe_loop_completion(&self, payload: &JsValue) {
        self.inner.observe_loop_completion(payload);
        *self.integrity.loop_completed.lock().unwrap() =
            payload.get("result").and_then(|r| r.get("type")) == Some(&"success".into());
    }

    fn validate_event(&self, payload: &JsValue) -> Result<EventGuard, HostError> {
        let mut state = self.forwarding.lock().unwrap();
        // Own lifecycle check first: an invalid event is cut off before the
        // inner contract sees it, exactly like it is cut off before
        // application. Then forward the inner handler's validation so wrapping
        // it never drops a rejection a directly-embedded handler enforces.
        self.integrity
            .observe_event(payload)
            .map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?;
        let inner = self.inner.validate_event(payload)?;
        let key = Arc::new(());
        let slot = state.reserve();
        state.wire.push_back((key.clone(), slot));
        drop(state);
        let forwarding = self.forwarding.clone();
        Ok(EventGuard::new(move || {
            // event() normally consumes this slot. Abandoning the queue or an
            // unpolled callback must release it too, without retaining handler.
            // Delivery::drop splices its unfinished predecessor into the send
            // chain instead of making a successor ready ahead of that work.
            let mut forwarding = forwarding.lock().unwrap();
            forwarding.wire.retain(|(id, _)| !Arc::ptr_eq(id, &key));
            drop(forwarding);
            drop(inner);
        }))
    }

    fn event(&self, event: JsValue) -> HostFuture<()> {
        // TS client:662 uses truthiness; the wire decoder separately enforces
        // a boolean final. Embedded callbacks reach this boundary directly.
        let final_event = event.get("final").is_some_and(super::submit::truthy);
        if final_event {
            self.forwarding.lock().unwrap().stopped = true;
        }
        let payload = event.get("payload").cloned();
        // TS readAgentEvent rejects this shape; wire inputs fail earlier in
        // validateModuleMessage. Embedded applications retain the reader guard.
        // hostAgentEventBridge is a separate event.emit producer: it swallows
        // known module failures, not wire/reader failures (ledger R4, #822).
        if !final_event && payload.as_ref().and_then(JsValue::as_object).is_none() {
            return Box::pin(async {
                Err(HostError::new(
                    HostErrorKind::Protocol,
                    "Sidecar event payload is not an AgentEvent.",
                ))
            });
        }
        let applied = self.inner.event(event);
        let delivery = {
            let mut forwarding = self.forwarding.lock().unwrap();
            if final_event {
                forwarding.reserve()
            } else {
                forwarding
                    .wire
                    .pop_front()
                    .map(|(_, slot)| slot)
                    .unwrap_or_else(|| forwarding.reserve())
            }
        };
        self.forward(
            applied,
            if final_event { None } else { payload },
            Some(delivery),
            false,
            false,
        )
    }

    fn dispose(&self) -> HostFuture<()> {
        // The inner turn closes its event port synchronously, before its
        // asynchronous cleanup completes. Never pump that closed port.
        self.forwarding.lock().unwrap().stopped = true;
        self.inner.dispose()
    }

    fn observe_transport(&self, observation: ReconnectObservation) {
        self.inner.observe_transport(observation);
    }
}

/// AgentLoop prefix/terminal validation, independently on receipt and delivery.
/// Host lifecycle events never justify a sidecar final. HostClient owns strict
/// wire sequence/identity and suppresses the loop's non-final turn_completed.
struct StreamIntegrity {
    state: Mutex<IntegrityState>,
    host_error: Mutex<Option<String>>,
    wire_events: Mutex<usize>,
    loop_completed: Mutex<bool>,
    module_id: Mutex<Option<JsString>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityState {
    Initial,
    Running,
    Failed,
    Completed,
}

impl StreamIntegrity {
    fn new() -> Self {
        Self {
            state: Mutex::new(IntegrityState::Initial),
            host_error: Mutex::default(),
            wire_events: Mutex::new(0),
            loop_completed: Mutex::new(false),
            module_id: Mutex::default(),
        }
    }

    fn observe_event(&self, event: &JsValue) -> Result<(), RuntimeError> {
        self.observe_source(event, true)
    }

    fn observe_source(&self, event: &JsValue, wire: bool) -> Result<(), RuntimeError> {
        if let Some(error) = self.host_error.lock().unwrap().as_ref() {
            return Err(RuntimeError::new(error));
        }
        let Some(event_type) = event.get("type").and_then(JsValue::as_string) else {
            return Err(RuntimeError::new(
                "Stream integrity violation: AgentEvent without a type.",
            ));
        };
        let mut state = self.state.lock().unwrap();
        if *state == IntegrityState::Completed {
            return Err(RuntimeError::new(
                "Stream integrity violation: event after the terminal (premature final).",
            ));
        }
        if super::runner::HOST_LIFECYCLE_EVENTS
            .iter()
            .any(|kind| event_type.equals_str(kind))
        {
            return Err(RuntimeError::new(
                "Stream integrity violation: host lifecycle event in AgentLoop stream.",
            ));
        }
        if event_type.equals_str("turn_completed") {
            return Err(RuntimeError::new(
                "Stream integrity violation: sidecar turn_completed must not reach the session stream; the module final is the authoritative terminal.",
            ));
        }
        if event_type.equals_str("turn_failed") {
            *state = IntegrityState::Failed;
        } else if wire && *state == IntegrityState::Initial {
            *state = IntegrityState::Running;
        }
        if wire {
            *self.wire_events.lock().unwrap() += 1;
        }
        Ok(())
    }

    /// Observe the authoritative terminal projected from ExecuteResult.
    /// `outcome` is the terminal result's type (success/error/max_turns/aborted).
    /// The check is strict against the lifecycle this instance observed: on the
    /// delivered prefix a success/error/max_turns terminal must be justified by
    /// events the consumer actually received (TS reads the final only after
    /// yielding every earlier event, agentLoopSidecarClient.ts:552-683), while
    /// on the receive-path instance the same predicate additionally rejects a
    /// terminal contradicting an event that was received but dropped before
    /// delivery. A cancellation request alone never rescues either. The final
    /// itself was already validated by the host layer.
    fn observe_terminal(&self, outcome: Option<&JsString>) -> Result<(), RuntimeError> {
        if let Some(error) = self.host_error.lock().unwrap().as_ref() {
            return Err(RuntimeError::new(error));
        }
        let outcome = outcome.map(|outcome| String::from_utf16_lossy(outcome.units()));
        let mut state = self.state.lock().unwrap();
        let current = *state;
        let accepted = match outcome.as_deref() {
            // Cancellation may stop a turn at any point, including before
            // turn_started reached the session (cancel before execute).
            Some("aborted") => true,
            // A success needs an observed loop prefix, never host start/accept.
            Some("success") => current == IntegrityState::Running,
            // TurnRunner.ts:209,256,282,437: every error path yields turn_failed.
            Some("error") => {
                current == IntegrityState::Failed && *self.wire_events.lock().unwrap() > 0
            }
            // AgentLoop.ts:2213-2224: max_turns yields status + turn_completed
            // only, without turn_failed; a loop prefix is still required.
            Some("max_turns") => current == IntegrityState::Running,
            // The host never validates unknown outcomes into a terminal.
            _ => false,
        };
        if accepted {
            *state = IntegrityState::Completed;
            Ok(())
        } else {
            Err(RuntimeError::new(format!(
                "Stream integrity violation: terminal without proper lifecycle (outcome: {:?}, state: {current:?}).",
                outcome
            )))
        }
    }

    fn observe_aborted(&self) -> Result<(), RuntimeError> {
        if let Some(error) = self.host_error.lock().unwrap().as_ref() {
            return Err(RuntimeError::new(error));
        }
        // Aborted turns may stop before any lifecycle event was observed,
        // including an input cancelled before execute was sent.
        *self.state.lock().unwrap() = IntegrityState::Completed;
        Ok(())
    }
}

fn parse_execute_request(
    input: &JsValue,
    options: &JsValue,
    history: Vec<JsValue>,
) -> Result<(ExecuteRequest, TurnInput), RuntimeError> {
    // Extract required fields from input/options
    let session_id = extract_string(options, "sessionId")?;
    let turn_id = extract_string(options, "turnId")?;
    let execution = options.get("execution").unwrap_or(options);
    let run_id = extract_string(execution, "runId").unwrap_or_else(|_| turn_id.clone());
    let operation_id = extract_string(execution, "operationId").unwrap_or_else(|_| turn_id.clone());
    let request_id = extract_string(options, "requestId").unwrap_or_else(|_| "default".into());

    let synthetic = match options.get("syntheticMessages") {
        None | Some(JsValue::Null | JsValue::Undefined) => vec![],
        Some(JsValue::Array(messages)) => messages.clone(),
        _ => return Err(RuntimeError::new("Expected syntheticMessages array.")),
    };
    let input = TurnInput::from_agent_input(input, options, history, synthetic)
        .map_err(|error| RuntimeError::new(error.to_string()))?;
    // Execution configuration is host-owned. Raw AgentInput cannot smuggle
    // an already-expanded message list or override execution parameters.
    let mut payload = JsObject::new();
    for (key, value) in options
        .as_object()
        .expect("identity checked above")
        .entries()
    {
        if !key.equals_str("syntheticMessages") {
            payload.insert(key.clone(), value.clone());
        }
    }
    payload.insert("messages".into(), JsValue::Array(input.loop_messages()));

    Ok((
        ExecuteRequest {
            identity: ExecutionIdentity {
                run_id,
                operation_id,
                request_id,
                session_id,
                turn_id,
            },
            payload,
            idempotency_key: execution
                .get("idempotencyKey")
                .and_then(JsValue::as_string)
                .cloned(),
            operation_deadline: execution
                .get("operationDeadline")
                .and_then(JsValue::as_string)
                .cloned(),
        },
        input,
    ))
}

fn extract_string(obj: &JsValue, key: &str) -> Result<JsString, RuntimeError> {
    match obj {
        JsValue::Object(map) => map
            .get(key)
            .and_then(|v| match v {
                JsValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::new(format!("Missing or invalid field: {}", key))),
        _ => Err(RuntimeError::new("Expected object")),
    }
}

fn mark_module_message(message: &mut JsValue, module: &JsString) -> Result<(), RuntimeError> {
    let kind = super::runner::classify_durable_message(message).map_err(RuntimeError::new)?;
    if kind != "durable_message"
        && let JsValue::Object(message) = message
    {
        let mut metadata = message
            .get("metadata")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default();
        metadata.insert("moduleId".into(), JsValue::String(module.clone()));
        message.insert("metadata".into(), JsValue::Object(metadata));
    }
    Ok(())
}

fn map_host_error(error: HostError) -> RuntimeError {
    if error.kind == HostErrorKind::Protocol
        && (error
            .message
            .starts_with("Sidecar event sequence mismatch:")
            || error.message == "Sidecar success final arrived before loop completion.")
    {
        RuntimeError::new(error.message)
    } else {
        RuntimeError::new(format!("Host execution failed: {}", error.message))
    }
}

#[cfg(test)]
pub(crate) fn test_recorder() -> Arc<Recorder> {
    use crate::storage::{JsonlTranscriptStore, TranscriptKey};
    let root = std::env::temp_dir().join(format!("pdrs-live-{}", uuid::Uuid::new_v4()));
    Arc::new(Recorder::new(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: root.clone(),
            pilot_home: root.join("home"),
            session_id: "session".into(),
            transcript_path: root.join("session.jsonl"),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_of(event_type: &str) -> JsValue {
        parse_json(&format!(r#"{{"type":"{event_type}"}}"#)).unwrap()
    }

    fn outcome(kind: &str) -> Option<JsString> {
        Some(JsString::from(kind))
    }

    #[test]
    fn success_sequence_with_intermediate_events_is_accepted() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("agent_status")).unwrap();
        integrity.observe_event(&event_of("tool_progress")).unwrap();
        integrity
            .observe_event(&event_of("file_artifacts"))
            .unwrap();
        integrity
            .observe_event(&event_of("steer_unapplied"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn early_failure_before_input_accepted_is_accepted() {
        // TurnRunner.ts:208-210: a transcript write failure ends the turn
        // before input_accepted with status, turn_failed, then the terminal.
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("agent_status")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn failure_after_input_accepted_is_accepted() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        // Legal tail events after turn_failed (TurnRunner.ts:431-438).
        integrity
            .observe_event(&event_of("file_artifacts"))
            .unwrap();
        integrity
            .observe_event(&event_of("steer_unapplied"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn second_turn_failed_before_the_error_terminal_is_accepted() {
        // TurnRunner.ts:397-402 forwards the loop's turn_failed; a post-loop
        // finalization step (:408-419) may then throw, and the catch (:437)
        // yields a second turn_failed before the same error terminal (:439).
        // hasRecordedVisibleFailureStatus dedupes only the status event, so
        // two turn_failed events are a legal TS stream.
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn max_turns_terminal_requires_no_turn_failed() {
        // AgentLoop.ts:2213-2224: max_turns yields status + turn_completed
        // only; requiring turn_failed here would falsely reject it.
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("max_turns").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn loop_events_need_no_host_lifecycle_prefix() {
        for first in ["model_request_started", "agent_status", "turn_failed"] {
            let integrity = StreamIntegrity::new();
            integrity.observe_event(&event_of(first)).unwrap();
            let kind = if first == "turn_failed" {
                "error"
            } else {
                "success"
            };
            assert!(
                integrity.observe_terminal(outcome(kind).as_ref()).is_ok(),
                "loop prefix accepted"
            );
        }
    }

    #[test]
    fn host_lifecycle_set_is_complete() {
        assert_eq!(
            super::super::runner::HOST_LIFECYCLE_EVENTS,
            ["turn_started", "input_accepted", "user_prompt_submitted"],
            "#936 host prefix set"
        );
        let integrity = StreamIntegrity::new();
        assert!(
            integrity
                .observe_event(&event_of("user_prompt_submitted"))
                .expect_err("#936 reject user_prompt_submitted")
                .message
                .contains("host lifecycle"),
            "#936 host prefix rejection"
        );
    }

    #[test]
    fn host_lifecycle_cannot_justify_a_sidecar_final() {
        let failed = StreamIntegrity::new();
        failed
            .observe_source(&event_of("turn_failed"), false)
            .unwrap();
        assert!(
            failed.observe_terminal(outcome("error").as_ref()).is_err(),
            "host failure is not loop evidence"
        );
        let host = StreamIntegrity::new();
        host.observe_source(&event_of("agent_status"), false)
            .unwrap();
        assert!(
            host.observe_terminal(outcome("success").as_ref()).is_err(),
            "host status is not loop evidence"
        );
        for kind in super::super::runner::HOST_LIFECYCLE_EVENTS {
            let integrity = StreamIntegrity::new();
            assert!(
                integrity
                    .observe_event(&event_of(kind))
                    .expect_err("host lifecycle leak must be rejected")
                    .message
                    .contains("host lifecycle")
            );
            assert!(
                integrity
                    .observe_terminal(outcome("success").as_ref())
                    .is_err(),
                "host lifecycle is not loop evidence"
            );
        }
    }

    #[test]
    fn sidecar_turn_completed_is_rejected_as_non_authoritative() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        let result = integrity.observe_event(&event_of("turn_completed"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("the module final is the authoritative terminal")
        );
    }

    #[test]
    fn event_after_terminal_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity
            .observe_terminal(outcome("success").as_ref())
            .unwrap();
        let result = integrity.observe_event(&event_of("model_event"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("premature final"));
    }

    #[test]
    fn event_without_a_type_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&JsValue::Object(JsObject::new()));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("without a type"));
    }

    #[test]
    fn success_terminal_without_loop_prefix_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_terminal(outcome("success").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn error_terminal_without_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        let result = integrity.observe_terminal(outcome("error").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn success_terminal_after_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_err()
        );
    }

    #[test]
    fn unknown_terminal_outcome_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("surprise").as_ref())
                .is_err()
        );
        assert!(integrity.observe_terminal(None).is_err());
    }

    #[test]
    fn second_terminal_projection_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity
            .observe_terminal(outcome("success").as_ref())
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_err()
        );
    }

    #[test]
    fn aborted_terminal_is_accepted_from_any_state() {
        for observations in [
            Vec::<&str>::new(),
            vec!["model_request_started"],
            vec!["model_request_started", "model_event"],
            vec!["model_request_started", "model_event", "turn_failed"],
        ] {
            let integrity = StreamIntegrity::new();
            for event_type in &observations {
                integrity.observe_event(&event_of(event_type)).unwrap();
            }
            assert!(
                integrity
                    .observe_terminal(outcome("aborted").as_ref())
                    .is_ok()
            );
            assert!(integrity.observe_aborted().is_ok());
            // After an aborted terminal the stream is closed.
            assert!(integrity.observe_event(&event_of("model_event")).is_err());
        }
    }

    #[test]
    fn max_turns_terminal_after_turn_failed_is_rejected() {
        // turn_failed is only ever followed by the same error terminal
        // (TurnRunner.ts:437-439); a max_turns final after it contradicts the
        // loop's max-turns path, which never emits turn_failed
        // (AgentLoop.ts:2218-2233).
        let integrity = StreamIntegrity::new();
        integrity
            .observe_event(&event_of("model_request_started"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("max_turns").as_ref())
                .is_err()
        );
    }

    #[test]
    fn a_final_missing_lifecycle_is_rejected_from_every_state() {
        // No cancellation flag can rescue these, on either instance: a missing
        // input_accepted or turn_failed is a sidecar violation regardless of
        // host cleanup. On the receive-path instance a matching delivery has no
        // bearing; on the delivered instance, events dropped by the host's
        // cancel exit are exactly what cannot serve as evidence.
        for observations in [Vec::<&str>::new()] {
            for kind in ["success", "max_turns"] {
                let integrity = StreamIntegrity::new();
                for event_type in &observations {
                    integrity.observe_event(&event_of(event_type)).unwrap();
                }
                assert!(
                    integrity.observe_terminal(outcome(kind).as_ref()).is_err(),
                    "{kind} after {observations:?} must require a loop prefix"
                );
            }
        }
        for observations in [Vec::<&str>::new(), vec!["model_request_started"]] {
            let integrity = StreamIntegrity::new();
            for event_type in &observations {
                integrity.observe_event(&event_of(event_type)).unwrap();
            }
            assert!(
                integrity
                    .observe_terminal(outcome("error").as_ref())
                    .is_err(),
                "error after {observations:?} must require turn_failed"
            );
        }
    }
}

#[cfg(test)]
#[path = "live_stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
#[path = "live_tail_tests.rs"]
mod tail_tests;

impl SessionState {
    pub(super) fn complete(&mut self, result: &JsValue) -> &'static str {
        self.status = match result.get("type").and_then(JsValue::as_string) {
            Some(kind) if kind.equals_str("aborted") => super::live_session::SessionStatus::Aborted,
            Some(kind) if kind.equals_str("error") => super::live_session::SessionStatus::Failed,
            _ => super::live_session::SessionStatus::Idle,
        };
        self.current_turn_id = None;
        if self.status == super::live_session::SessionStatus::Aborted {
            "other"
        } else {
            "prompt_input_exit"
        }
    }
    pub(super) fn fail(&mut self) {
        self.status = if self.signal.is_cancelled() {
            super::live_session::SessionStatus::Aborted
        } else {
            super::live_session::SessionStatus::Failed
        };
        self.current_turn_id = None;
    }
    pub(super) fn signal(&self) -> crate::runtime::hooks::Cancellation {
        self.signal.clone()
    }
}
