//! #1955/#2499: the sms listener's **request-head** deadline (Node
//! `headersTimeout`).
//!
//! #1955 established the deadline itself: without one, a client that wrote
//! half a request head and stopped kept the detached connection task and the
//! socket forever — unauthenticated connection exhaustion. hyper's
//! `.timer(TokioTimer::new()).header_read_timeout(60 s)` pair answered that
//! with a zero-byte close. #2499 (#2026 decision B) replaces the pair with
//! `mod.rs`'s [`HeadersTimeoutGate`]: the lane owns the read side and answers
//! the expiry with Node's 51-byte `408 Request Timeout` + `Connection: close`
//! (`crate::user_dialog::NODE_408_RESPONSE`) written straight onto the socket
//! — never through hyper's response path — landing on the 30 s
//! connections-checker grid, i.e. at the first grid point **strictly after**
//! the deadline (整除进一; a deadline that lands exactly on a grid point takes
//! its successor), with the grid's phase anchored at the listener's bind.
//! Measured on the pinned Node: a half-sent head at 89,995 ms, a zero-byte
//! head at 90,004 ms, a first head delayed 35 s at 120.008 s
//! (`_tmp/20261107-issue-1955/smoke/ts-half-head.out`,
//! `ts-zero-and-delayed.out`). The anchored deadline per request head:
//! connection establishment for the first head (Node's accept-time
//! `last_message_start_`), the head's own first byte once it arrives
//! (`on_message_begin`), the previous response's finish for a follow-up head —
//! disarmed the moment the awaited head parsed, and disarmed for good when
//! the channel's stop watch fires.
//!
//! The bounded differences that remain are registered in
//! `specs/gateway-parity/diffs/A8.3-CH-SMS-017.md`: the deterministic
//! strictly-after landing (Node's checker tick runs on real timer jitter) and
//! the idle keep-alive follow-up head's silent close at this 60 s deadline
//! where Node's `keepAliveTimeout` owns the idle connection at 5–6 s — that
//! path stays byte-silent here on purpose (012 residual 3's approved
//! reading).
//!
//! These cases live here rather than in `sms/tests.rs` because the two
//! modules' test identities are registered by different negative-control
//! tables: `tests/fixtures/channel-sms/` pins one module-level
//! `PREFIX = 'channel::sms::tests::'` for all of its rows, which cannot reach
//! `channel::sms::head_timeout_tests::`. #2020 therefore gave this module its
//! own table, `tests/fixtures/channel-sms-head-timeout/negative_controls.py`
//! (`PREFIX = 'channel::sms::head_timeout_tests::'`), instead of rewriting the
//! 71 existing rows. That is also why `NullSender` is duplicated from
//! `sms/tests.rs` instead of shared.
//!
//! #2026 (decision C) adds the `stop()` drain cases: `stop()` now drains the
//! in-flight connections bounded by `STOP_DRAIN_LIMIT` (60 s) instead of
//! detaching them, with the mutation switches `sms-stop-no-drain` (the
//! pre-#2026 detach shape) and `sms-stop-drain-unbounded` (the TS
//! unbounded-block shape) registered in the same table.
//!
//! #2257 adds the closed-channel cases: a `stop()` task that panics at its
//! first log line, or a handle dropped without `stop()`, leaves the accept
//! task unobserved — nobody awaits it — so a drain there would only hold the
//! listener's port (up to `STOP_DRAIN_LIMIT`) for a same-port `start` to fail
//! its bind against. The closed channel now skips the drain; the
//! unconditional one is the mutation switch `sms-stop-drain-on-sender-drop`,
//! and #2269's `sms-stop-drain-predrain-count` restores the r1 pre-drain
//! snapshot the truncation warn used to count with.
//!
//! #2499's landing cells run on the paused clock (`#[tokio::test(start_paused
//! = true)]` + the `manual_clock` harness from `native_http/tests.rs`):
//! `tokio::io::duplex` drives [`serve_sms_connection`] directly with an
//! explicit grid anchor and interval, so every landing is probed at an exact
//! virtual instant — never raced on the wall clock, and no 30 s waits. The
//! wall-clock cases inject the grid through `SmsShared::test_grid_interval`
//! instead (`INJECTED_GRID`), keeping a landing observable inside ~1 s while
//! the byte-exact 408 stays the primary discriminator.

use std::sync::Arc;
use std::time::Duration;

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::super::protocol::{ChannelLog, ChannelStartDeps};
use super::*;

/// Ports sit in the free gap above the wecom-callback golden replay segment
/// (18900–18921) and below the feishu segment (18960–18999: the feishu
/// oracle replay binds 18960–18972, its pins bind 18973/18980/18990–18993).
/// Within the gap the sms lane owns 18922–18923 (the http10 pins), 18924–18925
/// (this module's head-timeout pins, the constants right below), 18926 (the
/// json-null cell), 18927–18928 (the #2152 parser-admission pins) and
/// 18929–18932 (the #2691 port-guard cases: the v6-only occupier, the
/// TOCTOU bind race, the dual-stack preflight and the replay-form
/// post-start guard) — keep
/// this ledger in sync when an sms pin moves: #2257 fix-r4, fix-r5 and
/// #2152 fix-r1 each caught a port collision in this gap. fix-r5 moved the
/// head-timeout pins here off 18971/18972, which the feishu replay binds
/// (same class as the 18904 wecom overlap fixed in fix-r4). 18973–18979
/// remain inside the feishu registered range; only 18973 has an actual
/// cross-lane bind (the feishu NULL_PIN_ROUTER), tracked in #2446.
const PORT_HALF_HEAD: u16 = 18924;
const PORT_POSITIVE: u16 = 18925;
const PORT_DEFAULT_WIRING: u16 = 18973;
const PORT_DEFAULT_DEADLINE: u16 = 18974;
const PORT_STOP_DRAIN: u16 = 18975;
const PORT_STOP_BOUND: u16 = 18977;
const PORT_BIND_RACE: u16 = 18976;
const PORT_STOP_PANIC: u16 = 18978;
const PORT_STOP_DROP: u16 = 18979;
/// The paused-clock cells' `webhookPort`. They never bind: the cells drive
/// [`serve_sms_connection`] over a duplex directly, so the value only feeds
/// `SmsChannelOptions` (the port the channel parses before `start`). Kept in
/// the unclaimed stretch of the gap above so it cannot collide with a bound
/// pin even if a future change starts a listener on it.
const PORT_PAUSED_CELLS: u16 = 18933;

/// The injected deadline for the fast cases. Far above scheduler jitter (the
/// complete head is already in the socket buffer before the server polls) and
/// far below the read deadline the assertions allow.
const INJECTED: Duration = Duration::from_millis(300);
/// #2499: the injected checker-grid interval for the wall-clock fast cases.
/// The landing is the first grid point strictly after the deadline, so a
/// case's 408 arrives between `INJECTED` and `INJECTED + INJECTED_GRID` after
/// the bind instead of up to half a minute later — the wall-clock cells could
/// not afford the production 30 s grid per socket. The paused-clock cells do
/// not use this: they pass the interval to [`serve_sms_connection`] directly.
const INJECTED_GRID: Duration = Duration::from_millis(400);
/// The upper bound for a close that the injected deadline caused (#2020,
/// #2499). The lower bound (`>= INJECTED / 2`) only rejects a zero-length
/// deadline; on its own it lets a close that took 30 s pass. The 408 lands on
/// the injected grid, so the close lands within one grid interval plus
/// scheduler slack of the deadline — `INJECTED_GRID + INJECTED` plus margin —
/// and still far below `READ_LIMIT` so that "the close was slow" and "the
/// client gave up" stay distinguishable.
const CLOSE_UPPER: Duration = Duration::from_millis(1500);
/// How long a client read may take before the test calls it unbounded.
const READ_LIMIT: Duration = Duration::from_secs(5);
/// The head deadline injected by the `stop()` drain cases. The fast case
/// reuses [`INJECTED`]; the bound case needs a connection that **outlives**
/// the 60 s [`STOP_DRAIN_LIMIT`], so its head deadline is deliberately
/// longer than the bound.
const BOUND_DEADLINE: Duration = Duration::from_secs(90);
/// The window in which the drained connection's EOF must already be pending
/// once `stop()` returned: anything that ends the socket inside this window
/// after a bounded drain came from the drain, not from the head deadline.
const DRAIN_OBSERVE: Duration = Duration::from_millis(200);
/// The window in which the **aborted** connection of the bound case must have
/// reached EOF after `stop()` returned: the JoinSet drop aborts the task, and
/// the runtime needs a beat to run the cancellation.
const DRAIN_TRUNCATED: Duration = Duration::from_secs(2);
/// The bracket for the ignored bound case: `stop()` must return at the 60 s
/// drain bound (one tick of slack below), well before the 90 s injected head
/// deadline could have ended the connection on its own.
const BOUND_LOWER: Duration = Duration::from_secs(58);
const BOUND_UPPER: Duration = Duration::from_secs(75);
/// The outer wait for `stop()` in the ignored bound case: above
/// [`BOUND_UPPER`] with margin, below the 90 s connection deadline.
const BOUND_WAIT: Duration = Duration::from_secs(80);
/// The #2257 port-rebind bound: on the closed-channel path the accept task
/// returns without draining, so the listener's port rebinds immediately —
/// well inside this window. A drain nobody observes (the
/// `sms-stop-drain-on-sender-drop` mutation) holds the port for the whole
/// 60 s [`STOP_DRAIN_LIMIT`] instead, because the stalled connection's
/// [`BOUND_DEADLINE`] head deadline outlives the bound.
const REBIND_LIMIT: Duration = Duration::from_secs(2);
/// #2499: the paused-clock probe family's read window — the `timeout` each
/// probe arms on its read before its advance. The windowed read is armed
/// **before** the advance, so a still-open read resolves `Elapsed` while a
/// close the advance crosses lands wins (tokio polls the inner read before
/// the delay); the advance stays strictly greater than the window so the
/// probe always crosses the read's timeout.
const PAUSED_PROBE_READ_WINDOW: Duration = Duration::from_millis(50);
/// #2499: the paused-clock probe family's advance — the virtual time each
/// windowed probe crosses while its read is pending.
const PAUSED_PROBE_ADVANCE: Duration = Duration::from_millis(60);

