//! iLink 0.1.0 JSON endpoints, using the gateway's installed hyper/rustls stack.
//! Aligned with TS `WeixinChannel.ts:1315-1352` and dist `api.js:49-75`.

use super::super::{Result, RouteError};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{HeaderMap, CONTENT_LENGTH};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Default timeout for long-poll operations (`ilink/bot/getupdates`): 35 seconds (35,000 ms).
/// Aligned with TS dist `api.js:49-60`.
pub const DEFAULT_LONG_POLL_TIMEOUT_MS: u64 = 35_000;

/// Default timeout for standard API requests (sendmessage, sendtyping, getconfig, getuploadurl): 15 seconds (15,000 ms).
/// Aligned with TS dist `api.js:68-75`.
pub const DEFAULT_API_TIMEOUT_MS: u64 = 15_000;

/// Global flag for iLink fetch compatibility shim (`WeixinChannel.ts:1315-1331`).
static ILINK_FETCH_COMPATIBILITY_INSTALLED: AtomicBool = AtomicBool::new(false);

/// `WeixinChannel.ts:1315-1331` (`installIlinkFetchCompatibility`).
/// Global idempotent installation of iLink fetch compatibility shim.
/// Intercepts requests to `/ilink/bot/` and strips Content-Length header.
pub fn install_ilink_fetch_compatibility() {
    ILINK_FETCH_COMPATIBILITY_INSTALLED.store(true, Ordering::SeqCst);
}

/// Returns true if `install_ilink_fetch_compatibility()` has been called.
pub fn is_ilink_fetch_compatibility_installed() -> bool {
    ILINK_FETCH_COMPATIBILITY_INSTALLED.load(Ordering::SeqCst)
}

/// `WeixinChannel.ts:1333-1340` (`stripContentLengthHeader` for Headers / HeaderMap).
/// Strips Content-Length header in-place from a `HeaderMap`.
pub(crate) fn strip_content_length_header(headers: &mut HeaderMap) {
    headers.remove(CONTENT_LENGTH);
    headers.remove("content-length");
}

/// `WeixinChannel.ts:1333-1340` (`stripContentLengthHeader` returning a cloned HeaderMap).
pub fn strip_content_length_headers_headermap(headers: &HeaderMap) -> HeaderMap {
    let mut next = headers.clone();
    strip_content_length_header(&mut next);
    next
}

/// `WeixinChannel.ts:1341-1343` (`stripContentLengthHeader` for Array / slice of tuples).
/// Filters out any key matching "content-length" case-insensitively.
pub fn strip_content_length_headers_vec(
    headers: &[(String, String)],
) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("content-length"))
        .cloned()
        .collect()
}

