//! Twilio REST boundary (`require("twilio")` + `client.messages.create`).
//!
//! The TS module can run without its optional npm package. Explicit
//! `None` factories preserve that no-op contract for compatibility tests;
//! native production mounts use the built-in Messages REST sender below.
use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::channel::protocol::{BoxFuture, ChannelError};

/// `client.messages.create({body, from, to})`.
#[derive(Clone, Debug)]
pub struct SmsOutgoingMessage {
    pub body: JsString,
    pub from: JsString,
    pub to: JsString,
}
impl SmsOutgoingMessage {
    pub fn to_js(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert("body".into(), JsValue::String(self.body.clone()));
        object.insert("from".into(), JsValue::String(self.from.clone()));
        object.insert("to".into(), JsValue::String(self.to.clone()));
        object
    }
}

pub trait SmsSender: Send + Sync {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>>;
}

/// `twilio(accountSid, authToken)` — the constructor call in `start()`.
pub type TwilioFactory =
    Arc<dyn Fn(&JsString, &JsString) -> Result<Arc<dyn SmsSender>, ChannelError> + Send + Sync>;

// Native first-party Twilio Messages REST boundary. The optional SDK absence
// branch remains available via an explicit None factory on SmsChannelOptions.
pub fn native_twilio_factory() -> TwilioFactory {
    native_factory("https://api.twilio.com".into())
}
pub(crate) fn native_twilio_factory_with_base(base: String) -> TwilioFactory {
    native_factory(base)
}
fn native_factory(base: String) -> TwilioFactory {
    Arc::new(move |sid, token| {
        let account = String::from_utf16_lossy(sid.units());
        let secret = String::from_utf16_lossy(token.units());
        if account.is_empty() || secret.is_empty() {
            return Err(ChannelError::error("Twilio credentials are required"));
        }
        Ok(Arc::new(NativeTwilioSender::new(base.clone(), account, secret)) as Arc<dyn SmsSender>)
    })
}
struct NativeTwilioSender {
    base: String,
    account: String,
    authorization: String,
    client: crate::channel::http_client::OutboundClient,
}
impl NativeTwilioSender {
    fn new(base: String, account: String, token: String) -> Self {
        use base64::Engine;
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{account}:{token}"))
        );
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(
                    crate::runtime::model::http::webpki_client_config(),
                ));
        Self {
            base,
            account,
            authorization,
            client,
        }
    }
}
impl SmsSender for NativeTwilioSender {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("Body", &String::from_utf16_lossy(message.body.units()))
            .append_pair("From", &String::from_utf16_lossy(message.from.units()))
            .append_pair("To", &String::from_utf16_lossy(message.to.units()))
            .finish();
        let path = format!(
            "{}/2010-04-01/Accounts/{}/Messages.json",
            self.base.trim_end_matches('/'),
            crate::channel::http_client::encode_uri_component(&self.account)
        );
        let request = hyper::Request::builder()
            .method("POST")
            .uri(path)
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from(body)));
        let client = self.client.clone();
        Box::pin(async move {
            let request = request.map_err(|_| ChannelError::error("Invalid Twilio request"))?;
            tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                let response = client
                    .request(request)
                    .await
                    .map_err(|_| ChannelError::error("Twilio request failed"))?;
                let status = response.status();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|_| ChannelError::error("Twilio response failed"))?
                    .to_bytes();
                let parsed: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| ChannelError::error("Invalid Twilio JSON response"))?;
                if status.is_success() {
                    return Ok(());
                }
                Err(ChannelError::error(
                    parsed["message"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Twilio HTTP {}", status.as_u16())),
                ))
            })
            .await
            .map_err(|_| ChannelError::error("Twilio request timed out"))?
        })
    }
}
