//! Minimal outbound HTTP shared by the IM channels (qyapi for the WeCom
//! callback, the Lark Open API for feishu). Production uses the same rustls
//! connector as the provider transports; tests point the caller's base URL at
//! a loopback server so the exact production path (including JSON/errcode
//! handling) runs offline.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, header};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

pub(crate) type OutboundClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Node v22.22.0 global fetch (undici) settles a hanging exchange after its
/// default ~300 s `headersTimeout` (`TypeError: fetch failed`, cause
/// `HeadersTimeoutError`; measured 301,257 ms in
/// `_tmp/20261107-issue-1343-r2/harvest/arbitration/`); a `fetch(url,
/// { signal })` with no `AbortSignal.timeout` is therefore bounded at this
/// deadline, not unbounded.
pub(crate) const UNDICI_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub(crate) struct HttpClient {
    client: OutboundClient,
}

pub(crate) struct HttpResponse {
    pub status: u16,
    pub status_text: &'static str,
    pub body: Bytes,
}

impl HttpResponse {
    pub(crate) fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl HttpClient {
    pub(crate) fn new() -> Self {
        let config = crate::runtime::model::http::webpki_client_config();
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config)),
        }
    }

    /// One request with `AbortSignal.timeout` semantics: the deadline covers
    /// connect, headers and the full body read.
    pub(crate) async fn request(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: &str,
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        self.send(method, url, content_type, None, body, Some(timeout))
            .await
    }

    /// The `Authorization: Bearer <token>` variant the Lark Open API needs
    /// (the WeCom callback carries its token in the query string instead).
    pub(crate) async fn request_bearer(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        bearer: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        self.send(method, url, content_type, Some(bearer), body, Some(timeout))
            .await
    }

    /// `fetch(url, { signal })` without `AbortSignal.timeout`: no per-call
    /// deadline, but the exchange is still bounded — Node v22.22.0 global
    /// fetch is undici under the hood, whose default `headersTimeout`
    /// settles a hanging exchange after ~300 s as a plain
    /// `TypeError: fetch failed` (cause `HeadersTimeoutError`; measured
    /// 301,257 ms in `_tmp/20261107-issue-1343-r2/harvest/arbitration/`),
    /// not `AbortSignal.timeout`'s TimeoutError. The bluebubbles poll GET
    /// re-polls every 2.5 s, so without this bound a server that accepts
    /// and stays silent accumulates one hung exchange per tick with no
    /// recovery, where the TS process settles each one.
    pub(crate) async fn request_unbounded(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> Result<HttpResponse, String> {
        match tokio::time::timeout(
            UNDICI_DEFAULT_TIMEOUT,
            self.send(method, url, content_type, None, body, None),
        )
        .await
        {
            Ok(result) => result,
            // The undici default deadline surfaces with the ordinary
            // fetch-failure text (measured), so the `poll error` log keeps
            // the TS `TypeError: fetch failed` shape.
            Err(_) => Err("TypeError: fetch failed".to_owned()),
        }
    }

    async fn send(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        bearer: Option<&str>,
        body: &str,
        timeout: Option<Duration>,
    ) -> Result<HttpResponse, String> {
        let uri: hyper::Uri = url.parse().map_err(|error| format!("TypeError: {error}"))?;
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(bearer) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        let request = builder
            .body(Full::new(Bytes::from(body.to_owned())))
            .map_err(|error| format!("TypeError: {error}"))?;
        let exchange = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|error| format!("TypeError: fetch failed: {error}"))?;
            let status = response.status();
            let status_text = status.canonical_reason().unwrap_or("");
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| format!("TypeError: fetch failed: {error}"))?
                .to_bytes();
            Ok::<_, String>(HttpResponse {
                status: status.as_u16(),
                status_text,
                body,
            })
        };
        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, exchange)
                .await
                .map_err(|_| "TimeoutError: The operation was aborted due to timeout".to_owned())?,
            None => exchange.await,
        }
    }
}

