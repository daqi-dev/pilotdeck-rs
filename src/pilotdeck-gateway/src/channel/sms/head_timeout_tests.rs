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

use std::sync::Arc;
use std::time::Duration;

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::super::protocol::ChannelStartDeps;
use super::*;

/// Ports not used by the gold-master cases (18800–18855, 18929, 18941) or the
/// WeCom lane (18993–18998).
const PORT_HALF_HEAD: u16 = 18971;
const PORT_POSITIVE: u16 = 18972;
const PORT_DEFAULT_WIRING: u16 = 18973;
const PORT_DEFAULT_DEADLINE: u16 = 18974;
const PORT_STOP_DETACH: u16 = 18975;
const PORT_BIND_RACE: u16 = 18976;

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
/// The head deadline injected by the `stop()` detach case (#2035). Far above
/// the 1 s bound on `stop()`, so a `stop()` that waited for the connection
/// could not finish inside it, and far above the half-head read that proves
/// the connection is still open afterwards.
const DETACH_DEADLINE: Duration = Duration::from_secs(30);
/// The bound on `stop()` returning: the TS `server.close()` blocks on the
/// drain (#2026 residual 4), the Rust handle must not.
const STOP_RETURN_LIMIT: Duration = Duration::from_secs(1);
/// How long the detached connection must stay open after `stop()` returned.
const DETACH_OBSERVE: Duration = Duration::from_millis(500);

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

/// #2035: `SmsHandle::stop()` stops the **accept loop** and returns; the
/// connections it already accepted keep running as detached `tokio::spawn`
/// tasks. That is the registered shape: `specs/gateway-parity/diffs/
/// A8.3-CH-SMS-012.md` residual 4 records that the TS `server.close()` waits
/// for the drain while Rust does not, and decision card #2026 keeps that
/// difference as the current semantics. This case pins the detach instead of
/// leaving it implicit — `stop()` must come back while the connection still
/// lives, and the client must already be cleared at that moment (read through
/// `send_reply`, which returns false once `client` is `None`; the
/// client-clearing order is registered in `A8.3-CH-SMS-013.md`).
///
/// The other half is the mutation switch `sms-stop-waits-for-connections`: it
/// makes `stop()` drain the detached connections, and this case must then miss
/// the [`STOP_RETURN_LIMIT`] bound.
#[tokio::test]
async fn stop_returns_before_the_detached_connection_ends() {
    let (handle, shared) = start(PORT_STOP_DETACH, Some(DETACH_DEADLINE)).await;
    // A present client makes the post-`stop()` reading a real observation
    // instead of a check on a value that was never set.
    shared.runtime.lock().await.client = Some(Arc::new(NullSender) as Arc<dyn SmsSender>);
    assert!(
        super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("before")
        )
        .await,
        "#2035 the client is usable before stop(): send_reply takes the \
         SmsSender path (A8.3-CH-SMS-013)"
    );
    let mut socket = connect(PORT_STOP_DETACH).await;
    socket
        .write_all(b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("#2035 write the half head");
    // Wait for the accept loop to hand the connection to its detached task;
    // without this the case could stop before the connection was even taken.
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while shared
        .live_connections
        .load(std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "#2035 the accept loop never took the connection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let started = tokio::time::Instant::now();
    tokio::time::timeout(STOP_RETURN_LIMIT, handle.stop("test"))
        .await
        .expect("#2035 stop() must return while the detached connection still runs")
        .expect("#2035 stop");
    let elapsed = started.elapsed();

    assert!(
        shared
            .live_connections
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1,
        "#2035 the detached connection task is still running after stop() \
         returned: a stop that waited for it would have left the count at zero"
    );
    assert!(
        !super::send_reply(
            &shared,
            &JsString::from("+15550001"),
            JsString::from("after")
        )
        .await,
        "#2035 stop() clears the client while the detached connection still \
         runs: TS can still send during its drain, Rust cannot \
         (A8.3-CH-SMS-013 registers the order)"
    );
    assert!(
        elapsed < STOP_RETURN_LIMIT,
        "#2035 stop() returned inside the bound, took {elapsed:?}"
    );

    // The accept loop is gone, so the port refuses new connections even
    // though the accepted one is still served.
    match tokio::time::timeout(
        Duration::from_secs(1),
        TcpStream::connect(("127.0.0.1", PORT_STOP_DETACH)),
    )
    .await
    {
        Ok(Ok(_)) => panic!(
            "#2035 the stopped channel still accepts connections on \
             {PORT_STOP_DETACH}; stop() must close the listener"
        ),
        Ok(Err(_)) => {}
        Err(_) => panic!("#2035 the connect to the stopped port hung"),
    }

    // The detached connection stays open and silent: no EOF, no byte. With the
    // injected 30 s deadline, anything that ends it inside 500 ms came from
    // stop(), not from the head timer.
    let mut buffer = [0u8; 64];
    match tokio::time::timeout(DETACH_OBSERVE, socket.read(&mut buffer)).await {
        Err(_) => {}
        Ok(Ok(0)) => panic!(
            "#2035 stop() closed the detached connection; the registered \
             semantics leave it running"
        ),
        Ok(Ok(count)) => panic!(
            "#2035 the detached connection produced {count} bytes after \
             stop(); the head deadline is {DETACH_DEADLINE:?}"
        ),
        Ok(Err(error)) => panic!("#2035 the detached connection failed after stop(): {error}"),
    }
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