struct NullSender;
impl SmsSender for NullSender {
    fn send(&self, _message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

fn options(port: u16, injected: Option<Duration>) -> SmsChannelOptions {
    let mut extra = JsObject::new();
    extra.insert(JsString::from("accountSid"), JsValue::from("AC1"));
    extra.insert(JsString::from("authToken"), JsValue::from("tok"));
    extra.insert(JsString::from("phoneNumber"), JsValue::from("+15550000"));
    extra.insert(JsString::from("webhookHost"), JsValue::from("127.0.0.1"));
    extra.insert(
        JsString::from("webhookPort"),
        JsValue::Number(f64::from(port)),
    );
    SmsChannelOptions {
        extra: Some(extra),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(Arc::new(|_sid: &JsString, _token: &JsString| {
            Ok(Arc::new(NullSender) as Arc<dyn SmsSender>)
        })),
        record_unhandled: None,
        request_timeout: injected,
    }
}

/// Start the channel on `port` and return the handle plus the shared state
/// (the leak check compares the `Arc` strong count against a baseline).
///
/// `webhookPort` cannot be 0, so the module uses fixed ports like every other
/// listener test in this crate. A port already taken by another worktree would
/// make `SmsChannel::start` log and return a no-op handle instead of failing,
/// and the test would then measure somebody else's listener — a false PASS. The
/// probe below turns that case into a named failure; the single-shot `connect`
/// covers the other half (our own bind failed, so nothing is listening).
///
/// The probe and the real bind are still two syscalls, so a third party can win
/// the window between them (#2020). The `Arc::strong_count` guard after
/// `start()` closes that hole for every case in this module: the no-op handle
/// holds no `SmsShared` at all, so it cannot pass. The guard's own criterion is
/// pinned from the failing side by `a_start_that_loses_the_bind_race_holds_no_listener`.
///
/// #2499: `grid` injects the connections-checker interval before `start()`
/// takes it for the accept loop's spawn (`None` keeps Node's 30 s).
async fn start_with_grid(
    port: u16,
    injected: Option<Duration>,
    grid: Option<Duration>,
) -> (Arc<dyn ChannelHandle>, Arc<SmsShared>) {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(probe) => drop(probe),
        Err(error) => panic!(
            "#1955 port {port} is already in use ({error}); another worktree is \
             running this suite — re-run when it is done"
        ),
    }
    let channel = SmsChannel::new(options(port, injected));
    let shared = channel.shared.clone();
    if let Some(grid) = grid {
        *shared
            .test_grid_interval
            .lock()
            .expect("#2499 the test grid interval lock") = grid;
    }
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1955 sms start");
    // Dropping the constructor leaves the handle and the accept loop as the
    // only other owners; a start that lost the bind race owns neither.
    drop(channel);
    assert!(
        Arc::strong_count(&shared) >= 2,
        "#1955 start() on port {port} returned the no-op handle (the bind lost \
         the race with another listener), so these assertions would measure a \
         foreign server"
    );
    (handle, shared)
}

/// [`start_with_grid`] with Node's production grid — the cases whose timing
/// does not depend on the landing.
async fn start(port: u16, injected: Option<Duration>) -> (Arc<dyn ChannelHandle>, Arc<SmsShared>) {
    start_with_grid(port, injected, None).await
}

/// [`start`] with an injected logger: the #2257 panic case needs the stop
/// task's first log line to panic, which the plain helper's `log: None`
/// cannot produce.
async fn start_with_logger(
    port: u16,
    injected: Option<Duration>,
    logger: Option<ChannelLog>,
) -> (Arc<dyn ChannelHandle>, Arc<SmsShared>) {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(probe) => drop(probe),
        Err(error) => panic!(
            "#2257 port {port} is already in use ({error}); another worktree is \
             running this suite — re-run when it is done"
        ),
    }
    let channel = SmsChannel::new(options(port, injected));
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: logger,
            turn_gateway: None,
        })
        .await
        .expect("#2257 sms start");
    drop(channel);
    assert!(
        Arc::strong_count(&shared) >= 2,
        "#2257 start() on port {port} returned the no-op handle (the bind lost \
         the race with another listener), so these assertions would measure a \
         foreign server"
    );
    (handle, shared)
}

/// `start()` awaits the bind, so one connection attempt is enough: a retry loop
/// would only paper over a failed bind by waiting for somebody else's listener.
async fn connect(port: u16) -> TcpStream {
    TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("#1955 cannot connect to the sms listener on {port}: {error}")
        })
}

/// Read until the server closes (EOF) or `limit`, returning the bytes seen
/// plus whether the **server** closed. The flag matters: a stalled connection
/// that the server never retires also yields zero bytes, and once the client
/// gives up and drops its socket the connection task ends anyway — so a
/// bytes-only or `Arc`-count-only assertion passes with the deadline removed.
/// Only `closed == true` inside the limit proves the head timer fired. (The
/// mutation switch `sms-header-read-no-timeout` is what pins this: without it
/// the read times out instead of seeing EOF.) A reset counts as closed: that is
/// still the server ending the connection.
async fn read_until_close(socket: &mut TcpStream, limit: Duration) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 512];
    let read = async {
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) => break true,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break true,
                Err(_) => break false,
            }
        }
    };
    match tokio::time::timeout(limit, read).await {
        Ok(closed) => (bytes, closed),
        Err(_) => (bytes, false),
    }
}

/// The fast cases' bounded read.
async fn read_until_eof(socket: &mut TcpStream) -> (Vec<u8>, bool) {
    read_until_close(socket, READ_LIMIT).await
}

/// `Arc::strong_count` of the shared state once every detached connection task
/// has released its clone. Tasks that never return keep a clone alive, so a
/// bounded wait that never reaches the baseline is the leak signal.
async fn wait_for_task_release(shared: &Arc<SmsShared>, baseline: usize) -> usize {
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    loop {
        let count = Arc::strong_count(shared);
        if count <= baseline || tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ── #2499: the paused-clock landing harness (the `native_http/tests.rs`
// manual-clock shape, trimmed to this lane's duplex-driven
// `serve_sms_connection`). Virtual time moves only through the explicit
// advances below, so every landing instant is probed, never raced. ──

/// #2499: the virtual-clock harness from `native_ws::tests` (via
/// `native_http/tests.rs`): the wrapped future is polled against yields of
/// the current-thread runtime, so the serve task makes progress between
/// advances. The wall guard only diagnoses a stalled future; it is never a
/// timing result.
async fn manual_clock<F: std::future::Future>(future: F) -> F::Output {
    tokio::pin!(future);
    let wall = std::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = tokio::task::yield_now() => {}
        }
        assert!(
            wall.elapsed() < Duration::from_secs(60),
            "#2499 manual-clock harness stalled: the serve future never resolved"
        );
    }
}

/// #2499: give the spawned serve task scheduling slots on the paused clock —
/// the native lane's eight yields, enough for the accept → read → arm cycle
/// of one connection between advances.
async fn yield_a_beat() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// #1250's twin helper for reading probe buffers: latin-1 so every byte is
/// one character and the expected ASCII never re-encodes on the way out.
fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

/// #2499: probe for data/EOF at the current virtual time — the windowed read
/// is armed before its `PAUSED_PROBE_ADVANCE` advance, so a still-open read
/// resolves `Elapsed`, while a close latched during the advance wins.
async fn probe_still_open(client: &mut tokio::io::DuplexStream, what: &str) {
    yield_a_beat().await;
    let mut probe = [0u8; 1];
    let pending = tokio::time::timeout(PAUSED_PROBE_READ_WINDOW, client.read(&mut probe));
    tokio::time::advance(PAUSED_PROBE_ADVANCE).await;
    yield_a_beat().await;
    match pending.await {
        Err(_quiet) => {}
        Ok(Ok(0)) => panic!("#2499 {what}"),
        Ok(Ok(count)) => panic!("#2499 {what}: {count} unexpected byte(s) arrived"),
        Ok(Err(error)) => panic!("#2499 {what}: {error}"),
    }
}

