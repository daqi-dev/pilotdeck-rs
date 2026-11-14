//! Streaming HTTP reverse proxy and upgrade coordination.
use crate::{Body, HttpClient, ProxyError, acceptor, pairing, upstream};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;
pub struct Proxy {
    pub upstream: String,
    pub client: HttpClient,
}

impl Proxy {
    /// All accepted connections use the same synchronous upgrade read boundary.
    pub async fn serve_connection<S>(self: Arc<Self>, stream: S) -> Result<(), hyper::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        serve_connection_with(stream, move |request, lease| {
            let proxy = self.clone();
            async move { proxy.serve::<S>(request, lease).await }
        })
        .await
    }

    async fn serve<S>(
        &self,
        request: Request<Incoming>,
        lease: Option<ReadLease>,
    ) -> Result<Response<Body>, std::io::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.serve_with_http::<S, _, _>(request, lease, |request| self.http(request))
            .await
    }

    async fn serve_with_http<S, F, Fut>(
        &self,
        mut request: Request<Incoming>,
        lease: Option<ReadLease>,
        http: F,
    ) -> Result<Response<Body>, std::io::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(Request<Incoming>) -> Fut,
        Fut: std::future::Future<Output = Response<Body>>,
    {
        // Node dispatches any Upgrade protocol only when Connection also has
        // an upgrade token, then handleUpgrade checks the pathname and key.
        if is_upgrade(&request) {
            if request.uri().path() != "/ws" {
                // A service error makes Hyper close the socket without an HTTP
                // response, matching TS's socket.destroy() upgrade rejection.
                return Err(std::io::Error::other("upgrade endpoint not found"));
            }
            if acceptor::websocket_accept(request.headers()).is_none() {
                return Err(std::io::Error::other("missing websocket key"));
            }
            return Ok(match self.websocket::<S>(&mut request, lease).await {
                Ok(response) => response,
                Err(error) => {
                    // #2558: a best-effort console line, like TS's
                    // `console.error` — an `eprintln!` here panics when the
                    // write fails (std behavior); the panic would unwind this
                    // connection's service future, so the 502 below would
                    // never reach the client either.
                    crate::native_process::channels::console_log_line(
                        &mut std::io::stderr(),
                        &format!("WS pairing: {error}"),
                    );
                    plain(StatusCode::BAD_GATEWAY, "upstream websocket unavailable")
                }
            });
        }
        Ok(http(request).await)
    }

    async fn http(&self, mut request: Request<Incoming>) -> Response<Body> {
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let uri: hyper::Uri = match format!("{}{path}", self.upstream).parse() {
            Ok(uri) => uri,
            Err(_) => return plain(StatusCode::BAD_REQUEST, "invalid request URI"),
        };
        let host = uri
            .authority()
            .expect("validated upstream")
            .as_str()
            .to_owned();
        *request.uri_mut() = uri;
        strip_hop_headers(request.headers_mut());
        request
            .headers_mut()
            .insert(header::HOST, host.parse().expect("URI authority"));
        match self.client.request(request).await {
            Ok(mut response) => {
                strip_hop_headers(response.headers_mut());
                response.map(|body| {
                    body.map_err(|error| -> ProxyError { Box::new(error) })
                        .boxed()
                })
            }
            Err(error) => {
                // #2558: same non-panicking shape as the pairing line above —
                // a failed stderr write must not take down the service future
                // that still owes the client its 502.
                crate::native_process::channels::console_log_line(
                    &mut std::io::stderr(),
                    &format!("HTTP upstream: {error}"),
                );
                plain(StatusCode::BAD_GATEWAY, "upstream HTTP unavailable")
            }
        }
    }

    async fn websocket<S>(
        &self,
        request: &mut Request<Incoming>,
        lease: Option<ReadLease>,
    ) -> Result<Response<Body>, ProxyError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let accept = acceptor::websocket_accept(request.headers()).expect("validated key");
        let path = request
            .uri()
            .path_and_query()
            .map_or("/ws", |value| value.as_str());
        let (upstream, upstream_headers) = upstream::connect(
            &self.upstream,
            path,
            request.method(),
            request.headers(),
            &accept,
        )
        .await?;
        let upgraded = hyper::upgrade::on(request);
        tokio::spawn(async move {
            let upgraded = upgraded.await;
            drop(lease);
            match upgraded {
                Ok(downstream) => {
                    if let Err(error) =
                        pair_upgraded::<S, _>(downstream, TokioIo::new(upstream)).await
                    {
                        // #2558: a best-effort console line, like TS's
                        // `console.error` — an `eprintln!` here panics when the
                        // write fails (std behavior); tokio would contain the
                        // panic, but it would still kill this pairing task
                        // (both upgraded sockets drop, the connection dies)
                        // while TS's `console.error` loses neither the
                        // connection nor the diagnostic line.
                        crate::native_process::channels::console_log_line(
                            &mut std::io::stderr(),
                            &format!("WS pairing: {error}"),
                        );
                    }
                }
                Err(error) => {
                    // #2558: same best-effort shape as the pairing line above
                    // — a failed stderr write must not kill the upgrade task.
                    crate::native_process::channels::console_log_line(
                        &mut std::io::stderr(),
                        &format!("downstream upgrade: {error}"),
                    );
                }
            }
        });
        Ok(acceptor::websocket_response(upstream_headers, &accept))
    }
}

/// Node's upgrade head ends at the read that completed the HTTP headers.
/// Hyper otherwise performs another read while the service awaits upstream 101.
#[derive(Debug, Default)]
struct ReadState {
    paused: bool,
    waker: Option<Waker>,
}

type SharedReadState = Arc<Mutex<ReadState>>;

