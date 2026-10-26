//! Turn publication (InProcessGateway.ts:903-960,1207-1253,1681-1701,1783-1831).
//!
//! Replay is recorded synchronously; live delivery waits for durable append.
//! All host publications enter through TurnPublisher (including external emit),
//! serializing each TS synchronous segment before any await. Registry
//! implementations own durable write order, as the TS RunRegistryPort does. FileRegistry serializes operations off the async executor. No WS or
//! Module sequence is allocated here: registry entries alone own registry seq.
//!
//! Projected completion events are deferred until `finish` (session end). That
//! barrier snapshots pending writes like TS Promise.all: an external emit that
//! arrives during the drain may still append, but enqueue after close is a
//! no-op. `next` waits for those trailing writes before retaining replay and
//! detaching the sink. A result_unknown is an operational uncertainty, not a
//! second business completion or the final WS response (owned by connection).
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};

use pilotdeck_protocol::{JsString, JsValue};
use tokio::sync::{Notify, oneshot};

use super::coordinator::{TurnEventCoordinator, TurnSinkRegistration};
use super::projector::{
    GatewayAgentEventProjectionInput, GatewayAgentEventProjector, ProjectionResult,
};
use crate::metadata::object;

use crate::storage::{FileRunRegistry, RunAcceptInput, RunRef, RunRegistryError};

type RegistryResult<T> = Result<T, RunRegistryError>;
pub type RegistryFuture<T> = Pin<Box<dyn Future<Output = RegistryResult<T>> + Send + 'static>>;

/// Methods are invoked synchronously in host event order and return an owned
/// future. Ports must preserve durable FIFO like the TS FileRunRegistry.
/// FileRegistry guarantees this even with multiple Tokio/blocking workers;
/// publication also preserves live order if executor wakeups are reordered.
pub trait TurnRunRegistry: Send + Sync {
    fn accept(&self, input: &RunAcceptInput) -> RegistryFuture<bool>;
    fn events(&self, run: &RunRef) -> RegistryFuture<Vec<JsValue>>;
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()>;
}

type RegistryJob = Box<dyn FnOnce(&FileRunRegistry) + Send>;

/// FIFO adapter for the existing durable store; never performs disk I/O on a
/// Tokio executor thread. Dropping the adapter drains already accepted jobs.
pub struct FileRegistry {
    jobs: mpsc::Sender<RegistryJob>,
}
impl FileRegistry {
    pub fn new(registry: FileRunRegistry) -> std::io::Result<Self> {
        let (jobs, receiver) = mpsc::channel::<RegistryJob>();
        std::thread::Builder::new()
            .name("turn-run-registry".into())
            .spawn(move || {
                for job in receiver {
                    job(&registry);
                }
            })?;
        Ok(Self { jobs })
    }
    pub(crate) fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&FileRunRegistry) -> RegistryResult<T> + Send + 'static,
    ) -> RegistryFuture<T> {
        let (tx, rx) = oneshot::channel();
        let _ = self.jobs.send(Box::new(move |registry| {
            let _ = tx.send(operation(registry));
        }));
        Box::pin(async move {
            rx.await
                .map_err(|_| RunRegistryError::Invalid("Run registry worker stopped.".into()))?
        })
    }
}
impl TurnRunRegistry for FileRegistry {
    fn accept(&self, input: &RunAcceptInput) -> RegistryFuture<bool> {
        let input = input.clone();
        self.call(move |registry| registry.accept(&input).map(|accepted| accepted.duplicate))
    }
    fn events(&self, run: &RunRef) -> RegistryFuture<Vec<JsValue>> {
        let run = run.clone();
        self.call(move |registry| {
            registry.events(&run, None, None).map(|entries| {
                entries
                    .iter()
                    .map(|entry| field(entry, "event").clone())
                    .collect()
            })
        })
    }
    fn append(&self, run: &RunRef, event: &JsValue) -> RegistryFuture<()> {
        let run = run.clone();
        let event = event.clone();
        self.call(move |registry| registry.append(&run, &event))
    }
}

#[derive(Debug, PartialEq)]
pub enum TurnAcceptance {
    Accepted,
    /// Replay is returned as business events, without registry seq wrappers.
    Replay(Vec<JsValue>),
    Rejected(JsValue),
}

