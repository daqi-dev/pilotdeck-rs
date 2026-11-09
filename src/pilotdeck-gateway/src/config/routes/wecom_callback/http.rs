use super::*;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::time::Duration;
pub(super) struct Api {
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
}
impl Api {
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
    pub async fn request(
        &self,
        url: &str,
        body: Option<Value>,
        seconds: u64,
    ) -> Result<(bool, Value)> {
        let request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(url)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(
                body.map(|v| v.to_string()).unwrap_or_default(),
            )))
            .map_err(|_| failure("Invalid WeCom HTTP request"))?;
        let request = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("WeCom HTTP connection failed"))?;
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| failure("WeCom HTTP response failed"))?
                .to_bytes();
            let raw = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
            Ok((status.is_success(), raw))
        };
        tokio::time::timeout(Duration::from_secs(seconds), request)
            .await
            .map_err(|_| failure("WeCom HTTP request timed out"))?
    }
}
pub(super) fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