#[derive(Debug)]
pub(crate) struct ReadLease(SharedReadState);
impl Drop for ReadLease {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.0.lock().expect("upgrade read state");
            state.paused = false;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct UpgradeReadGate<S> {
    stream: S,
    state: SharedReadState,
    /// #1250: every successfully read byte is Node socket activity, so a
    /// segment of the next request head pushes the idle deadline out
    /// (`node-dribble-valid-segments.out`); without this the connection can
    /// be closed by the previous response's deadline mid-arrival.
    activity: watch::Sender<Option<tokio::time::Instant>>,
    /// #1250/#1292 fix-r1: how many request heads this connection has parsed.
    /// `> 0` is the old `served` flag the idle refresh and the stop-close
    /// logic use ("at least one head completed"); the headers deadline's own
    /// disarm is the [`Self::disarm_after`] watermark instead, so it can
    /// re-arm per request.
    served_heads: Arc<AtomicUsize>,
    /// #1292 fix-r1: the lane's `headersTimeout`, kept so the deadline can
    /// be re-armed per request. Node starts it at the connection's accept
    /// and again at every request head's first parsed byte
    /// (`node_http_parser.cc` `on_message_begin` sets `last_message_start_`;
    /// probe `probe-p11.out`: after a served first request, a follow-up head
    /// dribbling one byte per second — every segment refreshing the 6 s idle
    /// deadline — is answered with the 51-byte 408 at the head's first byte
    /// (first byte + 60 s). fix-r2 probe `probe-c1.out`: the same holds for
    /// the *first*
    /// head — connect, silent 30 s, then an incomplete head gets its 408 at
    /// first byte + 90 s on the checker grid, not at connect + 60 s.
    headers_timeout: Option<std::time::Duration>,
    /// #1292: when the currently awaited request head's `headersTimeout`
    /// expires ([`NODE_HEADERS_TIMEOUT`]); `None` once disarmed. Disarmed the
    /// moment the awaited head completed (the [`Self::disarm_after`]
    /// watermark passing) or the stop was sent (`b3-T3d.out`:
    /// `server.close()` clears Node's connections checker, so the deadline
    /// stays silent after it).
    headers_deadline: Option<tokio::time::Instant>,
    /// #1292 fix-r1: the native listener's bind instant — the phase anchor of
    /// Node's connections checker, whose 30 s interval grid the 408's landing
    /// quantizes to (`setInterval(checkConnections, 30_000)` armed at the
    /// `'listening'` event, so the phase is the server's, not the
    /// connection's; fix-r1 probe `probe-p12a.out`: a 6 s timeout whose
    /// head starts 1.5 s after listen lands at the absolute 9 s grid
    /// point, not the connection-relative 7.5 s). `None` fires at the
    /// deadline itself — no listener stands behind the socket.
    grid_anchor: Option<tokio::time::Instant>,
    /// #1292 fix-r1: response-finish generations, bumped by each
    /// [`IdleBody`] completion (`IdleBody::release`). Node leaves no
    /// headers deadline between requests (`on_message_complete` clears
    /// `last_message_start_`); this lane arms at the finish — the 6 s idle
    /// close owns the silent case just the same — and re-arms at the next
    /// head's first byte, which also covers a head already mid-arrival
    /// when its predecessor's response ends. fix-r2: when such a head is
    /// already mid-arrival the finish keeps that head's own deadline
    /// instead of re-arming (Node's `last_message_start_` was set at the
    /// head's first byte, and the response ending does not move it).
    rearm: watch::Receiver<usize>,
    /// #1292 fix-r1: `served_heads` at deadline-arm time: the head that
    /// completes after an arming is the one that disarms it.
    disarm_after: usize,
    /// #1292 fix-r2: set while the awaited request head has received its
    /// first byte (Node's `on_message_begin` has fired for it): the armed
    /// deadline is that head's own, and later dribble segments never defer
    /// it. Cleared when the head parsed (the disarm watermark passing) —
    /// the next read in the dead zone before its response finishes
    /// belongs to the *next* head and starts its own window (fix-r2
    /// review #2: a follow-up head already arriving while the previous
    /// response still streams is timed from its first byte, not from the
    /// rearm at the response's finish).
    awaited_head_started: bool,
    /// #1292 fix-r2: hyper has written bytes for a response that has not
    /// finished. Node's `socketOnError` answers a headers-timeout expiry
    /// with the 51-byte 408 only while no in-flight `ServerResponse` has
    /// sent its head (`!this._httpMessage || !this._httpMessage
    /// ._headerSent`; probe `probe-g2.out`/`probe-c2.out`: a streaming
    /// response is simply destroyed with no 408, `probe-g4.out`: a held
    /// response that has written nothing still gets the 408). Mirrors
    /// `_headerSent` at the hand-off (fix-r2, mutant
    /// `native-http-headersent-needs-acceptance`): `poll_write` called
    /// with a non-empty buffer already counts — hyper never writes a
    /// response's body ahead of its head.
    response_head_written: bool,
    /// #1517: hyper's write side holds bytes its flush has not confirmed on
    /// the wire — either the gate itself wrote them (`poll_write`) or the
    /// response's frames were handed off to hyper (`IdleBody::release`, fix-r1:
    /// a tail that never reaches the gate at all — the transport accepts no
    /// byte — must extend the same window, or the quadrant where the tail sits
    /// entirely in hyper's `write_buf` loses both halves at once). Node's
    /// `finish` — the moment `_httpMessage` detaches and the 408 guard opens —
    /// needs the flush, not just the last frame's hand-off to hyper
    /// (`probe-1517.out` T1: `end()` at +88 s, `writableLength` 2 097 167
    /// still unflushed at the +90 s expiry, and the destroy carries no 408), so
    /// the in-flight window this flag extends is exactly Node's attach window:
    /// while it holds, the `_headerSent` mirror stays set (the expiry closes
    /// silently) and a next head's bytes stay withheld gate-side — otherwise
    /// they reach hyper's read buffer and its `mid_message_detect_eof` parks on
    /// the non-empty buffer, stranding the parked headers deadline unpolled
    /// (nothing would ever fire at the grid point). Shared with `IdleBody`
    /// because the release happens on the write side of the same task.
    write_pending: Arc<AtomicBool>,
    /// The connection's own stop signal, consulted only while
    /// [`Self::headers_deadline`] is armed: a stop disarms it (and blocks
    /// re-arming; Node clears its checker in `server.close()`).
    stop: watch::Receiver<bool>,
    /// The parked [`NODE_HEADERS_TIMEOUT`] sleep, polled under the socket
    /// read's waker so a silent first head still wakes at its deadline.
    /// Created only once the deadline is actually consulted: a `Sleep` cannot
    /// exist outside a Tokio runtime, and the gate's own unit test constructs
    /// one runtime-less (with no deadline).
    headers_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    /// #1292 fix-r1: the #1250 in-flight slot count — a request whose head
    /// already parsed is never header-timeout-eligible (Node's
    /// `requestTimeout` governs a busy socket; it is registered as not
    /// replicated). This also covers a pipelined head that parsed before the
    /// response-finish re-arm was consumed.
    in_flight: Arc<AtomicUsize>,
    /// #1292: Node's 408 bytes still to write, and how much of them was
    /// written; the read side reports EOF and hyper's own writes are refused
    /// once they are on the socket.
    timeout_408: Option<&'static [u8]>,
    timeout_408_written: usize,
    timeout_408_closed: bool,
    /// #1292 fix-r2: request-body bytes the connection still owes the handler
    /// — set from the parsed request's `Content-Length` (a chunked body
    /// stays "open", the conservative residual) — so the read gate can tell
    /// the request's own body segments from a *next* head's first byte
    /// arriving mid-response. Node's parser consumes both from the socket,
    /// but only a head start opens a `headersTimeout` window
    /// (`last_message_start_` is set at `on_message_begin`, not by body
    /// bytes).
    body_remaining: Arc<AtomicI64>,
    /// #1292 fix-r2: bytes of a next request head that arrived while the
    /// previous response was still streaming, parked gate-side instead of
    /// hyper's read buffer. Hyper cannot parse them until the response
    /// finishes (`can_read_head` needs `Reading::Init`), and once they land
    /// in its read buffer its `mid_message_detect_eof` parks on the
    /// non-empty buffer without polling io again — stranding the gate (and
    /// its parked headers deadline) without a poll to fire in. Held back and
    /// re-fed to hyper's parse path once the response finishes, exactly like
    /// Node's socket buffer holds them for its parser.
    withheld_head: Vec<u8>,
    /// #1292 fix-r2: EOF seen while withholding — parked with the bytes and
    /// reported to hyper only when they are, the same way hyper parks a
    /// mid-stream EOF behind a non-empty read buffer.
    withheld_eof: bool,
}
impl<S> UpgradeReadGate<S> {
    #[allow(clippy::too_many_arguments)] // The per-request headersTimeout state machine's inputs.
    fn new(
        stream: S,
        state: SharedReadState,
        activity: watch::Sender<Option<tokio::time::Instant>>,
        served_heads: Arc<AtomicUsize>,
        headers_timeout: Option<std::time::Duration>,
        headers_deadline: Option<tokio::time::Instant>,
        grid_anchor: Option<tokio::time::Instant>,
        stop: Option<watch::Receiver<bool>>,
        rearm: watch::Receiver<usize>,
        in_flight: Arc<AtomicUsize>,
        write_pending: Arc<AtomicBool>,
        body_remaining: Arc<AtomicI64>,
    ) -> Self {
        // A stop-less connection gets a never-true receiver: the gate only
        // consults its stop while the deadline is armed.
        let (never_stop, gate_stop) = watch::channel(false);
        drop(never_stop);
        Self {
            stream,
            state,
            activity,
            served_heads,
            headers_timeout,
            headers_deadline,
            grid_anchor,
            stop: stop.unwrap_or(gate_stop),
            rearm,
            disarm_after: 0,
            awaited_head_started: false,
            response_head_written: false,
            write_pending,
            headers_sleep: headers_deadline
                .map(|deadline| Box::pin(tokio::time::sleep_until(deadline))),
            in_flight,
            timeout_408: None,
            timeout_408_written: 0,
            timeout_408_closed: false,
            body_remaining,
            withheld_head: Vec::new(),
            withheld_eof: false,
        }
    }

    /// #1292 fix-r1/fix-r2: arm the headers deadline for a request head.
    /// Node starts a fresh `headersTimeout` at every `on_message_begin`
    /// (`node_http_parser.cc` sets `last_message_start_`), so this runs at
    /// each response finish (a standby window, `started == false`, whose
    /// first byte re-arms at the true request start) and at a head's first
    /// byte (`started == true`: the deadline is that head's own and later
    /// dribble segments never defer it).
    fn arm_headers_deadline(&mut self, started: bool) {
        if crate::mutants::on("native-http-headers-rearm-off") {
            // The pre-fix-r1 behavior the negative control kills: the
            // deadline armed once at the connection's start and never
            // again, so a follow-up head that keeps the idle deadline
            // refreshed hangs past every headers deadline.
            return;
        }
        let Some(timeout) = self.headers_timeout else {
            return;
        };
        if *self.stop.borrow_and_update() {
            return;
        }
        let landing =
            headers_deadline_landing(tokio::time::Instant::now() + timeout, self.grid_anchor);
        self.headers_deadline = Some(landing);
        self.headers_sleep = Some(Box::pin(tokio::time::sleep_until(landing)));
        self.disarm_after = self.served_heads.load(Ordering::Relaxed);
        self.awaited_head_started = started;
    }

    /// #1292 fix-r2: a request is in flight whose body is fully read, so a
    /// read now can only serve the *next* request's head — bytes hyper
    /// cannot parse until the response finishes (`can_read_head` needs
    /// `Reading::Init`). Only the native lane withholds them: it is what
    /// keeps the read gate — and the parked headers deadline — pollable
    /// through hyper's `mid_message_detect_eof` while the response streams
    /// (`probe-g2.out`/`probe-c2.out`); the proxy lane has no headers
    /// deadline and keeps reading through.
    /// #1517: the in-flight window extends past the last frame's hand-off to
    /// hyper — while the write side has not flushed, Node's `_httpMessage` is
    /// still attached (its `finish` needs the flush), and releasing the head
    /// bytes into hyper's read buffer would park its read machinery on the
    /// non-empty buffer, stranding the parked deadline unpolled (`_headerSent`
    /// keeps the byte-reading half of the same window; the two flags cover
    /// one attach window together). fix-r1: `write_pending` covers the tail
    /// that never reached the gate too — set at `IdleBody::release` — or a
    /// transport that accepts no byte after the body's end parks hyper with
    /// `in_flight == 0` and nothing withheld, and the grid point goes quiet
    /// (review concurrency #1, `probe-1517-t3.out` T3a).
    fn withholding_mid_stream(&self) -> bool {
        self.headers_timeout.is_some()
            && (self.in_flight.load(Ordering::Relaxed) > 0
                || (self.write_pending.load(Ordering::Relaxed)
                    // The pre-#1517 withholding the negative control kills:
                    // the in-flight slot alone, so a tail stuck entirely in
                    // hyper's write buffer withholds nothing and the half
                    // head parks the parked deadline away.
                    && !crate::mutants::on("native-http-withholding-unflushed-tail")))
            && self.body_remaining.load(Ordering::Relaxed) <= 0
    }

    /// #1292 fix-r2: count `read` bytes against the awaited request's body,
    /// so the reader can tell where the body ends and the next head starts —
    /// Node's parser sets `last_message_start_` only at `on_message_begin`.
    fn consume_request_body(&self, read: usize) {
        let remaining = self.body_remaining.load(Ordering::Relaxed);
        if remaining > 0 {
            self.body_remaining
                .fetch_sub(std::cmp::min(remaining, read as i64), Ordering::Relaxed);
        }
    }

    /// #1250: both read paths (the withheld mid-stream read and the plain
    /// one) refresh Node's idle deadline on a read segment with the same
    /// condition, kept in one place so each fixture switch has the one
    /// compiled site the negative-controls preflight requires.
    fn read_refreshes_idle(&self) -> bool {
        if crate::mutants::on("native-http-read-refresh-off") {
            // The pre-fix-r1 behavior the negative control kills: a read
            // segment no longer refreshes the idle deadline, so the previous
            // response's deadline closes the connection while the follow-up
            // head is still arriving.
            return false;
        }
        crate::mutants::on("native-http-unserved-read-arms-idle")
            // The pre-fix-r1 behavior the negative control kills: refreshing
            // on any read, so a first head's segments arm the idle deadline
            // and poison the socket.
            || self.served_heads.load(Ordering::Relaxed) > 0
    }

