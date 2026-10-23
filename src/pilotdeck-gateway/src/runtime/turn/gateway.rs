//! Production connection wiring. All transport operations, submit pumps and
//! replay callbacks run on one current-thread host, preserving JS synchronous
//! steps. Awaiting a submit never blocks another request or socket close.
//!
//! WS seq belongs to GatewayConnection, durable seq to FileRunRegistry, and
//! Module sequence to the live adapter. This layer neither allocates nor strips
//! any of them. The live adapter and read-method handler remain explicit ports.
use super::{
    coordinator::TurnEventCoordinator,
    replay::{ReplayTimer, ThreadTimer, TurnReplayOptions, TurnReplayStore},
    submit::TurnSubmitter,
};
use crate::runtime::{
    connection::{
        Call, ConnectionGateway, GatewayConnection, GatewayTextConnection, RequestError,
        binding::WsBinding,
    },
    interaction::{GatewayInteractionCoordinator, InteractionBinding},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{AbortHandle, JoinSet},
};

type Callback = Arc<dyn Fn() + Send + Sync>;
type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Deadlines = Arc<Mutex<HashMap<JsString, u64>>>;

/// Deadline tasks only enqueue. The consumer runs callbacks on a later host
/// turn, after the current synchronous poll (including ready continuations).
/// Generation validation also cancels callbacks already detached from sleep.
struct HostSerialTimer {
    entries: Deadlines,
    sequence: AtomicU64,
    due: mpsc::UnboundedSender<(JsString, u64, Callback)>,
    driver: AbortHandle,
    clock: Arc<dyn ReplayTimer>,
}
impl HostSerialTimer {
    fn new() -> Self {
        Self::with_clock(Arc::new(ThreadTimer::new()))
    }
    fn with_clock(clock: Arc<dyn ReplayTimer>) -> Self {
        let entries: Deadlines = Arc::default();
        let pending = entries.clone();
        let (due, mut receiver) = mpsc::unbounded_channel::<(JsString, u64, Callback)>();
        let driver = tokio::spawn(async move {
            while let Some((key, serial, callback)) = receiver.recv().await {
                // An expiry is a macrotask: let ready host work finish first.
                tokio::task::yield_now().await;
                let current = {
                    let mut entries = pending.lock().unwrap();
                    if entries.get(&key).is_some_and(|id| *id == serial) {
                        entries.remove(&key);
                        true
                    } else {
                        false
                    }
                };
                if current {
                    callback();
                }
            }
        })
        .abort_handle();
        Self {
            entries,
            sequence: AtomicU64::new(0),
            due,
            driver,
            clock,
        }
    }
}
impl ReplayTimer for HostSerialTimer {
    fn set(&self, key: &JsString, delay: Duration, callback: Callback) {
        self.cancel(key);
        let serial = self.sequence.fetch_add(1, Ordering::Relaxed);
        let key = key.clone();
        let due = self.due.clone();
        self.entries.lock().unwrap().insert(key.clone(), serial);
        let deadline_key = key.clone();
        self.clock.set(
            &deadline_key,
            delay,
            Arc::new(move || {
                let _ = due.send((key.clone(), serial, callback.clone()));
            }),
        );
    }
    fn cancel(&self, key: &JsString) {
        self.entries.lock().unwrap().remove(key);
        self.clock.cancel(key);
    }
    fn cancel_all(&self) {
        self.entries.lock().unwrap().clear();
        self.clock.cancel_all();
    }
}
impl Drop for HostSerialTimer {
    fn drop(&mut self) {
        self.cancel_all();
        self.driver.abort();
    }
}

/// Non-submit methods are supplied by the existing host/read domain (pr7).
/// Calls execute on the same host as submission and interaction lifecycle.
pub trait GatewayRequests: Send + Sync {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>>;
    fn request(&self, method: JsString, params: JsValue)
    -> Call<'_, Result<JsValue, RequestError>>;
}
pub struct GatewayServices {
    pub submitter: Arc<TurnSubmitter>,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub requests: Arc<dyn GatewayRequests>,
}