/// Request material is the caller's canonical execution fingerprint (submit
/// owns input normalization); a reconnect never starts a second pump.
pub async fn accept_turn(
    registry: Option<&dyn TurnRunRegistry>,
    input: &RunAcceptInput,
) -> TurnAcceptance {
    let Some(registry) = registry else {
        return TurnAcceptance::Accepted;
    };
    let result: RegistryResult<Option<Vec<JsValue>>> = async {
        if registry.accept(input).await? {
            Ok(Some(registry.events(&input.run).await?))
        } else {
            Ok(None)
        }
    }
    .await;
    match result {
        Ok(None) => TurnAcceptance::Accepted,
        Ok(Some(events)) => TurnAcceptance::Replay(events),
        Err(error) => {
            let conflict = error.code() == Some("conflict");
            let mut event = error_event(
                &input.run.run_id,
                if conflict {
                    "conflict"
                } else {
                    "result_unknown"
                },
                &error.to_string(),
                conflict,
            );
            if !conflict {
                insert(&mut event, "userHint", "Gateway acceptance could not be durably confirmed. Inspect the run state before retrying.".into());
            }
            TurnAcceptance::Rejected(event)
        }
    }
}

/// Preflight results also wait for durability, but never start replay/live state.
pub async fn persist_preflight_event(
    registry: Option<&dyn TurnRunRegistry>,
    run: &RunRef,
    event: JsValue,
) -> JsValue {
    if let Some(registry) = registry
        && registry.append(run, &event).await.is_err()
    {
        return persistence_failure(&run.run_id, true);
    }
    event
}

#[derive(Default)]
struct State {
    queue: VecDeque<JsValue>,
    deferred: Vec<JsValue>,
    pending: HashSet<u64>,
    next_write: u64,
    previous: Option<oneshot::Receiver<()>>,
    failure_published: bool,
    finishing: bool,
    closed: bool,
}
struct Publication {
    state: Mutex<State>,
    changed: Notify,
    registry: Option<Arc<dyn TurnRunRegistry>>,
    run: RunRef,
    executor: tokio::runtime::Handle,
}
impl Publication {
    fn enqueue(self: &Arc<Self>, event: JsValue) {
        let Some(registry) = &self.registry else {
            self.publish(event);
            return;
        };
        // Serialize request invocation, not the awaited I/O, so a second emit
        // can arrive during an append without holding any publication lock.
        let mut state = self.state.lock().unwrap();
        let write = state.next_write;
        state.next_write += 1;
        state.pending.insert(write);
        let future = registry.append(&self.run, &event);
        let previous = state.previous.take();
        let (done, receiver) = oneshot::channel();
        state.previous = Some(receiver);
        drop(state);
        let publication = Arc::clone(self);
        self.executor.spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            // Supervise only after the predecessor settles, preserving FIFO.
            // Panic follows the same failure publication and cleanup as Err.
            let result = publication
                .executor
                .spawn(future)
                .await
                .unwrap_or_else(|_| {
                    Err(RunRegistryError::Invalid(
                        "Run registry append task failed.".into(),
                    ))
                });
            let mut state = publication.state.lock().unwrap();
            match result {
                Ok(()) => {
                    if !state.closed {
                        state.queue.push_back(event);
                    }
                }
                Err(_) if !state.failure_published => {
                    state.failure_published = true;
                    if !state.closed {
                        state
                            .queue
                            .push_back(persistence_failure(&publication.run.run_id, false));
                    }
                }
                Err(_) => {}
            }
            state.pending.remove(&write);
            drop(state);
            publication.changed.notify_waiters();
            let _ = done.send(());
        });
    }
    fn publish(&self, event: JsValue) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.queue.push_back(event);
        }
        drop(state);
        self.changed.notify_waiters();
    }
    async fn wait_for(&self, writes: &HashSet<u64>) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.lock().unwrap().pending.is_disjoint(writes) {
                return;
            }
            notified.await;
        }
    }
}

