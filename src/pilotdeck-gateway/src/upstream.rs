//! One raw upgraded TS connection per downstream. No RFC-strict frame codec.
use crate::{ProxyError, frame::Frame};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Method, Request, StatusCode, header, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

pub(crate) async fn connect(
    origin: &str,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    accept: &str,
) -> Result<(Upgraded, HeaderMap), ProxyError> {
    let origin: hyper::Uri = origin.parse()?;
    let authority = origin.authority().expect("validated upstream").as_str();
    tokio::time::timeout(Duration::from_secs(10), async {
        let socket = TcpStream::connect(authority).await?;
        handshake(socket, authority, path, method, headers, accept).await
    })
    .await?
}

async fn handshake<S>(
    socket: S,
    authority: &str,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    accept: &str,
) -> Result<(Upgraded, HeaderMap), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            // #2558: a best-effort console line, like TS's `console.error` —
            // an `eprintln!` here panics when the write fails (std behavior);
            // tokio would contain the panic, but it would still kill this
            // connection task (the upgraded upstream socket drops with it),
            // while TS's `console.error` loses neither the line nor the task.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!("upstream HTTP upgrade: {error}"),
            );
        }
    });
    let mut upgrade = Request::builder()
        .method(method.clone())
        .uri(path)
        .body(Full::new(Bytes::new()))?;
    *upgrade.headers_mut() = headers.clone();
    upgrade
        .headers_mut()
        .insert(header::HOST, authority.parse()?);
    let mut response = sender.send_request(upgrade).await?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS
        || response
            .headers()
            .get(header::SEC_WEBSOCKET_ACCEPT)
            .is_none_or(|value| value.as_bytes() != accept.as_bytes())
    {
        return Err::<_, ProxyError>("upstream rejected websocket upgrade".into());
    }
    let headers = response.headers().clone();
    let upgraded = hyper::upgrade::on(&mut response).await?;
    Ok((upgraded, headers))
}