/// #2499: read the 51-byte headersTimeout 408 and the EOF that follows it,
/// inside the same windowed probe pattern as `probe_still_open`. The bytes
/// are asserted equal to `crate::user_dialog::NODE_408_RESPONSE` itself — the
/// exact canned response Node writes, never a re-typed literal. A missing 408
/// (the `sms-headers-408-bytes-off` mutant, or a landing that never comes)
/// fails at this helper's own message instead of hanging the harness.
async fn headers_timeout_408_arrives(client: &mut tokio::io::DuplexStream, what: &str) {
    let expected = crate::user_dialog::NODE_408_RESPONSE;
    let mut buffer = Vec::new();
    while buffer.len() < expected.len() {
        let mut chunk = [0u8; 64];
        let pending = tokio::time::timeout(PAUSED_PROBE_READ_WINDOW, client.read(&mut chunk));
        tokio::time::advance(PAUSED_PROBE_ADVANCE).await;
        yield_a_beat().await;
        match pending.await {
            Ok(Ok(0)) => panic!(
                "#2499 {what}: EOF before the 408 arrived (buffer so far: {:?})",
                latin1_string(&buffer)
            ),
            Ok(Ok(count)) => buffer.extend_from_slice(&chunk[..count]),
            Ok(Err(error)) => panic!("#2499 {what}: {error}"),
            Err(_no_408) => {
                panic!(
                    "#2499 {what}: the 51-byte 408 never arrived at the head \
                     deadline's grid landing"
                )
            }
        }
    }
    assert_eq!(
        buffer, expected,
        "#2499 {what}: the bytes are exactly Node's canned 51-byte 408"
    );
    let mut tail = [0u8; 1];
    let pending = tokio::time::timeout(PAUSED_PROBE_READ_WINDOW, client.read(&mut tail));
    tokio::time::advance(PAUSED_PROBE_ADVANCE).await;
    yield_a_beat().await;
    match pending.await {
        Ok(Ok(0)) => {}
        other => panic!(
            "#2499 {what}: the connection closes after the 408 like Node's \
             destroy(), got {other:?}"
        ),
    }
}

/// #2499: advance to ~10 ms shy of the expected silent-close landing (the
/// caller's `elapsed`), then read the latched EOF through the windowed probe.
/// The zero-byte follow-up head's close must carry no bytes at all; a stray
/// 408 (the `sms-headers-idle-followup-408` mutant) fails with the bytes
/// read, a missing close (an early or absent deadline) with the window
/// budget. The helper's advance crosses the landing before the read's own
/// timeout fires, so an EOF latched during the advance wins — the same
/// arithmetic as `native_http/tests.rs`'s `eof_after_idle_close`.
async fn eof_without_a_408(client: &mut tokio::io::DuplexStream, elapsed: Duration, what: &str) {
    tokio::time::advance(elapsed).await;
    yield_a_beat().await;
    let mut probe = [0u8; 64];
    let pending = tokio::time::timeout(PAUSED_PROBE_READ_WINDOW, client.read(&mut probe));
    tokio::time::advance(PAUSED_PROBE_ADVANCE).await;
    yield_a_beat().await;
    match pending.await {
        Ok(Ok(0)) => {}
        Ok(Ok(count)) => panic!(
            "#2499 {what}: {count} byte(s) arrived before the EOF — the idle \
             follow-up head must close byte-silent, not with a 408"
        ),
        Ok(Err(error)) => panic!("#2499 {what}: {error}"),
        Err(_open) => panic!(
            "#2499 {what}: the idle keep-alive connection was never closed at \
             its standby deadline's grid landing"
        ),
    }
}

/// #2499: read one full keep-alive response of this lane — the 404 a wrong
/// path gets, content-length framed, ending in the body `Not Found`. The
/// response's last body byte is the instant the standby deadline arms (the
/// [`RearmBody`] release), so its arrival proves the gate re-armed; no
/// barrier beyond the read itself is needed.
async fn read_sms_response(client: &mut tokio::io::DuplexStream, what: &str) {
    let mut buffer = Vec::new();
    while !latin1_string(&buffer).contains("\r\n\r\nNot Found") {
        let mut chunk = [0u8; 256];
        let read = client
            .read(&mut chunk)
            .await
            .unwrap_or_else(|error| panic!("#2499 {what}: {error}"));
        if read == 0 {
            panic!(
                "#2499 {what}: EOF before the 404 response arrived (buffer so \
                 far: {:?})",
                latin1_string(&buffer)
            );
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// #2499 fix-r1: read one full TwiML response of this lane — the 200 the
/// sms path answers with, content-length framed, ending in the empty
/// `TWIML_OK` body (the same marker discipline as [`read_sms_response`]).
/// One probe window per chunk, like the other paused-clock probes: the
/// answer arrives at the first poll in real usage, while a gate that
/// withholds the request body starves the handler forever — the window
/// turns that into this cell's own fast failure instead of a hang.
async fn read_sms_twiml(client: &mut tokio::io::DuplexStream, what: &str) {
    let mut buffer = Vec::new();
    while !latin1_string(&buffer).contains(TWIML_OK) {
        let mut chunk = [0u8; 256];
        let pending = tokio::time::timeout(PAUSED_PROBE_READ_WINDOW, client.read(&mut chunk));
        tokio::time::advance(PAUSED_PROBE_ADVANCE).await;
        yield_a_beat().await;
        let read = match pending.await {
            Ok(read) => read.unwrap_or_else(|error| panic!("#2499 {what}: {error}")),
            Err(_elapsed) => panic!(
                "#2499 {what}: the TwiML answer never arrived — the request \
                 body was withheld from the handler",
            ),
        };
        if read == 0 {
            panic!(
                "#2499 {what}: EOF before the TwiML response arrived (buffer so \
                 far: {:?})",
                latin1_string(&buffer)
            );
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// #2499: spawn one duplex-driven sms connection serving the half head's
/// landing cells. The grid anchor plays the listener's bind (captured before
/// the spawn, at the same virtual instant), the interval is
/// [`NODE_CONNECTIONS_CHECKING_INTERVAL`] — the production grid, exactly what
/// the paused clock is for — and the stop watch starts `false` for the cells
/// that never stop.
#[allow(clippy::type_complexity)]
fn spawn_paused(
    headers_timeout: Option<Duration>,
    grid_interval: Duration,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<()>,
    tokio::sync::watch::Sender<bool>,
) {
    let grid_anchor = tokio::time::Instant::now();
    let (server, client) = tokio::io::duplex(4096);
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let channel = SmsChannel::new(options(PORT_PAUSED_CELLS, headers_timeout));
    let shared = channel.shared.clone();
    drop(channel);
    let task = tokio::spawn(serve_sms_connection(
        server,
        shared,
        stop_rx,
        grid_anchor,
        grid_interval,
    ));
    (client, task, stop)
}

/// #1955/#2499 negative pair: a half-sent head — and a connection that never
/// sends a byte at all — is answered at the deadline's grid landing with
/// Node's 51-byte 408 + close, and the detached connection task that served
/// it does not survive the close. Binds the negative control
/// `sms-header-read-no-timeout` (no deadline at all: the read times out
/// instead of seeing EOF) and `sms-headers-408-bytes-off` (the close comes,
/// but byte-silent instead of with the 408).
#[tokio::test]
async fn half_sent_head_is_closed_at_the_deadline_and_leaves_no_task() {
    let (handle, shared) =
        start_with_grid(PORT_HALF_HEAD, Some(INJECTED), Some(INJECTED_GRID)).await;
    // After `start()` the only strong references are the channel, the handle
    // and the accept loop; every connection task adds one for its lifetime.
    let baseline = Arc::strong_count(&shared);
    let half_head = b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n";

    // (socket, when its head timer started) — the timer arms at accept, which
    // is right after `connect`, and the write lands immediately after that.
    let mut sockets = Vec::new();
    for index in 0..10 {
        let mut socket = connect(PORT_HALF_HEAD).await;
        let armed = tokio::time::Instant::now();
        if index < 8 {
            socket
                .write_all(half_head)
                .await
                .expect("#1955 write the half head");
        }
        // Keep the write side open: this is the stall that used to pin the
        // connection forever. The last two send nothing at all, so the
        // zero-byte first head is bounded by the same deadline.
        sockets.push((socket, armed));
    }

    for (mut socket, armed) in sockets {
        let (bytes, closed) = read_until_eof(&mut socket).await;
        let elapsed = armed.elapsed();
        assert!(
            closed,
            "#1955 the server retired the stalled head at the deadline; without \
             the timer this read would time out instead of seeing EOF"
        );
        // #2499: the answer is Node's own canned 408 — the exact
        // `user_dialog::NODE_408_RESPONSE` bytes (measured on the pinned
        // Node at 89,995 ms / 90,004 ms for the half-sent and the zero-byte
        // first head), not hyper's zero-byte close and not a re-typed
        // literal.
        assert_eq!(
            bytes,
            crate::user_dialog::NODE_408_RESPONSE,
            "#2499 the head deadline's answer is Node's canned 51-byte 408 + \
             Connection: close (the pre-#2499 zero-byte close is the dead \
             residual A8.3-CH-SMS-012)"
        );
        // A close that lands in the same instant as the write would mean the
        // deadline was effectively zero (e.g. an accidental `Duration::ZERO`
        // injection), not the injected 300 ms; a close well past the deadline
        // plus one grid interval means the timer never fired and something
        // else ended the read.
        assert!(
            elapsed >= INJECTED / 2,
            "#1955 the close came from the injected {INJECTED:?} deadline, took {elapsed:?}"
        );
        assert!(
            elapsed < CLOSE_UPPER,
            "#1955 the close landed inside one injected grid interval of the \
             {INJECTED:?} deadline ({CLOSE_UPPER:?}); took {elapsed:?}, so the \
             head timer did not decide it"
        );
    }

    let leaked = wait_for_task_release(&shared, baseline).await;
    assert_eq!(
        leaked, baseline,
        "#1955 every connection task released its SmsShared clone: a task that \
         never returned would still hold one"
    );

    handle.stop("test").await.expect("#1955 stop");
}

/// #2499 (§4.2 off-grid): the 408 lands on the first checker grid point
/// **after** the deadline, never at it. A +15 s deadline on a 30 s grid is
/// off-grid: nothing is answered at +14.46 s (before the deadline), nothing at
/// +29.46 s (past the deadline, before the +30 s landing), and the 51-byte
/// 408 + close arrive at +30 s. Binds the negative control
/// `sms-headers-grid-off` (deadline mode: the 408 fires at the +15 s deadline
/// itself and the +29.46 s probe reads it).
#[tokio::test(start_paused = true)]
async fn the_408_lands_on_the_first_grid_point_after_the_deadline() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the half head");
        // Let the serve task run once before the advances: the deadline is
        // armed at the connection's first poll and re-anchored at the head's
        // first byte, both at the same virtual instant here.
        yield_a_beat().await;
        // t = +14.46 s: still unanswered just before the +15 s deadline.
        tokio::time::advance(Duration::from_millis(14_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the half head stays unanswered just before its +15 s deadline",
        )
        .await;
        // t = +29.46 s: past the deadline, still nothing — the landing is the
        // grid's +30 s point, not the deadline itself.
        tokio::time::advance(Duration::from_millis(14_940)).await;
        probe_still_open(
            &mut client,
            "#2499 the half head stays unanswered between its +15 s deadline and \
             the +30 s grid landing",
        )
        .await;
        // t = +30.06 s: the landing.
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "off-grid half head").await;
        task.await
            .expect("#2499 the off-grid landing cell's serve task join");
    })
    .await;
}

/// #2499 (§4.2 on-grid): a deadline that lands exactly on a grid point fires
/// at its **successor** — the checker's comparison is strictly greater
/// (`ConnectionsList::Expired`, node_http_parser.cc), so the +30 s deadline on
/// the 30 s grid answers at +60 s, not +30 s. The probes bracket the deadline
/// instant (+30.66 s, past it) and the gap before the landing (+59.46 s):
/// nothing may answer in between. Binds the negative control
/// `sms-headers-grid-off` from the other side (deadline mode: the 408 fires
/// at the +30 s deadline and the +30.66 s probe reads it).
#[tokio::test(start_paused = true)]
async fn an_on_grid_deadline_fires_at_its_successor_grid_point() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(30)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the half head");
        yield_a_beat().await;
        // t = +30.6 s: past the deadline instant, nothing — the deadline was
        // exactly on the grid, so the 408 holds for the next point.
        tokio::time::advance(Duration::from_millis(30_600)).await;
        probe_still_open(
            &mut client,
            "#2499 the on-grid half head stays unanswered past its own +30 s \
             deadline: the strictly-greater comparison holds it for +60 s",
        )
        .await;
        // t = +59.46 s: just before the +60 s landing.
        tokio::time::advance(Duration::from_millis(28_740)).await;
        probe_still_open(
            &mut client,
            "#2499 the on-grid half head stays unanswered just before the +60 s \
             successor grid point",
        )
        .await;
        // t = +60.06 s: the landing.
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "on-grid half head").await;
        task.await
            .expect("#2499 the on-grid landing cell's serve task join");
    })
    .await;
}

