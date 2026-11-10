//! Explicitly started, loopback-only native WS listener over the serial turn host.
//! There is no upstream or implicit environment/home discovery here. Embeddings
//! construct GatewayServices on TurnGateway and own their persistent resources.
use crate::{
    ProxyError, acceptor,
    frame::{self, Decoder},
    http_proxy, native_http,
    runtime::{connection::GatewayTextConnection, turn::gateway::TurnGateway},
};
use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};

/// The TS literal the token route's loopback gate is derived from
/// (GatewayServer.ts: `host === "127.0.0.1" || host === "localhost"`), as a
/// predicate on the resolved bind address: `localhost` resolves to
/// 127.0.0.1, so the literal is exactly the Ipv4Addr::LOCALHOST arm — not
/// the wider `is_loopback()` class, which also accepts all of 127.0.0.0/8
/// and ::1 (probes `b4-L1.out`: 127.0.0.2 is a 404 even with
/// allowRemoteHost, `b4-L2.out`/`b4-L3.out`: localhost and 127.0.0.1 serve
/// the token). The bind policy above still only ever passes
/// Ipv4Addr::LOCALHOST here (pinned by the `native_configuration` refusal
/// cases), so the tightening precedes any widening of that policy (#1292).
fn ts_loopback_literal(ip: std::net::IpAddr) -> bool {
    if crate::mutants::on("native-ws-loopback-literal") {
        ip.is_loopback()
    } else {
        matches!(ip, std::net::IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

/// Native is opt-in through this constructor; no default port is claimed.
/// Port zero is intended for isolated probes. Deployed ports must be >= 1024.
pub struct NativeWsServer {
    listener: TcpListener,
    host: TurnGateway,
    token: String,
    version: String,
    resources: Resources,
    http: native_http::Config,
    /// #1292 fix-r1: the bind instant — Node anchors its connections
    /// checker's 30 s grid (`setInterval(checkConnections, 30_000)` armed
    /// at `'listening'`) here, so every connection's `headersTimeout` 408
    /// lands on a phase measured from the server, not the connection
    /// (fix-r1 probe `probe-p12a.out`).
    grid_anchor: tokio::time::Instant,
    /// #1292: how many connection servings consumed a genuine stop, so a
    /// test can deterministically sequence its next step after the stop (the
    /// replacement for the #1209 stop tests' fixed 100/200 ms sleeps).
    stop_observed: Arc<std::sync::atomic::AtomicUsize>,
}
impl NativeWsServer {
    pub async fn bind(
        address: SocketAddr,
        host: TurnGateway,
        token: String,
        version: String,
    ) -> Result<Self, ProxyError> {
        Self::bind_with_policy(address, host, token, version, None).await
    }

    // Only the legacy server embedding can select the separately validated
    // host policy; the isolated native entry retains its loopback/high-port gate.
    pub(crate) async fn bind_with_policy(
        address: SocketAddr,
        host: TurnGateway,
        token: String,
        version: String,
        legacy_loopback: Option<bool>,
    ) -> Result<Self, ProxyError> {
        if legacy_loopback.is_none()
            && !crate::mutants::on("native-ws-bind-policy")
            && (address.ip() != Ipv4Addr::LOCALHOST
                || (address.port() != 0 && address.port() < 1024))
        {
            return Err("native WS requires 127.0.0.1 and a high port (or 0)".into());
        }
        if !crate::mutants::on("native-ws-token-policy")
            && crate::metadata::js_trim(&token).is_empty()
        {
            return Err("native WS requires an explicit nonempty token".into());
        }
        // The token route's loopback gate is derived from the bind address,
        // not asserted: a future non-loopback bind policy must not leak
        // /auth/local-token. #1292 tightened the derivation to the TS literal
        // (`host === "127.0.0.1" || host === "localhost"` — probes
        // `b4-L1.out`/`b4-L2.out`/`b4-L3.out`: 127.0.0.2 + allowRemoteHost
        // is a 404, localhost and 127.0.0.1 serve the token): the wide
        // `is_loopback()` (all of 127.0.0.0/8 and ::1) had to shrink before
        // the bind policy ever opens to those addresses. The test-only
        // loopback:false routing goes through the native_http router tests'
        // own Config construction.
        let http = native_http::Config {
            token: token.clone(),
            loopback: legacy_loopback.unwrap_or_else(|| ts_loopback_literal(address.ip())),
            static_root: None,
            webhook: None,
            control: None,
            api_web: None,
            approvals: None,
            public_host: None,
        };
        Ok(Self {
            // Node's checker phase anchors at the 'listening' event; this
            // Instant is taken just before the bind, so the Rust grid runs a
            // bind-RTT earlier — well under the 30 s interval, registered in
            // A8.4-NATIVE-HTTP-002-R2 with the other sub-interval residuals.
            grid_anchor: tokio::time::Instant::now(),
            listener: TcpListener::bind(address).await?,
            host,
            token,
            version,
            resources: Resources::new(
                if crate::mutants::on("ws-caps-default-connections") {
                    1
                } else {
                    MAX_CONNECTIONS
                },
                if crate::mutants::on("ws-caps-default-input") {
                    1
                } else {
                    MAX_TOTAL_INPUT_BYTES
                },
            ),
            http,
            stop_observed: Arc::default(),
        })
    }

    /// Serve the TS static fallback from an explicit root. The default
    /// (native_process) keeps TS serving static assets; only tests and
    /// later A8.4 packages opt in.
    pub fn with_static_root(mut self, root: PathBuf) -> Self {
        self.http.static_root = Some(root);
        self
    }

    /// Mount the `ANY /feishu/webhook` entry on this listener
    /// (`options.feishuWebhook`): the resolver is consulted per request, so a
    /// channel start/stop/reload is visible without rebinding.
    pub fn with_webhook(mut self, resolver: native_http::WebhookResolver) -> Self {
        self.http.webhook = Some(resolver);
        self
    }

    pub(crate) fn with_control(mut self, handler: native_http::control::ControlHandler) -> Self {
        self.http.control = Some(handler);
        self
    }

    /// Mount the authenticated module-host callback routes on this listener.
    pub(crate) fn with_public_host(mut self, config: crate::module_host::Config) -> Self {
        self.http.public_host = Some(config);
        self
    }

    pub(crate) fn with_approvals(mut self, config: crate::module_host::approvals::Config) -> Self {
        self.http.approvals = Some(config);
        self
    }

    /// Mount the `/api/web` route group on this listener (#2354): the token
    /// is the same bearer the WS lane checks, and the catalog is the native
    /// metadata registry over the same home the WS lane reads.
    pub(crate) fn with_api_web(mut self, config: crate::api_web::Config) -> Self {
        self.http.api_web = Some(config);
        self
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Test-only: the accept loop's live-connection high-water mark, so a
    /// test can deterministically wait for its connection to be accepted
    /// (spawned out of the listener backlog) before signaling stop, instead
    /// of racing a fixed sleep against the accept loop.
    #[cfg(test)]
    pub(crate) fn accepted_connections(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.resources.task_high_watermark.clone()
    }

    /// Test-only: how many connection servings consumed a genuine stop, so
    /// a test can deterministically wait for the stop to reach its
    /// connection before sending the next step, instead of racing a fixed
    /// sleep against the serving loop.
    #[cfg(test)]
    pub(crate) fn stop_observed(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.stop_observed.clone()
    }

    /// Stop accepting on `stop`, then drain existing sockets and their owned
    /// requests. Like TS server.close(), existing WS clients may finish their
    /// work. Unlike its unbounded Promise, the isolated runner has an explicit
    /// drain budget: on expiry it returns an error and shuts down the host.
    /// The budget also covers incomplete HTTP handshakes and peers that never
    /// finish. A timeout is never reported as successful quiescence.
    ///
    /// Exactly [`Self::run_phased`]'s two halves in sequence, minus the
    /// switch-only A5 close frame — every existing `run()` caller keeps the
    /// pre-#1353 behavior: survivors are aborted, not closed. The A5 close
    /// frames are a registered switch-path difference
    /// (specs/gateway-parity/diffs/), so they fire only through
    /// [`Self::run_phased`].
    pub fn run(
        self,
        stop: impl Future<Output = ()> + Send + 'static,
        drain_budget: Duration,
    ) -> impl Future<Output = Result<(), ProxyError>> {
        let phased = self.phased_run(stop, drain_budget, false);
        async move {
            phased.drained().await;
            phased.finish().await
        }
    }

    /// The #1353 A8.5-pr2 phased run: `drained()` is the switch machine's S2
    /// half — stop accepting on `stop` and drain existing connections within
    /// the budget, resolving `false` when the budget expired first.
    /// `finish()` is S4's WS half — survivors get an explicit close frame
    /// (A5), a bounded flush window, the strong abort, and the host
    /// shutdown with the same error reporting `run()` has always produced.
    ///
    /// The accept loop runs from construction — a phased run serves steady
    /// state exactly like the awaited `run()` future did; only the *waiting*
    /// is split. The stop future is erased to `Send` boxed form so the
    /// returned handle is a concrete type the switch wiring can hold across
    /// faces (`native_process/switch_wiring.rs`); every existing caller
    /// already spawns the run, so the bound changes nothing observable.
    pub fn run_phased(
        self,
        stop: impl Future<Output = ()> + Send + 'static,
        drain_budget: Duration,
    ) -> PhasedRun {
        self.phased_run(stop, drain_budget, true)
    }

    /// The shared construction behind [`Self::run`] and [`Self::run_phased`]:
    /// `switch_close` picks whether the S4 half owes the survivors their
    /// A5 close frames — only the switch path does.
    fn phased_run(
        self,
        stop: impl Future<Output = ()> + Send + 'static,
        drain_budget: Duration,
        switch_close: bool,
    ) -> PhasedRun {
        let cleanup = StopHost(
            self.host.clone(),
            if crate::mutants::on("native-ws-unbounded-cancel") {
                Duration::MAX
            } else {
                drain_budget
            },
        );
        // A5: the close-frame notice the S4 half fires at the drain boundary.
        // The sender lives as long as the run: a dropped sender would let the
        // connections' `changed()` resolve (with an error) and read as a close
        // request.
        let (close_notice, close_notice_rx) = watch::channel(false);
        let inner = Arc::new(tokio::sync::Mutex::new(PhasedInner {
            listener: Some(self.listener),
            host: self.host,
            token: self.token,
            version: self.version,
            resources: self.resources,
            http: self.http,
            grid_anchor: self.grid_anchor,
            stop_observed: self.stop_observed,
            stop: Box::pin(stop),
            connections: JoinSet::new(),
            stop_http: None,
            close_notice,
            close_notice_rx,
            switch_close,
            drain_budget,
            accept_error: None,
            drained: None,
            finished: None,
            finish_started: false,
        }));
        // The steady-state loop: accept until `stop`, then drain the
        // connections. `drained()` observes its completion through the
        // watch; dropping the handle aborts it, restoring the drop
        // semantics of the unawaited `run()` future.
        let (runner_done, done) = watch::channel(None::<bool>);
        let runner_inner = inner.clone();
        let runner = tokio::spawn(async move {
            let mut guard = runner_inner.lock().await;
            if guard.drained.is_none() {
                guard.drain_entries().await;
            }
            let drained = guard.drained.unwrap_or(false);
            drop(guard);
            runner_done.send_replace(Some(drained));
        });
        // #2557: the walk's synchronous re-issue handle — cloned before the
        // literal below moves the guard into the run.
        let host = cleanup.0.clone();
        PhasedRun {
            _cleanup: cleanup,
            inner,
            done,
            runner,
            drain_budget,
            host,
        }
    }
}

/// The phased run's shared state, serialized by the mutex: the runner holds
/// it through the accept loop and the drain, `finish()` through the abort
/// and host shutdown.
struct PhasedInner {
    listener: Option<tokio::net::TcpListener>,
    host: TurnGateway,
    token: String,
    version: String,
    resources: Resources,
    http: native_http::Config,
    grid_anchor: tokio::time::Instant,
    stop_observed: Arc<std::sync::atomic::AtomicUsize>,
    stop: Pin<Box<dyn Future<Output = ()> + Send>>,
    connections: JoinSet<()>,
    stop_http: Option<watch::Sender<bool>>,
    close_notice: watch::Sender<bool>,
    close_notice_rx: watch::Receiver<bool>,
    /// Whether the S4 half owes the survivors their A5 close frames — the
    /// switch path (`run_phased`) yes, a plain `run()` no (the registered
    /// difference is switch-scoped).
    switch_close: bool,
    drain_budget: Duration,
    accept_error: Option<std::io::Error>,
    /// `None` until the accept loop and drain ran; `Some(false)` is the
    /// expired-budget shape `finish()` must report.
    drained: Option<bool>,
    /// Once-only `finish()`: the death-march path after a halted machine
    /// calls it a second time and must observe the recorded result.
    finished: Option<Result<(), String>>,
    /// #2363: the in-progress marker that makes once-only hold for a
    /// cancelled first run too. `finish()` runs holding the inner mutex,
    /// so a caller that acquires the lock and finds this set without a
    /// recorded verdict knows the earlier run was dropped mid-body (the
    /// owner backstop cancelling the S4 face), not that it is still
    /// running — a running finish holds the lock this caller waited on.
    finish_started: bool,
}

impl PhasedInner {
    /// The accept loop and the bounded connection drain — everything up to
    /// the S2/S3 boundary.
    async fn drain_entries(&mut self) {
        // Node's server.close() closes idle keep-alive connections (probe
        // `probe-fixr1.out` B); each connection task hears the stop and
        // shuts down its HTTP serving when the accept loop exits.
        let (stop_http, stop_http_rx) = watch::channel(false);
        self.stop_http = Some(stop_http);
        let mut stop = self.stop.as_mut();
        let accept_error = loop {
            tokio::select! {
                biased;
                _ = &mut stop => break None,
                // Reap before accept: a permanently ready listener must not
                // accumulate completed task records outside the live cap.
                _ = self.connections.join_next(), if !self.connections.is_empty() && !crate::mutants::on("ws-caps-unreaped-records") => {},
                accepted = self.listener.as_mut().expect("the listener is held until stop").accept() => match accepted {
                    Ok((socket, _)) => {
                        let Ok(permit) = self.resources.connections.clone().try_acquire_owned() else {
                            // No task/HTTP parser is created for an excess peer.
                            drop(socket);
                            continue;
                        };
                        let host = self.host.clone();
                        let token = self.token.clone();
                        let version = self.version.clone();
                        let input = self.resources.input.clone();
                        let http = self.http.clone();
                        let stop_http = stop_http_rx.clone();
                        let close_notice = self.close_notice_rx.clone();
                        let stop_observed = self.stop_observed.clone();
                        let grid_anchor = self.grid_anchor;
                        self.connections.spawn(async move {
                            let _permit = if crate::mutants::on("ws-caps-early-connection-release") {
                                drop(permit);
                                None
                            } else {
                                Some(permit)
                            };
                            if let Err(error) =
                                serve_socket(socket, host, token, version, input, http, grid_anchor, stop_http, stop_observed, close_notice)
                                .await
                            {
                                // #2434: a best-effort console line, like TS's
                                // `console.error` — an `eprintln!` here panics
                                // when the write fails (std behavior); the
                                // JoinSet contains it (the `join_next` branch
                                // drops the JoinError), so the blast radius
                                // is the dead connection task and the lost
                                // diagnostic line, while TS's `console.error`
                                // loses neither the line nor the handler.
                                crate::native_process::channels::console_log_line(
                                    &mut std::io::stderr(),
                                    &format!("native WS connection: {error}"),
                                );
                            }
                            if crate::mutants::on("ws-caps-connection-leak") {
                                std::mem::forget(_permit);
                            }
                        });
                        #[cfg(test)]
                        self.resources.task_high_watermark.fetch_max(self.connections.len(), std::sync::atomic::Ordering::SeqCst);
                    }
                    Err(error) if matches!(error.kind(), std::io::ErrorKind::InvalidInput | std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported) => break Some(error),
                    Err(error) => {
                        // #2434: same best-effort shape as the connection
                        // line above — a failed stderr write must not panic
                        // the accept loop.
                        crate::native_process::channels::console_log_line(
                            &mut std::io::stderr(),
                            &format!("native WS accept: {error}"),
                        );
                        tokio::select! {
                            _ = &mut stop => break None,
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                        }
                    }
                },
            }
        };
        self.accept_error = accept_error;
        self.listener = None;
        if let Some(stop_http) = self.stop_http.take() {
            let _ = stop_http.send(true);
        }
        if crate::mutants::on("native-ws-skip-drain") {
            self.drained = Some(true);
            self.finished = Some(Ok(()));
            return;
        }
        let budget = if crate::mutants::on("native-ws-drain-no-deadline")
            || crate::mutants::on("native-ws-handshake-no-deadline")
        {
            Duration::from_secs(3600)
        } else {
            self.drain_budget
        };
        let drained = tokio::time::timeout(budget, async {
            while self.connections.join_next().await.is_some() {}
        })
        .await
        .is_ok();
        self.drained = Some(drained);
    }

    /// The S4 half: A5 close notice, bounded frame flush, strong abort, host
    /// shutdown, and the error reporting `run()` has always produced.
    async fn finish(&mut self) -> Result<(), ProxyError> {
        if let Some(result) = self.finished.clone() {
            return result.map_err(ProxyError::from);
        }
        if self.finish_started && !crate::mutants::on("native-ws-finish-rerun") {
            // `native-ws-finish-rerun` (negative control, #2363): the
            // in-progress marker deleted, so the revisit re-runs the whole
            // half — another round of close frames, aborts and shutdowns.
            // The marker set without a recorded verdict means the earlier
            // finish() was cancelled mid-body (see the field's doc): S4 must
            // not re-run. The interrupted verdict is recorded, so every
            // later caller observes the same result.
            // #2363 fix-r2: the cancelled run may have been dropped before
            // it reached the shutdown request below (a drop inside the
            // close-notice window never got there), which would leave the
            // host teardown to `StopHost::drop` — a request nothing awaits.
            // Re-issue it here: synchronous and idempotent, a no-op when
            // the cancelled run already sent it.
            self.host.request_shutdown(self.drain_budget);
            let result = Err("native WS owner finish was interrupted".to_owned());
            self.finished = Some(result.clone());
            return result.map_err(ProxyError::from);
        }
        self.finish_started = true;
        // A5: the drain boundary notice — every surviving WS connection gets
        // an explicit close(1001, "server switching") and stops dispatching
        // new requests, before the strong abort takes them. Switch path
        // only: a plain `run()` keeps the pre-#1353 abort behavior (the
        // registered difference is scoped to the switch).
        if self.switch_close {
            let _ = self.close_notice.send(true);
            if !self.connections.is_empty() {
                let _ = tokio::time::timeout(CLOSE_FRAME_WINDOW, async {
                    while self.connections.join_next().await.is_some() {}
                })
                .await;
            }
        }
        // Aborting transport waiters never cancels business jobs on the serial
        // host. Its shutdown cancels owned turns and waits for finally fences.
        // #2363 fix-r2: the shutdown request goes out here, before the abort
        // join's first await — a first finish() dropped inside the join (an
        // un-abortable connection parked there) or inside `host.shutdown`
        // would otherwise leave the teardown request to `StopHost::drop`,
        // which nothing awaits. Synchronous and idempotent (`send_if_modified`
        // keeps the first budget), so the `host.shutdown` below just awaits
        // the drain it already started — the graceful close-notice window
        // above has already closed by this point, so nothing observes the
        // earlier request.
        self.host.request_shutdown(self.drain_budget);
        if crate::mutants::on("native-ws-handshake-leak") {
            self.connections.detach_all();
        } else {
            self.connections.abort_all();
            while self.connections.join_next().await.is_some() {}
        }
        // #2363: the shutdown's own error folds into the verdict below — an
        // early `?` return would skip the `finished` write, so a revisit (the
        // death march after a halted machine) would re-run the whole half.
        let shutdown = self.host.shutdown(self.drain_budget).await;
        // `native-ws-finish-error-escape` (negative control, #2363): the
        // pre-fix early escape restored — the shutdown error returns before
        // the verdict is recorded.
        if crate::mutants::on("native-ws-finish-error-escape")
            && let Err(error) = shutdown
        {
            return Err(std::io::Error::other(error).into());
        }
        let result = if let Some(error) = shutdown.err().map(|error| error.to_string()) {
            Err(error)
        } else if !self.drained.unwrap_or(false)
            && !crate::mutants::on("native-ws-drain-false-success")
        {
            Err("native WS drain deadline exceeded".to_owned())
        } else if let Some(error) = self.accept_error.take() {
            Err(error.to_string())
        } else {
            Ok(())
        };
        self.finished = Some(result.clone());
        result.map_err(ProxyError::from)
    }
}

/// #1353 A5 (DC-2): after the client-drain deadline expired, the machine
/// still owes the survivors their explicit close frames. This window bounds
/// the frame flush between the notice and the strong abort — the drain
/// deadline itself governs the *waiting* for clients, this is the machine's
/// own delivery grace.
const CLOSE_FRAME_WINDOW: Duration = Duration::from_secs(2);

/// The two-phase handle [`NativeWsServer::run_phased`] returns. The steady
/// state (the accept loop) runs from construction on the runner task;
/// `drained()` observes its completion and `finish()` takes over from there.
/// The drop guard keeps the `request_shutdown` semantics of the unrefactored
/// `run()`: dropping a phased run that never finished stops accepting and
/// still asks the host to shut down.
pub struct PhasedRun {
    /// Never read: the drop guard is the point — dropping an unfinished
    /// phased run still asks the host to shut down.
    _cleanup: StopHost,
    inner: Arc<tokio::sync::Mutex<PhasedInner>>,
    /// The runner's completion: `None` while accepting/draining, then the
    /// drain's own verdict. Only the runner owns the sender: panic or abort
    /// closes the channel, so every waiter observes failure without needing
    /// the inner mutex (which a concurrent `finish()` may hold).
    done: watch::Receiver<Option<bool>>,
    /// Aborted when this handle drops, so an abandoned phased run stops
    /// accepting exactly like an unawaited `run()` future did.
    runner: tokio::task::JoinHandle<()>,
    /// The drain budget this run was constructed with — immutable after
    /// construction, so readable without the inner lock. The post-halt walk
    /// bounds its own close join with it (`switch_wiring.rs`).
    drain_budget: Duration,
    /// #2557: the host handle, duplicated like the budget above — the
    /// post-halt walk re-issues the teardown request synchronously after
    /// its shared deadline cut a first `finish()` short, without going
    /// through `finish()` again (see `request_host_shutdown`).
    host: TurnGateway,
}

impl Drop for PhasedRun {
    fn drop(&mut self) {
        self.runner.abort();
    }
}

impl PhasedRun {
    /// S2: stop accepting and drain existing connections. `true` means the
    /// drain completed within the budget; `false` means the budget expired
    /// with connections outstanding, or the runner exited without a verdict
    /// (panic/abort). `finish()` reports the failure in either case.
    /// Idempotent — every waiter observes the same verdict.
    pub async fn drained(&self) -> bool {
        let mut receiver = self.done.clone();
        loop {
            if let Some(drained) = *receiver.borrow_and_update() {
                return drained;
            }
            // A published verdict is checked above even after the sender
            // closes. Closure without a verdict means the runner failed.
            if receiver.changed().await.is_err() {
                return false;
            }
        }
    }

    /// The drain budget this run was constructed with: the entries-scale
    /// bound the post-halt walk reuses for its own close join.
    pub fn drain_budget(&self) -> Duration {
        self.drain_budget
    }

    /// #2557: the post-halt walk's synchronous re-issue of the host teardown
    /// request after its shared deadline cut a first `finish()` mid-body —
    /// the walk returns on the timeout and the process exits, so no revisit
    /// is coming. The re-issue cannot go through `finish()`: it first awaits
    /// the inner lock, which a concurrent holder can hold past the deadline
    /// the walk just exhausted; and the revisit branch inside, once entered,
    /// would itself re-issue and then only report the interrupted verdict
    /// (#2363, once-only) — the direct synchronous request on the host gate
    /// needs neither. Idempotent like every other request site
    /// (`send_if_modified` keeps the first budget).
    pub fn request_host_shutdown(&self) {
        self.host.request_shutdown(self.drain_budget);
    }

    /// S4's WS half: close notice, abort, host shutdown, error reporting.
    /// Idempotent — a second call observes the recorded result, and a first
    /// call that was cancelled mid-run (the owner backstop dropping the S4
    /// face) leaves its own recorded verdict instead of a re-run (#2363).
    pub async fn finish(&self) -> Result<(), ProxyError> {
        let mut guard = self.inner.lock().await;
        guard.finish().await
    }
}

struct StopHost(TurnGateway, Duration);
impl Drop for StopHost {
    fn drop(&mut self) {
        if !crate::mutants::on("native-ws-cancel-leak") {
            self.0.request_shutdown(self.1);
        }
    }
}

// Resource safety differences from TS's unbounded custom WS implementation.
const MAX_INPUT_PAYLOAD: usize = 100 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_TOTAL_INPUT_BYTES: usize = 256 * 1024 * 1024;

// Reserve the actual UTF-8 replacement size before allocating a queued String.
// A malformed payload can expand to three bytes per input byte; charging the
// wire length or refunding on decoder consumption would undercount it.
fn request_text(payload: &[u8], budget: &Arc<Semaphore>) -> Option<(String, OwnedSemaphorePermit)> {
    let length: usize = payload
        .utf8_chunks()
        .map(|chunk| chunk.valid().len() + usize::from(!chunk.invalid().is_empty()) * 3)
        .sum();
    let charged = if crate::mutants::on("ws-caps-request-budget") {
        0
    } else if crate::mutants::on("ws-caps-request-lossy-length") {
        payload.len()
    } else {
        length
    };
    let permit = budget
        .clone()
        .try_acquire_many_owned(u32::try_from(charged).ok()?)
        .ok()?;
    let mut text = String::with_capacity(length);
    for chunk in payload.utf8_chunks() {
        text.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            text.push(if crate::mutants::on("ws-caps-request-lossy-replacement") {
                '?'
            } else {
                '\u{fffd}'
            });
        }
    }
    Some((text, permit))
}

struct Resources {
    connections: Arc<Semaphore>,
    input: Arc<Semaphore>,
    #[cfg(test)]
    task_high_watermark: Arc<std::sync::atomic::AtomicUsize>,
}
impl Resources {
    fn new(connections: usize, input: usize) -> Self {
        Self {
            connections: Arc::new(Semaphore::new(
                if crate::mutants::on("ws-caps-connections") {
                    connections + 1
                } else {
                    connections
                },
            )),
            input: Arc::new(Semaphore::new(input)),
            #[cfg(test)]
            task_high_watermark: Arc::default(),
        }
    }
}

/// Charge backing storage, not just unread length: Decoder::split_to may
/// retain the original allocation after a frame is consumed. Declaration order
/// drops the decoder before its lease, including task cancellation and errors.
struct InputBuffer {
    decoder: Decoder,
    allocation: Option<OwnedSemaphorePermit>,
    budget: Arc<Semaphore>,
}
impl InputBuffer {
    fn new(budget: Arc<Semaphore>) -> Self {
        Self {
            decoder: Decoder::default(),
            allocation: None,
            budget,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> bool {
        let needed = self.decoder.buffer.len() + bytes.len();
        if needed > self.decoder.buffer.capacity() {
            let capacity = needed
                .next_power_of_two()
                .min(MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES);
            // Keep the old allocation charged until the replacement is built
            // and installed. A slow peer never waits for another peer's lease.
            if crate::mutants::on("ws-caps-early-input-refund") {
                self.allocation = None;
            }
            if crate::mutants::on("ws-caps-split-input-refund")
                && self
                    .allocation
                    .as_ref()
                    .is_some_and(|permit| permit.num_permits() > self.decoder.buffer.capacity())
            {
                self.allocation = None;
            }
            let charged = if crate::mutants::on("ws-caps-input-budget") {
                0
            } else {
                capacity
            };
            let Ok(permit) = self.budget.clone().try_acquire_many_owned(charged as u32) else {
                return false;
            };
            let mut buffer = bytes::BytesMut::with_capacity(capacity);
            buffer.extend_from_slice(&self.decoder.buffer);
            self.decoder.buffer = buffer;
            self.allocation = Some(permit);
        }
        self.decoder.buffer.extend_from_slice(bytes);
        true
    }

    fn reclaim_empty(&mut self) {
        if !self.decoder.buffer.is_empty() && crate::mutants::on("ws-caps-refund-tail") {
            self.allocation = None;
        }
        if self.decoder.buffer.is_empty() && !crate::mutants::on("ws-caps-retain-empty") {
            // Called only after the decoded Frame (including its wire) drops.
            self.decoder = Decoder::default();
            self.allocation = None;
        }
    }
}
impl Drop for InputBuffer {
    fn drop(&mut self) {
        if crate::mutants::on("ws-caps-input-drop-leak") {
            std::mem::forget(self.allocation.take());
        }
    }
}
pub(crate) const MAX_OUTPUT_PAYLOAD: usize = MAX_INPUT_PAYLOAD;
const MAX_OUTPUT_BYTES: usize = MAX_OUTPUT_PAYLOAD + frame::MAX_SERVER_HEADER_BYTES;
const MAX_OUTPUT_FRAMES: usize = 64;
const MAX_PENDING_REQUESTS: usize = 64;
// TS has no flush timer. Bound our resource-safety difference with scheduling
// slack plus one second per remaining MiB, including the in-flight frame.
// A full 100 MiB frame + header receives 131s, not a fixed 1s deadline.
const CLOSE_BUDGET: Duration = Duration::from_secs(30);
const CLOSE_BYTES_PER_SECOND: usize = 1024 * 1024;

enum Output {
    Frame(Vec<u8>, OwnedSemaphorePermit),
    Close(Vec<u8>),
    End,
}
struct Text {
    output: mpsc::Sender<Output>,
    bytes: Arc<Semaphore>,
    end_slot: Mutex<Option<mpsc::OwnedPermit<Output>>>,
    close_slot: Mutex<Option<mpsc::OwnedPermit<Output>>>,
    closing: watch::Sender<Option<tokio::time::Instant>>,
    // Linearize close/send callbacks on the host without blocking that host.
    closed: Mutex<bool>,
}
impl Text {
    fn channel() -> (
        Arc<Self>,
        mpsc::Receiver<Output>,
        watch::Receiver<Option<tokio::time::Instant>>,
    ) {
        let slots = if crate::mutants::on("native-ws-output-slots") {
            65536
        } else {
            MAX_OUTPUT_FRAMES
        };
        let bytes = if crate::mutants::on("native-ws-output-bytes") {
            1024 * MAX_OUTPUT_BYTES
        } else {
            MAX_OUTPUT_BYTES
        };
        // These permits cannot be consumed by ordinary frames. Keep them until
        // the one Close and the one End have been enqueued, in FIFO order.
        let (output, outgoing) = mpsc::channel(slots + 2);
        let end_slot = output.clone().try_reserve_owned().expect("new End slot");
        let close_slot = output.clone().try_reserve_owned().expect("new Close slot");
        let (closing, closing_rx) = watch::channel(None);
        let text = Arc::new(Self {
            output,
            end_slot: Mutex::new(Some(end_slot)),
            close_slot: Mutex::new(Some(close_slot)),
            bytes: Arc::new(Semaphore::new(bytes)),
            closing,
            closed: Mutex::new(false),
        });
        (text, outgoing, closing_rx)
    }
    fn close_budget(&self) -> Duration {
        if crate::mutants::on("native-ws-short-close-budget") {
            return Duration::from_secs(1);
        }
        if crate::mutants::on("native-ws-ignore-flush-bytes") {
            return CLOSE_BUDGET;
        }
        let accepted = MAX_OUTPUT_BYTES.saturating_sub(self.bytes.available_permits());
        CLOSE_BUDGET + Duration::from_secs(accepted.div_ceil(CLOSE_BYTES_PER_SECOND) as u64)
    }
    fn deadline(&self, immediate: bool) {
        let candidate = tokio::time::Instant::now()
            + if immediate {
                Duration::ZERO
            } else {
                self.close_budget()
            };
        self.closing.send_if_modified(|current| {
            if current.is_none_or(|old| {
                (immediate || crate::mutants::on("native-ws-shorten-eof-deadline"))
                    && candidate < old
            }) || crate::mutants::on("native-ws-reset-close-deadline")
            {
                *current = Some(candidate);
                true
            } else {
                false
            }
        });
    }

    // Reserve both dimensions before frame encoding. A permit stays attached
    // through write_all, so dequeue does not hide a blocked in-flight frame.
    fn queue(&self, opcode: u8, payload: &[u8]) {
        let max_payload = if crate::mutants::on("native-ws-small-output-limit") {
            1024 * 1024
        } else {
            MAX_OUTPUT_PAYLOAD
        };
        let length = payload
            .len()
            .checked_add(frame::MAX_SERVER_HEADER_BYTES)
            .and_then(|n| u32::try_from(n).ok());
        let permit = length.and_then(|n| self.bytes.clone().try_acquire_many_owned(n).ok());
        let slot = self.output.try_reserve();
        if let (Some(permit), Ok(slot), true) = (permit, slot, payload.len() <= max_payload) {
            slot.send(Output::Frame(frame::server_frame(opcode, payload), permit));
        } else {
            // Sync host callbacks cannot await byte capacity. Reject overload
            // with a reserved close frame; a non-reading peer still has a deadline.
            self.queue_close(1009, "output limit exceeded");
        }
    }
    fn queue_close(&self, code: u16, reason: &str) {
        if self.output.capacity() == 0 && crate::mutants::on("native-ws-full-close-signal") {
            return;
        }
        self.deadline(false);
        if let Some(slot) = self.close_slot.lock().expect("native close slot").take() {
            let mut payload = code.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            let wire = frame::server_frame(8, &payload);
            if !crate::mutants::on("native-ws-drop-output-close") {
                slot.send(Output::Close(wire));
            }
        }
    }
    fn ping(&self, payload: &[u8]) {
        // TS replies to ping even after application close within a data batch.
        self.queue(10, payload);
    }
    fn end(&self) {
        *self.closed.lock().expect("native text state") = true;
        self.deadline(false);
        if crate::mutants::on("native-ws-drop-full-end") {
            let _ = self.output.try_send(Output::End);
        } else if let Some(slot) = self.end_slot.lock().expect("native end slot").take() {
            slot.send(Output::End);
        }
    }
}
impl GatewayTextConnection for Text {
    fn send_text(&self, message: String) {
        let closed = self.closed.lock().expect("native text state");
        if !*closed && self.closing.borrow().is_none() {
            self.queue(1, message.as_bytes());
        }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut closed = self.closed.lock().expect("native text state");
        if !*closed {
            *closed = true;
            self.queue_close(code, reason);
        }
    }
}

async fn closing_deadline(mut closing: watch::Receiver<Option<tokio::time::Instant>>) {
    loop {
        let deadline = *closing.borrow_and_update();
        if let Some(deadline) = deadline {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return,
                result = closing.changed() => if result.is_err() { return; },
            }
        } else if closing.changed().await.is_err() {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve_socket(
    socket: TcpStream,
    host: TurnGateway,
    token: String,
    version: String,
    input: Arc<Semaphore>,
    http: native_http::Config,
    grid_anchor: tokio::time::Instant,
    stop: watch::Receiver<bool>,
    stop_observed: Arc<std::sync::atomic::AtomicUsize>,
    close_notice: watch::Receiver<bool>,
) -> Result<(), ProxyError> {
    let (upgrade, upgraded) = oneshot::channel();
    let upgrade = Mutex::new(Some(upgrade));
    let upgrade_sender = &upgrade;
    let http_config = &http;
    http_proxy::serve_connection_native_with_shutdown(
        socket,
        move |mut request, lease| async move {
            // Node applies its TrackHeader span budget to upgrade requests
            // too (`b4-U1.out`: the over-budget `/ws` upgrade head gets the
            // same 431 + close as `b4-U2.out`'s non-upgrade head; the
            // boundary cells of the `native-ws-upgrade-budget` fixture
            // pin both lanes at tracked 16383 served / 16384 the 431,
            // replayed by `upgrade_budget_boundary_matches_ts`). The check
            // is gated on the diversion, not the whole closure, so the
            // non-upgrade lane keeps its own llhttp ordering — method and
            // target-grammar 400s before the budget, inside `route`.
            if http_proxy::is_upgrade(&request)
                && !crate::mutants::on("native-http-upgrade-budget-off")
                && native_http::over_header_budget(&request)
            {
                return Ok(native_http::request_header_fields_too_large());
            }
            if http_proxy::is_upgrade(&request) {
                match (
                    request.uri().path(),
                    acceptor::websocket_accept(request.headers()),
                    lease,
                ) {
                    ("/ws", Some(key), Some(lease)) => {
                        if let Some(upgrade) =
                            upgrade_sender.lock().expect("native upgrade state").take()
                        {
                            let _ = upgrade.send((hyper::upgrade::on(&mut request), lease));
                        }
                        Ok(acceptor::websocket_response(Default::default(), &key))
                    }
                    _ if crate::mutants::on("native-ws-invalid-upgrade") => {
                        Ok(acceptor::websocket_response(Default::default(), "invalid"))
                    }
                    _ => Err(std::io::Error::other("invalid native WS upgrade")),
                }
            } else {
                native_http::serve_with_body(request, http_config).await
            }
        },
        stop,
        Some(stop_observed),
        Some(grid_anchor),
    )
    .await?;
    drop(upgrade);
    let Ok((upgraded, lease)) = upgraded.await else {
        return Ok(());
    };
    let socket = http_proxy::upgraded_stream::<TcpStream>(upgraded.await?)?;
    drop(lease);
    transport_with_budget(socket, host, token, version, input, close_notice).await
}

// Poll the host completion futures directly: a finished host job must not
// occupy a slot just because a spawned local waiter has not been scheduled.
async fn reap_requests<F: Future + Unpin>(requests: &mut Vec<F>, wait: bool) {
    std::future::poll_fn(|cx| {
        let mut completed = false;
        let mut index = 0;
        while index < requests.len() {
            if std::pin::Pin::new(&mut requests[index]).poll(cx).is_ready() {
                drop(requests.swap_remove(index));
                completed = true;
            } else {
                index += 1;
            }
        }
        if !wait || completed || requests.is_empty() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn input_capacity(buffered: usize, available: usize) -> usize {
    let remaining = if crate::mutants::on("native-ws-buffer-limit") {
        usize::MAX
    } else {
        (MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES).saturating_sub(buffered)
    };
    available.min(remaining)
}

// The saturated state is defensive: an ordinary complete valid frame has
// already been consumed by the inner decode loop before the next read.
fn read_capacity(text: &Text, buffered: usize, available: usize) -> usize {
    let capacity = input_capacity(buffered, available);
    if capacity == 0 {
        let code = if crate::mutants::on("native-ws-buffer-close-code") {
            1000
        } else {
            1009
        };
        text.close(code, "frame too large");
    }
    capacity
}

async fn finish_write(
    write: impl Future<Output = std::io::Result<()>>,
    closing: watch::Receiver<Option<tokio::time::Instant>>,
) -> std::io::Result<()> {
    let timed_out = || {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "native WS writer close deadline exceeded",
        )
    };
    tokio::select! {
        biased;
        // Deterministically invert precedence only in the fixture mutant.
        _ = closing_deadline(closing.clone()), if crate::mutants::on("native-ws-writer-priority") => Err(timed_out()),
        result = write => result,
        _ = closing_deadline(closing), if !crate::mutants::on("native-ws-writer-deadline") => Err(timed_out()),
    }
}

async fn write_output<W: tokio::io::AsyncWrite + Unpin>(
    mut writer: W,
    mut outgoing: mpsc::Receiver<Output>,
    closing: watch::Receiver<Option<tokio::time::Instant>>,
) -> std::io::Result<()> {
    finish_write(
        async {
            while let Some(item) = outgoing.recv().await {
                match item {
                    Output::Frame(wire, permit) => {
                        if crate::mutants::on("native-ws-cancel-leaks-permit") {
                            std::mem::forget(permit);
                            writer.write_all(&wire).await?;
                        } else {
                            writer.write_all(&wire).await?;
                            if crate::mutants::on("native-ws-leak-output-permit") {
                                std::mem::forget(permit);
                            }
                        }
                    }
                    Output::Close(wire) => {
                        writer.write_all(&wire).await?;
                        break;
                    }
                    Output::End => break,
                }
            }
            writer.shutdown().await
        },
        closing,
    )
    .await
}

#[cfg(test)]
async fn transport<S>(
    socket: S,
    host: TurnGateway,
    token: String,
    version: String,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // The never-firing close notice: the sender is held for the future's
    // lifetime so `changed()` below cannot resolve through a dropped sender.
    let (close_notice_tx, close_notice) = watch::channel(false);
    let result = transport_with_budget(
        socket,
        host,
        token,
        version,
        Arc::new(Semaphore::new(MAX_TOTAL_INPUT_BYTES)),
        close_notice,
    )
    .await;
    drop(close_notice_tx);
    result
}

#[allow(clippy::too_many_arguments)]
async fn transport_with_budget<S>(
    socket: S,
    host: TurnGateway,
    token: String,
    version: String,
    input_budget: Arc<Semaphore>,
    mut close_notice: watch::Receiver<bool>,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(socket);
    let (text, outgoing, closing_rx) = Text::channel();
    let connection = host.connect(text.clone(), token, version);
    let mut writes = JoinSet::new();
    writes.spawn(write_output(writer, outgoing, closing_rx.clone()));
    let mut requests = Vec::new();
    let mut buffered = InputBuffer::new(input_budget);
    let mut input = [0; 8192];
    let mut writer_done = false;
    let reap_completed = !crate::mutants::on("native-ws-retain-completed-requests");
    let max_payload = if crate::mutants::on("native-ws-input-limit") {
        u64::MAX
    } else {
        MAX_INPUT_PAYLOAD as u64
    };
    let result: Result<(), ProxyError> = async {
        'read: loop {
            // TS handleData starts on each new data callback. A protocol close
            // ends that batch, but leaves buffered tail bytes for the next one.
            let capacity = read_capacity(&text, buffered.decoder.buffer.len(), input.len());
            if capacity == 0 {
                break;
            }
            let count = tokio::select! {
                result = writes.join_next(), if !writer_done => {
                    if let Some(result) = result { result??; }
                    writer_done = true;
                    continue;
                }
                _ = closing_deadline(closing_rx.clone()), if !crate::mutants::on("native-ws-reader-deadline") => {
                    return Err("native WS connection close deadline exceeded".into());
                }
                // #1353 A5: the drain boundary notice — an explicit
                // close(1001, "server switching") and no further dispatch;
                // the S4 abort takes whatever is still stuck afterwards.
                _ = close_notice.changed() => {
                    text.close(1001, "server switching");
                    break;
                }
                read = reader.read(&mut input[..capacity]) => read?,
                _ = reap_requests(&mut requests, true), if reap_completed && !requests.is_empty() => continue,
            };
            if count == 0 {
                break;
            }
            if !buffered.append(&input[..count]) {
                let code = if crate::mutants::on("ws-caps-input-close-code") { 1000 } else { 1009 };
                text.close(code, "aggregate input limit exceeded");
                break;
            }
            loop {
                let frame = match buffered.decoder.next_limited(max_payload) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(frame::DecodeError::TooLarge) => {
                        let code = if crate::mutants::on("native-ws-input-close-code") { 1000 } else { 1009 };
                        text.close(code, "frame too large");
                        break;
                    }
                    Err(error) => return Err(std::io::Error::other(format!("invalid WS frame: {error:?}")).into()),
                };
                match acceptor::action(&frame) {
                    acceptor::Action::Text => {
                        if reap_completed { reap_requests(&mut requests, false).await; }
                        if requests.len() >= MAX_PENDING_REQUESTS && !crate::mutants::on("native-ws-pending-limit") {
                            text.deadline(true);
                            return Err("native WS pending request limit exceeded".into());
                        }
                        let Some((message, permit)) = request_text(&frame.payload, &buffered.budget) else {
                            let code = if crate::mutants::on("ws-caps-request-close-code") { 1000 } else { 1009 };
                            text.close(code, "aggregate input limit exceeded");
                            break 'read;
                        };
                        if !crate::mutants::on("native-ws-drop-text") {
                            requests
                                .push(connection.enqueue_with_input_permit(message, Some(permit)).await.map_err(request_error)?);
                        }
                        if crate::mutants::on("native-ws-drop-application-tail")
                            && *text.closed.lock().expect("native text state")
                        {
                            break;
                        }
                    }
                    acceptor::Action::Ping => {
                        text.ping(&frame.payload);
                    }
                    acceptor::Action::Close => {
                        text.close(1000, "");
                        if !crate::mutants::on("native-ws-after-close") {
                            break;
                        }
                    }
                    acceptor::Action::Ignore => {}
                }
            }
            buffered.reclaim_empty();
            if crate::mutants::on("native-ws-drop-closed-read")
                && *text.closed.lock().expect("native text state")
            {
                break;
            }
            if crate::mutants::on("native-ws-writer-blocks-read") && !requests.is_empty() {
                if let Some(written) = writes.join_next().await {
                    written??;
                }
                break;
            }
        }
        Ok(())
    }
    .await;
    let retained_input = if crate::mutants::on("ws-caps-retain-input-during-drain") {
        Some(buffered)
    } else {
        drop(buffered);
        None
    };
    // Release the socket reader independently of uncompleted business jobs.
    let retained_reader = if crate::mutants::on("native-ws-retain-reader") {
        Some(reader)
    } else {
        drop(reader);
        None
    };
    text.end();
    // The writer's own deadline drops its socket half even when the host's
    // disconnect/owned requests never finish. The server bounds business drain.
    let closed = if crate::mutants::on("native-ws-skip-disconnect") {
        Ok(())
    } else {
        connection.closed().await
    };
    while !requests.is_empty() {
        reap_requests(&mut requests, true).await;
    }
    while let Some(written) = writes.join_next().await {
        written??;
    }
    result?;
    drop(retained_input);
    drop(retained_reader);
    closed.map_err(request_error)?;
    Ok(())
}
fn request_error(error: crate::runtime::connection::RequestError) -> std::io::Error {
    std::io::Error::other(String::from_utf16_lossy(error.message.units()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod caps_tests;