/// `encodeURIComponent` for the two query parameters the channel builds.
pub(crate) fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                *byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// #2285 r4 ⑤: the keeper's release flag as a scope guard — any exit
    /// path (including a failing assertion's unwind) releases the paused
    /// clock, so a failing verdict can never leave a keeper spinning on
    /// one core.
    struct ClockRelease(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for ClockRelease {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(
            encode_uri_component("a b&c/d?e=中"),
            "a%20b%26c%2Fd%3Fe%3D%E4%B8%AD"
        );
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }

    // The bluebubbles poll GET passes no `AbortSignal.timeout`, so its only
    // bound is undici's default 300 s `headersTimeout` (Node v22.22.0,
    // measured 301,257 ms). `request_unbounded` must keep that deadline
    // wired: a server that accepts and stays silent settles with the
    // ordinary fetch-failure text instead of hanging the exchange (and with
    // it every subsequent 2.5 s tick). Real loopback I/O does not share
    // Tokio's virtual clock, so a runnable keeper task holds the paused
    // clock back (`host/tests.rs` `ManualClock` precedent) until the
    // server's receipt proves the request bytes actually walked the
    // socket; only then may Tokio auto-advance, straight to the 300 s
    // deadline — the wall-clock cost stays in the milliseconds. The outer
    // 400 s bound only fires for the unbounded mutant and turns the hang
    // into a named assertion. The receipt wait itself races the request
    // task with a 10 s wall-clock watchdog (#2285 r4 ⑤), so a request that
    // dies before writing any bytes fails namedly instead of hanging the
    // paused-clock test forever. The elapsed pin closes the call-site blind
    // spot of the `unbounded-timeout-dropped` negative control (any
    // deadline drift inside (300 s, 400 s) settles the outcome assertion
    // but not the elapsed one), and the folded constant pin keeps the
    // value at the measured undici default.
    #[tokio::test(start_paused = true)]
    async fn request_unbounded_settles_a_silent_server_at_the_undici_default() {
        // Folded value pin (was the constant-only test in
        // channel/bluebubbles/tests.rs): the undici default, measured
        // 301,257 ms in `_tmp/20261107-issue-1343-r2/harvest/arbitration/`.
        crate::negative_assert_eq!(
            UNDICI_DEFAULT_TIMEOUT,
            Duration::from_secs(300),
            "#1343 the no-signal poll GET stays bounded at the undici default"
        );
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("#1343 silent server bind");
        let port = listener
            .local_addr()
            .expect("#1343 silent server addr")
            .port();
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel::<()>();
        let clock_held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let keeper = {
            let clock_held = clock_held.clone();
            tokio::spawn(async move {
                while clock_held.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
        };
        let _clock_release = ClockRelease(clock_held.clone());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("#1343 silent accept");
            // Signal once the request head arrived; drain the rest and
            // never answer.
            let mut sink = [0u8; 512];
            let mut receipt_tx = Some(receipt_tx);
            loop {
                match stream.read(&mut sink).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(tx) = receipt_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            }
        });
        let client = HttpClient::new();
        let started = tokio::time::Instant::now();
        let request = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(400),
                client.request_unbounded(
                    "GET",
                    &format!("http://127.0.0.1:{port}/api/v1/message"),
                    None,
                    "",
                ),
            )
            .await
        });
        // Real I/O first (the keeper holds the clock), the receipt before
        // any verdict: without it a paused-clock auto-advance could settle
        // the timeout before the request ever left the client. #2285 r4 ⑤:
        // the wait races the request task with a wall-clock watchdog — a
        // bare `receipt_rx.await` is unbounded and the test's only other
        // bound (400 s) rides the paused clock the keeper holds, so a
        // request that dies before writing any bytes would hang the test
        // forever with the keeper spinning on one core. `std::time::Instant`
        // is real wall clock, unaffected by the pause.
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut receipt = receipt_rx;
            let saw_receipt = loop {
                if let Ok(()) = receipt.try_recv() {
                    break true;
                }
                if request.is_finished() {
                    break false;
                }
                crate::negative_assert!(
                    std::time::Instant::now() < deadline,
                    "#1343 server never saw the request"
                );
                tokio::task::yield_now().await;
            };
            crate::negative_assert!(
                saw_receipt,
                "#1343 the request task ends only after the server saw the request"
            );
        }
        clock_held.store(false, std::sync::atomic::Ordering::SeqCst);
        let bounded = request.await.expect("#1343 poll GET task");
        let elapsed = started.elapsed();
        server.abort();
        keeper.abort();
        let outcome = match bounded {
            Ok(result) => result,
            Err(_) => Err("__never_settled__".to_owned()),
        };
        crate::negative_assert_eq!(
            outcome.err(),
            Some("TypeError: fetch failed".to_owned()),
            "#1343 a silent server settles the no-signal poll GET at the undici default"
        );
        // Tolerance band (#2285 comment 4/⑤): the paused clock auto-advances
        // exactly to the deadline, measured precise — the band guards
        // against a future tokio leaving a nanosecond residue, and still
        // kills any drift into the (300 s, 400 s) blind window.
        crate::negative_assert!(
            elapsed >= UNDICI_DEFAULT_TIMEOUT && elapsed < Duration::from_secs(301),
            "#1343 the settle deadline is the undici default, not a later bound"
        );
    }
}