    /// #1292 fix-r2: a request head's first byte just arrived (not a body
    /// segment): arm the head's own deadline. Two shapes reach here — a
    /// standby window's awaited head, for the connection's *first* head too
    /// (`probe-c1.out`: silent 30 s then an incomplete head is answered at
    /// first byte + 90 s, not at connect + 60 s), and a head already
    /// arriving while its predecessor's response streams
    /// (`probe-g2.out`/`probe-c2.out`) or between responses.
    fn arm_started_head(&mut self, cx: &mut Context<'_>, head_start: bool) {
        if !head_start || self.awaited_head_started {
            return;
        }
        let served = self.served_heads.load(Ordering::Relaxed) > self.disarm_after;
        let first_head = self.served_heads.load(Ordering::Relaxed) == 0;
        let standby = self.headers_deadline.is_some();
        if (standby && !(first_head && crate::mutants::on("native-http-first-head-anchor-off")))
            || (!standby
                && served
                // The pre-fix-r2 behavior the negative control kills: a
                // mid-stream head never anchoring, so the streaming
                // response's finish re-arms from its own instant instead of
                // the head's first byte.
                && !crate::mutants::on("native-http-mid-stream-head-anchor-off"))
        {
            self.arm_headers_deadline(true);
            // Park the fresh deadline under this poll's waker straight
            // away: while the previous response streams, this gate's own
            // poll is the only place the deadline will ever be consulted —
            // hyper's mid-message read keeps polling io exactly because the
            // withheld bytes never reach its read buffer — so the sleep
            // must register its wake before the poll that armed it parks.
            if let Some(sleep) = self.headers_sleep.as_mut() {
                let _ = sleep.as_mut().poll(cx);
            }
        }
    }
}
impl<S: AsyncWrite + Unpin> UpgradeReadGate<S> {
    /// Write Node's 408 on the task that owns the socket, then report EOF.
    /// The bytes stay in the kernel buffer exactly like Node's
    /// `socket.write(response)` followed by `destroy()`.
    fn poll_headers_timeout_408(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while let Some(raw) = self.timeout_408 {
            match Pin::new(&mut self.stream).poll_write(cx, &raw[self.timeout_408_written..]) {
                Poll::Ready(Ok(0)) => break,
                Poll::Ready(Ok(written)) => {
                    self.timeout_408_written += written;
                    if self.timeout_408_written == raw.len() {
                        break;
                    }
                }
                Poll::Ready(Err(_)) => break,
                // `poll_write` registered the write waker; self-waking here
                // would spin the executor until the socket drains.
                Poll::Pending => return Poll::Pending,
            }
        }
        self.timeout_408 = None;
        self.timeout_408_closed = true;
        Poll::Ready(Ok(()))
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for UpgradeReadGate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.timeout_408_closed {
            return Poll::Ready(Ok(()));
        }
        if self.timeout_408.is_some() {
            return self.poll_headers_timeout_408(cx);
        }
        // #1292 fix-r2: no in-flight response means no response head is on
        // the wire — Node's socket detaches `_httpMessage` at the response's
        // finish — so an expiry from here on writes the 408 (`probe-g4.out`).
        // While one is streaming, poll_write has set the flag and the expiry
        // closes silently instead (`probe-g2.out`/`probe-c2.out`).
        // #1517: Node's `finish` itself waits for the flush — an ended body
        // whose tail hyper has not flushed keeps `_httpMessage` attached, so
        // the mirror must stay set and the expiry still closes silently
        // (`probe-1517.out` T1/T2). fix-r1: the clear consults the shared
        // flag, which the release sets even when the tail never reaches the
        // gate. The mutant reverts to the in-flight slot alone — the
        // pre-#1517 clear — so the expiry arms the 408 ahead of the stuck
        // tail.
        if self.in_flight.load(Ordering::Relaxed) == 0
            && (!self.write_pending.load(Ordering::Relaxed)
                || crate::mutants::on("native-http-408-into-unflushed-tail"))
        {
            self.response_head_written = false;
        }
        // #1292 fix-r1: each response finish re-arms the deadline for the
        // next request head. The bump is only a signal — the instant is read
        // here — so a bump consumed late still lands after every reachable
        // idle close. fix-r2: a head already mid-arrival — its first byte
        // anchored its own deadline — keeps it; the response ending does not
        // move Node's `last_message_start_`.
        // #1517 fix-r1: a bump consumed while the write side has not
        // flushed would arm Node's stand-by window before the `finish` it is
        // measured from — with no follow-up head Node has no window at all
        // (`probe-1517-t3.out` T3b: the +150 s grid passes with the
        // connection open and the tail undelivered) — so the change stays
        // unconsumed until the flush confirms; hyper polls the gate the
        // whole time, and the flush that confirms is the same transition
        // that consumes it.
        if self.rearm.has_changed().unwrap_or(false)
            && (!self.write_pending.load(Ordering::Relaxed)
                // The pre-fix-r1 behavior the negative control kills: the
                // stand-by re-arm at the frame hand-off, arming a window
                // Node never opens while the tail is stuck.
                || crate::mutants::on("native-http-standby-rearm-ignores-unflushed"))
        {
            self.rearm.borrow_and_update();
            if !self.awaited_head_started
                || crate::mutants::on("native-http-rearm-resets-started-head")
            {
                self.arm_headers_deadline(false);
            }
        }
        if let Some(deadline) = self.headers_deadline {
            // #1292 fix-r2: the armed deadline belongs to one specific head.
            // `served`: that head parsed — the disarm watermark passing — so
            // the deadline disarms and the next read starts the next head's
            // own window. `busy`: a standby window (the anchored head has no
            // bytes yet) while another request is still in flight — a
            // pipelined head that parsed before the response-finish re-arm
            // was consumed. A *started* head's deadline rides out the
            // previous response's stream: Node's checker stays active
            // through it (`probe-g2.out`/`probe-c2.out`).
            let served = self.served_heads.load(Ordering::Relaxed) > self.disarm_after;
            let busy = self.in_flight.load(Ordering::Relaxed) > 0
                && (!self.awaited_head_started
                    // The pre-fix-r2 behavior the negative control kills:
                    // the in-flight check disarming a mid-stream head's own
                    // deadline too, so a head arriving during a streaming
                    // response is never bounded.
                    || crate::mutants::on("native-http-headers-timeout-busy-blocks-head"));
            if *self.stop.borrow_and_update()
                || (served || busy)
                    // The regression the negative control kills: answering a
                    // request that already parsed — its head completed, or its
                    // response is still streaming — with the 408. Node's
                    // parser leaves the checker's active list on
                    // `on_message_complete`, so a served or busy socket is
                    // never header-timeout-eligible.
                    && !crate::mutants::on("native-http-headers-timeout-ignores-served")
            {
                self.headers_deadline = None;
                self.awaited_head_started = false;
            } else {
                let sleep = self
                    .headers_sleep
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
                let _ = sleep.as_mut().poll(cx);
                if tokio::time::Instant::now() >= deadline
                    && !crate::mutants::on("native-http-headers-timeout-off")
                {
                    if self.response_head_written
                        && !crate::mutants::on("native-http-408-into-started-response")
                    {
                        // #1292 fix-r2: Node's `socketOnError` answers the
                        // expiry with a bare `destroy` once the in-flight
                        // response has sent its head (`_headerSent` guard) —
                        // no 408, the stream just ends
                        // (`probe-g2.out`/`probe-c2.out`: close-only at the
                        // grid point).
                        self.timeout_408_closed = true;
                        return Poll::Ready(Ok(()));
                    }
                    self.timeout_408 = Some(NODE_HEADERS_TIMEOUT_408);
                    self.timeout_408_written = 0;
                    return self.poll_headers_timeout_408(cx);
                }
            }
        }
        {
            let mut state = self.state.lock().expect("upgrade read state");
            if state.paused {
                state.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        // #1292 fix-r2: a next head's bytes arriving while the response
        // streams. Hyper parks once such bytes reach its read buffer (its
        // `mid_message_detect_eof` waits on the non-empty buffer without
        // polling io again), so the gate parks them itself — keeping
        // hyper's io polls, and the parked headers deadline, alive — and
        // re-feeds them to the parse path once the response finishes.
        if self.withholding_mid_stream() {
            let mut scratch = [0u8; 4096];
            let mut arrived = 0usize;
            loop {
                // #1292 fix-r3: stop reading once the cap is reached instead
                // of extending the buffer without bound — the socket's own
                // backpressure stands in for Node's 16 KB TrackHeader
                // rejection, which the lane reproduces through hyper's
                // `max_headers` parse after the response finishes.
                if self.withheld_head.len() >= WITHHELD_HEAD_CAP {
                    break;
                }
                let mut read_buf = ReadBuf::new(&mut scratch);
                match Pin::new(&mut self.stream).poll_read(cx, &mut read_buf) {
                    Poll::Ready(Ok(())) => {
                        let n = read_buf.filled().len();
                        if n == 0 {
                            // Peer EOF parks with the bytes: hyper today
                            // parks a mid-stream EOF behind a non-empty read
                            // buffer the same way.
                            self.withheld_eof = true;
                            break;
                        }
                        self.withheld_head.extend_from_slice(&scratch[..n]);
                        arrived += n;
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => break,
                }
            }
            if arrived > 0 {
                // The withheld bytes are all next-head bytes: the request's
                // own body is fully read (withholding_mid_stream), so the
                // first of them is the head's first byte — Node's
                // `on_message_begin` — and it anchors the head's own
                // deadline.
                self.arm_started_head(cx, true);
                if self.read_refreshes_idle() {
                    self.activity
                        .send_replace(Some(tokio::time::Instant::now()));
                }
            }
            if self.withheld_head.len() >= WITHHELD_HEAD_CAP {
                // #1292 fix-r3: at the cap the gate no longer polls the
                // socket, so nothing else would register a waker for the
                // parked headers deadline — poll the sleep here, the same
                // way `arm_started_head` parks it under this poll's waker.
                if let Some(sleep) = self.headers_sleep.as_mut() {
                    let _ = sleep.as_mut().poll(cx);
                }
            }
            return Poll::Pending;
        }
        // The response finished: hand the withheld head bytes to hyper's
        // parse path, one buffer at a time — the parse loop re-polls for
        // the rest.
        if !self.withheld_head.is_empty() {
            let n = std::cmp::min(buf.remaining(), self.withheld_head.len());
            buf.put_slice(&self.withheld_head[..n]);
            if n == self.withheld_head.len() {
                self.withheld_head.clear();
            } else {
                self.withheld_head.drain(..n);
            }
            return Poll::Ready(Ok(()));
        }
        if self.withheld_eof {
            return Poll::Ready(Ok(()));
        }
        let filled = buf.filled().len();
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > filled {
                    let read = buf.filled().len() - filled;
                    let body_before = self.body_remaining.load(Ordering::Relaxed);
                    self.consume_request_body(read);
                    // #1292 fix-r2: only the bytes past the awaited
                    // request's body can start its successor's head — the
                    // body's own segments never anchor a deadline (Node's
                    // `last_message_start_` is set at `on_message_begin`,
                    // not by body bytes).
                    let head_start = body_before <= 0 || read as i64 > body_before;
                    self.arm_started_head(cx, head_start);
                    if self.read_refreshes_idle() {
                        self.activity
                            .send_replace(Some(tokio::time::Instant::now()));
                    }
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for UpgradeReadGate<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // #1292: Node's 408 already terminated this connection; finish the
        // terminal write, then refuse every hyper byte so none of its own
        // encoding follows it.
        if self.timeout_408.is_some() {
            if self.poll_headers_timeout_408(cx).is_pending() {
                return Poll::Pending;
            }
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "connection closed",
            )));
        }
        if self.timeout_408_closed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "connection closed",
            )));
        }
        // #1292 fix-r2: hyper received response bytes — Node's `_headerSent`:
        // hyper never writes a response's body ahead of its head, so a
        // non-empty `poll_write` means the in-flight response's head has
        // been handed off, and a headers-timeout expiry from here on closes
        // silently (`probe-g2.out`/`probe-c2.out`).
        // #1517: the bytes are only *handed off* — Node's `finish` needs the
        // flush — so the same write also holds the response's in-flight
        // window open until `poll_flush` confirms the drain.
        // fix-r2: the mirror is set at the hand-off too, not at the
        // transport's acceptance — Node's `_headerSent` flips at
        // `res.write`, before the wire accepts a single byte
        // (`probe-1517-c.json`: a pipelined second response whose head sits
        // in the JS outputData queue with `res.socket === null` already
        // reports `headersSent === true`, zero bytes on the wire), so a head
        // whose first write the transport refuses — a kernel send buffer the
        // previous traffic filled, parked in hyper's write buffer — still
        // counts as started and the expiry destroys silently instead of
        // writing a 408 ahead of it. The mutant reverts to the
        // byte-accepted condition — the pre-fix-r2 mirror — so the expiry
        // arms the 408 into the never-accepted head's connection.
        if !buf.is_empty() && !crate::mutants::on("native-http-headersent-needs-acceptance") {
            self.response_head_written = true;
        }
        match Pin::new(&mut self.stream).poll_write(cx, buf) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.response_head_written = true;
                    self.write_pending.store(true, Ordering::Relaxed);
                }
                Poll::Ready(Ok(written))
            }
            other => other,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match Pin::new(&mut self.stream).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                // #1517: the flush confirms every byte hyper handed off is on
                // the wire — the same transition hyper itself treats as the
                // write side's finish (its `poll_flush` only reaches the io
                // layer once its own write buffer drained, `io.rs`:
                // `poll_flush` polls the io `poll_flush` only from the
                // drained state). The response's in-flight window closes
                // here, not at the last frame's hand-off, matching Node's
                // `_httpMessage` detach at `finish`. fix-r1: this is also
                // the one transition that clears the release's hand-off
                // share of the flag — a tail that never reached the gate is
                // confirmed by the same drained flush. A write error or a
                // shutdown instead leaves the flag set on purpose: hyper
                // tears the connection down on either (the response future
                // resolves and the socket drops), so no follow-up head can
                // ever observe the stale window (review fix-r1 Grok #1).
                if !crate::mutants::on("native-http-flush-clear-off") {
                    self.write_pending.store(false, Ordering::Relaxed);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

pub(crate) fn is_upgrade(request: &Request<Incoming>) -> bool {
    request.headers().contains_key(header::UPGRADE)
        && request
            .headers()
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

pub(crate) async fn serve_connection_with<S, F, Fut>(
    stream: S,
    handler: F,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    serve_connection_configured(stream, handler, false, None, None, None, None).await
}

/// Node v22.22.0's default `server.keepAliveTimeout` (the TS
/// `GatewayServer` does not override it and advertises
/// `Keep-Alive: timeout=5` — the bare value, not this deadline).
const NODE_KEEP_ALIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Node v22.22.0's fixed `server.keepAliveTimeoutBuffer` default. Node arms
/// the idle socket timeout with `keepAliveTimeout + keepAliveTimeoutBuffer`,
/// so an idle keep-alive connection actually closes 6 s after its last
/// activity (live TS server read back `keepAliveTimeout=5000`
/// `keepAliveTimeoutBuffer=1000`; #1250 probes `node-keepalive-matrix.out`
/// and `node-dribble-valid-segments.out`: the close lands at last activity +
/// 6000 ms — a response's quiet +5749 ms, a follow-up head's last segment
/// +6006 ms).
const NODE_KEEP_ALIVE_TIMEOUT_BUFFER: std::time::Duration = std::time::Duration::from_secs(1);

/// Node `http.createServer`'s default `headersTimeout` (v22.22.0: 60 000 ms).
/// The native listener bounds every still-unserved request head with it
/// (#1292 probes `b1-T1a.out`/`b1-T1b.out`: a partial first head and even a
/// zero-byte connection are answered with the 51-byte 408 at +60.05 s;
/// fix-r1 `probe-p11.out`: a served connection's follow-up head dribbling
/// one byte per second — every segment refreshing the 6 s idle deadline —
/// is answered 408 at the head's first byte + 60 s; a silent follow-up head
/// is owned by the 6 s idle close, `b1-T2.out`), like the `user_dialog`
/// store lane already does. The deadline is re-armed per request — at each
/// response finish and again at the next head's first byte — because Node
/// restarts `last_message_start_` at every `on_message_begin`
/// (`node_http_parser.cc`), and a stop disarms it like Node clears its
/// connections checker in `server.close()` (`b3-T3d.out`).
/// `requestTimeout` (300 s) is not replicated: on every probeable TS path it
/// is unreachable (`b3-T3b.out`: a real route answers immediately; `b3-T5.out`:
/// the one body-consuming route answers 401 before its body loop), also
/// registered in 002-R2.
const NODE_HEADERS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// #1292 fix-r3: upper bound on the bytes a next request head may park in
/// [`UpgradeReadGate::withheld_head`] while the previous response still
/// streams. Node never buffers such a head whole: its parser counts the head
/// against the 16 KB `TrackHeader` budget as the bytes arrive and answers
/// `HPE_HEADER_OVERFLOW` (431 + destroy) once it is passed. The gate caps at
/// twice that budget — far enough for hyper's own `max_headers(16384)` parse
/// to see the overflow and answer through the lane's registered 431 path,
/// small enough that no client can pin an unbounded buffer for the length of
/// a streaming response (MiniMax gate boundary P1: the uncapped `Vec` grew
/// with the client's upload rate, and refeeding it copied O(n²) bytes).
const WITHHELD_HEAD_CAP: usize = 32 * 1024;

/// Node v22.22.0 `_http_server.js`: the connections checker runs on
/// `setInterval(checkConnections, 30_000)` — the default
/// `connectionsCheckingInterval` — with its phase anchored at the server's
/// `'listening'` event, and a `headersTimeout` 408 only ever lands on the
/// tick that first finds the deadline passed (fix-r1 `probe-p12a.out`: a 6 s
/// timeout whose head starts 1.5 s after listen lands at the absolute 9 s
/// grid point, not the connection-relative 7.5 s; `probe-p11.out`: the
/// default 60 s follow-up deadline lands at +90 s; `mech-P2.out`: an
/// overridden 5 s timeout lands at +5.5 s). A deadline that falls within the
/// checker's tick-execution jitter of a grid point is bimodal in Node
/// (`b1-T1a.out`: the +60.04 s deadline was still caught by the +60 s tick,
/// ~45 ms of post-startup loop lag; `mech-P2.out`'s near-identical setup
/// waited for the next point) — registered as the one residual in
/// A8.4-NATIVE-HTTP-002-R2: this lane deterministically fires at the first
/// grid point strictly after the deadline, never at or before the deadline.
const NODE_CONNECTIONS_CHECKING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// #1292 fix-r1/fix-r2: the instant the headersTimeout 408 fires — the first
/// connections-checker grid point strictly *after* the deadline
/// ([`NODE_CONNECTIONS_CHECKING_INTERVAL`] ticks from the listener's bind),
/// or the deadline itself when no grid anchors the connection.
fn headers_deadline_landing(
    deadline: tokio::time::Instant,
    grid_anchor: Option<tokio::time::Instant>,
) -> tokio::time::Instant {
    if crate::mutants::on("native-http-headers-grid-off") {
        // The pre-grid-replication behavior the negative control kills:
        // firing at the deadline itself, up to one checker interval before
        // Node's next tick would.
        return deadline;
    }
    let Some(anchor) = grid_anchor else {
        return deadline;
    };
    let interval = NODE_CONNECTIONS_CHECKING_INTERVAL;
    let elapsed = deadline.saturating_duration_since(anchor).as_nanos();
    // The checker's comparison is strictly greater (`ConnectionsList::Expired`
    // expires when `last_message_start_ < now - timeout`, node_http_parser
    // v22.22.0 disassembly at 0xda3db0: `b.cs` skips the expiry on equality),
    // so a deadline that lands exactly on a grid point survives that tick and
    // fires at the next one — 整除进一. fix-r2 `probe-c1.out`: a first byte at
    // +30 s arms exactly the +90 s grid point and Node answers at +120.001 s.
    let ticks = if crate::mutants::on("native-http-headers-grid-inclusive") {
        // The pre-fix-r2 behavior the negative control kills: an on-grid
        // deadline firing at its own grid point, one checker interval before
        // Node's strictly-greater comparison would let it.
        elapsed.div_ceil(interval.as_nanos())
    } else {
        elapsed / interval.as_nanos() + 1
    };
    let ticks = u32::try_from(ticks).expect("the checker grid stays within u32 ticks");
    anchor + interval * ticks
}

/// Node v22.22.0 `_http_server.js` `onRequestTimeout`: exactly these bytes —
/// the same 51 the `user_dialog` store lane writes (its fixture
/// `user-dialog-http-store/header-lifecycle/node-408.bin` records the TS
/// shape; each lane keeps its own copy of the canned Node bytes, like the
/// two 431s).
const NODE_HEADERS_TIMEOUT_408: &[u8] =
    b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n";

/// The native listener's connection serving. TS emits literal header cases
/// (`Date`, `Connection` capitalized), so responses render Title-Case, and
/// every native_http handler response sets `Date` explicitly, so the
/// automatic date stays off: the bare parser-level 400s then match TS's
/// undated ones. The byte-level transport diffs this leaves are registered
/// in the gateway-parity diff ledger.
///
/// `stop` mirrors Node's `server.close()` semantics (probes
/// `probe-fixr1.out` B, `probe-fixr2.out` D, `probe-fixr3-b.out` B1): a
/// connection that is idle keep-alive at that instant is closed right away,
/// while a connection with a request in flight — a parsed head still inside
/// the handler or a response body still streaming, #1250 — or one that never
/// sent bytes, including requests it sends only after the stop — is answered
/// with the normal keep-alive response and never force-closed; such sockets
/// close at their own 6 s keep-alive idle timeout (below).
/// `graceful_shutdown` is hyper's equivalent for the idle-at-close case and
/// is a no-op for an already-upgraded connection. A dropped sender means no
/// stop signal, and the connection simply runs to its natural end.
///
/// Every still-unserved request head is additionally bounded by Node's
/// `headersTimeout` ([`NODE_HEADERS_TIMEOUT`], #1292): a partial or zero-byte
/// first head is answered with Node's 51-byte 408 and closed, and the
/// deadline is re-armed per request — at each response finish and again at
/// the next head's first byte (fix-r1, probe `probe-p11.out`; a silent
/// follow-up head is owned by the 6 s keep-alive idle close instead,
/// `b1-T2.out`). With `grid_anchor` (the listener's bind instant, the native
/// listener's own serving passes it) the 408 lands on the connections
/// checker's 30 s grid like Node's; without one it fires at the deadline
/// itself.
///
/// #1292: `stop_observed` is a test-only observation — it counts how often
/// this connection's serving loop has consumed a genuine stop (a dropped
/// sender is not one), so a test can deterministically sequence its next
/// step after the stop, the replacement for the fixed 100/200 ms sleeps the
/// #1209 stop tests used to race; it observes no production behavior.
pub(crate) async fn serve_connection_native_with_shutdown<S, F, Fut>(
    stream: S,
    handler: F,
    stop: watch::Receiver<bool>,
    stop_observed: Option<Arc<AtomicUsize>>,
    grid_anchor: Option<tokio::time::Instant>,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    serve_connection_configured(
        stream,
        handler,
        true,
        Some(stop),
        Some(NODE_HEADERS_TIMEOUT),
        stop_observed,
        grid_anchor,
    )
    .await
}

/// Decrements the in-flight request count when the response future is done
/// or dropped (hyper can cancel a service future when the connection goes
/// away mid-request), so a stop never mistakes a torn-down request for an
/// idle connection.
struct InFlightRequest(Arc<AtomicUsize>);
impl Drop for InFlightRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// #1250: a response body that keeps the request's in-flight slot held until
/// the body has actually finished streaming, and arms the keep-alive idle
/// deadline at that completion — Node's idle timer arms at the response
/// `finish`, not at handler return, so a mid-body hold is never closed by a
/// deadline armed at the head (probe `node-keepalive-matrix.out` cell S4a:
/// head + one chunk on the wire, no writes for 8 s, the `socket TIMEOUT`
/// event only fires after the body ends).
struct IdleBody {
    body: Body,
    held: Option<(InFlightRequest, watch::Sender<Option<tokio::time::Instant>>)>,
    /// #1292 fix-r1: the connection's per-request re-arm signal, bumped at the
    /// same body completion that re-arms the idle deadline.
    rearm: Option<watch::Sender<usize>>,
    /// #1517 fix-r1: the connection's shared write-side flag — the frame
    /// hand-off means hyper's write side now owns bytes it has not confirmed,
    /// whether or not the transport ever accepts them (a tail stuck entirely
    /// in hyper's `write_buf` must keep Node's attach window open exactly
    /// like one half-flushed through the gate). Cleared only by the gate's
    /// confirming `poll_flush`.
    write_pending: Option<Arc<AtomicBool>>,
}
impl IdleBody {
    /// Releases the slot and re-arms the idle deadline at body completion.
    /// Idempotent: the frame end and the drop can both reach it.
    ///
    /// #1517 fix-r3: that idempotence is load-bearing on the write-side flag
    /// specifically — `take()` is what makes it so. The first arrival is the
    /// frame end (`poll_frame`'s `Ready(None)`, hyper polling the body), and
    /// the second is this body's `Drop` (the response handed off and dropped,
    /// or the connection torn down). Between the two the flag may already
    /// have been cleared by the gate's confirming `poll_flush` — the tail
    /// fully on the wire — and a second `store(true)` here would re-set it
    /// with no write left behind it, so no later flush could ever clear it:
    /// the three read-side consumers (`withholding_mid_stream` withholding
    /// follow-up heads, the mirror clear in `poll_read`, the stand-by
    /// re-arm) would hold the flag forever, and the next request would be
    /// withheld indefinitely.
    fn release(&mut self) {
        if let Some(rearm) = self.rearm.take() {
            rearm.send_modify(|generation| *generation += 1);
        }
        if let Some(write_pending) = self.write_pending.take() {
            write_pending.store(true, Ordering::Relaxed);
        }
        if let Some((guard, activity)) = self.held.take() {
            drop(guard);
            activity.send_replace(Some(tokio::time::Instant::now()));
        }
    }
}
impl hyper::body::Body for IdleBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        let poll = Pin::new(&mut self.body).poll_frame(context);
        if matches!(poll, Poll::Ready(None)) {
            self.release();
        }
        poll
    }
    fn size_hint(&self) -> http_body::SizeHint {
        // Wrapping must not hide the inner body's exact size (the
        // parser-level 400/431 bodies are exact-0 and must stay unchunked).
        self.body.size_hint()
    }
}
impl Drop for IdleBody {
    fn drop(&mut self) {
        self.release();
    }
}