/// #2499 (§4.3, the native lane's `first_head_delayed_anchors_at_its_first_
/// byte_like_ts` mirror; TS `ts-zero-and-delayed.out`): the connection's
/// *first* request head anchors its own deadline at its first byte — Node's
/// `on_message_begin` — not at the connection's accept. Connect, stay silent
/// for 30 s, then send an incomplete head with the default 60 s deadline: the
/// head's own deadline (+90 s) lands exactly on the checker's grid, so the
/// strictly-greater comparison holds it for +120 s — which is also where the
/// TS baseline answered (120.008 s). The probes bracket the connection-start
/// deadline's landing (+59.46 s, before it), the head-deadline instant the
/// connect anchor would have answered at (+90.66 s, past it) and the gap
/// before the landing (+119.46 s). Binds the negative control
/// `sms-headers-first-byte-anchor-off` (connect-anchored mode: the 408 lands
/// at the +90 s grid point and the +90.66 s probe reads it).
#[tokio::test(start_paused = true)]
async fn a_delayed_first_head_re_anchors_the_deadline_at_its_first_byte() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(None, NODE_CONNECTIONS_CHECKING_INTERVAL);
        // The connect-anchored arm must land at t0, not at the first byte:
        // let the serve task take its first poll slots before the silence,
        // else the connection-establishment anchor would coincide with the
        // +30 s first byte and the two mutants' landings would coincide too.
        yield_a_beat().await;
        // The 30 s of silence: the connection-establishment anchor arms a
        // +60 s deadline whose landing would be +90 s — the instant the
        // re-anchored head's own deadline lands on-grid past.
        tokio::time::advance(Duration::from_secs(30)).await;
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the delayed half head");
        // Let the serve task read it: the first byte re-anchors the deadline
        // at +30 s — the head's own +90 s deadline, on-grid, lands at +120 s.
        yield_a_beat().await;
        // t = +59.46 s: before the connect-anchored arm's own deadline.
        tokio::time::advance(Duration::from_millis(29_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the delayed first head stays unanswered before the \
             connect-anchored arm's +60 s deadline",
        )
        .await;
        // t = +90.66 s: past the connect-anchored arm's +90 s landing — the
        // discriminator between the two anchors — and exactly on the head's
        // own deadline, which the grid holds for +120 s.
        tokio::time::advance(Duration::from_millis(31_200)).await;
        probe_still_open(
            &mut client,
            "#2499 the delayed first head stays unanswered past the \
             connect-anchored +90 s landing: its own deadline re-anchored at \
             its first byte and lands on-grid at +120 s (measured: 120.008 s)",
        )
        .await;
        // t = +119.46 s: just before the landing.
        tokio::time::advance(Duration::from_millis(28_740)).await;
        probe_still_open(
            &mut client,
            "#2499 the delayed first head stays unanswered just before its \
             +120 s grid landing",
        )
        .await;
        // t = +120.06 s: the landing.
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "delayed first head").await;
        task.await
            .expect("#2499 the delayed-first-head cell's serve task join");
    })
    .await;
}

/// #2499 (§4.4 first half): a zero-byte **follow-up** head on a connection
/// that already served a response is Node's `keepAliveTimeout` business —
/// the idle socket leaves the checker's active list at
/// `on_message_complete`, so its 5 s close owns it. This lane does not
/// replicate that idle close (the registered A8.3-CH-SMS-017 residual), so
/// its own standby deadline does fire at +30 s here — but byte-silent: an
/// EOF with no 408 in front of it. Binds the negative control
/// `sms-headers-idle-followup-408` (the mutant writes the 408 on this path
/// too and the helper reads the bytes).
#[tokio::test(start_paused = true)]
async fn an_idle_keep_alive_connection_closes_silently_without_a_408() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        client
            .write_all(b"POST /not-the-sms-path HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("#2499 write the served request");
        // No `Connection: close`: the connection keeps living after the 404,
        // into the standby window whose deadline this cell is about.
        read_sms_response(&mut client, "idle keep-alive first response").await;
        // The response's finish re-armed the deadline at this same virtual
        // instant (t = 0): a +15 s deadline on the 30 s grid, landing at +30 s.
        yield_a_beat().await;
        // Advance to 10 ms shy of the landing, then window the EOF in.
        eof_without_a_408(
            &mut client,
            Duration::from_millis(29_990),
            "the idle keep-alive connection",
        )
        .await;
        task.await
            .expect("#2499 the idle keep-alive cell's serve task join");
    })
    .await;
}

/// #2499 (§4.4 second half): the same standby window, but the follow-up head
/// **starts** — a half head at +20 s re-anchors the deadline at its own first
/// byte (+20 s + 15 s = +35 s, landing on the +60 s grid point), so the
/// standby window's own landing (+30 s) passes without a close and the 408
/// arrives at +60 s instead. Node keeps the head in the checker's active
/// list from `on_message_begin`, which is exactly this. Binds the negative
/// control `sms-headers-first-byte-anchor-off` from the follow-up side
/// (standby-anchored mode: the +30 s landing fires — silent, since the head
/// never registered as started — and the +30.66 s probe reads the EOF).
#[tokio::test(start_paused = true)]
async fn a_follow_up_half_head_after_a_served_response_is_answered_with_the_408() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        client
            .write_all(b"POST /not-the-sms-path HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("#2499 write the served request");
        read_sms_response(&mut client, "follow-up first response").await;
        yield_a_beat().await;
        // The follow-up head starts at +20 s — inside the standby window
        // (whose own deadline is +15 s, landing at +30 s on the grid).
        tokio::time::advance(Duration::from_secs(20)).await;
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the follow-up half head");
        // Let the serve task read it: the first byte re-anchors the deadline
        // at +20 s — its own +35 s deadline lands at +60 s.
        yield_a_beat().await;
        // t = +29.46 s: before the standby landing — the would-be close if
        // the re-anchoring were missing is still ahead of this probe.
        tokio::time::advance(Duration::from_millis(9_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up half head keeps the connection open before the \
             standby window's own +30 s landing",
        )
        .await;
        // t = +30.66 s: past the standby landing — the discriminator between
        // the standby anchor and the head's own first-byte anchor.
        tokio::time::advance(Duration::from_millis(1_200)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up half head stays unanswered past the standby \
             window's +30 s landing: its deadline re-anchored at its own first \
             byte and lands at +60 s",
        )
        .await;
        // t = +59.46 s: just before the +60 s landing.
        tokio::time::advance(Duration::from_millis(28_740)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up half head stays unanswered just before its \
             +60 s grid landing",
        )
        .await;
        // t = +60.06 s: the landing — and this head received bytes, so the
        // answer is the 408, not the silent close.
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "follow-up half head").await;
        task.await
            .expect("#2499 the follow-up head cell's serve task join");
    })
    .await;
}

