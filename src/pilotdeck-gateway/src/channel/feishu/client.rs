//! The Lark Open API surface `FeishuChannel` drives (baseline cc7c4e6):
//! tenant-access-token cache, text/post send and edit, and the processing
//! reaction. URLs are derived from one `base` so the replay fixture can point
//! the whole client at a loopback mock; production passes
//! `https://open.feishu.cn` (or `https://open.larksuite.com` for
//! `domainName: "lark"`).
use std::{sync::Arc, time::Duration};

use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use tokio::sync::Mutex;

use super::{parse::Clock, render::ActivityKind};
use crate::channel::http_client::{HttpClient, encode_uri_component};

/// `AbortSignal`-free TS `fetch` calls here have no deadline; the Rust client
/// keeps one so a stalled mock cannot pin a turn forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const TOKEN_REFRESH_BUFFER_MS: f64 = 5.0 * 60.0 * 1000.0;
const DEFAULT_TOKEN_TTL_SECONDS: f64 = 7200.0;
/// Feishu's "token expired / invalid" codes that drop the cached token.
const TOKEN_INVALID_CODES: [f64; 2] = [99991663.0, 99991664.0];

pub(crate) type Log = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// `string | undefined | false`, the TS send-result union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendResult {
    /// A message id, or `undefined` for the custom-send path.
    Ok(Option<String>),
    Failed,
}

