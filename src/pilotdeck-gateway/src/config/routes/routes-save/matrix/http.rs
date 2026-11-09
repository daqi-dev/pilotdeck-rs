use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};

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
    async fn request(
        &self,
        options: &MatrixOptions,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<Value> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}/_matrix/client/v3{path}", options.homeserver))
            .header("authorization", format!("Bearer {}", options.access_token))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|_| failure("Invalid Matrix request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("Matrix HTTP connection failed"))?;
        if !response.status().is_success() {
            return Err(failure("Matrix HTTP request rejected"));
        }
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| failure("Matrix HTTP response failed"))?
            .to_bytes();
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("Invalid Matrix HTTP response"))
    }
    pub async fn json(
        &self,
        options: &MatrixOptions,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(options, method, path, body.map(|v| v.to_string()))
            .await
    }
    pub async fn sync(&self, options: &MatrixOptions, since: Option<&str>) -> Result<Value> {
        let mut path = "/sync?timeout=30000".to_owned();
        if let Some(since) = since {
            path.push_str("&since=");
            path.push_str(&encode_component(since));
        }
        self.json(options, "GET", &path, None).await
    }
    pub async fn send(&self, options: &MatrixOptions, room: &str, text: JsString) -> Result<()> {
        let path = format!(
            "/rooms/{}/send/m.room.message/{}",
            encode_component(room),
            uuid::Uuid::new_v4()
        );
        let mut body = JsObject::new();
        body.insert("msgtype".into(), "m.text".into());
        body.insert("body".into(), JsValue::String(text));
        let body = stringify_json(&JsValue::Object(body))
            .map_err(|_| failure("Invalid Matrix message"))?
            .unwrap();
        self.request(options, "PUT", &path, Some(body)).await?;
        Ok(())
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