/// #2499 fix-r1 (r1 review concurrency#1 / boundary#1): a
/// `Transfer-Encoding: chunked` request body is the awaited request's own
/// body, not a withheld "next head" — the chunk bytes must reach the
/// handler's `frame()` reads while the request is in flight. The r1 −1
/// sentinel made `withholding_mid_stream`'s `<= 0` read the chunked body as
/// "nothing owed", so the gate swallowed every chunk into `withheld_head`
/// and parked: the handler's body read never resolved (the bytes only
/// re-feed once `in_flight` returns to 0, which that very read prevents —
/// the self-lock), and the request died at the injected `request_timeout`
/// with the body-read 408. The native lane's `i64::MAX` sentinel
/// (`http_proxy.rs`, "no mid-stream anchoring for it") keeps `<= 0` false
/// for the whole open body, so the chunks pass through to hyper. This cell
/// pins that: the head goes on the wire alone, the body follows in dribbles
/// whose 16 KB total passes hyper's first read buffer (8 KB,
/// `hyper::proto::h1::io`'s `INIT_BUFFER_SIZE`), and the handler reads all
/// of it and answers the empty TwiML 200 — never the 408 the body timeout
/// or the head deadline would leave. Binds the negative control
/// `sms-headers-chunked-body-withheld` (the mutant restores the −1
/// sentinel, the 408 lands and this cell's TwiML read fails).
#[tokio::test(start_paused = true)]
async fn a_chunked_request_body_reaches_the_handler_and_is_answered_not_408() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        // The head on its own: the service parses it, stores the chunked
        // body's "open" count, and the handler's body read is pending before
        // any chunk byte exists — the exact interleave the −1 sentinel broke.
        client
            .write_all(
                b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: \
                 chunked\r\nContent-Type: application/x-www-form-urlencoded\r\n\r\n",
            )
            .await
            .expect("#2499 write the chunked request head");
        yield_a_beat().await;
        // The body in dribbles: 8 × 2 KB segments, each its own write the gate
        // must pass through while the request is in flight, and a total past
        // hyper's first read buffer so no single read could hold the body
        // whole even if the client had sent it at once.
        let segment = "Body=".to_owned() + &"0123456789abcdef".repeat(128);
        for _ in 0..8 {
            client
                .write_all(format!("{:x}\r\n{segment}\r\n", segment.len()).as_bytes())
                .await
                .expect("#2499 write one chunked body segment");
            yield_a_beat().await;
        }
        client
            .write_all(b"0\r\n\r\n")
            .await
            .expect("#2499 write the terminating chunk");
        yield_a_beat().await;
        // Past the injected deadlines' reach at the response's virtual
        // instant: the request is done (the handler read the whole body), the
        // standby window re-armed at the response's finish, and its +15 s
        // deadline's +30 s landing is still ahead of this advance.
        tokio::time::advance(Duration::from_secs(16)).await;
        yield_a_beat().await;
        read_sms_twiml(&mut client, "chunked sms request").await;
        drop(client);
        task.await
            .expect("#2499 the chunked-body cell's serve task join");
    })
    .await;
}

/// #2499 fix-r1 (r1 review concurrency#2, interleave A): a request whose body
/// hyper read together with its head leaves no stale body count that
/// suppresses the follow-up head's first-byte re-anchor. The head and its
/// 96-byte body go on the wire in one write, so the gate's single read
/// delivers both together: the head parses (the service stores
/// `Content-Length: 96`), and the handler reads the body straight out of
/// hyper's read buffer — those 96 bytes never pass the gate's per-read
/// accounting again, so the count the *next* head's first read sees must not
/// still hold them. The pre-fix shape: `read > body_before` failed against
/// the stale 96, the follow-up head never re-anchored, and the standby
/// window's own +30 s landing closed the connection a grid point before the
/// head's own +60 s landing — Node re-anchors `last_message_start_` at
/// `on_message_begin` regardless of how the previous body reached the
/// parser. The probes bracket the standby landing (+30.66 s, the
/// discriminator), the gap (+59.46 s) and the re-anchored landing (+60.06 s).
#[tokio::test(start_paused = true)]
async fn a_follow_up_head_after_a_bodied_request_re_anchors_at_its_first_byte() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        // Head and body in one write: the gate's read returns both together,
        // and the handler's frames come out of hyper's buffer, never past the
        // gate's countdown again.
        let body = "Body=".to_owned() + &"0123456789".repeat(10);
        let request = format!(
            "POST /sms/incoming HTTP/1.1\r\nHost: x\r\nContent-Type: \
             application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("#2499 write the head and its body together");
        read_sms_twiml(&mut client, "the bodied request's response").await;
        // The response's finish re-armed the deadline at this same virtual
        // instant (t = 0): a +15 s deadline on the 30 s grid, landing at +30 s.
        yield_a_beat().await;
        // The follow-up head starts at +20 s — inside the standby window.
        tokio::time::advance(Duration::from_secs(20)).await;
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the follow-up half head");
        // Let the serve task read it: the first byte re-anchors the deadline
        // at +20 s — its own +35 s deadline lands at +60 s.
        yield_a_beat().await;
        // t = +29.46 s: before the standby landing.
        tokio::time::advance(Duration::from_millis(9_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head keeps the connection open before the \
             standby window's own +30 s landing",
        )
        .await;
        // t = +30.66 s: past the standby landing — the discriminator between
        // the standby anchor and the head's own first-byte anchor. A stale
        // body count that suppresses the re-anchor closes the connection
        // right here, one grid point before the head's own landing.
        tokio::time::advance(Duration::from_millis(1_200)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head stays unanswered past the standby \
             window's +30 s landing: its deadline re-anchored at its own first \
             byte and lands at +60 s",
        )
        .await;
        // t = +59.46 s: just before the +60 s landing.
        tokio::time::advance(Duration::from_millis(28_740)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head stays unanswered just before its +60 s \
             grid landing",
        )
        .await;
        // t = +60.06 s: the landing — and this head received bytes, so the
        // answer is the 408, not the silent close.
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "follow-up head after a bodied request").await;
        task.await
            .expect("#2499 the bodied-request follow-up cell's serve task join");
    })
    .await;
}

/// #2499 fix-r1 (r1 review concurrency#2, interleave A — the drain twin,
/// registered residual): the same stale-count shape against a request whose
/// handler never reads the body (`/not-the-sms-path` answers the 404 without
/// `into_body`), so hyper drains the 96 body bytes out of its own read
/// buffer — a path the gate cannot see or count (and post-response drain
/// reads that *do* cross the gate are indistinguishable from head bytes, so
/// zeroing the count on the 404 would mis-anchor exactly the trickled-body
/// shape the count currently gets right). The residual, registered in
/// `A8.3-CH-SMS-017` residual 3: the follow-up head's first-byte re-anchor
/// stays suppressed, so its expiry rides the standby window — closing at
/// the standby landing, one grid point before Node's own re-anchored
/// landing, never earlier and never left hanging. With no anchor recorded
/// the gate cannot tell this head received bytes, so the close is the
/// byte-silent one (residual 2's path), not a 408.
#[tokio::test(start_paused = true)]
async fn a_follow_up_head_after_an_unread_request_body_closes_on_the_standby_landing() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        let body = "z".repeat(96);
        let request = format!(
            "POST /not-the-sms-path HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("#2499 write the head and its body together");
        read_sms_response(&mut client, "the bodied 404's response").await;
        yield_a_beat().await;
        tokio::time::advance(Duration::from_secs(20)).await;
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the follow-up half head");
        yield_a_beat().await;
        tokio::time::advance(Duration::from_millis(9_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head keeps the connection open before the \
             standby window's own +30 s landing",
        )
        .await;
        // t = +30.66 s: the standby landing — the residual's boundary. Node,
        // re-anchored at the follow-up head's first byte, would hold this
        // connection until its own +60 s landing; the stale unread-body count
        // suppresses the re-anchor, so the standby window closes here, exactly
        // one grid point early and never sooner — byte-silent, because without
        // the anchor the gate cannot tell this head received bytes.
        eof_without_a_408(
            &mut client,
            Duration::from_millis(1_200),
            "follow-up head after an unread body",
        )
        .await;
        task.await
            .expect("#2499 the unread-body follow-up cell's serve task join");
    })
    .await;
}