pub(crate) async fn forward<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> std::io::Result<()> {
    // Decoder admits only masked client frames. Retaining the original mask key
    // preserves both the wire header and the decoded payload; never re-key it.
    writer.write_all(&frame.wire).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{client_frame, server_frame};
    use crate::test_native_ws_sample::RFC6455_SAMPLE_NONCE;
    use tokio::{io::AsyncReadExt, time::timeout};

    const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "downstream.invalid"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", RFC6455_SAMPLE_NONCE),
            ("sec-websocket-version", "13"),
            ("x-caller", "unchanged"),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_static(name),
                value.parse().unwrap(),
            );
        }
        headers
    }

    async fn request(io: &mut tokio::io::DuplexStream) -> String {
        let mut bytes = Vec::new();
        loop {
            bytes.push(io.read_u8().await.unwrap());
            if bytes.ends_with(b"\r\n\r\n") {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn upgrade_preserves_query_headers_and_bytes_after_http_response() {
        timeout(Duration::from_secs(3), async {
            let (socket, mut server) = tokio::io::duplex(4096);
            let early = server_frame(1, br#"{"type":"hello_ok","exact":true}"#);
            let masked = client_frame(0xf1, 127, b"hello unchanged");
            let expected = masked.clone();
            let server_task = tokio::spawn(async move {
                let request = request(&mut server).await;
                assert!(request.starts_with("GET /ws?kept=query HTTP/1.1\r\n"));
                let lowercase = request.to_ascii_lowercase();
                assert!(lowercase.contains("host: fixture.invalid:28790\r\n"));
                assert!(lowercase.contains("x-caller: unchanged\r\n"));
                assert!(request.contains(RFC6455_SAMPLE_NONCE));
                let mut response = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {ACCEPT}\r\nX-Upstream: retained\r\n\r\n").into_bytes();
                response.extend_from_slice(&early);
                server.write_all(&response).await.unwrap();
                let mut bytes = vec![0; expected.len()];
                server.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
                early
            });
            let (upgraded, response_headers) = handshake(socket, "fixture.invalid:28790", "/ws?kept=query", &Method::GET, &headers(), ACCEPT).await.unwrap();
            assert_eq!(response_headers["x-upstream"], "retained");
            let mut io = TokioIo::new(upgraded);
            io.write_all(&masked).await.unwrap();
            let early = server_task.await.unwrap();
            let mut actual = Vec::new();
            io.read_to_end(&mut actual).await.unwrap();
            assert_eq!(actual, early, "Hyper must retain bytes read past upgrade");
        }).await.expect("upgrade test timeout");
    }

    #[tokio::test]
    async fn upstream_rejection_or_wrong_accept_fails_handshake() {
        timeout(Duration::from_secs(3), async {
            for response in [
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: wrong\r\n\r\n",
            ] {
                let (socket, mut server) = tokio::io::duplex(4096);
                let server_task = tokio::spawn(async move {
                    request(&mut server).await;
                    server.write_all(response.as_bytes()).await.unwrap();
                });
                assert!(handshake(socket, "fixture.invalid", "/ws", &Method::GET, &headers(), ACCEPT).await.is_err());
                server_task.await.unwrap();
            }
        }).await.expect("rejection test timeout");
    }

    // --------------------------------------------------------------- #2558
    // stderr

    /// The re-exec probe's env: `run:<token>`; the child also requires the
    /// token as an argv positional, so a stray env value alone can never
    /// branch a normal test run into the child path (the #2301 probe-token
    /// shape, like the #2434 ws probe).
    const UPGRADE_STDERR_PROBE_ENV: &str = "PDRS_2558_UPSTREAM_STDERR_PROBE";
    const UPGRADE_STDERR_MARKER_ENV: &str = "PDRS_2558_UPSTREAM_STDERR_MARKER";

    fn upgrade_stderr_probe_token() -> Option<String> {
        let value = std::env::var_os(UPGRADE_STDERR_PROBE_ENV)?
            .to_str()?
            .to_owned();
        let (_, token) = value.split_once(':')?;
        if token.is_empty() {
            return None;
        }
        std::env::args()
            .any(|argument| argument == token)
            .then(|| token.to_owned())
    }

    /// #2558: the spawned connection task's error line was `eprintln!`,
    /// which panics when the stderr write fails (std behavior). Tokio would
    /// contain the panic, but it would still kill this connection task (the
    /// upgraded upstream socket drops with it), while TS's `console.error`
    /// loses neither the line nor the task. The fix is the shared
    /// `console_log_line`, which drops the write error. This control drives
    /// the error the connection task actually owns — an I/O read failure
    /// on the still-live upstream connection, injected by the [`FailReads`]
    /// wrapper below — in a re-exec'd child whose stderr is a pipe with the
    /// read end already dropped (every write fails with EPIPE); the child
    /// installs a panic hook first, so a panicking `eprintln!` leaves a
    /// marker file and exits 33 instead of passing. A read-only fd is
    /// not a failing sink for this purpose: `std::io::Stderr` redirects
    /// EBADF to a fresh `/dev/null` and reports success, so only EPIPE
    /// reaches the `eprintln!` panic. A malformed response head is not the
    /// connection task's error either: the parse failure surfaces on the
    /// handshake's `send_request`, and the connection future then ends
    /// `Ok` — measured live in this tree (the first negctl round survived
    /// on that premise). The read failure must also wait until the response
    /// has settled the request — hyper's dispatch delivers a read error to
    /// the pending request when one exists, and only with no request left
    /// does the error surface on the connection future — which is why the
    /// fake TS answers a complete rejection and the wrapper arms only
    /// after those bytes were delivered.
    ///
    /// [`FailReads`]: Self::FailReads
    #[tokio::test]
    async fn connection_task_error_line_survives_a_failed_stderr() {
        if let Some(token) = upgrade_stderr_probe_token() {
            upgrade_stderr_probe_child(&token).await;
            return;
        }
        let token = format!("pdrs-2558-{}", uuid::Uuid::new_v4().simple());
        let marker = std::env::temp_dir().join(format!("{token}.marker"));
        let mut child = tokio::process::Command::new(
            std::env::current_exe().expect("#2558 the probe binary path"),
        )
        .args([
            "--exact",
            "upstream::tests::connection_task_error_line_survives_a_failed_stderr",
            "--nocapture",
            "--test-threads=1",
            &token,
        ])
        .env(UPGRADE_STDERR_PROBE_ENV, format!("run:{token}"))
        .env(UPGRADE_STDERR_MARKER_ENV, &marker)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("#2558 the failed-stderr probe child spawns");
        // The failing sink: a pipe whose read end is dropped before the
        // child writes, so every stderr write fails with EPIPE.
        drop(child.stderr.take());
        // The stdout pipe is drained concurrently with the wait, never after
        // it (the #2301 probe's drain-before-wait shape).
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
            "#2558 the connection-task error line survives a failed stderr write (exit {status})\n{console}"
        );
        // Positive receipt — success alone is not evidence the probe case
        // ran: a renamed test makes the `--exact` filter match nothing and
        // libtest still exits 0 (the #273 / fix-r1 lesson).
        crate::negative_assert!(
            crate::test_exact_receipt::single_pass(
                &status,
                "upstream::tests::connection_task_error_line_survives_a_failed_stderr",
                &console,
            ),
            "#2558 the child's libtest receipt proves the probe case ran\n{console}"
        );
        // The child leaves a reached marker once the connection task's error
        // line has provably run.
        let reached = std::env::temp_dir().join(format!("{token}.reached"));
        crate::negative_assert!(
            reached.exists(),
            "#2558 the child never reached the connection-task error line"
        );
        let _ = std::fs::remove_file(&reached);
        crate::negative_assert!(
            !marker.exists(),
            "#2558 no panic marker after the connection-task error line ({}): {:?}",
            marker.display(),
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    /// A socket wrapper for
    /// [`connection_task_error_line_survives_a_failed_stderr`]: writes pass
    /// through untouched (the upgrade head must reach the fake TS), reads
    /// pass through only until the first response bytes have been
    /// delivered — every later read fails with an I/O error. That read
    /// failure is the error the spawned connection task owns: once the
    /// fake TS's complete rejection has settled the request, hyper's
    /// dispatch has no pending request left to absorb the error, so it
    /// surfaces on the connection future (measured in this tree: with the
    /// request still pending — a malformed or unterminated head — the error
    /// is delivered to `send_request` and the connection future ends `Ok`,
    /// which is exactly how the first negctl round survived). Failing reads
    /// from the very start is not an option either: the connection task
    /// then drops the socket before the upgrade head reaches the fake TS
    /// (also measured — the fake's own head-read unwrap panicked on the
    /// resulting early EOF). The unarmed read is a real pass-through poll
    /// (the duplex registers its waker, and the fake's response write is
    /// what wakes it), so no task ever parks on a waker that cannot fire.
    struct FailReads<S> {
        socket: S,
        seen_response: bool,
    }

    impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for FailReads<S> {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            if this.seen_response {
                return std::task::Poll::Ready(Err(std::io::Error::other(
                    "#2558 the upstream socket read failed mid-response",
                )));
            }
            match std::pin::Pin::new(&mut this.socket).poll_read(cx, buf) {
                std::task::Poll::Ready(Ok(())) if !buf.filled().is_empty() => {
                    this.seen_response = true;
                    std::task::Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for FailReads<S> {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.get_mut().socket).poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().socket).poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
        }
    }

    /// The child half of
    /// [`connection_task_error_line_survives_a_failed_stderr`]: panic hook
    /// first (marker + exit 33), then a handshake against a fake TS with the
    /// socket wrapped in [`FailReads`] — the upgrade head still reaches the
    /// fake, which answers a complete rejection; once that settles the
    /// request, the wrapper's read failure can no longer be absorbed by a
    /// pending request, so the spawned connection task hits its
    /// `if let Err(error)` arm and its error line runs; the fake side's EOF
    /// (the task dropping the socket when it ends) is the receipt that the
    /// line already ran. Nothing here prints to stdout: the single-threaded
    /// receipt needs the whole test line intact.
    async fn upgrade_stderr_probe_child(token: &str) {
        let marker = std::env::var(UPGRADE_STDERR_MARKER_ENV)
            .unwrap_or_else(|_| panic!("#2558 {token} is missing the marker env"));
        std::panic::set_hook(Box::new(move |info| {
            // The default hook would print to the failed stderr; the marker
            // is the parent's evidence instead.
            let _ = std::fs::write(&marker, format!("{info}"));
            std::process::exit(33);
        }));
        let (socket, mut server) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            let proxied = request(&mut server).await;
            assert!(
                proxied.starts_with("GET /ws HTTP/1.1"),
                "#2558 the proxy forwarded the upgrade head"
            );
            // A complete non-upgrade response: the request future settles
            // (and releases its callback) while the connection stays alive,
            // so the wrapper's next read failure has no request to absorb it
            // and must surface on the connection future the error line owns.
            server
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("#2558 the fake TS sends its complete rejection");
            let mut tail = Vec::new();
            server
                .read_to_end(&mut tail)
                .await
                .expect("#2558 the fake TS reads the connection task's EOF");
        });
        let result = timeout(
            Duration::from_secs(10),
            handshake(
                FailReads {
                    socket,
                    seen_response: false,
                },
                "fixture.invalid:28790",
                "/ws",
                &Method::GET,
                &headers(),
                ACCEPT,
            ),
        )
        .await
        .expect("#2558 the failing-read handshake is answered in time");
        assert!(
            result.is_err(),
            "#2558 the upstream rejection fails the handshake"
        );
        // EOF on the fake side is the receipt that the connection task
        // ended — and its error line ran before the socket dropped.
        timeout(Duration::from_secs(10), fake)
            .await
            .expect("#2558 the upstream connection task ends")
            .expect("#2558 the fake TS task succeeds");
        let _ = std::fs::write(
            std::env::temp_dir().join(format!("{token}.reached")),
            b"reached",
        );
    }
}
