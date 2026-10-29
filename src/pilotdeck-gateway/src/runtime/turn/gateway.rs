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
    current: HashMap<JsString, u64>,
    active: HashMap<u64, (Option<crate::storage::RunRef>, watch::Sender<bool>)>,
}
impl SubmissionDrains {
    fn admit(&mut self, id: u64, run: &crate::storage::RunRef) {
        let Some((identity, _)) = self.active.get_mut(&id) else {
            // The transport may drop before the owned submit reaches admission.
            return;
        };
        *identity = Some(run.clone());
        // begin overwrites the one session slot; obsolete drains never refill it.
        self.current.insert(run.session_key.clone(), id);
    }
}
struct SubmissionDrain {
    drains: Arc<Mutex<SubmissionDrains>>,
    id: u64,
}
impl SubmissionDrain {
    fn observer(&self) -> impl FnOnce(&crate::storage::RunRef) + Send + use<> {
        let drains = self.drains.clone();
        let id = self.id;
        move |run| drains.lock().unwrap().admit(id, run)
    }
}
impl Drop for SubmissionDrain {
    fn drop(&mut self) {
        let mut drains = self.drains.lock().unwrap();
        if let Some((run, done)) = drains.active.remove(&self.id) {
            if let Some(run) = run
                && drains.current.get(&run.session_key) == Some(&self.id)
            {
                drains.current.remove(&run.session_key);
            }
            done.send_replace(true);
        }
    }
}
struct ConnectionTurnGateway(GatewayServices, Arc<Mutex<SubmissionDrains>>);
impl ConnectionTurnGateway {
    fn track(&self) -> SubmissionDrain {
        let mut drains = self.1.lock().unwrap();
        let id = drains.next;
        drains.next += 1;
        drains.active.insert(id, (None, watch::channel(false).0));
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
            let drain = self.track();
            let mut submission = self.0.submitter.submit_observed(params, drain.observer());
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
            // TS uses strict Map keys and run-id equality. Non-string values
            // cannot match a live session/run; they must not become an
            // unfiltered session abort when converted to Option<JsString>.
            let Some(key) = params.get("sessionKey").and_then(JsValue::as_string) else {
                return Ok(());
            };
            if params
                .get("runId")
                .is_some_and(|run| !matches!(run, JsValue::String(_) | JsValue::Undefined))
            {
                return Ok(());
            }
            let run = params.get("runId").and_then(JsValue::as_string);
            let reason = params.get("reason").and_then(JsValue::as_string);
            let allowed = self
                .0
                .submitter
                .owner
                .start_gateway_abort(key, run, reason)
                .map_err(|error| RequestError::new(error.to_string()))?;
            if !allowed {
                return Ok(());
            }
            // Router.abort has returned. Model its async boundary before taking
            // the one current session slot, without first draining the owner.
            tokio::task::yield_now().await;
            let wait_for = {
                let drains = self.1.lock().unwrap();
                drains
                    .current
                    .get(key)
                    .and_then(|id| drains.active.get(id))
                    .map(|(_, done)| done.subscribe())
            };
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
    stop: watch::Sender<Option<Duration>>,
    stopped: watch::Receiver<Option<Result<(), String>>>,
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
    /// Keep the host executor alive while embedding-owned session/project
    /// resources dispose. The cleanup future must not enqueue new host jobs.
    /// Registered only after successful initialization: on a builder error,
    /// the caller remains responsible for disposing any partial resources.
    pub fn start_with_cleanup(
        options: TurnReplayOptions,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
        cleanup: impl Future<Output = Result<(), String>> + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock_and_cleanup(options, None, build, Some(Box::pin(cleanup)))
    }
    fn start_with_clock(
        options: TurnReplayOptions,
        clock: Option<Arc<dyn ReplayTimer>>,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock_and_cleanup(options, clock, build, None)
    }
    fn start_with_clock_and_cleanup(
        options: TurnReplayOptions,
        clock: Option<Arc<dyn ReplayTimer>>,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
        cleanup: Option<Call<'static, Result<(), String>>>,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (jobs, mut receive) = mpsc::unbounded_channel::<Job>();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let (stop, mut stopping) = watch::channel(None::<Duration>);
        let (finished, stopped) = watch::channel(None);
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
                let budget = loop {
                    tokio::select! {
                        _ = stopping.changed() => break *stopping.borrow(),
                        job = receive.recv() => match job { Some(job) => { running.spawn(job); }, None => break *stopping.borrow() },
                        _ = running.join_next(), if !running.is_empty() => {},
                    }
                };
                receive.close();
                while let Some(job) = receive.recv().await { running.spawn(job); }
                let drain = async {
                    let mut error = gateway.0.submitter.owner.shutdown().await.err().map(|e| e.to_string());
                    while let Some(result) = running.join_next().await {
                        if let Err(failure) = result { error.get_or_insert_with(|| failure.to_string()); }
                    }
                    if crate::mutants::on("native-ws-error-skips-cleanup") && error.is_some() { return error.map_or(Ok(()), Err); }
                    // One failed job must not skip embedding-owned disposals.
                    if !crate::mutants::on("native-ws-skip-host-cleanup") && let Some(cleanup) = cleanup && let Err(failure) = cleanup.await { error.get_or_insert(failure); }
                    error.map_or(Ok(()), Err)
                };
                // Preserve the original implicit-drop drain. Only an explicit
                // embedding shutdown opts into a deadline.
                let result = match budget {
                    Some(budget) => tokio::time::timeout(budget, drain).await.unwrap_or_else(|_| Err("gateway host drain deadline exceeded".into())),
                    None => drain.await,
                };
                running.abort_all();
                while running.join_next().await.is_some() {}
                coordinator.dispose();
                gateway.0.interactions.dispose(&"gateway_disposed".into());
                finished.send_replace(Some(result));
            });
        })?;
        let gateway = initialized
            .recv()
            .map_err(|_| std::io::Error::other("gateway host initialization stopped"))??;
        Ok(Self {
            jobs,
            gateway,
            stop,
            stopped,
        })
    }
    /// Signal shutdown synchronously, including from an embedding's Drop guard.
    pub fn request_shutdown(&self, budget: Duration) {
        self.stop.send_if_modified(|value| {
            if value.is_none() {
                *value = Some(budget);
                true
            } else {
                false
            }
        });
    }
    /// Explicitly stop the serial host, cancel owned turns and await cleanup.
    pub async fn shutdown(&self, budget: Duration) -> Result<(), String> {
        self.request_shutdown(budget);
        self.stopped().await
    }
    /// Observe shutdown without initiating it (also useful to await a cancelled embedding).
    pub async fn stopped(&self) -> Result<(), String> {
        let mut stopped = self.stopped.clone();
        loop {
            if let Some(result) = stopped.borrow_and_update().clone() {
                return result;
            }
            stopped
                .changed()
                .await
                .map_err(|_| "gateway host exited without drain result".to_owned())?;
        }
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
    /// Admit in caller order before returning the completion waiter. Only the
    /// synchronous receive prefix is fenced; a pending business await cannot
    /// block the next frame. Dropping either waiter never cancels the host job.
    pub async fn enqueue(
        &self,
        text: String,
    ) -> Result<Call<'static, Result<(), RequestError>>, RequestError> {
        let connection = self.connection.clone();
        let (admit, admitted) = oneshot::channel();
        let (done, result) = oneshot::channel();
        self.host
            .jobs
            .send(Box::pin(async move {
                let operation = connection.receive(&text);
                let _ = admit.send(());
                let _ = done.send(operation.await);
            }))
            .map_err(|_| RequestError::new("Gateway host stopped."))?;
        admitted
            .await
            .map_err(|_| RequestError::new("Gateway host operation stopped."))?;
        Ok(Box::pin(async move {
            result
                .await
                .map_err(|_| RequestError::new("Gateway host operation stopped."))?
        }))
    }
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
pub(crate) mod tests;
