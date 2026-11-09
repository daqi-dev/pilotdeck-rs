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
        options: &SlackOptions,
        method: &str,
        params: Value,
        app: bool,
    ) -> Result<Value> {
        self.call_body(options, method, params.to_string(), app)
            .await
    }
    pub async fn send_message(
        &self,
        options: &SlackOptions,
        channel: &str,
        thread: Option<&str>,
        text: pilotdeck_protocol::JsString,
    ) -> Result<Value> {
        let mut body = pilotdeck_protocol::JsObject::new();
        body.insert("channel".into(), channel.into());
        body.insert("text".into(), pilotdeck_protocol::JsValue::String(text));
        body.insert("mrkdwn".into(), pilotdeck_protocol::JsValue::Bool(true));
        if let Some(thread) = thread.filter(|s| !s.is_empty()) {
            body.insert("thread_ts".into(), thread.into());
        }
        let body = pilotdeck_protocol::stringify_json(&pilotdeck_protocol::JsValue::Object(body))
            .map_err(|_| failure("Invalid Slack message JSON"))?
            .unwrap();
        self.call_body(options, "chat.postMessage", body, false)
            .await
    }
    async fn call_body(
        &self,
        options: &SlackOptions,
        method: &str,
        body: String,
        app: bool,
    ) -> Result<Value> {
        let mut base =
            url::Url::parse(&options.base_url).map_err(|_| failure("Invalid Slack API URL"))?;
        let local = base.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(base.scheme() == "https" || base.scheme() == "http" && local)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(failure("Slack API requires HTTPS or loopback HTTP"));
        }
        base.path_segments_mut()
            .map_err(|_| failure("Invalid Slack API URL"))?
            .pop_if_empty()
            .push(method);
        let request = hyper::Request::builder()
            .method("POST")
            .uri(base.as_str())
            .header("content-type", "application/json; charset=utf-8")
            .header(
                "authorization",
                format!(
                    "Bearer {}",
                    if app {
                        &options.app_token
                    } else {
                        &options.bot_token
                    }
                ),
            )
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| failure("Invalid Slack API request"))?;
        let work = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("Slack HTTP transport failed"))?;
            let success = response.status().is_success();
            let mut bytes = Vec::new();
            let mut body = response.into_body();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| failure("Slack HTTP body interrupted"))?;
                if let Some(data) = frame.data_ref() {
                    if data.len() > 4_194_304usize.saturating_sub(bytes.len()) {
                        return Err(failure("Slack API response too large"));
                    }
                    bytes.extend_from_slice(data);
                }
            }
            let body: Value =
                serde_json::from_slice(&bytes).map_err(|_| failure("Invalid Slack API JSON"))?;
            if !success || body["ok"] != true {
                return Err(failure("Slack API rejected request"));
            }
            Ok(body)
        };
        tokio::time::timeout(Duration::from_secs(40), work)
            .await
            .map_err(|_| failure("Slack API request timed out"))?
    }
}
