//! Undici `fetch` redirect policy (`redirect: "follow"`), shared by every
//! Rust port of a TS call site that uses the default `fetch`: the provider
//! count transport (calibrated by `tests/fixtures/token-accounting`) and the
//! HTTP hook executor (`tests/fixtures/hooks-runtime`).
//!
//! Only the single HTTP exchange is injected (`send`): a per-dispatch timer
//! (undici `headersTimeout`) belongs inside `send`, an abort signal around the
//! returned future.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, body::Body};
use std::{future::Future, time::Duration};
use url::Url;

/// `buffer.constants.MAX_STRING_LENGTH` on Node 22.22 (V8 12.x): `text()` /
/// `json()` reject a body with more bytes than this.
pub(crate) const MAX_STRING_LENGTH: usize = 0x1fffffe8;
/// V8 / Node `ERR_STRING_TOO_LONG` message for [`MAX_STRING_LENGTH`].
pub(crate) const STRING_TOO_LONG: &str = "Cannot create a string longer than 0x1fffffe8 characters";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// More than the limit's bytes arrived.
    TooLong,
    /// A frame failed, or no data arrived within the idle timeout.
    Terminated,
}

/// Reads a response body like `response.text()` before decoding: bytes are
/// counted frame by frame and the read stops as soon as `limit` would be
/// exceeded, so a never-ending body cannot grow memory without bound.
/// `idle` is undici's `bodyTimeout`: the longest gap between two body frames
/// (including the wait for the first), not a deadline for the whole body.
pub(crate) async fn read_body<B: Body<Data = Bytes>>(
    body: B,
    limit: usize,
    idle: Option<Duration>,
) -> Result<Vec<u8>, BodyError> {
    let mut body = std::pin::pin!(body);
    let mut buffer = Vec::new();
    loop {
        let frame = match idle {
            Some(idle) => tokio::time::timeout(idle, body.frame())
                .await
                .map_err(|_| BodyError::Terminated)?,
            None => body.frame().await,
        };
        let Some(frame) = frame else {
            return Ok(buffer);
        };
        let frame = frame.map_err(|_| BodyError::Terminated)?;
        if let Some(data) = frame.data_ref() {
            if buffer.len().saturating_add(data.len()) > limit {
                return Err(BodyError::TooLong);
            }
            buffer.extend_from_slice(data);
        }
    }
}

/// Follow 301/302/303/307/308 like undici and return the first non-redirect
/// response (its body unread). Every network-level refusal maps to `failed()`
/// (TS `TypeError: fetch failed`).
pub(crate) async fn follow<B, E, F, Fut>(
    mut url: Url,
    mut method: Method,
    mut headers: HeaderMap,
    mut body: Bytes,
    failed: impl Fn() -> E,
    mut send: F,
) -> Result<Response<B>, E>
where
    F: FnMut(Request<Full<Bytes>>) -> Fut,
    Fut: Future<Output = Result<Response<B>, E>>,
{
    // Undici discards user Host on every hop; the HTTP client supplies the
    // authority of the current URL, including a redirected port.
    headers.remove(hyper::header::HOST);
    let mut redirects = 0;
    loop {
        // Fetch applies port blocking again after each redirect. Port 0 is
        // intentionally not in Node 22's list.
        if !matches!(url.scheme(), "http" | "https") || url.port().is_some_and(blocked_port) {
            return Err(failed());
        }
        // Fragments never travel on the wire.
        url.set_fragment(None);
        let mut request = Request::builder()
            .method(method.clone())
            .uri(url.as_str())
            .body(Full::new(body.clone()))
            .map_err(|_| failed())?;
        *request.headers_mut() = headers.clone();
        let response = send(request).await?;
        let status = response.status().as_u16();
        if !(matches!(status, 301 | 302 | 303 | 307 | 308)
            && response.headers().contains_key(hyper::header::LOCATION))
        {
            return Ok(response);
        }
        // Node normalizes non-ASCII Location bytes as UTF-8 before WHATWG URL
        // resolution (including relative/query locations).
        let location = response
            .headers()
            .get_all(hyper::header::LOCATION)
            .iter()
            .map(|value| String::from_utf8_lossy(value.as_bytes()))
            .collect::<Vec<_>>()
            .join(", ");
        let next = url.join(&location).map_err(|_| failed())?;
        if redirects == 20
            || !matches!(next.scheme(), "http" | "https")
            || !next.username().is_empty()
            || next.password().is_some()
        {
            return Err(failed());
        }
        if (matches!(status, 301 | 302) && method == Method::POST)
            || (status == 303 && method != Method::GET && method != Method::HEAD)
        {
            method = Method::GET;
            body = Bytes::new();
            for name in [
                "content-encoding",
                "content-language",
                "content-location",
                "content-type",
                "content-length",
            ] {
                headers.remove(name);
            }
        }
        if url.origin() != next.origin() {
            for name in ["authorization", "proxy-authorization", "cookie", "host"] {
                headers.remove(name);
            }
        }
        redirects += 1;
        url = next;
        // Do not collect a redirect body: a slow/never-ending body must not
        // prevent following its already received Location.
        drop(response);
    }
}

/// Fetch bad-port table (Node 22); shared by every fetch-shaped client.
pub(crate) fn blocked_port(port: u16) -> bool {
    // Node 22 / Fetch's bad-port table. Check before any network exchange.
    matches!(
        port,
        1 | 7
            | 9
            | 11
            | 13
            | 15
            | 17
            | 19
            | 20
            | 21
            | 22
            | 23
            | 25
            | 37
            | 42
            | 43
            | 53
            | 69
            | 77
            | 79
            | 87
            | 95
            | 101
            | 102
            | 103
            | 104
            | 109
            | 110
            | 111
            | 113
            | 115
            | 117
            | 119
            | 123
            | 135
            | 137
            | 139
            | 143
            | 161
            | 179
            | 389
            | 427
            | 465
            | 512
            | 513
            | 514
            | 515
            | 526
            | 530
            | 531
            | 532
            | 540
            | 548
            | 554
            | 556
            | 563
            | 587
            | 601
            | 636
            | 989
            | 990
            | 993
            | 995
            | 1719
            | 1720
            | 1723
            | 2049
            | 3659
            | 4045
            | 4190
            | 5060
            | 5061
            | 6000
            | 6566
            | 6665
            | 6666
            | 6667
            | 6668
            | 6669
            | 6679
            | 6697
            | 10080
    )
}