/// Kept private so callers cannot poll business futures on another executor.
#[derive(Default)]
struct SubmissionDrains {
    next: u64,
    active: HashMap<
        u64,
        (
            watch::Receiver<Option<crate::storage::RunRef>>,
            watch::Sender<bool>,
        ),
    >,
}
struct SubmissionDrain {
    drains: Arc<Mutex<SubmissionDrains>>,
    id: u64,
}
impl Drop for SubmissionDrain {
    fn drop(&mut self) {
        if let Some((_, done)) = self.drains.lock().unwrap().active.remove(&self.id) {
            done.send_replace(true);
        }
    }
}
struct ConnectionTurnGateway(GatewayServices, Arc<Mutex<SubmissionDrains>>);
impl ConnectionTurnGateway {
    fn track(&self, identity: watch::Receiver<Option<crate::storage::RunRef>>) -> SubmissionDrain {
        let mut drains = self.1.lock().unwrap();
        let id = drains.next;
        drains.next += 1;
        drains
            .active
            .insert(id, (identity, watch::channel(false).0));
        SubmissionDrain {
            drains: self.1.clone(),
            id,
        }
    }
}
impl ConnectionGateway for ConnectionTurnGateway {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        self.0.requests.describe_server()
    }
    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        if method.equals_str("describe_server") {
            return self.describe_server();
        }
        if method.equals_str("abort_turn") {
            return Box::pin(async move {
                self.abort_on_close(params).await?;
                Ok(crate::metadata::object([("ok", JsValue::Bool(true))]))
            });
        }
        self.0.requests.request(method, params)
    }
    fn submit<'a>(
        &'a self,
        params: JsValue,
        emit: &'a (dyn Fn(JsValue) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>> {
        Box::pin(async move {
            // Signal after the consumer-finally fence. On this serial host,
            // GatewayConnection emits final/error in the same poll as this
            // future returns, before an abort waiter can resume.
            let mut submission = self.0.submitter.submit(params);
            let _drain = self.track(submission.identity());
            while let Some(event) = submission.next().await.map_err(|error| RequestError {
                code: error.code,
                message: error.message.into(),
                details: None,
                validation: None,
            })? {
                emit(event);
            }
            Ok(())
        })
    }
    fn disconnect(
        &self,
        session: JsValue,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>> {
        Box::pin(async move {
            let session = session
                .as_string()
                .ok_or_else(|| RequestError::new("sessionKey must be a string."))?;
            let binding = InteractionBinding::from_value(
                &pilotdeck_protocol::parse_json(&binding.to_json().to_string())
                    .expect("WS binding is JSON"),
            )
            .map_err(RequestError::new)?;
            let result = self
                .0
                .interactions
                .disconnect(session, &binding)
                .map_err(RequestError::new)?;
            Ok(result.get("preserveTurn") == Some(&JsValue::Bool(true)))
        })
    }
    fn abort_on_close(&self, params: JsValue) -> Call<'_, Result<(), RequestError>> {
        Box::pin(async move {
            let key = params
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .ok_or_else(|| RequestError::new("sessionKey must be a string."))?;
            let run = params.get("runId").and_then(JsValue::as_string);
            let reason = params.get("reason").and_then(JsValue::as_string);
            // A stale explicit run must not wait for an unrelated submission.
            // This check precedes the await; the completion selected below must
            // instead be the latest session fence *after* owner abort drains.
            if let Some(run) = run {
                let active = self.0.submitter.owner.active_turn_run_id(key);
                let matching = self
                    .1
                    .lock()
                    .unwrap()
                    .active
                    .iter()
                    .filter(|(_, (identity, _))| {
                        identity
                            .borrow()
                            .as_ref()
                            .is_some_and(|submitted| &submitted.session_key == key)
                    })
                    .max_by_key(|(id, _)| *id)
                    .is_some_and(|(_, (identity, _))| {
                        identity
                            .borrow()
                            .as_ref()
                            .is_some_and(|submitted| &submitted.run_id == run)
                    });
                if active.as_ref() != Some(run) && !matching {
                    return Ok(());
                }
            }
            self.0
                .submitter
                .owner
                .abort_turn(key, run, reason)
                .await
                .map_err(|error| RequestError::new(error.to_string()))?;
            // InProcessGateway.abortTurn waits by session after router.abort.
            // A replacement admitted during that await owns the current fence,
            // even if its runId differs or admission has already been cleared.
            let wait_for = self
                .1
                .lock()
                .unwrap()
                .active
                .iter()
                .filter(|(_, (identity, _))| {
                    identity
                        .borrow()
                        .as_ref()
                        .is_some_and(|submitted| &submitted.session_key == key)
                })
                .max_by_key(|(id, _)| *id)
                .map(|(_, (_, done))| done.subscribe());
            if let Some(mut done) = wait_for {
                let _ = done.wait_for(|done| *done).await;
            }
            Ok(())
        })
    }
}

