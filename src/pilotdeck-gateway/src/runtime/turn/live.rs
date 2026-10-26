//! LiveSession production adapter: HostClient → AgentEvent stream.
//!
//! This connects the gateway's submit pipeline to a real sidecar process via
//! the host module transport. TurnRunner.ts:195-217 is the baseline: after
//! recordAcceptedInput, the loop.run() generator yields AgentEvents until
//! turn_completed.
//!
//! StreamIntegrity tracks the turn lifecycle on top of the host's wire
//! sequence/identity checks: turn_started must open the stream, input_accepted
//! appears at most once, turn_failed may repeat (TurnRunner's catch re-emits it
//! after a finalization failure) and the module final is the only authoritative
//! terminal (the host already suppresses a sidecar's non-final turn_completed,
//! agentLoopSidecarClient.ts:680-684). Two observations of the same sequence
//! back two different guarantees:
//!
//! - a synchronous `HostHandler::validate_event` hook sees every event that
//!   passed wire validation on the host's receive path, even when its
//!   application callback never completed, so a misbehaving sidecar is cut off
//!   at its first violating event;
//! - the session separately records the events it actually delivers to its
//!   consumer, and `finish()` projects the authoritative terminal only when
//!   that delivered prefix justifies it. TS is a pull-based generator: the
//!   final is read only after every earlier event was yielded to the consumer
//!   (agentLoopSidecarClient.ts:552-683), so a terminal can never publish
//!   before its `input_accepted` / `turn_failed`. The host's bounded cancel
//!   exit (POST_CANCEL_EXIT_TIMEOUT) may drop queued applications and return an
//!   already-received final; the consumer really never saw that dropped
//!   lifecycle, so the terminal fails closed instead of reporting a success
//!   whose `commit_accepted_input` never ran. A cancellation request alone
//!   never rescues a terminal.
//!
//! It does not claim dropped-frame or duplicate-frame detection.

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
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity, parse_json};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

type TurnHost = (HostClient, Arc<dyn HostHandler>, Arc<RuntimeLease>);
type PreparedHost = (
    HostClient,
    Arc<dyn HostHandler>,
    Arc<RuntimeLease>,
    Option<JsObject>,
);
type TurnHostFactory =
    dyn Fn(ExecutionIdentity, Arc<Recorder>) -> HostFuture<PreparedHost> + Send + Sync;

/// Host-acquired authority for one turn. Acquire the project lease with
/// ProjectRuntimeRegistry::acquire when this factory is invoked, not when the
/// LiveSessionAdapter is created. The sidecar cannot supply any of these ports.
/// Assemble a fresh router from that lease and the factory's ExecutionIdentity
/// every time: model/budget/context and capability's execution, context,
/// observer and plan/todo ports retain per-turn state. InteractionTurn::bind
/// only replaces the interaction handlers; it cannot sanitize these ports.
pub struct InteractionHost {
    pub client: HostClient,
    pub lease: crate::runtime::project::RuntimeLease,
    pub router: crate::host::modules::ModuleRouter,
    pub input: super::interaction::InteractionTurnInput,
}

/// Production LiveSession backed by a fresh HostClient and handler for each turn.
/// HostClient closes its process and disposes its handler after each execution.
pub struct LiveSessionAdapter {
    host_factory: Arc<TurnHostFactory>,
    recorder: Arc<Recorder>,
    history: Arc<dyn Fn() -> Vec<JsValue> + Send + Sync>,
    state: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
struct SessionState {
    disposed: bool,
    active: Option<Cancellation>,
}

struct ActiveTurn(Arc<Mutex<SessionState>>);

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.0.lock().unwrap().active = None;
    }
}

