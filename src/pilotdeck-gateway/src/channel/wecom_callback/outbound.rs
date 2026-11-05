//! Minimal outbound HTTP for qyapi. Production uses the same rustls connector
//! as the provider transports; tests point `base_url` at a loopback server so
//! the exact production path (including JSON/errcode handling) runs offline.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, header};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

pub(crate) type OutboundClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

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
        let uri: hyper::Uri = url.parse().map_err(|error| format!("TypeError: {error}"))?;
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
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
        tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| "TimeoutError: The operation was aborted due to timeout".to_owned())?
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

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(
            encode_uri_component("a b&c/d?e=中"),
            "a%20b%26c%2Fd%3Fe%3D%E4%B8%AD"
        );
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }
}
