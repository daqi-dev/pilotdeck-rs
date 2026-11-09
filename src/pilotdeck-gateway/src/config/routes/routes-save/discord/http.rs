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
    pub async fn request(
        &self,
        options: &DiscordOptions,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<Value> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", options.base_url))
            .header("authorization", format!("Bot {}", options.token))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|_| failure("Invalid Discord REST request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("Discord REST connection failed"))?;
        let success = response.status().is_success();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| failure("Discord REST response failed"))?
            .to_bytes();
        if !success {
            return Err(failure("Discord REST request rejected"));
        }
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("Invalid Discord REST JSON"))
    }
    pub async fn json(
        &self,
        options: &DiscordOptions,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(options, method, path, body.map(|v| v.to_string()))
            .await
    }
}
pub(super) fn encode_component(input: &str) -> String {
    let mut result = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
