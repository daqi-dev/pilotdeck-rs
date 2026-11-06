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

use std::sync::Arc;
use std::time::Duration;

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::super::protocol::ChannelStartDeps;
use super::*;

/// Ports not used by the gold-master cases (18800–18854, 18901) or the
/// WeCom lane (18994–18998).
const PORT_HALF_HEAD: u16 = 18971;
const PORT_POSITIVE: u16 = 18972;
const PORT_DEFAULT_DEADLINE: u16 = 18973;

/// The injected deadline for the fast cases. Far above scheduler jitter (the
/// complete head is already in the socket buffer before the server polls) and
/// far below the read deadline the assertions allow.
const INJECTED: Duration = Duration::from_millis(300);
/// How long a client read may take before the test calls it unbounded.
const READ_LIMIT: Duration = Duration::from_secs(5);

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
async fn start(port: u16, injected: Option<Duration>) -> (Arc<dyn ChannelHandle>, Arc<SmsShared>) {
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
    (handle, shared)
}

async fn connect(port: u16) -> TcpStream {
    let mut last = None;
    for _ in 0..100 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => return stream,
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    panic!("#1955 cannot connect to the sms listener: {last:?}");
}

/// Read until the server closes (EOF) or `limit`, returning the bytes seen
/// plus whether the **server** closed. The flag matters: a stalled connection
/// that the server never retires also yields zero bytes, and once the client
/// gives up and drops its socket the connection task ends anyway — so a
/// bytes-only or `Arc`-count-only assertion passes with the deadline removed.
/// Only `closed == true` inside the limit proves the head timer fired. (The
/// mutation switch `sms-header-read-no-timeout` is what pins this: without it
/// the read times out instead of seeing EOF.)
async fn read_until_close(socket: &mut TcpStream, limit: Duration) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 512];
    let read = async {
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) => break true,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
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

/// #1955 negative/positive pair: a half-sent head is closed at the deadline
/// (zero bytes, so it is the head timer and not a served response), and the
/// detached connection task that served it does not survive the close.
#[tokio::test]
async fn half_sent_head_is_closed_at_the_deadline_and_leaves_no_task() {
    let (handle, shared) = start(PORT_HALF_HEAD, Some(INJECTED)).await;
    // After `start()` the only strong references are the channel, the handle
    // and the accept loop; every connection task adds one for its lifetime.
    let baseline = Arc::strong_count(&shared);
    let half_head = b"POST /sms/incoming HTTP/1.1\r\nHost: x\r\n";

    let mut sockets = Vec::new();
    for _ in 0..8 {
        let mut socket = connect(PORT_HALF_HEAD).await;
        socket
            .write_all(half_head)
            .await
            .expect("#1955 write the half head");
        // Keep the write side open: this is the stall that used to pin the
        // connection forever.
        sockets.push(socket);
    }

    for mut socket in sockets {
        let (bytes, closed) = read_until_eof(&mut socket).await;
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

/// #1955 production default: the un-injected deadline is Node's 60 s
/// `headersTimeout`, not the test's injected value. Ignored from the normal
/// suite because it cannot finish faster than a minute; run explicitly with
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
