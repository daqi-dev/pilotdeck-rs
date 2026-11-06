//! `http.createServer(...).listen(port, "0.0.0.0")` plus Node 22
//! `server.close()` drain semantics: stop accepting, let in-flight requests
//! finish, close idle keep-alive connections.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

use super::Runtime;

/// Node's `http.Server` defaults, which are what make `server.close()` finish
/// even when a client stalls mid-request: `headersTimeout` retires a
/// connection that never finishes its header block, `requestTimeout` retires a
/// request that never finishes its body, and the client's own socket then
/// errors out. Without them hyper keeps the connection pending forever, the
/// drain in `stop()` never completes and a single half-written `POST` pins the
/// channel (and the host shutdown waiting on it).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timeouts {
    pub(crate) headers: Duration,
    pub(crate) request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        // `headersTimeout` and `requestTimeout` as documented for Node 22.
        Self {
            headers: Duration::from_secs(60),
            request: Duration::from_secs(300),
        }
    }
}

/// How long the accept loop waits after a persistent `accept()` failure before
/// retrying. Node emits an `error` event and keeps serving; retrying a
/// resource error (EMFILE) without a pause would spin a worker at 100% CPU.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Test-only request-entry counter. The replay harness has to know that the
/// server has read a request head and is now waiting on its body before it can
/// assert what `stop()` does to that connection, and nothing else about an
/// in-flight-but-silent request is observable; a fixed sleep was a wall-clock
/// guess that failed under load.
#[cfg(test)]
pub(crate) mod accepted {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNT: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn bump() {
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn count() -> usize {
        COUNT.load(Ordering::SeqCst)
    }
}

/// A listen failure formatted as the thrown JavaScript value would print:
/// `Error: listen EADDRINUSE: address already in use 0.0.0.0:8780` or the
/// synchronous `RangeError [ERR_SOCKET_BAD_PORT]` of a bad port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListenError(String);

impl std::fmt::Display for ListenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl ListenError {
    fn bad_port(port: u32) -> Self {
        Self(format!(
            "RangeError [ERR_SOCKET_BAD_PORT]: options.port should be >= 0 and < 65536. \
             Received type number ({port})."
        ))
    }

    fn from_bind(error: &std::io::Error, port: u16) -> Self {
        let described = match error.kind() {
            std::io::ErrorKind::AddrInUse => "EADDRINUSE: address already in use".to_owned(),
            std::io::ErrorKind::PermissionDenied => "EACCES: permission denied".to_owned(),
            _ => error.to_string(),
        };
        Self(format!("Error: listen {described} 0.0.0.0:{port}"))
    }
}

/// Accepts one request at a time per connection and routes it through the
/// adapter's HTTP face. `stop` is idempotent.
pub(crate) struct Listener {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Listener {
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) async fn stop(&self) {
        let _ = self.shutdown.send(true);
        let task = self.task.lock().expect("#1336 listener task").take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // A handle discarded without stop must not leave the accept loop
        // running forever. The detach is deliberate: an already accepted
        // request still completes, exactly like Node's closing server.
        let _ = self.shutdown.send(true);
    }
}

/// Bind on `0.0.0.0:port` and start serving. Resolves only after the socket
/// is listening, matching the `await new Promise(... listen ...)` shape.
pub(crate) async fn listen(runtime: Arc<Runtime>, port: u32) -> Result<Listener, ListenError> {
    listen_with(runtime, port, Timeouts::default()).await
}

/// `listen` with the connection timers spelled out; the tests drive the same
/// accept/drain code with short timers instead of waiting Node's 60 s.
pub(crate) async fn listen_with(
    runtime: Arc<Runtime>,
    port: u32,
    timeouts: Timeouts,
) -> Result<Listener, ListenError> {
    let port: u16 = port.try_into().map_err(|_| ListenError::bad_port(port))?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
        .await
        .map_err(|error| ListenError::from_bind(&error, port))?;
    let bound = listener
        .local_addr()
        .map_err(|error| ListenError::from_bind(&error, port))?
        .port();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                accepted = listener.accept() => {
                    let (stream, _peer) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            // Node's server emits `error` and keeps serving.
                            // Retrying a persistent failure (EMFILE, ENOBUFS)
                            // without a pause would spin this worker forever.
                            runtime.log("error", &format!("wecom_callback: accept failed: {error}"));
                            tokio::time::sleep(ACCEPT_BACKOFF).await;
                            continue;
                        }
                    };
                    let runtime = runtime.clone();
                    let mut shutdown = shutdown_rx.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request| {
                            let runtime = runtime.clone();
                            async move {
                                // The request head has been read and the body
                                // is pending; from here on `stop()` drains this
                                // connection instead of dropping it.
                                #[cfg(test)]
                                accepted::bump();
                                // Node's `requestTimeout`: a request that has
                                // not completed in time is answered 408 and the
                                // connection is closed instead of pending.
                                match tokio::time::timeout(timeouts.request, runtime.handle_request(request)).await {
                                    Ok(response) => Ok::<_, Infallible>(response),
                                    Err(_) => Ok::<_, Infallible>(super::response(408, None, b"")),
                                }
                            }
                        });
                        let connection = http1::Builder::new()
                            // Node keeps serving a request whose client has
                            // FINed its write side; hyper's default treats the
                            // same half-close as an incomplete-message error.
                            .half_close(true)
                            // `headersTimeout`: without a timer hyper's header
                            // read timeout is disabled and a half-written
                            // request keeps the connection (and `stop()`) alive
                            // forever.
                            .timer(TokioTimer::new())
                            .header_read_timeout(timeouts.headers)
                            .serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(connection);
                        // Poll the connection once before `shutdown` may
                        // pre-empt it. A connection accepted just as `stop()`
                        // flips the watch has its request head already in the
                        // socket buffer; without this poll the shutdown branch
                        // wins, `graceful_shutdown` sees an empty read buffer
                        // and closes the socket, and a request Node's
                        // `server.close()` would have served is reset. The
                        // oracle's drain cell depends on this too.
                        tokio::select! {
                            biased;
                            _ = connection.as_mut() => return,
                            _ = std::future::ready(()) => {}
                        }
                        tokio::select! {
                            _ = connection.as_mut() => {}
                            _ = shutdown.changed() => {
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                        }
                    });
                    // Finished connections are only reaped by `join_next`, so
                    // without this the JoinSet would grow with every callback
                    // the process has ever served.
                    while connections.try_join_next().is_some() {}
                }
            }
        }
        // In-flight requests finish; idle keep-alive connections were closed
        // by `graceful_shutdown` when the watch flipped.
        while connections.join_next().await.is_some() {}
    });
    Ok(Listener {
        port: bound,
        shutdown,
        task: Mutex::new(Some(task)),
    })
}