/// One admitted turn. The caller owns the pump and calls finish once the
/// session generator (including SessionEnd hooks) has completed. Exactly one
/// consumer calls next; dropping the publisher unregisters its sink.
pub struct TurnPublisher {
    publication: Arc<Publication>,
    coordinator: Arc<TurnEventCoordinator>,
    _registration: TurnSinkRegistration,
    /// Serializes every host synchronous segment: pump, external emit, finish
    /// and consumer cleanup. The owned sink only enqueues; no I/O is awaited.
    pump: Mutex<()>,
}
impl TurnPublisher {
    /// Call only after accept_turn returns Accepted, inside a Tokio runtime.
    pub fn start(
        run: RunRef,
        registry: Option<Arc<dyn TurnRunRegistry>>,
        coordinator: Arc<TurnEventCoordinator>,
    ) -> Self {
        let publication = Arc::new(Publication {
            state: Mutex::new(State::default()),
            changed: Notify::new(),
            registry,
            run: run.clone(),
            executor: tokio::runtime::Handle::current(),
        });
        let weak = Arc::downgrade(&publication);
        let registration = coordinator.start(&run.session_key, &run.run_id, move |event| {
            if let Some(publication) = weak.upgrade() {
                publication.enqueue(event.clone());
            }
        });
        Self {
            publication,
            coordinator,
            _registration: registration,
            pump: Mutex::new(()),
        }
    }
    /// External interaction event: the same synchronous host boundary as pump
    /// record+enqueue. Hosts must use this entry point instead of calling the
    /// underlying coordinator's emit directly during an active publication.
    pub fn emit(&self, event: &JsValue) -> bool {
        let _pump = self.pump.lock().unwrap();
        self.coordinator
            .emit(&self.publication.run.session_key, event)
    }
    /// Already-derived pump events, serialized with external emits and finish.
    pub fn gateway_event(&self, event: JsValue) -> bool {
        let _pump = self.pump.lock().unwrap();
        self.enqueue_gateway_event(event)
    }
    fn enqueue_gateway_event(&self, event: JsValue) -> bool {
        let mut state = self.publication.state.lock().unwrap();
        if state.finishing {
            return false;
        }
        if is(&event, "type", "turn_completed") {
            state.deferred.push(event);
            return true;
        }
        drop(state);
        self.coordinator
            .record(&self.publication.run.session_key, &event);
        self.publication.enqueue(event);
        true
    }
    pub fn agent_event(
        &self,
        projector: &GatewayAgentEventProjector,
        event: &JsValue,
        forward_subagent_text: bool,
    ) -> ProjectionResult<()> {
        let _pump = self.pump.lock().unwrap();
        let run_id = String::from_utf16_lossy(self.publication.run.run_id.units());
        for event in projector.project(&GatewayAgentEventProjectionInput {
            event,
            run_id: &run_id,
            forward_subagent_text,
        })? {
            self.enqueue_gateway_event(event);
        }
        Ok(())
    }
    /// TS timeout calls queue.close() immediately after enqueueing its error.
    /// Already-started durable appends still settle; events completing after
    /// close cannot re-open live delivery. The owned pump drains separately.
    pub fn close_live(&self) {
        let _pump = self.pump.lock().unwrap();
        self.publication.state.lock().unwrap().closed = true;
        self.publication.changed.notify_waiters();
    }
    pub async fn finish(&self) {
        let writes = {
            let _pump = self.pump.lock().unwrap();
            let deferred = {
                let mut state = self.publication.state.lock().unwrap();
                if state.finishing {
                    return;
                }
                state.finishing = true;
                std::mem::take(&mut state.deferred)
            };
            for event in deferred {
                self.coordinator
                    .record(&self.publication.run.session_key, &event);
                self.publication.enqueue(event);
            }
            self.publication.state.lock().unwrap().pending.clone()
        };
        self.publication.wait_for(&writes).await;
        {
            let _pump = self.pump.lock().unwrap();
            self.publication.state.lock().unwrap().closed = true;
        }
        self.publication.changed.notify_waiters();
    }
    pub async fn next(&self) -> Option<JsValue> {
        loop {
            let notified = self.publication.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let trailing = {
                let _pump = self.pump.lock().unwrap();
                let mut state = self.publication.state.lock().unwrap();
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.closed {
                    Some(state.pending.clone())
                } else {
                    None
                }
            };
            if let Some(writes) = trailing {
                self.publication.wait_for(&writes).await;
                let _pump = self.pump.lock().unwrap();
                self.coordinator.retain_terminal(
                    &self.publication.run.session_key,
                    &self.publication.run.run_id,
                );
                return None;
            }
            notified.await;
        }
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn is(event: &JsValue, key: &str, expected: &str) -> bool {
    field(event, key)
        .as_string()
        .is_some_and(|s| s.equals_str(expected))
}
fn insert(event: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(event) = event {
        event.insert(key.into(), value);
    }
}
fn error_event(run: &JsString, code: &str, message: &str, recoverable: bool) -> JsValue {
    object([
        ("type", "error".into()),
        ("runId", JsValue::String(run.clone())),
        ("code", code.into()),
        ("message", message.into()),
        ("recoverable", JsValue::Bool(recoverable)),
    ])
}
fn persistence_failure(run: &JsString, preflight: bool) -> JsValue {
    let mut event = error_event(
        run,
        "result_unknown",
        if preflight {
            "Gateway could not durably persist the preflight result."
        } else {
            "Gateway could not durably persist the run result."
        },
        false,
    );
    insert(
        &mut event,
        "userHint",
        "Do not resubmit this run automatically. Inspect the Gateway run state before retrying."
            .into(),
    );
    event
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