/// #2499 fix-r3 (r3 review concurrency#1, interleave 1 — the same-read
/// twin, registered residual): the same stale-count shape with the handler
/// *reading* the body, against a follow-up half head that arrived in the
/// **same gate read** as the previous request's head and body — one
/// `write_all` carrying the head, its 96-byte body and an 18-byte
/// follow-up head start. The gate's single read delivers all of it while
/// `body_remaining` is still 0 (`consume(0)`, `head_start` from the fresh
/// read belongs to the *first* head), so the follow-up bytes land in
/// hyper's own read buffer and never cross the gate's accounting again.
/// fix-r1's read-body-finish zeroing (`route_request`'s `store(0)`) only
/// covers bytes that arrive after the store — it cannot reach back into
/// bytes hyper already banked, and the gate cannot ask hyper how much it
/// read ahead. The residual, registered in `A8.3-CH-SMS-017` residual 3
/// (fix-r3): the follow-up head's first-byte re-anchor stays suppressed,
/// so its expiry rides the standby window anchored at the previous
/// response's finish, closing at that window's own +30 s grid landing —
/// byte-silent, because without the anchor the gate cannot tell this head
/// received bytes; never a 408 on this path. The probes bracket the
/// standby landing (+29.46 s before it, +30.66 s the EOF itself).
#[tokio::test(start_paused = true)]
async fn a_follow_up_head_swallowed_in_the_same_read_as_the_previous_request_closes_on_the_standby_landing()
 {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        // Head, body and the follow-up head start in ONE write: the gate's
        // read returns all of it together, and the follow-up bytes sit in
        // hyper's read buffer from here on — the drain this cell pins.
        let body = "Body=".to_owned() + &"z".repeat(91);
        let request = format!(
            "POST /sms/incoming HTTP/1.1\r\nHost: x\r\nContent-Type: \
             application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}POST \
             /sms/incoming",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("#2499 write the head, its body and the follow-up head together");
        read_sms_twiml(&mut client, "the same-read request's response").await;
        // The response's finish re-armed the deadline at this same virtual
        // instant (t = +0.06 s): a +15 s deadline on the 30 s grid, landing
        // at +30 s.
        yield_a_beat().await;
        // t = +29.46 s: before the standby landing — the swallowed head's
        // own first-byte anchor never registered, so nothing here can fire.
        tokio::time::advance(Duration::from_millis(29_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head swallowed in the same read as the previous \
             request keeps the connection open before the standby window's +30 s \
             landing",
        )
        .await;
        // t = +30.66 s: the standby landing — the residual's boundary. Node,
        // re-anchored at the follow-up head's first byte (its parser starts
        // on the buffered bytes at `on_message_begin`), answers this head;
        // the swallowed bytes never re-crossed the gate, so the standby
        // window closes here — exactly one grid point before Node's own
        // re-anchored landing and byte-silent, because without the anchor
        // the gate cannot tell this head received bytes.
        eof_without_a_408(
            &mut client,
            Duration::from_millis(1_200),
            "follow-up head swallowed in the same read as the previous request",
        )
        .await;
        task.await
            .expect("#2499 the same-read follow-up cell's serve task join");
    })
    .await;
}

