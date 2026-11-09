//! iLink 0.1.0 JSON endpoints, using the gateway's installed hyper/rustls stack.
use super::super::{Result, RouteError};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use hyper::header::{HeaderMap, CONTENT_LENGTH};
use serde_json::Value;
use std::time::Duration;

/// `WeixinChannel.ts:1333` (`stripContentLengthHeader`).
pub(crate) fn strip_content_length_header(headers: &mut HeaderMap) {
    headers.remove(CONTENT_LENGTH);
}

#[derive(Clone)]
pub(super) struct Http {
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
}
impl Http {
    pub fn new() -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        }
    }
    pub async fn json(
        &self,
        base: &str,
        endpoint: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value> {
        let base = origin(base)?;
        let url = base
            .join(endpoint)
            .map_err(|_| failure("Invalid iLink endpoint"))?;
        let mut request = hyper::Request::builder()
            .uri(url.as_str())
            .method(if body.is_some() { "POST" } else { "GET" })
            .header("iLink-App-ClientVersion", "1");
        if let Some(token) = token {
            use base64::Engine;
            let id = uuid::Uuid::new_v4();
            let uin = u32::from_be_bytes(id.as_bytes()[..4].try_into().unwrap());
            request = request
                .header("Authorization", format!("Bearer {token}"))
                .header("AuthorizationType", "ilink_bot_token")
                .header(
                    "X-WECHAT-UIN",
                    base64::engine::general_purpose::STANDARD.encode(uin.to_string()),
                );
        }
        let bytes = body
            .map(|mut body| {
                body["base_info"] = serde_json::json!({"channel_version":"weixin-ilink/0.1.0"});
                body.to_string()
            })
            .unwrap_or_default();
        let mut request = request
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(bytes)))
            .map_err(|_| failure("Invalid iLink request"))?;
        if endpoint.starts_with("ilink/bot/") {
            strip_content_length_header(request.headers_mut());
        }
        let work = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("iLink HTTP transport failed"))?;
            if !response.status().is_success() {
                return Err(failure(&format!(
                    "iLink HTTP status {}",
                    response.status().as_u16()
                )));
            }
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| failure("iLink response interrupted"))?;
                if let Ok(data) = frame.into_data() {
                    if bytes.len() + data.len() > 4 * 1024 * 1024 {
                        return Err(failure("iLink response too large"));
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            serde_json::from_slice(&bytes).map_err(|_| failure("Invalid iLink JSON response"))
        };
        tokio::time::timeout(Duration::from_secs(35), work)
            .await
            .map_err(|_| RouteError::new("WEIXIN_REQUEST_TIMEOUT", "iLink request timed out"))?
    }
}
pub(super) fn origin(base: &str) -> Result<url::Url> {
    let base = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
        .map_err(|_| failure("Invalid iLink base URL"))?;
    let loopback = base.host_str().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(failure(
            "iLink requires HTTPS or a loopback HTTP endpoint without URL credentials",
        ));
    }
    Ok(base)
}
pub(super) fn failure(message: &str) -> RouteError {
    RouteError::new("gateway_request_failed", message)
}
