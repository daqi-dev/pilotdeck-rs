//! #1955: the sms listener's **request-head** deadline (Node `headersTimeout`).
//!
//! The lane serves through a bare `http1::Builder`; hyper silently disables
//! its built-in header-read timeout unless a `Timer` is installed
//! (`hyper::common::time` turns `Dur::Default(Some(30 s))` into `None`), so
//! before this fix a client that wrote half a request head and stopped kept
//! the detached connection task and the socket forever — unauthenticated
//! connection exhaustion. `mod.rs` now installs the same
//! `.timer(TokioTimer::new()).header_read_timeout(60 s)` pair as the sibling
//! `wecom_callback` lane.
//!
//! Wire shape: the TS baseline (`createServer` with no options,
//! `headersTimeout = 60 s`, measured in
//! `_tmp/20261107-issue-1955/smoke/ts-half-head.out`) answers the same half
//! head with Node's 51-byte `408 Request Timeout` + `Connection: close` on the
//! 30 s connections-checker grid (measured: 90,004 ms). hyper has no public
//! hook for writing a response once it owns the read head, so Rust closes
//! without a byte. That presentation and timing difference is registered in
//! `specs/gateway-parity/diffs/A8.3-CH-SMS-012.md`; the assertions below pin
//! the bounded-close shape, not the TS bytes.
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

/// The injected deadline for the fast cases. Far above scheduler jitter (the
/// complete head is already in the socket buffer before the server polls) and
/// far below the read deadline the assertions allow.
const INJECTED: Duration = Duration::from_millis(300);
/// The upper bound for a close that the injected deadline caused (#2020).
///
/// The lower bound (`>= INJECTED / 2`) only rejects a zero-length deadline; on
/// its own it lets a close that took 30 s pass. This is the one constant the
/// other side of that bracket lives in: `2 * INJECTED`, i.e. one full extra
/// timer tick of slack for the scheduler, and still far below `READ_LIMIT` so
/// that "the close was slow" and "the client gave up" stay distinguishable.
const CLOSE_UPPER: Duration = Duration::from_millis(600);
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
async fn start(port: u16, injected: Option<Duration>) -> (Arc<dyn ChannelHandle>, Arc<SmsShared>) {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(probe) => drop(probe),
        Err(error) => panic!(
            "#1955 port {port} is already in use ({error}); another worktree is \
             running this suite — re-run when it is done"
        ),
    }
    let channel = SmsChannel::new(options(port, injected));
    let shared = channel.shared.clone();
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

