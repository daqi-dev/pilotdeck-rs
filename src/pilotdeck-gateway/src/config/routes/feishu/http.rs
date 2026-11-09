use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
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
    pub async fn call(
        &self,
        method: &str,
        url: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(bool, Value)> {
        let mut request = Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json; charset=utf-8");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(Full::new(Bytes::from(
                body.map(|body| body.to_string()).unwrap_or_default(),
            )))
            .map_err(|_| failure("Invalid Feishu HTTP request"))?;
        let call = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("Feishu HTTP connection failed"))?;
            let ok = response.status().is_success();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| failure("Feishu HTTP response failed"))?
                .to_bytes();
            Ok((
                ok,
                serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})),
            ))
        };
        tokio::time::timeout(Duration::from_secs(60), call)
            .await
            .map_err(|_| failure("Feishu HTTP timed out"))?
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
