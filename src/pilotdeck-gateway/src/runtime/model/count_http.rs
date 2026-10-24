//! Async count transport. HTTPS uses the provider transport's rustls connector
//! through the shared proxy-aware `super::http::proxy_https_connector`
//! (A6.4-pr5), reading the outgoing proxy from the environment exactly like
//! the model and hook transports.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Method, Request, Response, body::Body};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::future::Future;
use url::Url;

use crate::host::{HostError, ModuleError};

#[cfg(test)]
#[path = "count_http_tests.rs"]
mod tests;

/// `buffer.constants.MAX_STRING_LENGTH` on Node 22.22 (V8 12.x).
pub const MAX_STRING_LENGTH: usize = crate::fetch_redirect::MAX_STRING_LENGTH;

#[derive(Clone, Debug)]
pub struct CountHttpRequest {
    pub url: String,
    pub headers: JsObject,
    pub body: JsValue,
}

pub type CountFuture = crate::runtime::network::ExchangeFuture<Result<JsValue, HostError>>;

/// A transport owns no detached request work: dropping `post` cancels the call.
/// Runtime wraps both headers and response body in its timeout/abort boundary.
/// Preserve promise/timer completion sources with `CountFuture::ready`/`after`;
/// socket transports use `io`.
pub trait ProviderCountTransport: Send + Sync {
    fn post(&self, request: CountHttpRequest) -> CountFuture;
}

#[derive(Clone)]
pub struct HyperCountTransport {
    client: Client<hyper_rustls::HttpsConnector<super::http::ProxyConnect>, Full<Bytes>>,
}
impl HyperCountTransport {
    /// Trust exactly `roots` (tests use a throwaway CA).
    pub fn with_roots(roots: rustls::RootCertStore) -> Self {
        Self::from_config(super::http::rooted_client_config(roots))
    }
    fn from_config(config: rustls::ClientConfig) -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build(
                super::http::proxy_https_connector(
                    config,
                    super::http::ProxyConfig::from_settings(None),
                ),
            ),
        }
    }
}
/// Mozilla roots, like the stream transport.
impl Default for HyperCountTransport {
    fn default() -> Self {
        Self::from_config(super::http::webpki_client_config())
    }
}
impl ProviderCountTransport for HyperCountTransport {
    fn post(&self, request: CountHttpRequest) -> CountFuture {
        let client = self.client.clone();
        CountFuture::io(fetch_count(request, move |request| {
            let client = client.clone();
            async move { client.request(request).await.map_err(|_| failed()) }
        }))
    }
}

fn failed() -> HostError {
    ModuleError::new("fetch failed").into()
}

// The socket transport and scripted wire tests share this path. Only the
// single HTTP exchange is injected; redirect follow (`crate::fetch_redirect`)
// and JSON stay real.
async fn fetch_count<B, F, Fut>(request: CountHttpRequest, send: F) -> Result<JsValue, HostError>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
    F: FnMut(Request<Full<Bytes>>) -> Fut,
    Fut: Future<Output = Result<Response<B>, HostError>>,
{
    let url = Url::parse(&request.url)
        .map_err(|_| ModuleError::new(format!("Failed to parse URL from {}", request.url)))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ModuleError::new(format!(
            "Request cannot be constructed from a URL that includes credentials: {}",
            request.url
        ))
        .into());
    }
    let mut headers = HeaderMap::new();
    for (name, value) in request.headers.entries() {
        let name = name
            .to_utf8()
            .map_err(|_| ModuleError::new("fetch failed"))?;
        let value = crate::js_value::to_string_units(value).map_err(|_| failed())?;
        let value = String::from_utf16(&value).map_err(|_| failed())?;
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| failed())?,
            hyper::header::HeaderValue::from_str(&value).map_err(|_| failed())?,
        );
    }
    let body = super::cpu::run(move || {
        stringify_json(&request.body)
            .map(|body| body.unwrap_or_default())
            .map_err(|e| ModuleError::new(e.to_string()).into())
    })
    .await?;
    let response =
        crate::fetch_redirect::follow(url, Method::POST, headers, Bytes::from(body), failed, send)
            .await?;
    if !response.status().is_success() {
        return Err(ModuleError::new(format!(
            "Provider token count failed with HTTP {}.",
            response.status().as_u16()
        ))
        .into());
    }
    // Node 22 cannot materialize a string longer than
    // `buffer.constants.MAX_STRING_LENGTH` (0x1fffffe8). Bound the read so
    // a broken/malicious endpoint cannot stream unbounded bytes inside the
    // 1500ms window; the message matches V8's RangeError.
    let buffer = crate::fetch_redirect::read_body(
        response.into_body(),
        MAX_STRING_LENGTH,
        Some(crate::runtime::network::UNDICI_BODY_TIMEOUT),
    )
    .await
    .map_err(|error| match error {
        crate::fetch_redirect::BodyError::TooLong => {
            ModuleError::new(crate::fetch_redirect::STRING_TOO_LONG)
        }
        crate::fetch_redirect::BodyError::Terminated
        | crate::fetch_redirect::BodyError::Aborted => ModuleError::new("terminated"),
    })?;
    let bytes = Bytes::from(buffer);
    super::cpu::run(move || super::count_json::parse_response(&bytes)).await
}
