//! The channel lifecycle portion of baseline `cli/pilotdeckServer.ts`.
//!
//! Operations run independently of their callers' result futures, like JS
//! promises. Each key has a FIFO driver; idle drivers and their map entries are
//! removed. Call from a Tokio runtime and close before shutting that runtime down.
use super::protocol::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

struct Running {
    key: ChannelKey,
    adapter: Arc<dyn ChannelAdapter>,
    handle: Arc<dyn ChannelHandle>,
}
#[derive(Default)]
struct State {
    closing: bool,
    running: Vec<Running>, // TS Map insertion order, including stop failure restoration.
}
enum Operation {
    Replace(Arc<dyn ChannelAdapter>),
    Disable,
}
struct Request {
    operation: Operation,
    result: oneshot::Sender<Result<bool, ChannelError>>,
}
#[derive(Default)]
struct Reports {
    pending: VecDeque<ChannelStatus>,
    dispatching: bool,
}
struct Inner {
    state: Mutex<State>,
    queues: Mutex<HashMap<ChannelKey, mpsc::UnboundedSender<Request>>>,
    deps: ChannelStartDeps,
    reports: Mutex<Reports>,
    close_host: CloseHost,
    closed: watch::Sender<Option<Result<(), ChannelError>>>,
}
#[derive(Clone)]
pub struct ChannelServer(Arc<Inner>);

impl ChannelServer {
    /// Creates an unopened component. `start_channels` performs TS background startup.
    pub fn new(deps: ChannelStartDeps, close_host: CloseHost) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            queues: Mutex::new(HashMap::new()),
            deps,
            reports: Mutex::new(Reports::default()),
            close_host,
            closed: watch::Sender::new(None),
        }))
    }

    pub fn start_channels(&self, channels: Vec<Arc<dyn ChannelAdapter>>) {
        for channel in channels {
            let key = channel.channel_key().clone();
            let operation = self.hot_start_channel(channel);
            let inner = self.0.clone();
            tokio::spawn(async move {
                match operation.await {
                    Ok(()) => inner.log(
                        "info",
                        &format!("[adapters] channel {key} startup task completed"),
                    ),
                    Err(error) => inner.log(
                        "error",
                        &format!("[adapters] channel {key} start failed: {}", error.message()),
                    ),
                }
            });
        }
    }

    pub fn hot_start_channel(
        &self,
        channel: Arc<dyn ChannelAdapter>,
    ) -> BoxFuture<Result<(), ChannelError>> {
        if self.0.state.lock().expect("channel state").closing {
            return Box::pin(async { Err(closing_error()) });
        }
        let key = channel.channel_key().clone();
        let result = enqueue(&self.0, key, Operation::Replace(channel));
        Box::pin(async move { receive(result).await.map(|_| ()) })
    }

    pub fn reconcile_channels(
        &self,
        input: ReconcileInput,
    ) -> BoxFuture<Result<ReconcileResult, ChannelError>> {
        if self.0.state.lock().expect("channel state").closing {
            return Box::pin(async { Err(closing_error()) });
        }
        let mut desired = HashSet::new();
        for channel in &input.channels {
            if !desired.insert(channel.channel_key().clone()) {
                let error = ChannelError::error(format!(
                    "duplicate channel adapter in reconciliation: {}",
                    channel.channel_key()
                ));
                return Box::pin(async move { Err(error) });
            }
        }
        let mut managed = HashSet::new();
        let mut operations = Vec::new();
        for key in input.managed_channel_keys {
            if managed.insert(key.clone()) && !desired.contains(&key) {
                operations.push((key, Operation::Disable));
            }
        }
        for channel in input.channels {
            operations.push((channel.channel_key().clone(), Operation::Replace(channel)));
        }
        let inner = self.0.clone();
        let mut operations = operations.into_iter();
        // JS async functions execute through their first await before returning.
        let first = operations.next().map(|(key, operation)| {
            let replacing = matches!(operation, Operation::Replace(_));
            let result = enqueue(&inner, key.clone(), operation);
            (key, replacing, result)
        });
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut outcome = ReconcileResult::default();
            let mut next = first;
            let result = async {
                while let Some((key, replacing, result)) = next {
                    let changed = receive(result).await?;
                    if replacing {
                        outcome.started.push(key);
                    } else if changed {
                        outcome.stopped.push(key);
                    }
                    next = operations.next().map(|(key, operation)| {
                        let replacing = matches!(operation, Operation::Replace(_));
                        let result = enqueue(&inner, key.clone(), operation);
                        (key, replacing, result)
                    });
                }
                Ok(outcome)
            }
            .await;
            let _ = tx.send(result);
        });
        Box::pin(receive(rx))
    }

    /// Eager and idempotent. Claims handles before invoking stop, so concurrent
    /// replacement and shutdown cannot stop the same handle twice. Like TS,
    /// close does not wait for a start that has not yet produced a live handle;
    /// that start stops its own late handle and rejects when it eventually resolves.
    pub fn close(&self) -> BoxFuture<Result<(), ChannelError>> {
        let claimed = {
            let mut state = self.0.state.lock().expect("channel state");
            if state.closing {
                return Box::pin(wait_closed(self.0.clone()));
            }
            state.closing = true;
            std::mem::take(&mut state.running)
        };
        let mut stops = Vec::new();
        for entry in claimed {
            let stop = begin_stop(entry.handle.clone(), "server-shutdown");
            let inner = self.0.clone();
            stops.push(tokio::spawn(async move {
                finish_stop(&inner, entry, stop, "server-shutdown").await
            }));
        }
        let inner = self.0.clone();
        tokio::spawn(async move {
            let mut errors = Vec::new();
            // Futures are already running concurrently; collect errors in Map order.
            for stop in stops {
                match stop.await {
                    Ok(Ok(_)) => (),
                    Ok(Err(error)) => errors.push(error),
                    Err(error) => errors.push(ChannelError::error(format!(
                        "channel stop task failed: {error}"
                    ))),
                }
            }
            let close_host = inner.close_host.clone();
            if let Err(error) = task_result(
                tokio::spawn(async move { close_host().await }),
                "host close",
            )
            .await
            {
                errors.push(error);
            }
            let result = if errors.is_empty() {
                Ok(())
            } else {
                Err(ChannelError::Aggregate {
                    message: "PilotDeck server shutdown failed".into(),
                    errors,
                })
            };
            inner.closed.send_replace(Some(result));
        });
        Box::pin(wait_closed(self.0.clone()))
    }
}

