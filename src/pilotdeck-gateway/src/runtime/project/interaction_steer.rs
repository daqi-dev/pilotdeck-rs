//! Session-owned mailboxes with opaque generation/session/run bindings.
//! Retain the project lease through session disposal and durable turn cleanup.
//! Host prerequisite: mutation and transcript recorders must complete in bounded
//! time (including their own I/O deadlines). Disposal preserves durable-first
//! ordering and cannot release the lease while an admitted recorder is pending.
use super::{DisposeFuture, FamilyResource, RuntimeError};
use crate::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::{ModuleRouter, tool_context::SteerAuthorizations},
    },
    runtime::{
        hooks::Cancellation,
        steer::{
            CancelResult, EnqueueResult, MutationRecorder, SteerMailbox, SteerMessage, TurnSteers,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{runtime::Handle, sync::watch};

type Closed = Result<Vec<SteerMessage>, HostError>;
#[derive(Default)]
struct State {
    closed: bool,
    releases: usize,
    sessions: HashMap<JsString, Session>,
    draining: Vec<Arc<Run>>,
}
struct Session {
    identity: Arc<()>,
    mailbox: SteerMailbox,
    run: Option<Arc<Run>>,
}
struct Run {
    id: JsString,
    turn_id: JsString,
    steers: TurnSteers,
    external: Cancellation,
    authorizations: Option<SteerAuthorizations>,
    executor: Handle,
    closed: AtomicBool,
    delivered: AtomicBool,
    releases: AtomicUsize,
    applying: tokio::sync::Mutex<()>,
    done: watch::Sender<Option<Closed>>,
    #[cfg(test)]
    close_events: Mutex<Option<std::sync::mpsc::Sender<&'static str>>>,
}
impl Run {
    fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let run = self.clone();
        crate::runtime::cleanup_task::spawn(&self.executor, self.done.clone(), async move {
            // A dropped consumer cannot abandon an admitted durable write.
            let _applying = run.applying.lock().await;
            let result = run.steers.mailbox.close(run.turn_id.clone()).await;
            run.steers.mailbox.finish(&run.turn_id);
            run.releases.fetch_add(1, Ordering::SeqCst);
            result
        });
        #[cfg(test)]
        self.close_event("closed");
    }
    #[cfg(test)]
    fn close_event(&self, event: &'static str) {
        if let Some(events) = self.close_events.lock().unwrap().as_ref() {
            let _ = events.send(event);
        }
    }
    async fn wait(&self) -> Closed {
        let mut done = self.done.subscribe();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result;
            }
            done.changed().await.map_err(|_| stale())?;
        }
    }
}

#[derive(Clone, Default)]
pub struct InteractionSteer(Arc<Mutex<State>>);
impl InteractionSteer {
    /// A recorder belongs to this session's transcript, never to wire input.
    pub fn bind_session(
        &self,
        key: JsString,
        recorder: Option<Arc<dyn MutationRecorder>>,
    ) -> Result<SessionSteer, HostError> {
        let executor = Handle::try_current()
            .map_err(|_| ModuleError::new("Steer binding requires a Tokio runtime."))?;
        let mut state = self.0.lock().unwrap();
        if state.closed || state.sessions.contains_key(&key) {
            return Err(stale());
        }
        state
            .draining
            .retain(|run| !matches!(&*run.done.borrow(), Some(Ok(_))));
        let identity = Arc::new(());
        state.sessions.insert(
            key.clone(),
            Session {
                identity: identity.clone(),
                mailbox: SteerMailbox::new(recorder),
                run: None,
            },
        );
        Ok(SessionSteer {
            owner: self.clone(),
            key,
            identity,
            executor,
            current_run: Arc::default(),
        })
    }
    pub fn close(&self) {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        state.releases += 1;
        for session in std::mem::take(&mut state.sessions).into_values() {
            if let Some(run) = session.run {
                run.close();
                state.draining.push(run);
            }
        }
    }
    pub fn is_closed(&self) -> bool {
        self.0.lock().unwrap().closed
    }
    pub fn release_count(&self) -> usize {
        self.0.lock().unwrap().releases
    }
}
impl FamilyResource for InteractionSteer {
    fn dispose(&self) -> DisposeFuture<'_> {
        self.close();
        let runs = self.0.lock().unwrap().draining.clone();
        Box::pin(async move {
            let mut failures = Vec::new();
            for run in runs {
                if let Err(error) = run.wait().await {
                    failures.push(RuntimeError::new(error.to_string()));
                }
            }
            RuntimeError::aggregate("Failed to dispose steer mailboxes.", failures)
        })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Clone)]