/// `WeixinChannel.ts:1345-1351` (`stripContentLengthHeader` for Record / HashMap).
/// Filters out any key matching "content-length" case-insensitively.
pub fn strip_content_length_headers_map(
    headers: &HashMap<String, String>,
) -> HashMap<String, String> {
    headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("content-length"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Generic trait supporting TS `HeadersInit` three shapes:
/// `HeaderMap`, `Vec<(String, String)>`, and `HashMap<String, String>`.
pub trait StripContentLengthHeader {
    type Output;
    fn strip_content_length_header(&self) -> Self::Output;
}

impl StripContentLengthHeader for HeaderMap {
    type Output = HeaderMap;
    fn strip_content_length_header(&self) -> Self::Output {
        strip_content_length_headers_headermap(self)
    }
}

impl StripContentLengthHeader for [(String, String)] {
    type Output = Vec<(String, String)>;
    fn strip_content_length_header(&self) -> Self::Output {
        strip_content_length_headers_vec(self)
    }
}

impl StripContentLengthHeader for Vec<(String, String)> {
    type Output = Vec<(String, String)>;
    fn strip_content_length_header(&self) -> Self::Output {
        strip_content_length_headers_vec(self)
    }
}

impl StripContentLengthHeader for HashMap<String, String> {
    type Output = HashMap<String, String>;
    fn strip_content_length_header(&self) -> Self::Output {
        strip_content_length_headers_map(self)
    }
}

/// Returns true if the endpoint is a long-polling updates endpoint (`ilink/bot/getupdates`).
pub fn is_long_poll_endpoint(endpoint: &str) -> bool {
    endpoint.ends_with("getupdates") || endpoint.contains("getupdates")
}

/// Returns the timeout duration appropriate for the given endpoint.
/// 35s for getupdates (long-poll), 15s for other API calls.
pub fn timeout_for_endpoint(endpoint: &str) -> Duration {
    if is_long_poll_endpoint(endpoint) {
        Duration::from_millis(DEFAULT_LONG_POLL_TIMEOUT_MS)
    } else {
        Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
    }
}

/// Empty poll response `{ "ret": 0, "msgs": [] }` matching TS dist `api.js:49-60`.
pub fn empty_poll_response() -> Value {
    serde_json::json!({
        "ret": 0,
        "msgs": []
    })
}

#[derive(Clone)]
pub(crate) struct Http {
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

    /// Send a JSON request with the default timeout for the endpoint:
    /// 35s for `ilink/bot/getupdates`, 15s for other endpoints (`sendmessage`, `sendtyping`, etc.).
    pub async fn json(
        &self,
        base: &str,
        endpoint: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value> {
        let timeout = timeout_for_endpoint(endpoint);
        self.json_with_timeout(base, endpoint, token, body, timeout).await
    }

    /// Send a JSON request with an explicit timeout duration.
    ///
    /// For long-poll `getupdates`, a timeout is caught and converted to an empty success response
    /// (`{ "ret": 0, "msgs": [] }`), matching TS dist `api.js:49-60` AbortError handling.
    /// For other endpoints, timeout returns `RouteError("WEIXIN_REQUEST_TIMEOUT", ...)`.
    pub async fn json_with_timeout(
        &self,
        base: &str,
        endpoint: &str,
        token: Option<&str>,
        body: Option<Value>,
        timeout: Duration,
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

        // WeixinChannel.ts:1315-1331: Intercept /ilink/bot/ requests and strip Content-Length header.
        if endpoint.starts_with("ilink/bot/")
            || endpoint.contains("/ilink/bot/")
            || url.as_str().contains("/ilink/bot/")
            || is_ilink_fetch_compatibility_installed()
        {
            strip_content_length_header(request.headers_mut());
        }

        let is_poll = is_long_poll_endpoint(endpoint);
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

        match tokio::time::timeout(timeout, work).await {
            Ok(result) => result,
            Err(_) => {
                // TS dist api.js:49-60: getUpdates AbortError/timeout converts to ret:0 empty message.
                if is_poll {
                    Ok(empty_poll_response())
                } else {
                    Err(RouteError::new("WEIXIN_REQUEST_TIMEOUT", "iLink request timed out"))
                }
            }
        }
    }

    /// Dedicated helper for `ilink/bot/getupdates` endpoint.
    pub async fn get_updates(&self, base: &str, token: Option<&str>, buf: &str) -> Result<Value> {
        self.json(
            base,
            "ilink/bot/getupdates",
            token,
            Some(serde_json::json!({ "get_updates_buf": buf })),
        )
        .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::{HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn test_strip_content_length_header_map() {
        // WeixinChannel.ts:1334-1339: Headers instance removes Content-Length
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("123"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token"));

        strip_content_length_header(&mut headers);
        assert!(!headers.contains_key(CONTENT_LENGTH));
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer token");

        // Test immutable helper & trait
        let mut h2 = HeaderMap::new();
        h2.insert(CONTENT_LENGTH, HeaderValue::from_static("999"));
        h2.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        let stripped = strip_content_length_headers_headermap(&h2);
        assert!(!stripped.contains_key(CONTENT_LENGTH));
        assert_eq!(stripped.get(CONTENT_TYPE).unwrap(), "text/plain");

        let stripped_via_trait = h2.strip_content_length_header();
        assert!(!stripped_via_trait.contains_key(CONTENT_LENGTH));
        assert_eq!(stripped_via_trait.get(CONTENT_TYPE).unwrap(), "text/plain");
    }

    #[test]
    fn test_strip_content_length_headers_vec() {
        // WeixinChannel.ts:1341-1343: Array of [key, value] pairs
        let headers = vec![
            ("Content-Length".to_string(), "100".to_string()),
            ("content-length".to_string(), "200".to_string()),
            ("CONTENT-LENGTH".to_string(), "300".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Custom".to_string(), "value".to_string()),
        ];

        let stripped = strip_content_length_headers_vec(&headers);
        assert_eq!(stripped.len(), 2);
        assert_eq!(stripped[0], ("Content-Type".to_string(), "application/json".to_string()));
        assert_eq!(stripped[1], ("X-Custom".to_string(), "value".to_string()));

        // Trait usage
        let trait_stripped = headers.strip_content_length_header();
        assert_eq!(trait_stripped.len(), 2);
    }

    #[test]
    fn test_strip_content_length_headers_map_object() {
        // WeixinChannel.ts:1345-1351: Record<string, string>
        let mut map = HashMap::new();
        map.insert("Content-Length".to_string(), "123".to_string());
        map.insert("content-length".to_string(), "456".to_string());
        map.insert("Content-Type".to_string(), "application/json".to_string());
        map.insert("Host".to_string(), "127.0.0.1".to_string());

        let stripped = strip_content_length_headers_map(&map);
        assert!(!stripped.contains_key("Content-Length"));
        assert!(!stripped.contains_key("content-length"));
        assert_eq!(stripped.get("Content-Type").unwrap(), "application/json");
        assert_eq!(stripped.get("Host").unwrap(), "127.0.0.1");

        // Trait usage
        let trait_stripped = map.strip_content_length_header();
        assert!(!trait_stripped.contains_key("Content-Length"));
        assert_eq!(trait_stripped.get("Content-Type").unwrap(), "application/json");
    }

    #[test]
    fn test_install_ilink_fetch_compatibility() {
        // WeixinChannel.ts:1315-1331: Global idempotent install
        install_ilink_fetch_compatibility();
        assert!(is_ilink_fetch_compatibility_installed());
        // Idempotent second call
        install_ilink_fetch_compatibility();
        assert!(is_ilink_fetch_compatibility_installed());
    }

    #[test]
    fn test_timeout_configuration() {
        // Long poll vs API timeout constants
        assert_eq!(DEFAULT_LONG_POLL_TIMEOUT_MS, 35_000);
        assert_eq!(DEFAULT_API_TIMEOUT_MS, 15_000);

        // Long poll endpoint gets 35s
        assert!(is_long_poll_endpoint("ilink/bot/getupdates"));
        assert!(is_long_poll_endpoint("/ilink/bot/getupdates"));
        assert_eq!(
            timeout_for_endpoint("ilink/bot/getupdates"),
            Duration::from_millis(DEFAULT_LONG_POLL_TIMEOUT_MS)
        );

        // Other endpoints get 15s (sendtyping, sendmessage, getconfig, getuploadurl, etc.)
        assert!(!is_long_poll_endpoint("ilink/bot/sendtyping"));
        assert!(!is_long_poll_endpoint("ilink/bot/sendmessage"));
        assert!(!is_long_poll_endpoint("ilink/bot/getconfig"));
        assert!(!is_long_poll_endpoint("ilink/bot/getuploadurl"));

        assert_eq!(
            timeout_for_endpoint("ilink/bot/sendtyping"),
            Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
        );
        assert_eq!(
            timeout_for_endpoint("ilink/bot/sendmessage"),
            Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
        );
        assert_eq!(
            timeout_for_endpoint("ilink/bot/getconfig"),
            Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
        );
        assert_eq!(
            timeout_for_endpoint("ilink/bot/getuploadurl"),
            Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
        );

        // empty_poll_response structure
        let empty = empty_poll_response();
        assert_eq!(empty["ret"], 0);
        assert_eq!(empty["msgs"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_getupdates_timeout_returns_empty_success() {
        // TS dist api.js:49-60: getUpdates timeout maps to { ret: 0, msgs: [] }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });

        let http = Http::new();
        let resp = http
            .json_with_timeout(
                &base,
                "ilink/bot/getupdates",
                Some("test-token"),
                Some(serde_json::json!({"get_updates_buf": ""})),
                Duration::from_millis(30),
            )
            .await;

        let _ = server.await;

        assert!(resp.is_ok(), "poll timeout should return Ok: {:?}", resp);
        let val = resp.unwrap();
        assert_eq!(val["ret"], 0);
        assert_eq!(val["msgs"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_sendtyping_timeout_returns_error() {
        // Non-poll endpoint timing out returns WEIXIN_REQUEST_TIMEOUT
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });

        let http = Http::new();
        let resp = http
            .json_with_timeout(
                &base,
                "ilink/bot/sendtyping",
                Some("test-token"),
                Some(serde_json::json!({"status": 1})),
                Duration::from_millis(30),
            )
            .await;

        let _ = server.await;

        assert!(resp.is_err(), "sendtyping timeout should return Err");
        let err = resp.unwrap_err();
        assert_eq!(err.code, "WEIXIN_REQUEST_TIMEOUT");
        assert_eq!(err.message, "iLink request timed out");
    }

    #[tokio::test]
    async fn test_json_successful_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                let req_text = String::from_utf8_lossy(&buf[..n]);
                assert!(req_text.contains("POST /ilink/bot/sendmessage HTTP/1.1"));
                assert!(req_text.contains("Bearer my-token"));

                let body = r#"{"ret":0,"msg_id":"m123"}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });

        let http = Http::new();
        let res = http
            .json(
                &base,
                "ilink/bot/sendmessage",
                Some("my-token"),
                Some(serde_json::json!({"msg": "hello"})),
            )
            .await;

        let _ = server.await;

        assert!(res.is_ok(), "Request should succeed: {:?}", res);
        let val = res.unwrap();
        assert_eq!(val["ret"], 0);
        assert_eq!(val["msg_id"], "m123");
    }

    #[tokio::test]
    async fn test_get_updates_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                let req_text = String::from_utf8_lossy(&buf[..n]);
                assert!(req_text.contains("POST /ilink/bot/getupdates HTTP/1.1"));

                let body = r#"{"ret":0,"get_updates_buf":"cur-1","msgs":[]}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });

        let http = Http::new();
        let res = http.get_updates(&base, Some("tok"), "cur-0").await;
        let _ = server.await;

        assert!(res.is_ok());
        let val = res.unwrap();
        assert_eq!(val["ret"], 0);
        assert_eq!(val["get_updates_buf"], "cur-1");
    }

    #[test]
    fn test_origin_validation() {
        assert!(origin("https://ilinkai.weixin.qq.com").is_ok());
        assert!(origin("http://127.0.0.1:8080").is_ok());
        assert!(origin("http://localhost:3000").is_ok());
        assert!(origin("http://insecure.example.com").is_err());
        assert!(origin("https://user:pass@ilinkai.weixin.qq.com").is_err());
    }
}