/// #1955 negative/positive pair: a half-sent head — and a connection that never
/// sends a byte at all — is closed at the deadline (zero bytes, so it is the
/// head timer and not a served response), and the detached connection task that
/// served it does not survive the close.
#[tokio::test]
async fn half_sent_head_is_closed_at_the_deadline_and_leaves_no_task() {
    let (handle, shared) = start(PORT_HALF_HEAD, Some(INJECTED)).await;
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
        assert_eq!(
            bytes,
            Vec::<u8>::new(),
            "#1955 a head timeout closes without a response byte \
             (the TS 408 residual is A8.3-CH-SMS-012)"
        );
        // A close that lands in the same instant as the write would mean the
        // deadline was effectively zero (e.g. an accidental `Duration::ZERO`
        // injection), not the injected 300 ms; a close well past the deadline
        // means the timer never fired and something else ended the read.
        assert!(
            elapsed >= INJECTED / 2,
            "#1955 the close came from the injected {INJECTED:?} deadline, took {elapsed:?}"
        );
        assert!(
            elapsed < CLOSE_UPPER,
            "#1955 the close landed inside one extra timer tick of the injected \
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

/// #1955 production default: the un-injected deadline closes a stalled head at
/// Node's 60 s, not at the test's injected value. Ignored from the normal suite
/// because it cannot finish faster than a minute; run explicitly with
/// `cargo test -p pilotdeck-gateway --lib -- --ignored
/// channel::sms::head_timeout_tests` to reproduce the measured close.
#[ignore = "60 s wall-clock: the production default head deadline"]
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
    let (bytes, closed) = read_until_close(&mut socket, Duration::from_secs(90)).await;
    let elapsed = started.elapsed();
    assert!(
        closed,
        "#1955 the default deadline retired the stalled head"
    );
    assert_eq!(
        bytes,
        Vec::<u8>::new(),
        "#1955 no response bytes on a head timeout"
    );
    assert!(
        elapsed >= Duration::from_secs(58) && elapsed < Duration::from_secs(90),
        "#1955 the default close lands on the 60 s deadline, took {elapsed:?}"
    );
    handle.stop("test").await.expect("#1955 stop");
}

/// Keep a real duplicate of the listening socket alive across the production
/// stop path. This models a concurrent fork's descriptor before CLOEXEC runs:
/// dropping only the accept task's fd must not leave a connectable listener.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stop_disables_listening_even_while_a_duplicate_descriptor_is_alive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let duplicate = socket2::SockRef::from(&listener).try_clone().unwrap();
    let channel = SmsChannel::new(options(address.port(), None));
    let shared = channel.shared.clone();
    shared.runtime.lock().await.client = Some(Arc::new(NullSender) as Arc<dyn SmsSender>);
    let server = start_server(listener, shared.clone());
    let handle = SmsHandle {
        shared: shared.clone(),
        server: StdMutex::new(Some(server)),
    };

    // A complete response proves that this listener's real accept/HTTP path
    // ran before stop; Connection: close leaves no unrelated drain delay.
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /not-the-sms-path HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(READ_LIMIT, socket.read_to_end(&mut response))
        .await
        .expect("the SMS listener answers before stop")
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 404 "));

    tokio::time::timeout(READ_LIMIT, handle.stop("duplicate-listener"))
        .await
        .expect("the listener and accepted connections stop")
        .expect("the listener shutdown succeeds");
    assert!(
        TcpStream::connect(address).await.is_err(),
        "stop must disable the listening socket, not only drop one descriptor"
    );
    assert!(shared.runtime.lock().await.client.is_none());
    assert!(duplicate.local_addr().is_ok(), "the duplicate is still owned");
    drop(duplicate);
}

/// #2026 (decision C): `SmsHandle::stop()` drains the connections the accept
/// loop already handed out — the `wecom_callback` lane's
/// `stop_drains_a_half_sent_header_once_the_header_timeout_fires` shape. A
/// half-sent request head is drained to its **natural** deadline (the
/// injected head deadline, far shorter than the 60 s [`STOP_DRAIN_LIMIT`]
/// here): `stop()` must not come back before that deadline retires the
/// connection, and once it has returned the connection's EOF must already be
/// pending. The pre-#2026 detach shape (stop returns while the connection
/// lives) is the mutation switch `sms-stop-no-drain` and must fail this case.
///
/// The client clearing stays after the drain, like the TS `server.close()`
/// callback ordering (`A8.3-CH-SMS-013-R1`): `send_reply` keeps returning
/// true while the drain is in flight and false once `stop()` returned.
#[tokio::test]
async fn stop_drains_a_half_sent_header_once_the_header_timeout_fires() {
    let (handle, shared) = start(PORT_STOP_DRAIN, Some(INJECTED)).await;
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

    let started = tokio::time::Instant::now();
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
    tokio::time::timeout(READ_LIMIT, stopping)
        .await
        .expect("#2026 stop() must finish draining inside the read limit")
        .expect("#2026 the stop task must not panic")
        .expect("#2026 stop");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= INJECTED / 2,
        "#2026 stop() waited for the in-flight connection: it returned in \
         {elapsed:?}, before the injected {INJECTED:?} head deadline could \
         retire it (the drain is missing)"
    );
    assert!(
        elapsed < READ_LIMIT,
        "#2026 stop() returned inside the read limit, took {elapsed:?}"
    );
    // The drain ran the connection to its natural deadline, so by the time
    // stop() returned the EOF is already pending: zero bytes, immediate EOF.
    let (bytes, closed) = read_until_close(&mut socket, DRAIN_OBSERVE).await;
    assert!(
        closed,
        "#2026 the drained connection reached EOF before stop() returned: \
         no EOF inside {DRAIN_OBSERVE:?} means stop() came back while it \
         still lived"
    );
    assert_eq!(
        bytes,
        Vec::<u8>::new(),
        "#2026 the drain closes the connection at its own head deadline, \
         zero response bytes (the TS 408 residual is A8.3-CH-SMS-012)"
    );
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