impl LiveSessionAdapter {
    /// The factory must create turn-owned resources, including a fresh handler
    /// when its dispose implementation releases module resources.
    /// Pass the supplied recorder to model/tool durable ports; never construct
    /// a second writer. `history` reads the session's current projection.
    pub fn new(
        recorder: Arc<Recorder>,
        history: impl Fn() -> Vec<JsValue> + Send + Sync + 'static,
        host_factory: impl Fn(Arc<Recorder>) -> HostFuture<TurnHost> + Send + Sync + 'static,
    ) -> Self {
        Self {
            recorder,
            history: Arc::new(history),
            host_factory: Arc::new(move |_, recorder| {
                let host = host_factory(recorder);
                Box::pin(async move {
                    let (client, handler, lease) = host.await?;
                    Ok((client, handler, lease, None))
                })
            }),
            state: Arc::default(),
        }
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
            recorder,
            history: Arc::new(history),
            host_factory: Arc::new(move |identity, recorder| {
                let host = factory(identity.clone(), recorder);
                Box::pin(async move {
                    let host = host.await?;
                    let lease = Arc::new(host.lease);
                    let handler = super::interaction::InteractionTurn::bind(
                        lease.clone(),
                        &identity,
                        host.router,
                        host.input,
                    )
                    .await?;
                    let manifest = handler.manifest().clone();
                    Ok((
                        host.client,
                        Arc::new(handler) as Arc<dyn HostHandler>,
                        lease,
                        Some(manifest),
                    ))
                })
            }),
            state: Arc::default(),
        }
    }
}

impl LiveSession for LiveSessionAdapter {
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        // Invalid requests must not occupy the session's active turn slot.
        let (mut request, mut input) = parse_execute_request(&input, &options, vec![])?;
        let cancellation = Cancellation::default();
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
        }
        let active = Arc::new(ActiveTurn(self.state.clone()));
        // Read the projection only after admission owns the slot. Rejected
        // submissions must not observe history or race its current owner.
        input.history = (self.history)();
        request
            .payload
            .insert("messages".into(), JsValue::Array(input.loop_messages()));
        let owner = active.clone();
        let (recording_sender, recording_ready) = oneshot::channel();
        let host_factory = self.host_factory.clone();
        let recorder = self.recorder.clone();
        let identity = request.identity.clone();
        // Shared between the host-side validation hook (every received event)
        // and finish()'s second, contradiction-only check; see StreamHandler.
        let integrity = Arc::new(StreamIntegrity::new());
        let hook_integrity = integrity.clone();
        // Keep at most one delivered event ahead of the submit consumer.
        let (events, receiver) = mpsc::channel(1);
        Ok(Box::new(LiveTurnStream {
            execution: Some(Box::pin(async move {
                let (client, inner, lease, manifest) = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Ok(ExecuteResult::Aborted { terminal: None });
                    }
                    // select constructs every branch before polling; keep the
                    // factory call lazy so a pre-cancel never creates resources.
                    host = async { host_factory(request.identity.clone(), recorder.clone()).await } => host?,
                };
                let runner = TurnRunner::new(
                    recorder,
                    lease,
                    request.identity.session_id.clone(),
                    request.identity.turn_id.clone(),
                );
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
                });
                client
                    .execute_cancellable(request, handler, cancellation)
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
            accepted: false,
        }))
    }

    fn snapshot(&self) -> JsValue {
        // Return minimal snapshot; production sessions track state in transcript
        parse_json(r#"{"messages":[]}"#).unwrap()
    }

    fn abort(&self, reason: &JsString) -> RuntimeResult {
        if let Some(cancellation) = &self.state.lock().unwrap().active {
            cancellation.cancel(Some(reason.clone()));
        }
        Ok(())
    }

    fn dispose(&self) -> DisposeFuture<'_> {
        let mut state = self.state.lock().unwrap();
        state.disposed = true;
        if let Some(cancellation) = &state.active {
            cancellation.cancel(None);
        }
        Box::pin(async { Ok(()) })
    }
}

/// Streaming adapter for one turn execution.
struct LiveTurnStream {
    // Owned here rather than spawned: dropping the stream drops execute and
    // lets HostClient clean up its child and outstanding callbacks.
    execution: Option<HostFuture<ExecuteResult>>,
    result: Option<Result<ExecuteResult, HostError>>,
    events: mpsc::Receiver<JsValue>,
    identity: ExecutionIdentity,
    input: TurnInput,
    // The instance the host-side receive hook observes into. finish() also
    // consults it so a contradiction received but dropped before delivery (for
    // example turn_failed followed by success) still fails the terminal.
    integrity: Arc<StreamIntegrity>,
    // Lifecycle actually delivered to the consumer. finish() requires this
    // prefix to justify the terminal, like TS's pull-based final ordering.
    delivered: StreamIntegrity,
    // The last stream/runner/blocking-write owner releases the real slot.
    active: Option<Arc<ActiveTurn>>,
    recording_ready: oneshot::Receiver<Recording>,
    recording: Option<Recording>,
    // A cancelled next drops only its borrow, never the transition or payload.
    pending_event: Option<(JsValue, bool)>,
    accepted: bool,
}

