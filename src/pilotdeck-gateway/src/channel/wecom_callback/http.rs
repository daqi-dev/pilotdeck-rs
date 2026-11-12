//! `http.createServer(...).listen(port, "0.0.0.0")` plus Node 22
//! `server.close()` drain semantics: stop accepting, let in-flight requests
//! finish, close idle keep-alive connections.
//!
//! This is the second hand-rolled `node:http` transport in the gateway: the
//! shared lane (`crate::http_proxy::serve_connection_native_with_shutdown`,
//! used by `channel/webhook` and `channel/api_server`) carries #1292's
//! keep-alive/408 grid, while this module implements only what the WeCom
//! callback needs. The differences that can reach the wire, all deliberate and
//! all registered as diff-ledger entries — two still unapproved drafts
//! (`specs/gateway-parity/diffs/A8.3-WECOM-CALLBACK-408-001.md`: the
//! `requestTimeout` 408 is framed by hyper — `date` + `content-length: 0` —
//! instead of Node's 51 raw bytes;
//! `specs/gateway-parity/diffs/A8.3-WECOM-CALLBACK-KEEPALIVE-001.md`: no
//! `Keep-Alive: timeout=5` header, and an idle keep-alive connection retires on
//! the 60 s `header_read_timeout` instead of Node's 5 s `keepAliveTimeout`) and
//! one approved accepted diff
//! (`specs/gateway-parity/diffs/A8.3-WECOM-CALLBACK-BODY-LIMIT-001.md`, #1941:
//! an oversize `POST` body ends the connection with zero response bytes — the
//! `Err` from this module's service, never a 413; approval recorded in
//! `state/decisions-20261108-2033-wecom-body-limit-approval.md`).
//! Moving to the shared lane is tracked as a follow-up, not done here so the
//! channel's recorded behaviour stays what the golden fixture pins.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use hyper::{Response, header, server::conn::http1, service::service_fn};
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

/// Test-only request-entry counter, **per channel instance**. The replay
/// harness has to know that this server has read a request head and is now
/// waiting on its body before it can assert what `stop()` does to that
/// connection, and nothing else about an in-flight-but-silent request is
/// observable; a fixed sleep was a wall-clock guess that failed under load. A
/// process-global counter let a concurrently running test satisfy another
/// test's barrier, so the counter belongs to the channel under test.
pub(crate) type Accepted = Arc<std::sync::atomic::AtomicUsize>;

/// The 408 Node answers a request that exceeds `requestTimeout` with. Node
/// writes 51 raw bytes and destroys the socket; hyper frames a response
/// through its public API, so the wire adds `date` and `content-length: 0`
/// and the `connection: close` header is what closes the socket. The exact
/// bytes are pinned by `request_timeout_answers_408_and_releases_stop`.
fn request_timeout_response() -> Response<super::Body> {
    Response::builder()
        .status(408)
        .header(header::DATE, httpdate::fmt_http_date(SystemTime::now()))
        .header(header::CONNECTION, "close")
        .body(super::full(Bytes::new()))
        .expect("#1336 request timeout response")
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
    task: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
}

impl Listener {
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) async fn stop(&self) -> std::io::Result<()> {
        let _ = self.shutdown.send(true);
        let task = self.task.lock().expect("#1336 listener task").take();
        if let Some(task) = task {
            task.await.map_err(std::io::Error::other)?
        } else {
            Ok(())
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
pub(crate) async fn listen(
    runtime: Arc<Runtime>,
    port: u32,
    accepted: Option<Accepted>,
) -> Result<Listener, ListenError> {
    listen_with(runtime, port, Timeouts::default(), accepted).await
}

/// `listen` with the connection timers spelled out; the tests drive the same
/// accept/drain code with short timers instead of waiting Node's 60 s.
pub(crate) async fn listen_with(
    runtime: Arc<Runtime>,
    port: u32,
    timeouts: Timeouts,
    accepted: Option<Accepted>,
) -> Result<Listener, ListenError> {
    let port: u16 = port.try_into().map_err(|_| ListenError::bad_port(port))?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
        .await
        .map_err(|error| ListenError::from_bind(&error, port))?;
    start_listener(runtime, listener, timeouts, accepted)
        .map_err(|error| ListenError::from_bind(&error, port))
}

pub(super) fn start_listener(
    runtime: Arc<Runtime>,
    listener: TcpListener,
    timeouts: Timeouts,
    accepted: Option<Accepted>,
) -> std::io::Result<Listener> {
    let bound = listener.local_addr()?.port();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                picked = listener.accept() => {
                    let (stream, _peer) = match picked {
                        Ok(picked) => picked,
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
                    let accepted = accepted.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request| {
                            let runtime = runtime.clone();
                            let accepted = accepted.clone();
                            async move {
                                // The request head has been read and the body
                                // is pending; from here on `stop()` drains this
                                // connection instead of dropping it.
                                if let Some(accepted) = accepted.as_ref() {
                                    accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                }
                                // Node's `requestTimeout`: a request that has
                                // not completed in time is answered 408 and the
                                // connection is closed instead of pending.
                                // #1941: `handle_request` returns `Err` only for
                                // an oversize body, which hyper turns into a
                                // zero-byte close (no response is framed).
                                match tokio::time::timeout(timeouts.request, runtime.handle_request(request)).await {
                                    Ok(response) => response,
                                    Err(_) => Ok(request_timeout_response()),
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
                        // `stop()` may have flipped the watch between this
                        // connection's `accept()` and the receiver clone above.
                        // `changed()` only fires on a *later* update, so that
                        // connection would never see the stop, never
                        // `graceful_shutdown`, and `Listener::stop` would wait
                        // on the drain forever (an idle keep-alive connection
                        // has no timer). The watch value is the authority.
                        if *shutdown.borrow() {
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                            return;
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
        // Preserve this lane's listener release after the connection drain.
        // On Linux, disable the socket before dropping our fd so an inherited
        // CLOEXEC duplicate cannot keep it listening after stop returns.
        #[cfg(target_os = "linux")]
        let shutdown = socket2::SockRef::from(&listener).shutdown(std::net::Shutdown::Both);
        drop(listener);
        #[cfg(target_os = "linux")]
        shutdown?;
        Ok(())
    });
    Ok(Listener {
        port: bound,
        shutdown,
        task: Mutex::new(Some(task)),
    })
}