impl SendResult {
    pub(crate) fn is_failed(&self) -> bool {
        matches!(self, Self::Failed)
    }
    /// Registered diff A8.3-CH-FEISHU-005: TS reads the created message id to
    /// edit that same message in place; the Rust path sends each segment once
    /// and never re-reads the id.
    #[allow(dead_code)]
    pub(crate) fn message_id(&self) -> Option<&str> {
        match self {
            Self::Ok(Some(id)) => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct CachedToken {
    value: String,
    expires_at_ms: f64,
}

pub(crate) struct ApiClient {
    app_id: String,
    app_secret: String,
    base: String,
    http: HttpClient,
    clock: Arc<dyn Clock>,
    log: Log,
    token: Mutex<Option<CachedToken>>,
}

impl ApiClient {
    pub(crate) fn new(
        app_id: String,
        app_secret: String,
        base: String,
        clock: Arc<dyn Clock>,
        log: Log,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            base,
            http: HttpClient::new(),
            clock,
            log,
            token: Mutex::new(None),
        }
    }

    fn has_credentials(&self) -> bool {
        !self.app_id.is_empty() && !self.app_secret.is_empty()
    }

    /// `getTenantAccessToken()`: the cached value while more than the 5-minute
    /// refresh buffer remains, otherwise one refresh (concurrent callers wait
    /// on the same mutex and then read the fresh cache).
    pub(crate) async fn tenant_access_token(&self) -> Result<String, String> {
        let mut cache = self.token.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.expires_at_ms - TOKEN_REFRESH_BUFFER_MS > self.clock.now_ms()
        {
            return Ok(cached.value.clone());
        }

        let mut body = JsObject::new();
        body.insert(
            "app_id".into(),
            JsValue::String(JsString::from(self.app_id.as_str())),
        );
        body.insert(
            "app_secret".into(),
            JsValue::String(JsString::from(self.app_secret.as_str())),
        );
        let body = stringify_json(&JsValue::Object(body))
            .ok()
            .flatten()
            .unwrap_or_default();
        let response = self
            .http
            .request(
                "POST",
                &format!(
                    "{}/open-apis/auth/v3/tenant_access_token/internal",
                    self.base
                ),
                Some("application/json; charset=utf-8"),
                &body,
                REQUEST_TIMEOUT,
            )
            .await?;
        let json = parse_body(&response.body);
        let code = json.get("code").cloned().unwrap_or(JsValue::Undefined);
        let token = json
            .get("tenant_access_token")
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .filter(|value| !value.is_empty());
        if !number_is(&code, 0.0) || token.is_none() {
            return Err(format!(
                "tenant_access_token failed: code={} msg={}",
                display(Some(&code)),
                display(json.get("msg"))
            ));
        }
        let expire = json.get("expire").and_then(JsValue::as_number);
        let expire_sec = expire
            .filter(|value| value.is_finite())
            .unwrap_or(DEFAULT_TOKEN_TTL_SECONDS);
        let token = token.expect("#1329 token present");
        *cache = Some(CachedToken {
            value: token.clone(),
            expires_at_ms: self.clock.now_ms() + expire_sec * 1000.0,
        });
        Ok(token)
    }

    /// `sendRawMessage(chatId, msgType, content)`.
    pub(crate) async fn send_raw(
        &self,
        chat_id: &str,
        msg_type: &str,
        content: JsValue,
    ) -> SendResult {
        if !self.has_credentials() {
            (self.log)("warn", "feishu: cannot send — appId/appSecret missing");
            return SendResult::Failed;
        }
        let result = self
            .send_raw_inner(chat_id, msg_type, content, "send")
            .await;
        match result {
            Ok(id) => SendResult::Ok(id),
            Err(()) => SendResult::Failed,
        }
    }

    async fn send_raw_inner(
        &self,
        chat_id: &str,
        msg_type: &str,
        content: JsValue,
        label: &str,
    ) -> Result<Option<String>, ()> {
        let token = match self.tenant_access_token().await {
            Ok(token) => token,
            Err(error) => {
                (self.log)(
                    "error",
                    &format!("feishu: {label} {msg_type} threw: {error}"),
                );
                return Err(());
            }
        };
        let body = message_body(chat_id, msg_type, content);
        match self
            .http
            .request_bearer(
                "POST",
                &format!(
                    "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
                    self.base
                ),
                Some("application/json; charset=utf-8"),
                &token,
                &body,
                REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(response) => {
                let json = parse_body(&response.body);
                let code = json.get("code").cloned().unwrap_or(JsValue::Undefined);
                let code_ok = match code.as_number() {
                    Some(number) => number == 0.0,
                    None => code.is_undefined(),
                };
                if !(200..300).contains(&response.status) || !code_ok {
                    self.invalidate_token(&code).await;
                    (self.log)(
                        if label == "send" { "error" } else { "warn" },
                        &format!(
                            "feishu: {label} {msg_type} failed code={} msg={}",
                            display(Some(&code)),
                            display(json.get("msg"))
                        ),
                    );
                    return Err(());
                }
                Ok(json
                    .get("data")
                    .and_then(|data| data.get("message_id"))
                    .and_then(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units())))
            }
            Err(error) => {
                (self.log)(
                    if label == "send" { "error" } else { "warn" },
                    &format!("feishu: {label} {msg_type} threw: {error}"),
                );
                Err(())
            }
        }
    }

    /// `sendPostMessage(chatId, text, options)`. TS returns
    /// `json.data?.message_id ?? false`, so a successful code with no id is a
    /// `false` result and the caller falls back to a plain text send.
    pub(crate) async fn send_post(
        &self,
        chat_id: &str,
        text: &str,
        is_final: bool,
        activity_kind: Option<ActivityKind>,
    ) -> SendResult {
        if !self.has_credentials() {
            return SendResult::Failed;
        }
        let content = super::render::render_live_post(text, is_final, activity_kind);
        match self
            .send_raw_inner_named(chat_id, "post", content, "send post")
            .await
        {
            Ok(Some(id)) => SendResult::Ok(Some(id)),
            Ok(None) | Err(()) => SendResult::Failed,
        }
    }

    /// `editPostMessage` / `editTextMessage`: the same PUT with a different
    /// `msg_type`. Registered diff A8.3-CH-FEISHU-005: the Rust live-reply
    /// path flushes once instead of editing a message, so nothing calls this
    /// until that path is ported.
    #[allow(dead_code)]
    pub(crate) async fn edit_message(
        &self,
        message_id: &str,
        msg_type: &str,
        content: JsValue,
        label: &str,
    ) -> bool {
        if message_id.is_empty() || !self.has_credentials() {
            return false;
        }
        let token = match self.tenant_access_token().await {
            Ok(token) => token,
            Err(error) => {
                (self.log)("warn", &format!("feishu: {label} threw: {error}"));
                return false;
            }
        };
        let mut body = JsObject::new();
        body.insert("msg_type".into(), JsValue::String(JsString::from(msg_type)));
        body.insert(
            "content".into(),
            JsValue::String(JsString::from(
                stringify_json(&content).ok().flatten().unwrap_or_default(),
            )),
        );
        let body = stringify_json(&JsValue::Object(body))
            .ok()
            .flatten()
            .unwrap_or_default();
        let response = self
            .http
            .request_bearer(
                "PUT",
                &format!(
                    "{}/open-apis/im/v1/messages/{}",
                    self.base,
                    encode_uri_component(message_id)
                ),
                Some("application/json; charset=utf-8"),
                &token,
                &body,
                REQUEST_TIMEOUT,
            )
            .await;
        match response {
            Ok(response) => {
                let json = parse_body(&response.body);
                let code = json.get("code").cloned().unwrap_or(JsValue::Undefined);
                let code_ok = match code.as_number() {
                    Some(number) => number == 0.0,
                    None => code.is_undefined(),
                };
                if !(200..300).contains(&response.status) || !code_ok {
                    self.invalidate_token(&code).await;
                    (self.log)(
                        "warn",
                        &format!(
                            "feishu: {label} failed code={} msg={}",
                            display(Some(&code)),
                            display(json.get("msg"))
                        ),
                    );
                    return false;
                }
                true
            }
            Err(error) => {
                (self.log)("warn", &format!("feishu: {label} threw: {error}"));
                false
            }
        }
    }

    /// `addReaction(messageId)`: `Some(reaction_id)` only on `code === 0`.
    pub(crate) async fn add_reaction(&self, message_id: &str) -> Option<String> {
        if !self.has_credentials() {
            return None;
        }
        let token = match self.tenant_access_token().await {
            Ok(token) => token,
            Err(error) => {
                (self.log)("info", &format!("feishu: addReaction failed: {error}"));
                return None;
            }
        };
        let mut reaction = JsObject::new();
        reaction.insert("emoji_type".into(), JsValue::String("OnIt".into()));
        let mut body = JsObject::new();
        body.insert("reaction_type".into(), JsValue::Object(reaction));
        let body = stringify_json(&JsValue::Object(body))
            .ok()
            .flatten()
            .unwrap_or_default();
        let response = self
            .http
            .request_bearer(
                "POST",
                &format!(
                    "{}/open-apis/im/v1/messages/{message_id}/reactions",
                    self.base
                ),
                Some("application/json; charset=utf-8"),
                &token,
                &body,
                REQUEST_TIMEOUT,
            )
            .await;
        match response {
            Ok(response) => {
                let json = parse_body(&response.body);
                let code = json.get("code").cloned().unwrap_or(JsValue::Undefined);
                let reaction_id = json
                    .get("data")
                    .and_then(|data| data.get("reaction_id"))
                    .and_then(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units()))
                    .filter(|value| !value.is_empty());
                if number_is(&code, 0.0)
                    && let Some(reaction_id) = reaction_id
                {
                    return Some(reaction_id);
                }
                (self.log)(
                    "info",
                    &format!("feishu: addReaction non-zero code={}", display(Some(&code))),
                );
                None
            }
            Err(error) => {
                (self.log)("info", &format!("feishu: addReaction failed: {error}"));
                None
            }
        }
    }