impl LiveTurn for LiveTurnStream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if self.pending_event.is_some() {
                return self.settle_recording().await;
            }
            let mut delivered = None;
            if let Some(execution) = self.execution.as_mut() {
                let mut settled = None;
                tokio::select! {
                    // Drain events already applied by the handler before returning
                    // either the terminal or an execution error. A closed receiver
                    // disables this arm while execute finishes cleanup.
                    biased;
                    Some(event) = self.events.recv() => {
                        delivered = Some(event);
                    }
                    result = execution => settled = Some(result),
                }
                if let Some(result) = settled {
                    self.result = Some(result);
                    self.execution = None;
                }
            }
            if let Some(event) = delivered {
                return self.record_event(event, false).await;
            }
            // execute may have queued an event in the very poll that completed
            // it, after select already polled recv. Keep its result for next().
            if let Ok(event) = self.events.try_recv() {
                return self.record_event(event, false).await;
            }
            match self.result.take() {
                Some(result) => match self.finish(result) {
                    Ok(Some(event)) => self.record_event(event, true).await,
                    result => {
                        self.release();
                        result
                    }
                },
                None => Ok(None),
            }
        })
    }
}

impl LiveTurnStream {
    fn release(&mut self) {
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
        event: JsValue,
        terminal: bool,
    ) -> Result<Option<JsValue>, RuntimeError> {
        if let Ok(recording) = self.recording_ready.try_recv() {
            self.recording = Some(recording);
        }
        let action = match event.get("type").and_then(JsValue::as_string) {
            Some(kind) if kind.equals_str("turn_started") => Some(RecordingAction::Start),
            Some(kind) if kind.equals_str("input_accepted") => {
                Some(RecordingAction::Accept(self.input.clone()))
            }
            // Early failure/abort without accepted input belongs to the failure
            // integration slice. Preserve its existing terminal handling here.
            _ if terminal && self.accepted => Some(RecordingAction::Complete(
                event.get("result").cloned().unwrap_or(JsValue::Undefined),
            )),
            _ => None,
        };
        if let (Some(recording), Some(action)) = (self.recording.as_mut(), action) {
            if let Err(error) = recording.begin(action) {
                self.release();
                return Err(RuntimeError::new(error.to_string()));
            }
            self.pending_event = Some((event, terminal));
            return self.settle_recording().await;
        }
        if terminal {
            self.release();
            Ok(Some(event))
        } else {
            self.publish(event)
        }
    }

    async fn settle_recording(&mut self) -> Result<Option<JsValue>, RuntimeError> {
        let outcome = self
            .recording
            .as_mut()
            .expect("pending event owns recording")
            .settle()
            .await;
        let (event, terminal) = self.pending_event.take().expect("pending event");
        // A successful await may still be the runner's failure transition.
        // Only the requested committed transition can acknowledge this payload.
        let outcome = outcome.and_then(|events| {
            if events.len() == 1 && events[0].get("type") == event.get("type") {
                Ok(())
            } else {
                Err(crate::storage::recorder::RecorderError::State(
                    "Recording did not commit the pending transition.".into(),
                ))
            }
        });
        if let Err(error) = outcome {
            self.release();
            return Err(RuntimeError::new(error.to_string()));
        }
        if event.get("type") == Some(&"input_accepted".into()) {
            self.accepted = true;
        }
        if terminal {
            self.release();
            Ok(Some(event))
        } else {
            self.publish(event)
        }
    }