/// Owns the production serial host. Build services here, using the supplied
/// coordinator, so SessionOwner and every spawned submit task share its runtime.
/// The builder must only construct dependencies; it must not start another host.
#[derive(Clone)]
pub struct TurnGateway {
    jobs: mpsc::UnboundedSender<Job>,
    gateway: Arc<ConnectionTurnGateway>,
}
impl TurnGateway {
    pub fn start(
        options: TurnReplayOptions,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock(options, None, build)
    }
    fn start_with_clock(
        options: TurnReplayOptions,
        clock: Option<Arc<dyn ReplayTimer>>,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (jobs, mut receive) = mpsc::unbounded_channel::<Job>();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new().name("turn-gateway-host".into()).spawn(move || {
            runtime.block_on(async move {
                let timer = Arc::new(clock.map_or_else(HostSerialTimer::new, HostSerialTimer::with_clock));
                let coordinator = Arc::new(TurnEventCoordinator::with_replay_store(
                    TurnReplayStore::with_options(options.with_timer(timer)),
                ));
                let services = match build(coordinator.clone()) {
                    Ok(services) if Arc::ptr_eq(&services.submitter.coordinator, &coordinator) => services,
                    Ok(_) => { let _ = ready.send(Err(std::io::Error::other("submitter must use the serial host coordinator"))); return; }
                    Err(error) => { let _ = ready.send(Err(error)); return; }
                };
                let gateway = Arc::new(ConnectionTurnGateway(services, Arc::default()));
                if ready.send(Ok(gateway.clone())).is_err() { return; }
                let mut running = JoinSet::new();
                loop {
                    tokio::select! {
                        job = receive.recv() => match job { Some(job) => { running.spawn(job); }, None => break },
                        _ = running.join_next(), if !running.is_empty() => {},
                    }
                }
                // No remaining transport handles: cancel owned turns and let
                // consumer/finally fences unwind before disposing replay.
                let _ = gateway.0.submitter.owner.shutdown().await;
                while running.join_next().await.is_some() {}
                coordinator.dispose();
                gateway.0.interactions.dispose(&"gateway_disposed".into());
            });
        })?;
        let gateway = initialized
            .recv()
            .map_err(|_| std::io::Error::other("gateway host initialization stopped"))??;
        Ok(Self { jobs, gateway })
    }
    async fn call<T: Send + 'static>(
        &self,
        operation: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, RequestError> {
        let (done, result) = oneshot::channel();
        self.jobs
            .send(Box::pin(async move {
                let _ = done.send(operation.await);
            }))
            .map_err(|_| RequestError::new("Gateway host stopped."))?;
        result
            .await
            .map_err(|_| RequestError::new("Gateway host operation stopped."))
    }
    pub fn connect(
        &self,
        text: Arc<dyn GatewayTextConnection>,
        token: String,
        server_version: String,
    ) -> HostedConnection {
        HostedConnection {
            host: self.clone(),
            connection: Arc::new(GatewayConnection::new(
                text,
                self.gateway.clone(),
                token,
                server_version,
            )),
        }
    }
    /// External permission/hooks also enter on the host; never expose the
    /// underlying coordinator to an external concurrent emitter.
    pub async fn emit(&self, session: JsString, event: JsValue) -> Result<bool, RequestError> {
        let gateway = self.gateway.clone();
        self.call(async move { gateway.0.submitter.coordinator.emit(&session, &event) })
            .await
    }
}
#[derive(Clone)]
pub struct HostedConnection {
    host: TurnGateway,
    connection: Arc<GatewayConnection>,
}
impl HostedConnection {
    pub async fn receive(&self, text: String) -> Result<(), RequestError> {
        let connection = self.connection.clone();
        self.host
            .call(async move { connection.receive(&text).await })
            .await?
    }
    pub async fn closed(&self) -> Result<(), RequestError> {
        let connection = self.connection.clone();
        self.host
            .call(async move { connection.closed().await })
            .await?
    }
    pub async fn notify(&self, name: String, payload: Option<JsValue>) -> Result<(), RequestError> {
        let connection = self.connection.clone();
        self.host
            .call(async move {
                connection.notify(&name, payload);
            })
            .await
    }
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