    /// `removeReaction(messageId, reactionId)`: best effort, result ignored.
    pub(crate) async fn remove_reaction(&self, message_id: &str, reaction_id: &str) {
        if !self.has_credentials() {
            return;
        }
        let token = match self.tenant_access_token().await {
            Ok(token) => token,
            Err(error) => {
                (self.log)("info", &format!("feishu: removeReaction failed: {error}"));
                return;
            }
        };
        if let Err(error) = self
            .http
            .request_bearer(
                "DELETE",
                &format!(
                    "{}/open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}",
                    self.base
                ),
                None,
                &token,
                "",
                REQUEST_TIMEOUT,
            )
            .await
        {
            (self.log)("info", &format!("feishu: removeReaction failed: {error}"));
        }
    }

    async fn send_raw_inner_named(
        &self,
        chat_id: &str,
        msg_type: &str,
        content: JsValue,
        _label: &str,
    ) -> Result<Option<String>, ()> {
        // The post path logs with its own label; the shared body builder and
        // response handling stay identical to `sendRawMessage`.
        let token = self.tenant_access_token().await.map_err(|error| {
            (self.log)("warn", &format!("feishu: send post threw: {error}"));
        })?;
        let body = message_body(chat_id, msg_type, content);
        let response = self
            .http
            .request_bearer(
                "POST",
                &format!(
                    "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
                    self.base
                ),
                Some("application/json; charset=utf-8"),
                &token,
                &body,
                REQUEST_TIMEOUT,
            )
            .await
            .map_err(|error| {
                (self.log)("warn", &format!("feishu: send post threw: {error}"));
            })?;
        let json = parse_body(&response.body);
        let code = json.get("code").cloned().unwrap_or(JsValue::Undefined);
        let code_ok = match code.as_number() {
            Some(number) => number == 0.0,
            None => code.is_undefined(),
        };
        if !(200..300).contains(&response.status) || !code_ok {
            self.invalidate_token(&code).await;
            (self.log)(
                "warn",
                &format!(
                    "feishu: send post failed code={} msg={}",
                    display(Some(&code)),
                    display(json.get("msg"))
                ),
            );
            return Err(());
        }
        Ok(json
            .get("data")
            .and_then(|data| data.get("message_id"))
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units())))
    }

    async fn invalidate_token(&self, code: &JsValue) {
        if let Some(number) = code.as_number()
            && TOKEN_INVALID_CODES.contains(&number)
        {
            *self.token.lock().await = None;
        }
    }
}