    /// Record an applied event as delivered and hand it to the consumer.
    /// Delivered order is a prefix of the receive order the hook already
    /// validated, so this cannot fail on the production path; propagating the
    /// error keeps a broken prefix from ever being projected as a terminal.
    fn publish(&self, event: JsValue) -> Result<Option<JsValue>, RuntimeError> {
        self.delivered.observe_event(&event)?;
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
                // TS reads the final only after every earlier event was
                // yielded (agentLoopSidecarClient.ts:552-683), so the consumer
                // here must have received a lifecycle that justifies the
                // terminal. The host's bounded cancel exit may return a
                // received-but-unapplied final after dropping queued
                // applications; the consumer never saw those events, so the
                // success/error terminal must fail closed (submit.rs commits
                // accepted input only on a delivered input_accepted).
                self.delivered.observe_terminal(outcome)?;
                // The received stream can still expose a contradiction the
                // consumer never got (for example a queued turn_failed before
                // a success final); keep this second, fail-closed gate so the
                // cancellation exit cannot hide it.
                self.integrity.observe_terminal(outcome)?;
                Ok(Some(self.completed_event(
                    terminal.get("payload").unwrap_or(&JsValue::Undefined),
                )))
            }
            Ok(ExecuteResult::Aborted { terminal }) => {
                self.integrity.observe_aborted()?;
                self.delivered.observe_aborted()?;
                let terminal = terminal.unwrap_or_else(|| self.aborted_result());
                Ok(Some(self.completed_event(&terminal)))
            }
            Ok(ExecuteResult::Rejected(error)) => Err(RuntimeError {
                message: "Turn execution rejected.".into(),
                causes: vec![RuntimeError::new(format!("{:?}", error))],
                js_error: None,
            }),
            Err(error) => Err(map_host_error(error)),
        }
    }

    fn completed_event(&self, terminal: &JsValue) -> JsValue {
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
        JsValue::Object(event)
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
            JsValue::Array(self.input.loop_messages()),
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
    events: mpsc::Sender<JsValue>,
    integrity: Arc<StreamIntegrity>,
    forwarding: Arc<Mutex<Forwarding>>,
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
impl StreamHandler {
    fn forward<T: Send + 'static>(
        &self,
        applied: HostFuture<T>,
        payload: Option<JsValue>,
        delivery: Option<Delivery>,
    ) -> HostFuture<T> {
        // Capture and validate synchronously at callback invocation. Even a
        // subsequently dropped callback must leave contradiction evidence.
        let buffered = self
            .inner
            .drain_events()
            .and_then(|events| {
                for event in &events {
                    self.integrity
                        .observe_event(&JsValue::Object(event.clone()))
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
        if buffered.as_ref().is_ok_and(Vec::is_empty) && payload.is_none() && delivery.is_none() {
            return applied; // pending model/tool calls must not stall wire events
        }
        // One send chain across module and wire callbacks: concurrent futures
        // cannot interleave batches or overtake an earlier callback's payload.
        let mut delivery = delivery.unwrap_or_else(|| self.forwarding.lock().unwrap().reserve());
        let events = self.events.clone();
        let integrity = self.integrity.clone();
        Box::pin(async move {
            let buffered = buffered?;
            let result = applied.await?;
            delivery.wait().await;
            for event in buffered.into_iter().map(JsValue::Object).chain(payload) {
                let permit = events.reserve().await.map_err(|_| {
                    HostError::new(HostErrorKind::Callback, "LiveTurn event receiver closed.")
                })?;
                // A callback may have passed validation before a later host
                // drain failed, then waited for ordering or channel capacity.
                // Check and send atomically with respect to recording failure.
                let error = integrity.host_error.lock().unwrap();
                if let Some(error) = error.as_ref() {
                    return Err(HostError::new(HostErrorKind::Protocol, error));
                }
                permit.send(event);
            }
            drop(delivery); // wake the next sender only after the whole batch
            Ok(result)
        })
    }
}

impl HostHandler for StreamHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.forward(self.inner.module_call(call), None, None)
    }

    fn validate_event(&self, payload: &JsValue) -> Result<EventGuard, HostError> {
        // Own lifecycle check first: an invalid event is cut off before the
        // inner contract sees it, exactly like it is cut off before
        // application. Then forward the inner handler's validation so wrapping
        // it never drops a rejection a directly-embedded handler enforces.
        self.integrity
            .observe_event(payload)
            .map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?;
        let inner = self.inner.validate_event(payload)?;
        let key = Arc::new(());
        {
            let mut forwarding = self.forwarding.lock().unwrap();
            let slot = forwarding.reserve();
            forwarding.wire.push_back((key.clone(), slot));
        }
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
        let payload = event.get("payload").cloned();
        // TS agentLoopSidecarClient.ts:1310-1317,1739-1743 rejects missing or
        // non-object AgentEvent payloads with this message. Wire shape/type
        // checks live in protocol/host; this also guards embedded callbacks.
        // Unlike hostAgentEventBridge.ts:35-49, this boundary rejects instead
        // of forwarding unchecked: A7.2-INTERACTION-CONSUME-001 (unapproved).
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
        )
    }

    fn dispose(&self) -> HostFuture<()> {
        self.inner.dispose()
    }

    fn observe_transport(&self, observation: ReconnectObservation) {
        self.inner.observe_transport(observation);
    }
}

