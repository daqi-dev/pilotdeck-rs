//! Async count transport. TLS is supplied by the provider transport in A6.4-pr5.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, body::Body};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::future::Future;
use url::Url;

use crate::host::{HostError, HostFuture, ModuleError};

#[cfg(test)]
#[path = "count_http_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub struct CountHttpRequest {
    pub url: String,
    pub headers: JsObject,
    pub body: JsValue,
}

/// A transport owns no detached request work: dropping `post` cancels the call.
/// Runtime wraps both headers and response body in its timeout/abort boundary.
pub trait ProviderCountTransport: Send + Sync {
    fn post(&self, request: CountHttpRequest) -> HostFuture<JsValue>;
}

#[derive(Clone)]
pub struct HyperCountTransport {
    client: Client<HttpConnector, Full<Bytes>>,
}
impl Default for HyperCountTransport {
    fn default() -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build_http(),
        }
    }
}
impl ProviderCountTransport for HyperCountTransport {
    fn post(&self, request: CountHttpRequest) -> HostFuture<JsValue> {
        let client = self.client.clone();
        Box::pin(fetch_count(request, move |request| {
            let client = client.clone();
            async move { client.request(request).await.map_err(|_| failed()) }
        }))
    }
}

fn failed() -> HostError {
    ModuleError::new("fetch failed").into()
}

// Keep the redirect policy shared by the socket transport and scripted wire
// tests. Only the single HTTP exchange is injected; follow/JSON stay real.
async fn fetch_count<B, F, Fut>(
    request: CountHttpRequest,
    mut send: F,
) -> Result<JsValue, HostError>
where
    B: Body<Data = Bytes>,
    F: FnMut(Request<Full<Bytes>>) -> Fut,
    Fut: Future<Output = Result<Response<B>, HostError>>,
{
    let mut url = Url::parse(&request.url).map_err(|_| failed())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(failed());
    }
    let mut headers = HeaderMap::new();
    for (name, value) in request.headers.entries() {
        let name = name
            .to_utf8()
            .map_err(|_| ModuleError::new("fetch failed"))?;
        let value = value
            .as_string()
            .and_then(|v| v.to_utf8().ok())
            .ok_or_else(|| ModuleError::new("fetch failed"))?;
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| failed())?,
            hyper::header::HeaderValue::from_str(&value).map_err(|_| failed())?,
        );
    }
    // Undici discards user Host on every hop; the HTTP client supplies the
    // authority of the current URL, including a redirected port.
    headers.remove(hyper::header::HOST);
    let body = super::cpu::run(move || {
        stringify_json(&request.body)
            .map(|body| body.unwrap_or_default())
            .map_err(|e| ModuleError::new(e.to_string()).into())
    })
    .await?;
    let mut body = Bytes::from(body);
    let mut method = Method::POST;
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
        if matches!(status, 301 | 302 | 303 | 307 | 308)
            && response.headers().contains_key(hyper::header::LOCATION)
        {
            // Node normalizes non-ASCII Location bytes as UTF-8 before
            // WHATWG URL resolution (including relative/query locations).
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
            // Do not collect a redirect body: a slow/never-ending body
            // must not prevent following its already received Location.
            drop(response);
            continue;
        }
        if !response.status().is_success() {
            return Err(ModuleError::new(format!(
                "Provider token count failed with HTTP {}.",
                response.status().as_u16()
            ))
            .into());
        }
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| ModuleError::new("fetch failed"))?
            .to_bytes();
        let value = super::cpu::run(move || super::count_json::parse_response(&bytes)).await?;
        return Ok(value);
    }
}

fn blocked_port(port: u16) -> bool {
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