/// #2499 fix-r3 (r3 review concurrency#1, interleave 2 — the read-ahead
/// twin, registered residual): the same shape with the body itself split
/// across writes — the head and the first 40 of its 96 body bytes in one
/// write, the remaining 56 body bytes plus a 20-byte follow-up head start
/// in the second. The gate's read of the second write sees
/// `read = 76 ≤ body_before = 96`, so `head_start` is false and the 20
/// follow-up bytes bank in hyper's read buffer un-anchored — the same
/// suppression as the same-read twin, reached through a read-ahead body
/// segment instead of one whole write. The handler *does* read the body
/// (its remaining 56 bytes arrive with the follow-up head), so this is the
/// accounting-reading path fix-r1's zeroing covers for later arrivals —
/// just not for bytes hyper banked in the same read. The residual,
/// registered in `A8.3-CH-SMS-017` residual 3 (fix-r3): the standby window
/// anchored at the response's finish closes the follow-up head at its own
/// +30 s grid landing, byte-silent, never a 408. The second write stays
/// inside the 15 s injected body-read deadline (it carries the body's
/// tail), landing at +10 s. Probes: +29.46 s before the landing, +30.66 s
/// the EOF itself.
#[tokio::test(start_paused = true)]
async fn a_follow_up_head_swallowed_with_a_read_ahead_body_segment_closes_on_the_standby_landing() {
    manual_clock(async {
        let (mut client, task, _stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        // The head with 40 of the 96 body bytes: hyper parses it, the
        // service stores `Content-Length: 96`, and the handler's body read
        // parks with 40 bytes banked in hyper's buffer.
        let body = "Body=".to_owned() + &"z".repeat(91);
        let head = format!(
            "POST /sms/incoming HTTP/1.1\r\nHost: x\r\nContent-Type: \
             application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            &body[..40]
        );
        client
            .write_all(head.as_bytes())
            .await
            .expect("#2499 write the head with the first body segment");
        yield_a_beat().await;
        // The body's 56-byte tail and the follow-up head start, one write at
        // +10 s — inside the 15 s body-read deadline the tail still owes.
        tokio::time::advance(Duration::from_secs(10)).await;
        client
            .write_all(format!("{}POST /sms/incoming H", &body[40..]).as_bytes())
            .await
            .expect("#2499 write the body tail with the follow-up head");
        // The handler completes the body out of this same read and answers;
        // the response's finish re-arms the deadline at +10.06 s: a +15 s
        // deadline on the 30 s grid, landing at +30 s.
        read_sms_twiml(&mut client, "the read-ahead request's response").await;
        yield_a_beat().await;
        // t = +29.46 s: before the standby landing.
        tokio::time::advance(Duration::from_millis(19_400)).await;
        probe_still_open(
            &mut client,
            "#2499 the follow-up head swallowed with a read-ahead body segment \
             keeps the connection open before the standby window's +30 s landing",
        )
        .await;
        // t = +30.66 s: the standby landing — the residual's boundary, the
        // same close path as the same-read twin: one grid point before
        // Node's own re-anchored landing and byte-silent.
        eof_without_a_408(
            &mut client,
            Duration::from_millis(1_200),
            "follow-up head swallowed with a read-ahead body segment",
        )
        .await;
        task.await
            .expect("#2499 the read-ahead follow-up cell's serve task join");
    })
    .await;
}

/// #2499 (§4.5, fast path): a stop disarms the head deadline — Node clears
/// its connections checker in `httpServerPreClose`, so a draining connection
/// never answers its half-sent head with a 408. The stop watch fires at t = 0
/// with the +15 s deadline armed (landing +30 s); the probes at +31 s show
/// the connection still open past the landing — no 408, no EOF — and only
/// the client's own disconnect ends the task. Binds the negative control
/// `sms-headers-stop-no-disarm` (the deadline survives the stop, the 408
/// lands at +30 s and the +31 s probe reads it).
#[tokio::test(start_paused = true)]
async fn stop_disarms_the_head_deadline_for_connections_still_draining() {
    manual_clock(async {
        let (mut client, task, stop) = spawn_paused(
            Some(Duration::from_secs(15)),
            NODE_CONNECTIONS_CHECKING_INTERVAL,
        );
        client
            .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("#2499 write the half head");
        yield_a_beat().await;
        stop.send(true).expect("#2499 the stop watch is alive");
        // t = +31.06 s: past the +30 s landing the armed deadline would have
        // fired at — the stop disarmed it, so the connection is still open,
        // with nothing on the wire.
        tokio::time::advance(Duration::from_millis(31_000)).await;
        probe_still_open(
            &mut client,
            "#2499 the stop disarmed the half head's deadline: the draining \
             connection stays open past its would-be +30 s grid landing, with \
             no 408 and no EOF",
        )
        .await;
        // The client's own disconnect ends the task — the drain's business,
        // #2026 decision C's side, not the head deadline's.
        drop(client);
        task.await
            .expect("#2499 the stop-disarm cell's serve task join");
    })
    .await;
}

/// #1955 positive control: a complete request head is read and served well
/// inside the same short deadline, so the timer only retires stalls.
#[tokio::test]
async fn a_complete_request_head_is_served_before_the_deadline() {
    let (handle, _shared) = start(PORT_POSITIVE, Some(INJECTED)).await;
    let mut socket = connect(PORT_POSITIVE).await;
    socket
        .write_all(b"POST /not-the-sms-path HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .expect("#1955 write the complete head");
    let (bytes, closed) = read_until_eof(&mut socket).await;
    assert!(
        closed,
        "#1955 the served response ends with the server's close"
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.starts_with("HTTP/1.1 404"),
        "#1955 a complete head reaches the handler (404 for the wrong path), got {} bytes: {text:?}",
        bytes.len()
    );
    assert!(
        text.ends_with("Not Found"),
        "#1955 the handler's body is served, got {text:?}"
    );
    handle.stop("test").await.expect("#1955 stop");
}

/// #1955 production wiring, without the wall clock: an un-injected option still
/// means Node's `headersTimeout`, both as the constant and as the value the
/// channel actually serves with. (The 60 s close itself is the ignored case
/// below; this one keeps the wrong-constant regression cheap to catch.)
#[tokio::test]
async fn the_uninjected_head_deadline_is_nodes_sixty_seconds() {
    assert_eq!(
        DEFAULT_HEADERS_TIMEOUT,
        Duration::from_secs(60),
        "#1955 the default is Node v22.22.0's headersTimeout"
    );
    let (handle, shared) = start(PORT_DEFAULT_WIRING, None).await;
    assert_eq!(
        shared.headers_timeout,
        Duration::from_secs(60),
        "#1955 production never sets the injection, so the served deadline is the default"
    );
    handle.stop("test").await.expect("#1955 stop");
}

/// #1955 production default: the un-injected deadline answers a stalled head
/// with Node's 51-byte 408 at the first 30 s checker grid point after the 60 s
/// deadline — with the bind-anchored grid, that is +90 s from the start. The
/// window brackets the deadline (+58 s) and the landing plus the grid's phase
/// margin (+95 s). Ignored from the normal suite because it cannot finish
/// faster than a minute and a half; run explicitly with
/// `cargo test -p pilotdeck-gateway --lib -- --ignored
/// channel::sms::head_timeout_tests` to reproduce the measured landing.
#[ignore = "90 s wall-clock: the production default head deadline's grid landing"]
#[tokio::test]
async fn the_default_head_deadline_is_nodes_sixty_seconds() {
    let (handle, shared) = start(PORT_DEFAULT_DEADLINE, None).await;
    assert_eq!(
        shared.headers_timeout, DEFAULT_HEADERS_TIMEOUT,
        "#1955 an unset injection means Node's headersTimeout default"
    );
    let started = tokio::time::Instant::now();
    let mut socket = connect(PORT_DEFAULT_DEADLINE).await;
    socket
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#1955 write the half head");
    let (bytes, closed) = read_until_close(&mut socket, Duration::from_secs(100)).await;
    let elapsed = started.elapsed();
    assert!(
        closed,
        "#1955 the default deadline retired the stalled head"
    );
    assert_eq!(
        bytes,
        crate::user_dialog::NODE_408_RESPONSE,
        "#2499 the default deadline's answer is Node's canned 51-byte 408, got \
         {} bytes",
        bytes.len()
    );
    assert!(
        elapsed >= Duration::from_secs(58) && elapsed < Duration::from_secs(95),
        "#2499 the default 60 s deadline's 408 lands on the first 30 s checker \
         grid point after it (+90 s), took {elapsed:?}"
    );
    handle.stop("test").await.expect("#1955 stop");
}

/// #2026 (decision C) + #2499 (§4.5): `SmsHandle::stop()` drains the
/// connections the accept loop already handed out — the `wecom_callback`
/// lane's shape — and the head deadline is **disarmed** while it drains
/// (Node clears the connections checker in `server.close()`), so the drain
/// ends by the client's own disconnect, not by a 408 at the head deadline's
/// landing. The pre-#2026 detach shape (stop returns while the connection
/// lives) is the mutation switch `sms-stop-no-drain` and must fail this case;
/// the pre-#2499 deadline-survives-stop shape is `sms-headers-stop-no-disarm`
/// and must fail the no-408 probe below.
///
/// The client clearing stays after the drain, like the TS `server.close()`
/// callback ordering (`A8.3-CH-SMS-013-R1`): `send_reply` keeps returning
/// true while the drain is in flight and false once `stop()` returned.
#[tokio::test]
async fn stop_drains_a_half_sent_header_once_the_header_timeout_fires() {
    let (handle, shared) =
        start_with_grid(PORT_STOP_DRAIN, Some(INJECTED), Some(INJECTED_GRID)).await;
    // A present client makes the during/after-`stop()` readings real
    // observations instead of checks on a value that was never set.
    shared.runtime.lock().await.client = Some(Arc::new(NullSender) as Arc<dyn SmsSender>);
    assert!(
        super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("before")
        )
        .await,
        "#2026 the client is usable before stop(): send_reply takes the \
         SmsSender path (A8.3-CH-SMS-013)"
    );
    // After `start()` the baseline strong count is the channel-less handle
    // plus the accept loop; the connection task adds one for its lifetime.
    let baseline = Arc::strong_count(&shared);
    let mut socket = connect(PORT_STOP_DRAIN).await;
    socket
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2026 write the half head");
    // Wait for the accept loop to hand the connection to its task; without
    // this the case could stop before the connection was taken, and a stop
    // that never saw the connection cannot prove anything about the drain.
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2026 the accept loop never took the connection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let stopping = tokio::spawn({
        let handle = handle.clone();
        async move { handle.stop("test").await }
    });
    // While the drain is in flight (the injected deadline is 300 ms, so at
    // 100 ms the connection is provably still draining) the client must
    // still be usable — the clearing happens after the drain.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("during")
        )
        .await,
        "#2026 the Twilio client stays usable while stop() drains: a stop \
         that returned early cleared it (A8.3-CH-SMS-013-R1)"
    );
    // #2499: past the would-be landing (deadline 300 ms + injected grid
    // 400 ms, well behind us at 600 ms) the draining connection is still
    // open with nothing on the wire — the stop disarmed the head deadline.
    // A deadline that survived the stop (`sms-headers-stop-no-disarm`)
    // lands its 408 here and both halves of this probe read it.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (bytes, closed) = read_until_close(&mut socket, DRAIN_OBSERVE).await;
    assert!(
        !closed,
        "#2499 stop() disarmed the head deadline: the draining connection \
         stays open past its would-be grid landing — no EOF inside \
         {DRAIN_OBSERVE:?}"
    );
    assert!(
        bytes.is_empty(),
        "#2499 stop() disarmed the head deadline: no 408 bytes on the \
         draining connection, got {} bytes: {:?}",
        bytes.len(),
        String::from_utf8_lossy(&bytes)
    );
    // The drain ends by the client's own disconnect: half-close the write
    // side, the server's read gets the EOF mid-head, the task ends and the
    // drain reaps it.
    socket
        .shutdown()
        .await
        .expect("#2026 close the client half of the draining connection");
    tokio::time::timeout(READ_LIMIT, stopping)
        .await
        .expect("#2026 stop() must finish draining inside the read limit")
        .expect("#2026 the stop task must not panic")
        .expect("#2026 stop");
    assert!(
        !super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("after")
        )
        .await,
        "#2026 stop() clears the client once the drain is done, like the TS \
         close-callback ordering (A8.3-CH-SMS-013-R1)"
    );
    // The accept loop is gone, so the port refuses new connections.
    match tokio::time::timeout(
        Duration::from_secs(1),
        TcpStream::connect(("127.0.0.1", PORT_STOP_DRAIN)),
    )
    .await
    {
        Ok(Ok(_)) => panic!(
            "#2026 the stopped channel still accepts connections on \
             {PORT_STOP_DRAIN}; stop() must close the listener"
        ),
        Ok(Err(_)) => {}
        Err(_) => panic!("#2026 the connect to the stopped port hung"),
    }
    // The drained connection task must have released its SmsShared clone. The
    // finished accept loop releases its own clone too, so the settled count is
    // one below the post-`start()` baseline (which the loop was still part
    // of); a connection task that never returned would sit above it.
    let settled = baseline - 1;
    let leaked = wait_for_task_release(&shared, settled).await;
    assert_eq!(
        leaked, settled,
        "#2026 every drained connection task released its SmsShared clone"
    );
}