/// Turn lifecycle validation against the TurnRunner baseline. The host layer
/// owns wire sequence/identity; this machine only tracks lifecycle order:
/// turn_started opens the turn, input_accepted appears at most once, turn_failed
/// follows it and may repeat before the error terminal (TurnRunner re-emits it), and the authoritative terminal is projected exactly
/// once by the session from ExecuteResult. One instance observes the receive
/// path (cut-off and contradiction gate); another observes only what the
/// consumer was actually delivered (the terminal justification). It does not
/// detect unknown dropped frames; the host's strict sequence check
/// (client:879-893) owns that.
struct StreamIntegrity {
    state: Mutex<IntegrityState>,
    host_error: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityState {
    /// No AgentEvent observed yet.
    Initial,
    /// turn_started observed.
    Started,
    /// input_accepted observed after turn_started.
    InputAccepted,
    /// turn_failed observed; it may legally repeat (TurnRunner's catch
    /// re-emits it after a finalization failure) and intermediate events may
    /// still arrive before the terminal.
    Failed,
    /// The authoritative terminal has been projected from ExecuteResult.
    Completed,
}

impl StreamIntegrity {
    fn new() -> Self {
        Self {
            state: Mutex::new(IntegrityState::Initial),
            host_error: Mutex::default(),
        }
    }