pub struct SessionSteer {
    owner: InteractionSteer,
    key: JsString,
    identity: Arc<()>,
    executor: Handle,
    current_run: Arc<Mutex<Option<Arc<Run>>>>,
}
impl SessionSteer {
    fn session<'a>(&self, state: &'a mut State) -> Result<&'a mut Session, HostError> {
        if state.closed {
            return Err(stale());
        }
        state
            .sessions
            .get_mut(&self.key)
            .filter(|s| Arc::ptr_eq(&s.identity, &self.identity))
            .ok_or_else(stale)
    }
    /// A new turn can start only after close + finish has drained the old one.
    /// The mailbox is reused by this session; equal textual IDs grant no authority.
    pub fn start_turn(
        &self,
        turn_id: JsString,
        run_id: JsString,
        messages: Vec<JsValue>,
        signal: Cancellation,
        router: &mut ModuleRouter,
    ) -> Result<TurnSteer, HostError> {
        let mut state = self.owner.0.lock().unwrap();
        let session = self.session(&mut state)?;
        if session
            .run
            .as_ref()
            .is_some_and(|run| !matches!(&*run.done.borrow(), Some(Ok(_))))
            || signal.is_cancelled()
        {
            return Err(stale());
        }
        session.mailbox.start(turn_id.clone());
        router.turn.authorizations = router
            .turn
            .authorizations
            .as_ref()
            .map(SteerAuthorizations::fresh_for_turn);
        let run = Arc::new(Run {
            id: run_id,
            turn_id: turn_id.clone(),
            steers: TurnSteers::new(session.mailbox.clone(), turn_id, messages),
            external: signal,
            authorizations: router.turn.authorizations.clone(),
            executor: self.executor.clone(),
            closed: AtomicBool::new(false),
            delivered: AtomicBool::new(false),
            releases: AtomicUsize::new(0),
            applying: tokio::sync::Mutex::new(()),
            done: watch::channel(None).0,
            #[cfg(test)]
            close_events: Mutex::default(),
        });
        session.run = Some(run.clone());
        *self.current_run.lock().unwrap() = Some(run.clone());
        let binding = TurnSteer {
            session: self.clone(),
            run,
        };
        let drain = binding.clone();
        router.turn.ports.drain_steer = Some(Arc::new(move || {
            let result = drain.drain(false).map(|(messages, _)| messages);
            Box::pin(async move { result })
        }));
        let drain = binding.clone();
        router.turn.ports.drain_or_close_steer = Some(Arc::new(move || {
            let result = drain.drain(true).map(|(messages, closed)| {
                let mut value = JsObject::new();
                value.insert(
                    "messages".into(),
                    JsValue::Array(messages.into_iter().map(JsValue::Object).collect()),
                );
                value.insert("closed".into(), JsValue::Bool(closed));
                value
            });
            Box::pin(async move { result })
        }));
        // Preserve compaction and the host's tool-checkpoint authorization table.
        let monitor = binding.run.clone();
        let owner = self.owner.clone();
        self.executor.spawn(async move {
            tokio::select! {
                _ = monitor.external.cancelled() => {
                    // Same admission lock as mutate: close must not pass the
                    // check -> mailbox queue insertion critical section.
                    let _state = match owner.0.try_lock() {
                        Ok(state) => state,
                        Err(std::sync::TryLockError::WouldBlock) => {
                            #[cfg(test)]
                            monitor.close_event("blocked");
                            owner.0.lock().unwrap()
                        }
                        Err(std::sync::TryLockError::Poisoned(error)) => panic!("{error}"),
                    };
                    monitor.close();
                },
                _ = monitor.wait() => {},
            }
        });
        Ok(binding)
    }
    pub fn dispose(&self) -> HostFuture<()> {
        let mut state = self.owner.0.lock().unwrap();
        if state
            .sessions
            .get(&self.key)
            .is_some_and(|s| Arc::ptr_eq(&s.identity, &self.identity))
            && let Some(run) = state.sessions.remove(&self.key).and_then(|s| s.run)
        {
            run.close();
            state.draining.push(run);
        }
        let run = self.current_run.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(run) = run {
                run.wait().await?;
            }
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct TurnSteer {
    session: SessionSteer,
    run: Arc<Run>,
}
impl TurnSteer {
    fn check_identity(&self, state: &mut State) -> Result<(), HostError> {
        let session = self.session.session(state)?;
        if !session
            .run
            .as_ref()
            .is_some_and(|run| Arc::ptr_eq(run, &self.run))
        {
            return Err(stale());
        }
        Ok(())
    }
    fn check(&self, state: &mut State) -> Result<(), HostError> {
        self.check_identity(state)?;
        if self.run.closed.load(Ordering::SeqCst) || self.run.external.is_cancelled() {
            return Err(stale());
        }
        Ok(())
    }
    pub fn run_id(&self) -> &JsString {
        &self.run.id
    }
    pub fn enqueue(&self, message: SteerMessage) -> HostFuture<EnqueueResult> {
        self.mutate(|run| run.steers.mailbox.enqueue(run.turn_id.clone(), message))
    }
    pub fn cancel(&self, item_id: JsString) -> HostFuture<CancelResult> {
        self.mutate(|run| run.steers.mailbox.cancel(run.turn_id.clone(), item_id))
    }
    fn mutate<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Run) -> HostFuture<T>,
    ) -> HostFuture<T> {
        let mut state = self.session.owner.0.lock().unwrap();
        let future = self.check(&mut state).map(|()| operation(&self.run));
        let turn = self.clone();
        Box::pin(async move {
            let result = future?.await?;
            turn.check(&mut turn.session.owner.0.lock().unwrap())?;
            Ok(result)
        })
    }
    fn drain(&self, close: bool) -> Result<(Vec<JsObject>, bool), HostError> {
        let mut state = self.session.owner.0.lock().unwrap();
        self.check_identity(&mut state)?;
        if self.run.closed.load(Ordering::SeqCst) && !self.run.external.is_cancelled() {
            return Err(stale());
        }
        let (messages, closed) = if close {
            let result = self.run.steers.drain_or_close();
            (result.messages, result.closed)
        } else {
            (self.run.steers.drain(), false)
        };
        Ok((
            messages.iter().map(SteerMessage::to_object).collect(),
            closed,
        ))
    }
    /// Host event consumer: claim -> durable transcript -> grant -> append/ack.
    /// The item and grant paths come from this run's drained message, not wire IDs.
    pub fn persist_and_apply(
        &self,
        item_id: JsString,
        message: JsValue,
        record: impl FnOnce(JsValue) -> HostFuture<()> + Send + 'static,
    ) -> HostFuture<()> {
        let turn = self.clone();
        let task = self.session.executor.spawn(async move {
            let _applying = turn.run.applying.lock().await;
            {
                let mut state = turn.session.owner.0.lock().unwrap();
                turn.check(&mut state)?;
                if message
                    .get("metadata")
                    .and_then(|v| v.get("queueItemId"))
                    .and_then(JsValue::as_string)
                    != Some(&item_id)
                    || !turn
                        .run
                        .steers
                        .unacknowledged()
                        .iter()
                        .any(|m| m.item_id == item_id)
                {
                    return Err(stale());
                }
            }
            let checked = turn.clone();
            turn.run
                .steers
                .on_durable_message(message, move |message| {
                    let valid = checked.check(&mut checked.session.owner.0.lock().unwrap());
                    Box::pin(async move {
                        valid?;
                        record(message).await
                    })
                })
                .await?;
            // A successful durable write commits delivery, even if cancellation
            // or session disposal happened during record. `applying` prevents
            // close/finish/restart; these objects belong to this exact run, so
            // ack and grants cannot target a replacement session or run.
            if let Some(authorizations) = &turn.run.authorizations {
                authorizations.apply(&item_id);
            }
            turn.run.steers.on_steer_applied(&item_id);
            Ok(())
        });
        Box::pin(async move { task.await.map_err(|e| ModuleError::new(e.to_string()))? })
    }
    pub fn messages(&self) -> Vec<JsValue> {
        self.run.steers.messages()
    }
    pub fn close(&self) -> HostFuture<Vec<SteerMessage>> {
        let _state = self.session.owner.0.lock().unwrap();
        self.run.close();
        let run = self.run.clone();
        Box::pin(async move {
            let discarded = run.wait().await?;
            Ok(if run.delivered.swap(true, Ordering::SeqCst) {
                vec![]
            } else {
                discarded
            })
        })
    }
    pub fn release_count(&self) -> usize {
        self.run.releases.load(Ordering::SeqCst)
    }
}
fn stale() -> HostError {
    ModuleError::new("Steer session/run is closed, busy or superseded.").into()
}
pub(super) fn close_resource(
    resource: Option<Arc<dyn FamilyResource>>,
) -> Result<Option<InteractionSteer>, RuntimeError> {
    resource
        .map(|resource| {
            let steer = resource
                .as_any()
                .downcast_ref::<InteractionSteer>()
                .ok_or_else(|| RuntimeError::new("Invalid interaction steer resource"))?
                .clone();
            steer.close();
            Ok(steer)
        })
        .transpose()
}

#[cfg(test)]
mod tests;