/// #2026 (decision C): the drain is **bounded**. Connections that outlive
/// [`STOP_DRAIN_LIMIT`] (here: an injected head deadline of 90 s, longer than
/// the 60 s bound) must not pin `stop()` — it returns at the bound, and the
/// truncation aborts the outlived connections (JoinSet drop) instead of
/// letting them run to their own deadlines. The TS shape (blocking until the
/// client leaves) is the mutation switch `sms-stop-drain-unbounded` and must
/// fail this case.
///
/// Three connections pin the truncation warn's count (r2): one retires
/// naturally during the drain (its client drops the socket, the task ends,
/// and the drain loop reaps it out of the set), two outlive the bound. The
/// warn must count exactly the two — a pre-drain snapshot counts all three
/// and misreports the retired one as aborted.
///
/// Ignored from the normal suite like the 60 s default-deadline case above:
/// the production bound cannot be injected away, so the case costs a full
/// drain-bound of wall clock.
#[ignore = "60 s wall-clock: the production drain bound"]
#[tokio::test]
async fn stop_returns_at_the_drain_bound_when_a_connection_outlives_it() {
    let (handle, shared) = start(PORT_STOP_BOUND, Some(BOUND_DEADLINE)).await;
    shared.runtime.lock().await.client = Some(Arc::new(NullSender) as Arc<dyn SmsSender>);
    // Capture the channel's warnings before `stop()`: the truncation warn's
    // count is an assertion target.
    let warns: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = warns.clone();
    shared
        .boundaries
        .lock()
        .expect("#2026 boundaries mutex")
        .logger = Some(Arc::new(move |level: &str, message: &str| {
        if level == "warn" {
            sink.lock()
                .expect("#2026 warnings mutex")
                .push(message.to_owned());
        }
    }));
    let mut retired = connect(PORT_STOP_BOUND).await;
    retired
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2026 write the half head");
    let mut outlived_a = connect(PORT_STOP_BOUND).await;
    outlived_a
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2026 write the half head");
    let mut outlived_b = connect(PORT_STOP_BOUND).await;
    outlived_b
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2026 write the half head");
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        < 3
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2026 the accept loop never took the connections"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Retire one connection naturally: its client socket goes away, the
    // server task completes on the EOF mid-head, and the drain loop reaps it
    // out of the set. Waiting for the drop makes the "two truncated" count a
    // real observation, not a timing accident.
    drop(retired);
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        > 2
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2026 the naturally retired connection never ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let started = tokio::time::Instant::now();
    tokio::time::timeout(BOUND_WAIT, handle.stop("test"))
        .await
        .expect("#2026 the drain bound must return stop() while the connection outlives it")
        .expect("#2026 stop");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= BOUND_LOWER,
        "#2026 stop() waited for the drain bound ({STOP_DRAIN_LIMIT:?}), \
         returned in only {elapsed:?}"
    );
    assert!(
        elapsed < BOUND_UPPER,
        "#2026 stop() returned at the {STOP_DRAIN_LIMIT:?} drain bound, \
         took {elapsed:?}"
    );
    // The outlived connections' own 90 s head deadline has not fired, so the
    // bound — not the deadline — is what ended the drain: the truncation
    // drops the task's JoinSet, which aborts the remaining connection tasks
    // and closes their sockets. The abort is scheduled on the runtime, so
    // the EOF can land just after `stop()` returned — bounded window, still
    // zero bytes.
    for socket in [&mut outlived_a, &mut outlived_b] {
        let (bytes, closed) = read_until_close(socket, DRAIN_TRUNCATED).await;
        assert!(
            closed,
            "#2026 the truncation aborted the outlived connection: no EOF \
             inside {DRAIN_TRUNCATED:?} after stop() returned"
        );
        assert_eq!(
            bytes,
            Vec::<u8>::new(),
            "#2026 the truncated connection closed with zero bytes; its own \
             {BOUND_DEADLINE:?} head deadline had not fired"
        );
    }
    // At the truncation the client is cleared together with the connections
    // — the bounded-ization residual of the TS ordering
    // (`A8.3-CH-SMS-014`), where `stop()` is still blocked in the close
    // callback and the client stays usable.
    assert!(
        !super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("after")
        )
        .await,
        "#2026 stop() clears the client when the bound truncates the drain"
    );
    // The truncation warn counted only the connections still in the set at
    // the bound: the naturally retired one was reaped out of it, so the two
    // outlived ones — not all three — are the aborted.
    let recorded = std::mem::take(&mut *warns.lock().expect("#2026 warnings mutex"));
    assert!(
        recorded.len() == 1 && recorded[0].contains("; 2 in-flight connection(s) aborted"),
        "#2026 the truncation warn counts only the connections still in the \
         set at the bound (the naturally retired one must not be counted), \
         saw {recorded:?}"
    );
}

/// A spawned `handle.stop()` outcome as a message: `None` when it completed,
/// otherwise the stop error or the panic payload it aborted with. Local twin
/// of `sms/tests.rs`'s `stop_outcome` — the two modules' negative-control
/// tables split their prefixes, so this module cannot reach that helper.
fn stop_payload(
    joined: Result<Result<(), ChannelError>, tokio::task::JoinError>,
) -> Option<String> {
    match joined {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("#2257 stop returned an error: {error:?}")),
        Err(join_error) => {
            let panic = join_error.into_panic();
            Some(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_else(|| "#2257 non-string panic payload".to_owned()),
            )
        }
    }
}

/// #2257: the port must rebind inside [`REBIND_LIMIT`]. The closed-channel
/// path releases the listener with the accept task's return; an unobserved
/// drain (the `sms-stop-drain-on-sender-drop` mutation) holds the port for
/// the whole [`STOP_DRAIN_LIMIT`], because the stalled connection's
/// [`BOUND_DEADLINE`] head deadline outlives the bound.
async fn assert_port_rebinds(port: u16) {
    let deadline = tokio::time::Instant::now() + REBIND_LIMIT;
    loop {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(rebound) => {
                drop(rebound);
                return;
            }
            Err(_busy) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!(
                "#2257 the port {port} did not rebind inside {REBIND_LIMIT:?} \
                 ({error}); the closed-channel path still held it — an \
                 unobserved drain"
            ),
        }
    }
}

/// #2257: one closed-channel path. A `stop()` whose async block panics at its
/// first log line (the #1339 logger-panic shape — `server.take()` already ran,
/// so the [`ServerEntry`], the watch sender and the task handle drop together
/// in the unwind) leaves the accept task with nobody holding the shutdown
/// sender, and nobody awaiting the task. The pre-#2257 shape drained on this
/// path too — unobserved — and held the listener's port for up to
/// [`STOP_DRAIN_LIMIT`], so a same-port `start` failed its bind and silently
/// degraded to a no-op handle (#2026's error-path release delay). With the fix
/// the closed channel skips the drain: the task returns, the `JoinSet`'s drop
/// aborts the in-flight connection, and the port rebinds immediately.
#[tokio::test]
async fn a_stop_task_that_panics_at_its_log_line_frees_the_port_without_a_drain() {
    let logger: ChannelLog = Arc::new(|_level: &str, message: &str| {
        if message.starts_with("sms: stopping (boom") {
            panic!("#2257 injected logger panic");
        }
    });
    let (handle, shared) =
        start_with_logger(PORT_STOP_PANIC, Some(BOUND_DEADLINE), Some(logger)).await;
    // The stalled connection: its 90 s head deadline outlives the 60 s drain
    // bound, so it can only leave through the abort of a skipped drain — or
    // pin the port for the whole bound under the mutation.
    let mut stalled = connect(PORT_STOP_PANIC).await;
    stalled
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2257 write the half head");
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        < 1
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2257 the accept loop never took the connection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let stopping = tokio::spawn({
        let handle = handle.clone();
        async move { handle.stop("boom").await }
    });
    let payload = stop_payload(stopping.await);
    crate::negative_assert!(
        payload
            .as_deref()
            .is_some_and(|message| message.contains("#2257 injected logger panic")),
        "#2257 the stop task must surface the injected logger panic: {payload:?}"
    );
    assert_port_rebinds(PORT_STOP_PANIC).await;
    let (bytes, closed) = read_until_close(&mut stalled, DRAIN_TRUNCATED).await;
    assert!(
        closed,
        "#2257 the returned task aborted the stalled connection: no EOF \
         inside {DRAIN_TRUNCATED:?}"
    );
    assert_eq!(
        bytes,
        Vec::<u8>::new(),
        "#2257 the aborted connection closed with zero bytes; its own \
         {BOUND_DEADLINE:?} head deadline had not fired"
    );
}

/// #2257: the other closed-channel path — a handle dropped without calling
/// `stop()` at all. Dropping the handle drops its [`ServerEntry`] and with it
/// the watch sender, so the accept task observes the closed channel
/// unobserved. The same port-rebind bound as the panic case: the skip
/// releases the listener with the task's return, while the unconditional
/// drain (the `sms-stop-drain-on-sender-drop` mutation) holds the port for
/// the whole [`STOP_DRAIN_LIMIT`].
#[tokio::test]
async fn a_dropped_handle_frees_the_port_without_a_drain() {
    let (handle, shared) = start(PORT_STOP_DROP, Some(BOUND_DEADLINE)).await;
    let mut stalled = connect(PORT_STOP_DROP).await;
    stalled
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2257 write the half head");
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        < 1
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2257 the accept loop never took the connection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    drop(handle);
    assert_port_rebinds(PORT_STOP_DROP).await;
    let (bytes, closed) = read_until_close(&mut stalled, DRAIN_TRUNCATED).await;
    assert!(
        closed,
        "#2257 the returned task aborted the stalled connection: no EOF \
         inside {DRAIN_TRUNCATED:?}"
    );
    assert_eq!(
        bytes,
        Vec::<u8>::new(),
        "#2257 the aborted connection closed with zero bytes; its own \
         {BOUND_DEADLINE:?} head deadline had not fired"
    );
}

/// #2026: the drain bound is a named constant at the head deadline's
/// magnitude, not an arbitrary literal buried in the call. Keeps a wrong
/// constant from being catchable only by the `#[ignore]`d 60 s case above.
#[test]
fn the_drain_bound_is_the_head_deadlines_sixty_seconds() {
    assert_eq!(
        STOP_DRAIN_LIMIT,
        Duration::from_secs(60),
        "#2026 the drain bound stays at the head deadline's magnitude \
         (A8.3-CH-SMS-014)"
    );
}

/// #2020: the [`start`] helper's `Arc::strong_count` guard has to be able to
/// fail, otherwise it is decoration. This case produces the exact shape it
/// guards against: the port is taken before `start()`, so `SmsChannel::start`
/// logs and returns the no-op handle (the bind-error path in `sms/mod.rs`)
/// instead of a listener. The no-op handle carries no `SmsShared`, so the
/// count stays at one — exactly what the helper rejects on every other case.
#[tokio::test]
async fn a_start_that_loses_the_bind_race_holds_no_listener() {
    let held = std::net::TcpListener::bind(("127.0.0.1", PORT_BIND_RACE))
        .expect("#2020 hold the port the losing start races for");
    let channel = SmsChannel::new(options(PORT_BIND_RACE, Some(INJECTED)));
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#2020 a lost bind is still a started channel");
    drop(channel);
    assert_eq!(
        Arc::strong_count(&shared),
        1,
        "#2020 the no-op handle from a lost bind owns no SmsShared; a guard \
         that cannot observe this case would let the probe-then-bind race pass"
    );
    // Production hands this no-op handle to the supervisor when the port is
    // taken, so it must still be stoppable.
    handle
        .stop("test")
        .await
        .expect("#2020 stop the no-op handle");
    drop(held);
}