    /// Observe one AgentEvent received by the host (called from the
    /// `validate_event` hook on the receive path, in wire order, before the
    /// event is applied). Non-lifecycle events (model/tool/status/steer/
    /// file_artifacts/...) are legal while the turn is running; only their
    /// position relative to the lifecycle events and the terminal is
    /// constrained.
    fn observe_event(&self, event: &JsValue) -> Result<(), RuntimeError> {
        if let Some(error) = self.host_error.lock().unwrap().as_ref() {
            return Err(RuntimeError::new(error));
        }
        let Some(event_type) = event.get("type").and_then(JsValue::as_string) else {
            return Err(RuntimeError::new(
                "Stream integrity violation: AgentEvent without a type.",
            ));
        };
        let name = String::from_utf16_lossy(event_type.units());
        let mut state = self.state.lock().unwrap();
        // TurnRunner.ts:150: turn_started is always the first AgentEvent.
        if event_type.equals_str("turn_started") {
            return match *state {
                IntegrityState::Initial => {
                    *state = IntegrityState::Started;
                    Ok(())
                }
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: turn_started after the turn already opened (state: {other:?})."
                ))),
            };
        }
        // TurnRunner.ts:217: input_accepted follows turn_started exactly once.
        if event_type.equals_str("input_accepted") {
            return match *state {
                IntegrityState::Started => {
                    *state = IntegrityState::InputAccepted;
                    Ok(())
                }
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: input_accepted out of order (state: {other:?})."
                ))),
            };
        }
        // TurnRunner.ts:209,256,282,437: turn_failed precedes the terminal,
        // possibly before input_accepted (transcript write failure). A second
        // turn_failed is legal: TurnRunner.ts:397-402 forwards the loop's
        // turn_failed, and if a post-loop finalization step (:408-419) then
        // throws, the catch (:437) yields another turn_failed before the same
        // error terminal. hasRecordedVisibleFailureStatus dedupes only the
        // status event, not turn_failed itself.
        if event_type.equals_str("turn_failed") {
            return match *state {
                IntegrityState::Started | IntegrityState::InputAccepted => {
                    *state = IntegrityState::Failed;
                    Ok(())
                }
                IntegrityState::Failed => Ok(()),
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: turn_failed out of order (state: {other:?})."
                ))),
            };
        }
        // The host suppresses a sidecar's non-final turn_completed
        // (agentLoopSidecarClient.ts:680-684); only the module final projected
        // by this session is authoritative, so one reaching here is a
        // double-published terminal.
        if event_type.equals_str("turn_completed") {
            return Err(RuntimeError::new(
                "Stream integrity violation: sidecar turn_completed must not reach the session stream; the module final is the authoritative terminal.",
            ));
        }
        match *state {
            IntegrityState::Started | IntegrityState::InputAccepted | IntegrityState::Failed => {
                Ok(())
            }
            IntegrityState::Initial => Err(RuntimeError::new(format!(
                "Stream integrity violation: event '{name}' before turn_started."
            ))),
            IntegrityState::Completed => Err(RuntimeError::new(format!(
                "Stream integrity violation: event '{name}' after the terminal (premature final)."
            ))),
        }
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
            // TurnRunner.ts:217: a successful loop always accepted input.
            Some("success") => current == IntegrityState::InputAccepted,
            // TurnRunner.ts:209,256,282,437: every error path yields turn_failed.
            Some("error") => current == IntegrityState::Failed,
            // AgentLoop.ts:2213-2224: max_turns yields status + turn_completed
            // only, without turn_failed; input was still accepted.
            Some("max_turns") => current == IntegrityState::InputAccepted,
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
    let run_id = extract_string(options, "runId").unwrap_or_else(|_| turn_id.clone());
    let operation_id = extract_string(options, "operationId").unwrap_or_else(|_| "default".into());
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
            idempotency_key: None,
            operation_deadline: None,
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

fn map_host_error(error: HostError) -> RuntimeError {
    RuntimeError::new(format!("Host execution failed: {}", error.message))
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("max_turns").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn event_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&event_of("model_event"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("before turn_started"));
        // The terminal still fails closed afterwards.
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn duplicate_lifecycle_events_are_rejected() {
        // A second turn_started after the turn already opened.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        let result = integrity.observe_event(&event_of("turn_started"));
        assert!(result.unwrap_err().message.contains("already opened"));

        // A second input_accepted after the turn is running.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        let result = integrity.observe_event(&event_of("input_accepted"));
        assert!(result.unwrap_err().message.contains("out of order"));

        // A repeated turn_failed is NOT a duplicate: TurnRunner's catch may
        // re-emit it after a finalization failure before the same error
        // terminal (TurnRunner.ts:397-402, :437), unlike turn_started and
        // input_accepted, which appear exactly once.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn input_accepted_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&event_of("input_accepted"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("out of order"));
    }

    #[test]
    fn turn_failed_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        assert!(integrity.observe_event(&event_of("turn_failed")).is_err());
    }

    #[test]
    fn sidecar_turn_completed_is_rejected_as_non_authoritative() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
    fn success_terminal_without_input_accepted_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        let result = integrity.observe_terminal(outcome("success").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn error_terminal_without_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        let result = integrity.observe_terminal(outcome("error").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn success_terminal_after_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
            vec!["turn_started"],
            vec!["turn_started", "input_accepted"],
            vec!["turn_started", "input_accepted", "turn_failed"],
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
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
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
        for observations in [Vec::<&str>::new(), vec!["turn_started"]] {
            for kind in ["success", "max_turns"] {
                let integrity = StreamIntegrity::new();
                for event_type in &observations {
                    integrity.observe_event(&event_of(event_type)).unwrap();
                }
                assert!(
                    integrity.observe_terminal(outcome(kind).as_ref()).is_err(),
                    "{kind} after {observations:?} must require input_accepted"
                );
            }
        }
        for observations in [Vec::<&str>::new(), vec!["turn_started"]] {
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