async fn serve_connection_configured<S, F, Fut>(
    stream: S,
    handler: F,
    native: bool,
    stop: Option<watch::Receiver<bool>>,
    headers_timeout: Option<std::time::Duration>,
    stop_observed: Option<Arc<AtomicUsize>>,
    grid_anchor: Option<tokio::time::Instant>,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    let state = SharedReadState::default();
    let served_heads = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    // #1517 fix-r1: hyper's write side holding unconfirmed bytes — set by the
    // gate's own writes and by `IdleBody::release` alike (a tail stuck in
    // hyper's `write_buf` extends the same attach window), cleared by the
    // gate's confirming `poll_flush`.
    let write_pending = Arc::new(AtomicBool::new(false));
    // #1292 fix-r2: the bytes the awaited request still owes its handler —
    // set at each parsed head — so the read gate can tell the request's own
    // body segments from a next head's first byte arriving mid-response.
    let body_remaining = Arc::new(AtomicI64::new(0));
    let (activity, mut activity_rx) = watch::channel(None::<tokio::time::Instant>);
    // #1292 fix-r1: each finished response bumps the gate's re-arm signal, so
    // the next request head is bounded by a fresh `headersTimeout` like
    // Node re-arms `last_message_start_` at every `on_message_begin`.
    let (rearm, rearm_rx) = watch::channel(0usize);
    // #1292: the native lane bounds its still-unserved first head with
    // Node's `headersTimeout`; the proxy lane fronts a different TS
    // counterpart and keeps its previous behavior (no probe).
    let headers_deadline = native
        .then(|| headers_timeout.map(|timeout| tokio::time::Instant::now() + timeout))
        .flatten()
        .map(|deadline| headers_deadline_landing(deadline, grid_anchor));
    let stream = UpgradeReadGate::new(
        stream,
        state.clone(),
        activity.clone(),
        served_heads.clone(),
        native.then_some(headers_timeout).flatten(),
        headers_deadline,
        grid_anchor,
        stop.clone(),
        rearm_rx,
        in_flight.clone(),
        write_pending.clone(),
        body_remaining.clone(),
    );
    let served_heads_in_connection = served_heads.clone();
    let in_flight_in_connection = in_flight.clone();
    let write_pending_in_connection = write_pending.clone();
    let activity_in_connection = activity.clone();
    let rearm_in_connection = rearm.clone();
    let body_remaining_in_connection = body_remaining.clone();
    let service = service_fn(move |request| {
        served_heads_in_connection.fetch_add(1, Ordering::Relaxed);
        // #1292 fix-r2: this request's body byte count. Node's parser sets
        // `last_message_start_` only at `on_message_begin` — a body segment
        // never opens a headersTimeout window — so the gate counts the body
        // down to tell where it ends; a chunked body stays "open" (the
        // conservative residual: no mid-stream anchoring for it).
        let body = match (
            request
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok()),
            request.headers().contains_key(header::TRANSFER_ENCODING),
        ) {
            (Some(length), _) => length,
            (None, true) => i64::MAX,
            (None, false) => 0,
        };
        body_remaining_in_connection.store(body, Ordering::Relaxed);
        // This must run before returning a future: Hyper can poll read again
        // before it polls that future. Invalid upgrade requests close anyway.
        let lease = if is_upgrade(&request)
            && request.uri().path() == "/ws"
            && acceptor::websocket_accept(request.headers()).is_some()
        {
            state.lock().expect("upgrade read state").paused = true;
            Some(ReadLease(state.clone()))
        } else {
            None
        };
        activity_in_connection.send_replace(Some(tokio::time::Instant::now()));
        in_flight_in_connection.fetch_add(1, Ordering::Relaxed);
        let in_flight = InFlightRequest(in_flight_in_connection.clone());
        let activity = activity_in_connection.clone();
        let rearm = rearm_in_connection.clone();
        let write_pending = write_pending_in_connection.clone();
        let handler = handler(request, lease);
        async move {
            let result = handler.await;
            match result {
                Ok(response) => {
                    // #1250: Node's keep-alive idle timer arms at the
                    // response `finish`, not at handler return, so the
                    // in-flight slot stays held until the body has actually
                    // finished streaming (`node-keepalive-matrix.out` cell
                    // S4a: an 8 s mid-body hold with no writes is not closed
                    // by the idle deadline armed at the response head).
                    let held = if crate::mutants::on("native-http-inflight-body-off") {
                        // The pre-#1250 behavior the negative control
                        // kills: release and re-arm at handler return.
                        drop(in_flight);
                        activity.send_replace(Some(tokio::time::Instant::now()));
                        None
                    } else {
                        Some((in_flight, activity))
                    };
                    Ok(response.map(|body| {
                        IdleBody {
                            body,
                            held,
                            rearm: Some(rearm.clone()),
                            write_pending: Some(write_pending.clone()),
                        }
                        .boxed()
                    }))
                }
                Err(error) => {
                    drop(in_flight);
                    Err(error)
                }
            }
        }
    });
    let mut builder = http1::Builder::new();
    if native {
        builder.title_case_headers(true).auto_date_header(false);
        if !crate::mutants::on("native-http-header-count-off") {
            // Node v22.22.0 has no header-count limit: its binding constraint
            // is the 16384-byte URL+header-names+values budget the router
            // replicates (probe `probe-fixr3-c.out` C4: 16363 one-byte
            // headers stay served, 16364 hits the byte budget), so a count
            // cap of 16384 never binds before that budget; it only bounds
            // hyper's per-parse smallvec allocation. hyper's default of 100
            // would 431 requests TS serves.
            if crate::mutants::on("native-http-header-cap-low") {
                builder.max_headers(3000);
            } else {
                builder.max_headers(16384);
            }
        }
    }
    let connection = builder
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    let Some(stop) = stop else {
        return connection.await;
    };
    tokio::pin!(connection);
    // Node's keep-alive idle close, in force with or without a stop: the
    // deadline is the last activity — a request parse, a read segment of the
    // next request head, or the completion of a response body — plus the
    // effective `keepAliveTimeout + keepAliveTimeoutBuffer` (6 s), re-armed
    // on every new activity. Before the first request is served no deadline
    // exists at all (Node arms the timer only after a response finished;
    // fix-r1 probes `node-first-head-segmented.out`), and while a request is
    // in flight, which includes a response body still streaming, the timer
    // never fires (Node's requestTimeout, not keepAliveTimeout, governs a
    // busy socket), and the next activity update re-arms it.
    let idle = async {
        if crate::mutants::on("native-http-keep-alive-timeout-off") {
            std::future::pending::<()>().await;
        }
        let buffer = if crate::mutants::on("native-http-idle-close-buffer-off") {
            std::time::Duration::ZERO
        } else if crate::mutants::on("native-http-idle-close-buffer-half") {
            // The half-buffer mutant pins the fixed 1 s buffer: a 500 ms
            // buffer closes at 5.5 s, inside the tightened (5.99, 6.09] test
            // window's still-open probe.
            std::time::Duration::from_millis(500)
        } else {
            NODE_KEEP_ALIVE_TIMEOUT_BUFFER
        };
        loop {
            let deadline = activity_rx
                .borrow_and_update()
                .map(|at| at + NODE_KEEP_ALIVE_TIMEOUT + buffer);
            let Some(deadline) = deadline else {
                if activity_rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    if in_flight.load(Ordering::Relaxed) > 0 {
                        if activity_rx.changed().await.is_err() {
                            std::future::pending::<()>().await;
                        }
                        continue;
                    }
                    return;
                }
                _ = activity_rx.changed() => {}
            }
        }
    };
    tokio::pin!(idle);
    let mut stop = Some(stop);
    loop {
        tokio::select! {
            result = &mut connection => return result,
            changed = async {
                match stop.as_mut() {
                    Some(receiver) => receiver
                        .wait_for(|stopped| *stopped)
                        .await
                        .map(|stopped| *stopped),
                    None => std::future::pending::<
                        Result<bool, tokio::sync::watch::error::RecvError>,
                    >()
                    .await,
                }
            } => {
                if changed.is_err() {
                    // The sender is dropped without a stop: no stop can ever
                    // arrive, so only the idle close and the peer end this.
                    stop = None;
                    continue;
                }
                // #1292: test-only observation that this connection's
                // serving loop has consumed a genuine stop, so a test can
                // sequence its next step after it deterministically (the
                // replacement for the #1209 stop tests' fixed sleeps).
                if let Some(observed) = &stop_observed
                    && !crate::mutants::on("native-http-stop-observed-off")
                {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
                let idle_now = (served_heads.load(Ordering::Relaxed) > 0
                    && (in_flight.load(Ordering::Relaxed) == 0
                        || crate::mutants::on("native-http-inflight-close")))
                    || crate::mutants::on("native-http-unserved-close");
                if idle_now && !crate::mutants::on("native-http-idle-close-off") {
                    connection.as_mut().graceful_shutdown();
                }
                // close() only destroys the connections that were idle at
                // that instant; requests in flight — which includes a
                // response body still streaming — and later requests are
                // answered normally, and their sockets still end at the
                // keep-alive idle close below, so the stop is consumed but
                // the connection keeps being served.
                stop = None;
                continue;
            }
            _ = &mut idle => {
                connection.as_mut().graceful_shutdown();
                return connection.as_mut().await;
            }
        }
    }
}

