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
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::Poll,
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
            // TS looks runs up by Map key and by run-id equality. Rust keys
            // its run state with `JsString`, so a non-string `sessionKey`
            // cannot reach a live session here; whether TS misses too is *not*
            // settled — SameValueZero compares raw values, so a run stored
            // under `5` does match (same key-domain gap as
            // A7.8-INTERACTION-NONSTRING-OWNER-001-R1; tracked in #1465). What
            // must not happen is a non-string key degrading into an
            // unfiltered session abort, hence the early return.
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
    /// A builder error still runs the host teardown before the failure is
    /// reported: the coordinator it created is disposed and this cleanup is
    /// polled on the host thread (#1746). Neither await is unbounded — the
    /// refused-owner shutdown is capped at [`INITIALIZATION_TEARDOWN_DEADLINE`]
    /// and the cleanup is then given that same bound on its own clock, so the
    /// failure path can take up to twice it, but a cleanup that never completes
    /// cannot turn the start into a hang; the only case left to the embedding
    /// is a host thread that never started (runtime build or spawn failure).
    /// An embedding whose cleanup carries its own budget should use
    /// [`Self::start_with_cleanup_budget`] so that budget, not the default
    /// bound, decides how long a legitimate teardown may take.
    pub fn start_with_cleanup(
        options: TurnReplayOptions,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
        cleanup: impl Future<Output = Result<(), String>> + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock_and_cleanup(
            options,
            None,
            INITIALIZATION_TEARDOWN_DEADLINE,
            build,
            Some(Box::pin(cleanup)),
        )
    }
    /// [`Self::start_with_cleanup`] for an embedding that bounds its own
    /// teardown: the failure path gives the embedding cleanup `teardown_budget`
    /// plus [`INITIALIZATION_TEARDOWN_SLACK`], so a cleanup that legitimately
    /// runs to its own budget is never cut mid-poll by a shorter host bound.
    /// The refused-owner shutdown that runs before it is bounded separately.
    /// A `teardown_budget` too large for the clock — `Duration::MAX`, or any
    /// budget whose deadline `Instant::checked_add` overflows — is equivalent
    /// to none (see `declared_cleanup_budget` for the #1949 waiver) and
    /// explicitly waives #1746's no-hang guarantee: a cleanup that never
    /// settles keeps this start blocked forever, with the coordinator and
    /// interaction disposals and the failure report behind that unbounded
    /// await.
    pub fn start_with_cleanup_budget(
        options: TurnReplayOptions,
        teardown_budget: Duration,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
        cleanup: impl Future<Output = Result<(), String>> + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock_and_cleanup(
            options,
            None,
            declared_cleanup_budget(teardown_budget),
            build,
            Some(Box::pin(cleanup)),
        )
    }
    fn start_with_clock(
        options: TurnReplayOptions,
        clock: Option<Arc<dyn ReplayTimer>>,
        build: impl FnOnce(Arc<TurnEventCoordinator>) -> std::io::Result<GatewayServices>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_clock_and_cleanup(
            options,
            clock,
            INITIALIZATION_TEARDOWN_DEADLINE,
            build,
            None,
        )
    }
    fn start_with_clock_and_cleanup(
        options: TurnReplayOptions,
        clock: Option<Arc<dyn ReplayTimer>>,
        teardown_deadline: Duration,
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
                    Ok(services) => {
                        // #1746: the host refuses these services, so it also
                        // owns their teardown — the same owner/interactions
                        // pair the drain below disposes.
                        let error = std::io::Error::other("submitter must use the serial host coordinator");
                        dispose_initialization(
                            &coordinator,
                            Some(services),
                            cleanup,
                            teardown_deadline,
                        )
                        .await;
                        let _ = ready.send(Err(error));
                        return;
                    }
                    Err(error) => {
                        // #1746: a failed builder must not skip the teardown.
                        // Whatever the builder staged is already dropped with
                        // its closure, but the coordinator the host created
                        // and the embedding cleanup it was handed are still
                        // live; both settle here, before the failure is
                        // reported, exactly as the successful drain would.
                        dispose_initialization(&coordinator, None, cleanup, teardown_deadline).await;
                        let _ = ready.send(Err(error));
                        return;
                    }
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
                    // #1942: the drain polls the same third-party cleanup the
                    // failed-initialization teardown does, so a panic in it must
                    // not skip the coordinator and interaction disposals below
                    // either — the drain twin of the guard `dispose_initialization`
                    // uses.
                    if !crate::mutants::on("native-ws-skip-host-cleanup") && let Some(cleanup) = cleanup && let Err(failure) = caught_cleanup(cleanup, crate::mutants::on("native-ws-drain-cleanup-panic-uncaught")).await { error.get_or_insert(failure); }
                    error.map_or(Ok(()), Err)
                };
                // Preserve the original implicit-drop drain. Only an explicit
                // embedding shutdown opts into a deadline; a bound too large
                // for the clock, or within tokio's tick conversion of its
                // ceiling, is the #1949 waiver — see `declared_cleanup_budget`
                // and the tier decision `deadline_or_waiver` makes for both
                // poll sites — so the drain is awaited without scheduling one.
                // Tokio's own `timeout` saturates an overflowing budget into a
                // far-future timer and panics on a near-full one just as
                // `timeout_at` does — both register through the same unchecked
                // tick conversion; the explicit spelling instead takes the
                // waiver branch for both, matching `dispose_initialization`'s
                // bound.
                let result = match budget
                    .and_then(|budget| deadline_or_waiver(tokio::time::Instant::now(), budget))
                {
                    Some(deadline) => tokio::time::timeout_at(deadline, drain).await.unwrap_or_else(|_| Err("gateway host drain deadline exceeded".into())),
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
        self.enqueue_with_input_permit(text, None).await
    }
    /// Keep admitted input charged on the owning host job, including time in
    /// the job queue and business drain after the socket/waiter has gone away.
    pub(crate) async fn enqueue_with_input_permit(
        &self,
        text: String,
        input_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<Call<'static, Result<(), RequestError>>, RequestError> {
        // Field order also frees text before the lease on host-job cancellation.
        struct Input {
            text: String,
            permit: Option<tokio::sync::OwnedSemaphorePermit>,
        }
        impl Drop for Input {
            fn drop(&mut self) {
                if crate::mutants::on("ws-caps-cancel-request-leak") {
                    std::mem::forget(self.permit.take());
                }
            }
        }
        let mut input = Input {
            text,
            permit: input_permit,
        };
        if crate::mutants::on("ws-caps-admission-request-refund") {
            input.permit = None;
        }
        let connection = self.connection.clone();
        let (admit, admitted) = oneshot::channel();
        let (done, result) = oneshot::channel();
        self.host
            .jobs
            .send(Box::pin(async move {
                if crate::mutants::on("ws-caps-early-request-refund") {
                    input.permit = None;
                }
                let operation = connection.receive(&input.text);
                let _ = admit.send(());
                let outcome = operation.await;
                // Free the input before returning its capacity, even if the
                // completion receiver has been cancelled or is never polled.
                if crate::mutants::on("ws-caps-request-permit-leak") {
                    std::mem::forget(input.permit.take());
                }
                drop(input);
                let _ = done.send(outcome);
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

/// #1746: what a host that never finished initializing still owes. The order
/// mirrors the successful drain — submission owner, the embedding cleanup,
/// then the coordinator and its interactions — so a builder error cannot leave
/// live turns, a staged runtime registry or an undisposed coordinator behind.
/// The build closure's own captures are dropped when it returns; the services
/// it handed over (only the refusal branch) are disposed here.
///
/// #1746: this path runs before the embedding ever had a chance to hand in a
/// shutdown budget, and neither await may starve the other. The refused owner —
/// which may be bound to a host this thread does not drive — gets at most the
/// default teardown bound, and the embedding cleanup then gets its own full
/// deadline on a clock that starts after the shutdown, so an owner shutdown
/// that runs long cannot leave the cleanup with an already-expired deadline.
/// The deadline is not a fixed guess either:
/// [`TurnGateway::start_with_cleanup_budget`] derives it from the cleaning-up
/// embedding's own teardown budget, so the production cleanup — which bounds
/// itself by the drain budget — is never cut mid-poll by a shorter host bound.
/// It only keeps an embedding whose cleanup (or whose refused owner) never
/// settles from turning a fast builder failure into a hung
/// `start_with_cleanup`. The two synchronous disposals below always run.
///
/// #1942: the three fixture-only switches below exist so this cleanup has a
/// standing negative control, not just the positive
/// `a_builder_error_disposes_the_coordinator_and_the_embedding_cleanup`:
/// `native-ws-skip-initialization-cleanup` skips the whole teardown (the
/// coordinator and its interactions are left live),
/// `native-ws-skip-initialization-embedding-cleanup` skips only the embedding
/// cleanup poll, and `native-ws-initialization-cleanup-panic-uncaught` (see
/// [`caught_cleanup`]) lets a panicking cleanup escape instead of being
/// converted to an `Err`. Each is registered in the native-ws-entry roster
/// (`tests/fixtures/native-ws-entry/negative_controls.py`) and must be killed
/// by that test's own marker.
async fn dispose_initialization(
    coordinator: &TurnEventCoordinator,
    services: Option<GatewayServices>,
    cleanup: Option<Call<'static, Result<(), String>>>,
    teardown_deadline: Duration,
) {
    let skip_teardown = crate::mutants::on("native-ws-skip-initialization-cleanup");
    let skip_cleanup =
        skip_teardown || crate::mutants::on("native-ws-skip-initialization-embedding-cleanup");
    let interactions = services
        .as_ref()
        .map(|services| services.interactions.clone());
    let (owner_bound, cleanup_bound) = initialization_teardown_bounds(teardown_deadline);
    if let Some(services) = services {
        let shutdown_deadline = tokio::time::Instant::now() + owner_bound;
        let _ =
            tokio::time::timeout_at(shutdown_deadline, services.submitter.owner.shutdown()).await;
    }
    if !skip_cleanup && let Some(cleanup) = cleanup {
        // #1949: a bound too large for the clock (an embedding's
        // `Duration::MAX`, the idiomatic "no bound" a library caller can pass
        // straight into the public `start_with_cleanup_budget`) must not
        // overflow the deadline arithmetic. The std panic would kill the host
        // thread and turn the declared deadline semantics into an opaque
        // "gateway host initialization stopped" with the coordinator and its
        // interactions left live. A deadline past every representable instant
        // is equivalent to none, so the cleanup is awaited without one — the
        // embedding that declared MAX explicitly waived #1746's no-hang
        // guarantee, which only covers bounded budgets. A bound that fits but
        // lands within tokio's tick conversion of the ceiling is the same
        // waiver (`TICK_CONVERSION_HEADROOM`): registering it would panic,
        // not saturate.
        let uncaught = crate::mutants::on("native-ws-initialization-cleanup-panic-uncaught");
        match deadline_or_waiver(tokio::time::Instant::now(), cleanup_bound) {
            Some(cleanup_deadline) => {
                let _ =
                    tokio::time::timeout_at(cleanup_deadline, caught_cleanup(cleanup, uncaught))
                        .await;
            }
            None => {
                let _ = caught_cleanup(cleanup, uncaught).await;
            }
        }
    }
    if skip_teardown {
        return;
    }
    coordinator.dispose();
    if let Some(interactions) = interactions {
        interactions.dispose(&"gateway_disposed".into());
    }
}

/// #1942: the embedding cleanup is third-party code polled on the host
/// thread, so a panic in it must not skip the coordinator and interaction
/// disposals behind it — the same shape `runtime::replacement` gives its
/// port calls. Every poll (not just the synchronous prefix) is caught, and a
/// caught panic becomes an `Err` the callers already ignore, letting the
/// teardown finish. Both poll sites use it — the failed-initialization
/// teardown and the shutdown drain — and each call site reads its own
/// fixture-only switch (`crate::mutants::on`) at exactly one compiled site,
/// restoring the original unguarded poll at that site for the negative
/// control: `native-ws-initialization-cleanup-panic-uncaught` and
/// `native-ws-drain-cleanup-panic-uncaught`.
fn caught_cleanup(
    mut cleanup: Call<'static, Result<(), String>>,
    uncaught: bool,
) -> impl Future<Output = Result<(), String>> {
    std::future::poll_fn(move |cx| {
        match catch_unwind(AssertUnwindSafe(|| cleanup.as_mut().poll(cx))) {
            Ok(poll) => poll,
            Err(payload) => {
                if uncaught {
                    std::panic::resume_unwind(payload);
                }
                let message = crate::runtime::cleanup_task::panic_message(payload.as_ref())
                    .unwrap_or("embedding cleanup panicked");
                Poll::Ready(Err(message.to_owned()))
            }
        }
    })
}

/// #1746: the two awaits of [`dispose_initialization`] do not share a clock,
/// so neither can leave the other with no time at all. Returns
/// `(refused-owner shutdown bound, embedding cleanup bound)`: the owner is
/// capped at the default teardown bound (it is the host's own resource, not
/// something the embedding budgeted for), while the cleanup always gets the
/// full deadline its embedding declared.
fn initialization_teardown_bounds(teardown_deadline: Duration) -> (Duration, Duration) {
    (
        teardown_deadline.min(INITIALIZATION_TEARDOWN_DEADLINE),
        teardown_deadline,
    )
}

/// Default bound for the teardown a failed initialization runs before it
/// reports the failure (#1746), used when the embedding does not declare one.
/// Long enough for a real cleanup to finish and short enough that a stuck one
/// cannot hold `start_with_cleanup` open. Embeddings whose cleanup carries its
/// own budget pass it to [`TurnGateway::start_with_cleanup_budget`] instead.
#[cfg(not(test))]
const INITIALIZATION_TEARDOWN_DEADLINE: Duration = Duration::from_secs(30);
/// Tests drive the same bound with a value short enough to observe.
#[cfg(test)]
const INITIALIZATION_TEARDOWN_DEADLINE: Duration = Duration::from_millis(50);

/// Room on top of an embedding-declared teardown budget: that budget bounds the
/// embedding's cleanup alone, and the deadline handed to the host is a little
/// longer so the host's own timer never fires before the cleanup's inner one
/// (#1746). The refused-owner shutdown is bounded separately, by
/// [`INITIALIZATION_TEARDOWN_DEADLINE`].
#[cfg(not(test))]
const INITIALIZATION_TEARDOWN_SLACK: Duration = Duration::from_secs(5);
/// Tests observe the derived bound with values short enough to time.
/// #2482: the e2e wiring pair (`the_declared_cleanup_budget_reaches_the_
/// initialization_teardown` and `..._does_not_outlive_the_declared_bound`)
/// sleeps at SLACK-derived offsets around this slack, so its width *is* the
/// pair's discrimination margin. 10 ms left only 5–10 ms of margin between
/// the fixture sleep and the cut — a load spike flipped the boolean under
/// observation (a false FAIL), so the test constant is a full second: each
/// side of the window is now ~500 ms, two orders past scheduler jitter. The
/// exact bound arithmetic stays pinned deterministically by
/// `a_slow_refused_owner_does_not_shrink_the_cleanup_bound` (pure values,
/// no clock); the e2e pair only needs to catch gross wiring regressions.
#[cfg(test)]
const INITIALIZATION_TEARDOWN_SLACK: Duration = Duration::from_secs(1);

/// #1949: tokio turns a deadline into a timer tick by adding
/// `Duration::from_nanos(999_999)` through the *unchecked* `Instant +
/// Duration` at its `time/instant.rs:169` (tokio 1.48; the addition
/// `TimeSource::deadline_to_tick` makes before reading the tick), so a
/// deadline that fits the clock but sits within that addition of its ceiling
/// panics on registration instead of saturating. Measured on this tree's
/// tokio: `timeout_at` at the maximal fitting deadline panics
/// "overflow when adding duration to instant", and a `timeout(budget, …)`
/// call on a budget that fits but lands inside the same band panics there
/// too (an earlier probe that showed the old call surviving at the maximal
/// fitting budget was measuring the overflow tier: `timeout`'s own later
/// clock read pushed that budget past the ceiling, taking the far-future
/// branch). One second is far more than the nanoseconds tokio adds, so
/// both poll sites that hand a deadline to `timeout_at` require this much
/// room past it and treat anything closer to the ceiling as the same
/// #1949 waiver. `pub(crate)`: the post-halt walk (`switch_wiring.rs`)
/// filters its own shared deadline with the same slack instead of spelling
/// the literal a second time (#2363 fix-r2).
pub(crate) const TICK_CONVERSION_HEADROOM: Duration = Duration::from_secs(1);

/// #1949: the tier decision both poll sites share — the deadline a budget
/// would be awaited under, or `None` for the waiver (the waiver rule itself
/// is [`declared_cleanup_budget`]'s to state, once). A budget too large for
/// the clock overflows the first `checked_add`; one whose deadline still
/// fits but lands within [`TICK_CONVERSION_HEADROOM`] of the clock's
/// ceiling fails the second, because tokio converts a deadline into a
/// timer tick through the same unchecked addition there — the panic the
/// probe test measures. `now` is a parameter, not a read, so both callers
/// pass their own clock and the deterministic tier test can pin the
/// boundaries on one captured `now` (#2501: an e2e budget cannot — the site
/// reads its own, later clock).
fn deadline_or_waiver(now: tokio::time::Instant, budget: Duration) -> Option<tokio::time::Instant> {
    now.checked_add(budget)
        .filter(|deadline| deadline.checked_add(TICK_CONVERSION_HEADROOM).is_some())
}

/// The deadline a host actually gets out of an embedding-declared teardown
/// budget: the budget plus [`INITIALIZATION_TEARDOWN_SLACK`].
/// [`TurnGateway::start_with_cleanup_budget`] calls it on the way in and the
/// bound test calls it too, so the slack is pinned by the one function both
/// read instead of being copied into the test by hand (#1960).
/// #1949, stated once here for both poll sites that turn a declared budget
/// back into a deadline (`dispose_initialization`'s cleanup await and the
/// shutdown drain): a budget too large for the clock — an embedding's
/// `Duration::MAX`, which also saturates the slack addition above when it
/// flows through here — overflows [`deadline_or_waiver`]'s first
/// `checked_add`, and that overflow is the embedding's explicit waiver of the
/// deadline: the await runs without one, equivalent to none, so the embedding
/// that declared it opts out of #1746's no-hang guarantee, which covers
/// bounded budgets only. A budget that still fits the clock but lands
/// within [`TICK_CONVERSION_HEADROOM`] of its ceiling is the same explicit
/// waiver: tokio converts a deadline to a timer tick through an unchecked
/// addition a near-ceiling deadline overflows (a panic, not a saturation),
/// so the site awaits without a deadline there too — [`deadline_or_waiver`]
/// returns `None` for both tiers.
fn declared_cleanup_budget(teardown_budget: Duration) -> Duration {
    teardown_budget.saturating_add(INITIALIZATION_TEARDOWN_SLACK)
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
pub(crate) mod tests;