impl Inner {
    fn log(&self, level: &str, text: &str) {
        if let Some(log) = &self.deps.log {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log(level, text)));
        }
    }
    // Queue under the state lock at each linearization point. Callbacks run
    // outside both locks, in FIFO order, and may reenter the lifecycle API.
    fn queue_report(
        &self,
        key: &ChannelKey,
        state: ChannelState,
        message: String,
        error: Option<String>,
    ) {
        self.reports
            .lock()
            .expect("channel reports")
            .pending
            .push_back(ChannelStatus {
                key: key.clone(),
                state,
                message,
                error,
            });
    }
    fn flush_reports(&self) {
        {
            let mut reports = self.reports.lock().expect("channel reports");
            if reports.dispatching {
                return;
            }
            reports.dispatching = true;
        }
        loop {
            let next = {
                let mut reports = self.reports.lock().expect("channel reports");
                match reports.pending.pop_front() {
                    Some(next) => next,
                    None => {
                        reports.dispatching = false;
                        return;
                    }
                }
            };
            if let Some(report) = &self.deps.report_status {
                let key = next.key.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| report(next))).is_err()
                {
                    self.log(
                        "warn",
                        &format!(
                            "[adapters] failed to write runtime status for {key}: reporter panicked"
                        ),
                    );
                }
            }
        }
    }
    fn report(
        &self,
        key: &ChannelKey,
        state: ChannelState,
        message: String,
        error: Option<String>,
    ) {
        self.queue_report(key, state, message, error);
        self.flush_reports();
    }
}
async fn task_result<T>(
    task: tokio::task::JoinHandle<Result<T, ChannelError>>,
    operation: &str,
) -> Result<T, ChannelError> {
    task.await.unwrap_or_else(|_| {
        Err(ChannelError::error(format!(
            "channel {operation} task panicked"
        )))
    })
}
fn begin_stop(handle: Arc<dyn ChannelHandle>, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
    // stop's synchronous prefix closes ingress immediately. Isolate both that
    // prefix and its future so a Rust adapter panic cannot strand close waiters.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle.stop(reason))) {
        Ok(future) => Box::pin(task_result(tokio::spawn(future), "stop")),
        Err(_) => Box::pin(async { Err(ChannelError::error("channel stop task panicked")) }),
    }
}