async fn pair_upgraded<S, U>(
    downstream: hyper::upgrade::Upgraded,
    upstream: U,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin,
{
    pairing::run(upgraded_stream::<S>(downstream)?, upstream).await
}

pub(crate) fn upgraded_stream<S>(downstream: hyper::upgrade::Upgraded) -> std::io::Result<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let parts = downstream
        .downcast::<TokioIo<UpgradeReadGate<S>>>()
        .map_err(|_| std::io::Error::other("unexpected downstream upgrade IO type"))?;
    // GatewayServer.ts:67 ignores Node's upgrade `head`. Hyper's equivalent
    // is read_buf: the synchronous gate ensures it contains only bytes read
    // with the HTTP headers, never a later read while waiting for upstream.
    // Upstream response read-ahead remains intact because it is TS output.
    drop(parts.read_buf);
    Ok(parts.io.into_inner().stream)
}

pub(crate) fn strip_hop_headers(headers: &mut hyper::HeaderMap) {
    let nominated: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(|part| part.trim().to_owned()))
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

pub(crate) fn empty() -> Body {
    Full::new(Bytes::new())
        .map_err(|never: Infallible| match never {})
        .boxed()
}
pub(crate) fn plain(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(message.as_bytes()))
            .map_err(|never: Infallible| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_native_ws_sample::RFC6455_SAMPLE_NONCE;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::{Duration, timeout},
    };

    #[test]
    fn upgrade_key_acceptance_matches_ts_without_rfc_validation() {
        let mut headers = hyper::HeaderMap::new();
        assert!(acceptor::websocket_accept(&headers).is_none());
        for key in ["", "not-base64", "YQ=="] {
            headers.insert(header::SEC_WEBSOCKET_KEY, key.parse().unwrap());
            assert!(acceptor::websocket_accept(&headers).is_some());
        }
        headers.insert(
            header::SEC_WEBSOCKET_KEY,
            RFC6455_SAMPLE_NONCE.parse().unwrap(),
        );
        assert_eq!(
            acceptor::websocket_accept(&headers).as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        // Method and Sec-WebSocket-Version do not participate in TS accept.
        headers.insert(header::SEC_WEBSOCKET_VERSION, "12".parse().unwrap());
        assert!(acceptor::websocket_accept(&headers).is_some());
    }

    #[test]
    fn upstream_upgrade_headers_survive_with_downstream_accept_key() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_ACCEPT,
            "upstream-key".parse().unwrap(),
        );
        headers.insert("x-upstream", "retained".parse().unwrap());
        headers.append(header::SET_COOKIE, "one=1".parse().unwrap());
        headers.append(header::SET_COOKIE, "two=2".parse().unwrap());
        let response = acceptor::websocket_response(headers, "downstream-key");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response.headers()[header::SEC_WEBSOCKET_ACCEPT],
            "downstream-key"
        );
        assert_eq!(response.headers()["x-upstream"], "retained");
        assert_eq!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            2
        );
    }

    async fn rejected_upgrade(path: &str, key: Option<&str>, protocol: &str, connection: &str) {
        let (server_io, mut client_io) = tokio::io::duplex(4096);
        let proxy = Arc::new(Proxy {
            upstream: "http://127.0.0.1:28790".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
        let key = key
            .map(|key| format!("Sec-WebSocket-Key: {key}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: {protocol}\r\nConnection: {connection}\r\n{key}\r\n"
        );
        timeout(
            Duration::from_secs(3),
            client_io.write_all(request.as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(3), client_io.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.is_empty(), "TS destroys socket without HTTP/WS reply");
        assert!(
            timeout(Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_client_key_destroys_socket_before_upstream_connect() {
        rejected_upgrade("/ws?allowed=query", None, "websocket", "Upgrade").await;
    }

    #[tokio::test]
    async fn only_ws_path_upgrades_and_other_paths_destroy_socket() {
        for path in ["/", "/WS", "/ws/", "/other?path=/ws"] {
            rejected_upgrade(path, Some("arbitrary-key"), "websocket", "Upgrade").await;
        }
    }

    #[tokio::test]
    async fn arbitrary_upgrade_protocol_uses_ts_path_and_key_rejections() {
        for protocol in ["h2c", "WebSocket, custom", "custom-protocol"] {
            rejected_upgrade("/other", Some("key"), protocol, "Upgrade").await;
            rejected_upgrade("/ws", None, protocol, "Upgrade").await;
        }
    }

    #[tokio::test]
    async fn upgrade_header_without_connection_token_uses_http_proxy() {
        timeout(Duration::from_secs(3), async {
            // Inject the HTTP handler so this routing regression needs no
            // listener or upstream service. Production uses Proxy::http.
            for connection in [None, Some("keep-alive"), Some("x-upgrade")] {
                let (server_io, client_io) = tokio::io::duplex(4096);
                let proxy = Arc::new(Proxy {
                    upstream: "http://fixture.invalid".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                });
                let server = tokio::spawn(async move {
                    serve_connection_with(server_io, move |request, lease| {
                        let proxy = proxy.clone();
                        async move {
                            proxy
                                .serve_with_http::<tokio::io::DuplexStream, _, _>(
                                    request,
                                    lease,
                                    |request| async move {
                                        assert_eq!(request.uri().path(), "/health");
                                        assert_eq!(request.headers()[header::UPGRADE], "h2c");
                                        plain(StatusCode::OK, "healthy")
                                    },
                                )
                                .await
                        }
                    })
                    .await
                    .unwrap();
                });
                let (mut sender, client) =
                    hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                        .await
                        .unwrap();
                let client = tokio::spawn(client);
                let mut request = Request::builder()
                    .uri("/health")
                    .header(header::HOST, "localhost")
                    .header(header::UPGRADE, "h2c")
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                if let Some(connection) = connection {
                    request
                        .headers_mut()
                        .insert(header::CONNECTION, connection.parse().unwrap());
                }
                let response = sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(
                    response.into_body().collect().await.unwrap().to_bytes(),
                    "healthy"
                );
                drop(sender);
                client.await.unwrap().unwrap();
                server.await.unwrap();
            }
        })
        .await
        .expect("HTTP routing test timeout");
    }

    #[tokio::test]
    async fn connection_upgrade_token_list_uses_h2c_path_and_key_rejections() {
        for connection in [
            "keep-alive, Upgrade",
            "keep-alive, uPgRaDe",
            "keep-alive\r\nConnection: \tUPGRADE\t, close",
        ] {
            rejected_upgrade("/health", Some("key"), "h2c", connection).await;
            rejected_upgrade("/ws", None, "h2c", connection).await;
        }
    }

    #[tokio::test]
    async fn get_and_first_frame_in_same_write_discards_upgrade_head() {
        use crate::frame::{client_frame, server_frame};
        use tokio::io::DuplexStream;

        timeout(Duration::from_secs(3), async {
            let (downstream, mut client) = tokio::io::duplex(4096);
            let (upstream, mut fake_ts) = tokio::io::duplex(4096);
            let first = client_frame(0x81, 127, b"discard this first frame");
            let second = client_frame(0x81, 126, b"forward only after upgrade");
            let mut request = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n".to_vec();
            request.extend_from_slice(&first);
            // Queue the entire packet before Hyper starts reading so the first
            // frame deterministically belongs to upgrade head, not a later read.
            client.write_all(&request).await.unwrap();
            let (upgrade_tx, upgrade_rx) = tokio::sync::oneshot::channel();
            let upgrade_tx = Arc::new(std::sync::Mutex::new(Some(upgrade_tx)));
            let http = tokio::spawn(async move {
                let handler = move |mut request: Request<Incoming>, lease| {
                    let upgraded = hyper::upgrade::on(&mut request);
                    upgrade_tx.lock().unwrap().take().unwrap().send((upgraded, lease)).unwrap();
                    async { Ok(acceptor::websocket_response(hyper::HeaderMap::new(), "accept")) }
                };
                serve_connection_with(downstream, handler).await.unwrap();
            });
            let (upgraded, _lease) = upgrade_rx.await.unwrap();
            let upgraded = upgraded.await.unwrap();
            let pair = tokio::spawn(pair_upgraded::<DuplexStream, _>(upgraded, upstream));
            let mut response = Vec::new();
            while !response.ends_with(b"\r\n\r\n") {
                response.push(client.read_u8().await.unwrap());
            }
            assert!(response.starts_with(b"HTTP/1.1 101"));
            client.write_all(&second).await.unwrap();
            let mut forwarded = vec![0; second.len()];
            fake_ts.read_exact(&mut forwarded).await.unwrap();
            assert_eq!(forwarded, second, "pre-read first frame must be discarded");
            let reply = server_frame(1, b"only second frame reached TS");
            fake_ts.write_all(&reply).await.unwrap();
            fake_ts.shutdown().await.unwrap();
            let mut output = Vec::new();
            client.read_to_end(&mut output).await.unwrap();
            assert_eq!(output, reply);
            pair.await.unwrap().unwrap();
            http.await.unwrap();
        }).await.expect("upgrade head test timeout");
    }

    #[test]
    fn http_hop_headers_do_not_remove_end_to_end_headers() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            "keep-alive, x-private-hop".parse().unwrap(),
        );
        headers.insert("x-private-hop", "remove".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer isolated".parse().unwrap());
        headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        strip_hop_headers(&mut headers);
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-private-hop"));
        assert_eq!(headers[header::AUTHORIZATION], "Bearer isolated");
        assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    }

    // --------------------------------------------------------------- #2558
    // stderr

    /// The re-exec probe's env: `run:<token>`; the child also requires the
    /// token as an argv positional, so a stray env value alone can never
    /// branch a normal test run into the child path (the #2301 probe-token
    /// shape, like the #2434 ws probe).
    const STDERR_PROBE_ENV: &str = "PDRS_2558_PROXY_STDERR_PROBE";
    const STDERR_MARKER_ENV: &str = "PDRS_2558_PROXY_STDERR_MARKER";

    /// The accept `RFC6455_SAMPLE_NONCE` derives — the pinned key/accept
    /// pair the acceptor unit test above asserts, reused by the probe's fake
    /// TS so its 101 passes the upstream handshake check.
    const PROBE_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn stderr_probe_token() -> Option<String> {
        let value = std::env::var_os(STDERR_PROBE_ENV)?.to_str()?.to_owned();
        let (_, token) = value.split_once(':')?;
        if token.is_empty() {
            return None;
        }
        std::env::args()
            .any(|argument| argument == token)
            .then(|| token.to_owned())
    }

    /// #2558: this module's four error lines (`WS pairing` on the
    /// upstream-connect failure and inside the spawned pairing task,
    /// `HTTP upstream` in the plain proxy path, `downstream upgrade` on the
    /// upgrade error) were `eprintln!`, which panics when the stderr write
    /// fails (std behavior). For the awaited serve paths the panic unwinds
    /// the connection's service future, so the 502 the line precedes would
    /// never reach the client; for the spawned pairing task tokio would
    /// contain the panic but still kill the connection (both upgraded
    /// sockets drop), while TS's `console.error` never loses either. The
    /// fix is the shared `console_log_line`, which drops the write error.
    /// This control drives all four sites in a re-exec'd child whose stderr
    /// is a pipe with the read end already dropped (every write fails with
    /// EPIPE); the child installs a panic hook first, so a panicking
    /// `eprintln!` leaves a marker file and exits 33 instead of passing. A
    /// read-only fd is not a failing sink for this purpose: `std::io::Stderr`
    /// redirects EBADF to a fresh `/dev/null` and reports success, so only
    /// EPIPE reaches the `eprintln!` panic.
    #[tokio::test]
    async fn error_console_lines_survive_a_failed_stderr_write() {
        if let Some(token) = stderr_probe_token() {
            stderr_probe_child(&token).await;
            return;
        }
        let token = format!("pdrs-2558-{}", uuid::Uuid::new_v4().simple());
        let marker = std::env::temp_dir().join(format!("{token}.marker"));
        let mut child = tokio::process::Command::new(
            std::env::current_exe().expect("#2558 the probe binary path"),
        )
        .args([
            "--exact",
            "http_proxy::tests::error_console_lines_survive_a_failed_stderr_write",
            "--nocapture",
            "--test-threads=1",
            &token,
        ])
        .env(STDERR_PROBE_ENV, format!("run:{token}"))
        .env(STDERR_MARKER_ENV, &marker)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("#2558 the failed-stderr probe child spawns");
        // The failing sink: a pipe whose read end is dropped before the child
        // writes, so every stderr write fails with EPIPE.
        drop(child.stderr.take());
        // The stdout pipe is drained concurrently with the wait, never after
        // it (the #2301 probe's drain-before-wait shape): a full pipe — a
        // failing child prints a whole backtrace — would block the child
        // before it could exit.
        let stdout = child.stdout.take();
        let console_task = tokio::spawn(async move {
            let mut out = Vec::new();
            if let Some(mut pipe) = stdout {
                let _ = pipe.read_to_end(&mut out).await;
            }
            String::from_utf8_lossy(&out).into_owned()
        });
        // Every wait states its termination condition: 30 seconds on the
        // child, then kill + reap before the named failure.
        let status = match timeout(Duration::from_secs(30), child.wait()).await {
            Ok(status) => status.expect("#2558 the failed-stderr probe child is waitable"),
            Err(_) => {
                child
                    .kill()
                    .await
                    .expect("#2558 the timed-out probe child is killed");
                let _ = child.wait().await;
                crate::negative_assert!(
                    false,
                    "#2558 the failed-stderr probe child did not finish within 30s"
                );
                unreachable!("negative_assert! panics");
            }
        };
        let console = console_task
            .await
            .expect("#2558 the probe's stdout drain finishes");
        crate::negative_assert!(
            status.success(),
            "#2558 the error lines survive a failed stderr write (exit {status})\n{console}"
        );
        // Positive receipt — success alone is not evidence the probe case
        // ran: a renamed test makes the `--exact` filter match nothing and
        // libtest still exits 0 (the #273 / fix-r1 lesson).
        crate::negative_assert!(
            crate::test_exact_receipt::single_pass(
                &status,
                "http_proxy::tests::error_console_lines_survive_a_failed_stderr_write",
                &console,
            ),
            "#2558 the child's libtest receipt proves the probe case ran\n{console}"
        );
        // The child leaves one reached marker per scenario, each written only
        // after that error line's code path has provably run.
        for scenario in ["a", "b", "c", "d"] {
            let reached = std::env::temp_dir().join(format!("{token}.reached.{scenario}"));
            crate::negative_assert!(
                reached.exists(),
                "#2558 the child never reached error-line scenario {scenario}"
            );
            let _ = std::fs::remove_file(&reached);
        }
        crate::negative_assert!(
            !marker.exists(),
            "#2558 no panic marker after the error lines ({}): {:?}",
            marker.display(),
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    /// The child half of [`error_console_lines_survive_a_failed_stderr_write`]:
    /// panic hook first (marker + exit 33), then the four error-line
    /// scenarios in sequence. Nothing here prints to stdout: the
    /// single-threaded receipt needs the whole test line intact.
    async fn stderr_probe_child(token: &str) {
        let marker = std::env::var(STDERR_MARKER_ENV)
            .unwrap_or_else(|_| panic!("#2558 {token} is missing the marker env"));
        std::panic::set_hook(Box::new(move |info| {
            // The default hook would print to the failed stderr; the marker is
            // the parent's evidence instead.
            let _ = std::fs::write(&marker, format!("{info}"));
            std::process::exit(33);
        }));
        let reached =
            |scenario: char| std::env::temp_dir().join(format!("{token}.reached.{scenario}"));

        // Scenario A — `HTTP upstream`: a plain request whose upstream
        // connect fails. The line runs inside the awaited serve future
        // before the 502 is built, so the response below is the proof the
        // write already happened.
        {
            let (server_io, client_io) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: "http://127.0.0.1:1".into(),
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                    .await
                    .expect("#2558 scenario A handshake");
            let connection = tokio::spawn(connection);
            let request = Request::builder()
                .uri("/")
                .header(header::HOST, "localhost")
                .body(Full::new(Bytes::new()))
                .expect("#2558 scenario A request");
            let response = timeout(Duration::from_secs(10), sender.send_request(request))
                .await
                .expect("#2558 scenario A is answered in time")
                .expect("#2558 scenario A sends its request");
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "#2558 scenario A: the 502 the error line still owes the client"
            );
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                "upstream HTTP unavailable",
                "#2558 scenario A: the 502 body the error line still owes the client"
            );
            drop(sender);
            let _ = timeout(Duration::from_secs(3), connection)
                .await
                .expect("#2558 scenario A connection ends");
            let _ = timeout(Duration::from_secs(3), server)
                .await
                .expect("#2558 scenario A serve future ends");
            let _ = std::fs::write(reached('a'), b"reached");
        }

        // Scenario B — `WS pairing` on the upstream-connect failure: the
        // same 502 family through the websocket arm (an unparseable
        // upstream origin fails before any socket is opened).
        {
            let (server_io, client_io) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: "invalid upstream".into(),
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                    .await
                    .expect("#2558 scenario B handshake");
            let connection = tokio::spawn(connection);
            let upgrade = Request::builder()
                .uri("/ws")
                .header(header::HOST, "localhost")
                .header(header::CONNECTION, "Upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_KEY, "key")
                .body(Full::new(Bytes::new()))
                .expect("#2558 scenario B request");
            let response = timeout(Duration::from_secs(10), sender.send_request(upgrade))
                .await
                .expect("#2558 scenario B is answered in time")
                .expect("#2558 scenario B sends its upgrade");
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "#2558 scenario B: the 502 the pairing line still owes the client"
            );
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                "upstream websocket unavailable",
                "#2558 scenario B: the 502 body the pairing line still owes the client"
            );
            drop(sender);
            let _ = timeout(Duration::from_secs(3), connection)
                .await
                .expect("#2558 scenario B connection ends");
            let _ = timeout(Duration::from_secs(3), server)
                .await
                .expect("#2558 scenario B serve future ends");
            let _ = std::fs::write(reached('b'), b"reached");
        }

        // Scenario C — the pairing task's `WS pairing` line. A real fake TS
        // accepts the upstream upgrade; the client parks the send pump with
        // a close frame (a close never completes `send`; receive owns final
        // teardown) and then dies, and the next server frame makes
        // receive's downstream write fail — the pairing task's error line.
        // The fake TS's EOF is the receipt that the task (and its line)
        // finished; a bare downstream death would instead let `send` return
        // on its EOF and complete the pair cleanly, so the close frame is
        // what keeps the error path deterministic.
        {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("#2558 scenario C binds the fake TS");
            let upstream_origin = format!(
                "http://{}",
                listener
                    .local_addr()
                    .expect("#2558 scenario C bound address")
            );
            let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let fake = tokio::spawn(async move {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("#2558 scenario C accepts the proxy");
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let mut byte = [0u8; 1];
                    socket
                        .read_exact(&mut byte)
                        .await
                        .expect("#2558 scenario C reads the proxied head");
                    head.extend_from_slice(&byte);
                }
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                             Connection: Upgrade\r\nSec-WebSocket-Accept: {PROBE_ACCEPT}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .expect("#2558 scenario C answers the upgrade");
                go_rx
                    .await
                    .expect("#2558 scenario C waits for the downstream death");
                socket
                    .write_all(&crate::frame::server_frame(1, b"#2558 late frame"))
                    .await
                    .expect("#2558 scenario C sends the late frame");
                let mut tail = Vec::new();
                socket
                    .read_to_end(&mut tail)
                    .await
                    .expect("#2558 scenario C reads the pairing task's EOF");
                let _ = done_tx.send(());
            });
            let (server_io, mut client_io) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: upstream_origin,
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
            client_io
                .write_all(
                    format!(
                        "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                         Upgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("#2558 scenario C sends the upgrade");
            let mut response = Vec::new();
            while !response.ends_with(b"\r\n\r\n") {
                let mut byte = [0u8; 1];
                client_io
                    .read_exact(&mut byte)
                    .await
                    .expect("#2558 scenario C reads the 101 head");
                response.extend_from_slice(&byte);
            }
            assert!(
                response.starts_with(b"HTTP/1.1 101"),
                "#2558 scenario C: the upgrade completed"
            );
            // Park the send pump with a close frame, then kill the
            // downstream; the buffered head still parses after the drop.
            client_io
                .write_all(&crate::frame::client_frame(0x88, 0, b""))
                .await
                .expect("#2558 scenario C sends the close frame");
            drop(client_io);
            let _ = go_tx.send(());
            timeout(Duration::from_secs(10), done_rx)
                .await
                .expect("#2558 scenario C pairing task ends")
                .expect("#2558 scenario C done receipt survives");
            let _ = timeout(Duration::from_secs(3), server)
                .await
                .expect("#2558 scenario C serve future ends");
            let _ = timeout(Duration::from_secs(3), fake)
                .await
                .expect("#2558 scenario C fake TS ends");
            let _ = std::fs::write(reached('c'), b"reached");
        }

        // Scenario D — `downstream upgrade`: the client dies before the
        // upgrade is handed over, so the spawned task's upgrade future
        // resolves with the connection error instead. The request bytes
        // are buffered before the drop, so the proxy still parses the whole
        // head and reaches the response write that fails on the dead
        // downstream.
        {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("#2558 scenario D binds the fake TS");
            let upstream_origin = format!(
                "http://{}",
                listener
                    .local_addr()
                    .expect("#2558 scenario D bound address")
            );
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let fake = tokio::spawn(async move {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("#2558 scenario D accepts the proxy");
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let mut byte = [0u8; 1];
                    socket
                        .read_exact(&mut byte)
                        .await
                        .expect("#2558 scenario D reads the proxied head");
                    head.extend_from_slice(&byte);
                }
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                             Connection: Upgrade\r\nSec-WebSocket-Accept: {PROBE_ACCEPT}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .expect("#2558 scenario D answers the upgrade");
                let mut tail = Vec::new();
                socket
                    .read_to_end(&mut tail)
                    .await
                    .expect("#2558 scenario D reads the upgrade task's EOF");
                let _ = done_tx.send(());
            });
            let (server_io, mut client_io) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: upstream_origin,
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
            client_io
                .write_all(
                    format!(
                        "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                         Upgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("#2558 scenario D sends the upgrade");
            drop(client_io);
            timeout(Duration::from_secs(10), done_rx)
                .await
                .expect("#2558 scenario D upgrade task ends")
                .expect("#2558 scenario D done receipt survives");
            let _ = timeout(Duration::from_secs(3), server)
                .await
                .expect("#2558 scenario D serve future ends");
            let _ = timeout(Duration::from_secs(3), fake)
                .await
                .expect("#2558 scenario D fake TS ends");
            let _ = std::fs::write(reached('d'), b"reached");
        }
    }
}

#[cfg(test)]
#[path = "http_proxy_tests.rs"]
mod read_boundary_tests;