fn message_body(chat_id: &str, msg_type: &str, content: JsValue) -> String {
    let mut body = JsObject::new();
    body.insert(
        "receive_id".into(),
        JsValue::String(JsString::from(chat_id)),
    );
    body.insert("msg_type".into(), JsValue::String(JsString::from(msg_type)));
    body.insert(
        "content".into(),
        JsValue::String(JsString::from(
            stringify_json(&content).ok().flatten().unwrap_or_default(),
        )),
    );
    stringify_json(&JsValue::Object(body))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// `await res.json().catch(() => ({}))`: an unparsable body is an empty object.
fn parse_body(bytes: &bytes::Bytes) -> JsValue {
    let text = String::from_utf8_lossy(bytes);
    pilotdeck_protocol::parse_json(&text).unwrap_or_else(|_| JsValue::Object(JsObject::new()))
}

fn number_is(value: &JsValue, expected: f64) -> bool {
    value.as_number().is_some_and(|number| number == expected)
}

/// JavaScript string interpolation of a JSON value (`undefined` included).
fn display(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".into(),
        Some(JsValue::Null) => "null".into(),
        Some(JsValue::Bool(value)) => value.to_string(),
        Some(JsValue::Number(value)) => crate::js_value::number_string(*value),
        Some(JsValue::String(value)) => String::from_utf16_lossy(value.units()),
        Some(JsValue::Array(_)) | Some(JsValue::Object(_)) => value
            .and_then(|value| crate::js_value::display(value).ok())
            .unwrap_or_else(|| "undefined".into()),
    }
}