fn closing_error() -> ChannelError {
    ChannelError::error("channel server is closing")
}
async fn receive<T>(rx: oneshot::Receiver<Result<T, ChannelError>>) -> Result<T, ChannelError> {
    rx.await
        .unwrap_or_else(|_| Err(ChannelError::error("channel operation driver dropped")))
}
async fn wait_closed(inner: Arc<Inner>) -> Result<(), ChannelError> {
    let mut rx = inner.closed.subscribe();
    loop {
        if let Some(result) = rx.borrow_and_update().clone() {
            return result;
        }
        rx.changed()
            .await
            .map_err(|_| ChannelError::error("channel close driver dropped"))?;
    }
}
fn enqueue(
    inner: &Arc<Inner>,
    key: ChannelKey,
    operation: Operation,
) -> oneshot::Receiver<Result<bool, ChannelError>> {
    let (result, rx) = oneshot::channel();
    let mut queues = inner.queues.lock().expect("channel queues");
    if queues.get(&key).is_some_and(|sender| sender.is_closed()) {
        queues.remove(&key);
    }
    let tx = queues.entry(key.clone()).or_insert_with(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(drive(inner.clone(), key, rx));
        tx
    });
    // The driver removes the sender under this same mutex before exiting.
    let _ = tx.send(Request { operation, result });
    rx
}
async fn drive(inner: Arc<Inner>, key: ChannelKey, mut rx: mpsc::UnboundedReceiver<Request>) {
    loop {
        let request = {
            let mut queues = inner.queues.lock().expect("channel queues");
            match rx.try_recv() {
                Ok(request) => request,
                Err(_) => {
                    queues.remove(&key);
                    return;
                }
            }
        };
        let result = match request.operation {
            Operation::Replace(channel) => replace(&inner, &key, channel).await.map(|_| true),
            Operation::Disable => stop(&inner, &key, "config-disabled").await,
        };
        let _ = request.result.send(result);
    }
}
async fn start(
    inner: &Arc<Inner>,
    key: &ChannelKey,
    adapter: Arc<dyn ChannelAdapter>,
) -> Result<(), ChannelError> {
    {
        let state = inner.state.lock().expect("channel state");
        if state.closing {
            return Err(closing_error());
        }
        inner.queue_report(
            key,
            ChannelState::Starting,
            format!("{key}: starting in background"),
            None,
        );
    }
    inner.flush_reports();
    let starter = adapter.clone();
    let deps = inner.deps.clone();
    let handle = task_result(
        tokio::spawn(async move { starter.start(deps).await }),
        "start",
    )
    .await?;
    let closing = {
        // Keep the JS synchronous check-and-register region atomic on all runtimes.
        let mut state = inner.state.lock().expect("channel state");
        if state.closing {
            true
        } else {
            state.running.push(Running {
                key: key.clone(),
                adapter,
                handle: handle.clone(),
            });
            inner.queue_report(
                key,
                ChannelState::Connected,
                format!("{key}: started"),
                None,
            );
            false
        }
    };
    if closing {
        begin_stop(handle, "server-shutdown").await?;
        inner.report(
            key,
            ChannelState::Stopped,
            format!("{key}: startup completed after server shutdown"),
            None,
        );
        return Err(closing_error());
    }
    inner.flush_reports();
    Ok(())
}
async fn stop(inner: &Arc<Inner>, key: &ChannelKey, reason: &str) -> Result<bool, ChannelError> {
    let entry = {
        let mut state = inner.state.lock().expect("channel state");
        state
            .running
            .iter()
            .position(|entry| entry.key == *key)
            .map(|index| state.running.remove(index))
    };
    let Some(entry) = entry else {
        return Ok(false);
    };
    let future = begin_stop(entry.handle.clone(), reason);
    finish_stop(inner, entry, future, reason).await
}
async fn finish_stop(
    inner: &Arc<Inner>,
    entry: Running,
    future: BoxFuture<Result<(), ChannelError>>,
    reason: &str,
) -> Result<bool, ChannelError> {
    let key = entry.key.clone();
    match future.await {
        Ok(()) => {
            inner.report(
                &key,
                ChannelState::Stopped,
                format!("{key}: stopped ({reason})"),
                None,
            );
            Ok(true)
        }
        Err(error) => {
            {
                let mut state = inner.state.lock().expect("channel state");
                if !state.running.iter().any(|item| item.key == key) {
                    state.running.push(entry);
                }
                inner.queue_report(
                    &key,
                    ChannelState::Failed,
                    format!("{key}: stop failed"),
                    Some(error.message().into()),
                );
            }
            inner.flush_reports();
            Err(error)
        }
    }
}
async fn replace(
    inner: &Arc<Inner>,
    key: &ChannelKey,
    channel: Arc<dyn ChannelAdapter>,
) -> Result<(), ChannelError> {
    if inner.state.lock().expect("channel state").closing {
        return Err(closing_error());
    }
    let previous = inner
        .state
        .lock()
        .expect("channel state")
        .running
        .iter()
        .find(|entry| entry.key == *key)
        .map(|entry| entry.adapter.clone());
    if previous.is_some() {
        stop(inner, key, "hot-reload").await?;
    }
    match start(inner, key, channel).await {
        Ok(()) => Ok(()),
        Err(error) => {
            inner.report(
                key,
                ChannelState::Failed,
                format!("{key}: startup failed"),
                Some(error.message().into()),
            );
            if let Some(previous) = previous {
                if let Err(restore_error) = start(inner, key, previous).await {
                    inner.report(
                        key,
                        ChannelState::Failed,
                        format!("{key}: rollback failed"),
                        Some(restore_error.message().into()),
                    );
                    return Err(ChannelError::Aggregate {
                        message: format!("channel {key} replacement and rollback failed"),
                        errors: vec![error, restore_error],
                    });
                }
                inner.log(
                    "warn",
                    &format!(
                        "[adapters] channel {key} replacement failed; restored previous adapter"
                    ),
                );
            }
            Err(error)
        }
    }
}
