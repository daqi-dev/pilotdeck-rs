use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
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
        options: &TelegramOptions,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.call_body(options, method, params.to_string()).await
    }
    pub async fn send_message(
        &self,
        options: &TelegramOptions,
        chat: &str,
        text: pilotdeck_protocol::JsString,
    ) -> Result<Value> {
        let mut body = pilotdeck_protocol::JsObject::new();
        body.insert("chat_id".into(), chat.into());
        body.insert("text".into(), pilotdeck_protocol::JsValue::String(text));
        let body = pilotdeck_protocol::stringify_json(&pilotdeck_protocol::JsValue::Object(body))
            .map_err(|_| failure("Invalid Telegram message JSON"))?
            .unwrap();
        self.call_body(options, "sendMessage", body).await
    }
    async fn call_body(
        &self,
        options: &TelegramOptions,
        method: &str,
        body: String,
    ) -> Result<Value> {
        let mut base = url::Url::parse(&options.base_url)
            .map_err(|_| failure("Invalid Telegram API origin"))?;
        let local = base.host_str().is_some_and(|s| {
            s == "localhost"
                || s.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(base.scheme() == "https" || base.scheme() == "http" && local)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(failure("Telegram API requires HTTPS or loopback HTTP"));
        }
        base.path_segments_mut()
            .map_err(|_| failure("Invalid Telegram API origin"))?
            .pop_if_empty()
            .push(&format!("bot{}", options.token))
            .push(method);
        let request = hyper::Request::builder()
            .method("POST")
            .uri(base.as_str())
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| failure("Invalid Telegram HTTP request"))?;
        let work = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("Telegram HTTP transport failed"))?;
            let success = response.status().is_success();
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| failure("Telegram HTTP body interrupted"))?;
                if let Some(data) = frame.data_ref() {
                    if data.len() > 4_194_304usize.saturating_sub(bytes.len()) {
                        return Err(failure("Telegram HTTP response too large"));
                    }
                    bytes.extend_from_slice(data);
                }
            }
            let body: Value =
                serde_json::from_slice(&bytes).map_err(|_| failure("Invalid Telegram API JSON"))?;
            if !success || body["ok"] != true {
                return Err(failure("Telegram API rejected request"));
            }
            Ok(body["result"].clone())
        };
        tokio::time::timeout(Duration::from_secs(40), work)
            .await
            .map_err(|_| failure("Telegram API request timed out"))?
    }
}
