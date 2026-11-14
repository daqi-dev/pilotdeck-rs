//! Port of `weixin-ilink` dist `{api,client,auth}.js` (the vendored npm
//! package the baseline channel uses): 5 POST endpoints over hyper, the
//! stateful `ILinkClient` with a persistent poll cursor, and QR login.
//!
//! Requests/responses stay `JsValue` so field presence matches the TS
//! `res.json()` objects exactly (`ret !== undefined` distinctions included).
//! `sendMedia`/`getUploadUrl` are the attachment path and land with pr2.
//! This unit's tests only speak to a loopback mock; the real-service quirk
//! handled by TS `installIlinkFetchCompatibility` (stripping the explicit
//! Content-Length on `/ilink/bot/` posts) is transport framing, not part of
//! the recorded protocol, and belongs to the real-account follow-up.
use crate::channel::protocol::BoxFuture;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, body::Bytes};
use hyper_util::client::legacy::Client;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{sync::Arc, sync::Mutex, time::Duration};

const DEFAULT_CHANNEL_VERSION: &str = "weixin-ilink/0.1.0";
const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
/// auth.js QR poll interval between status checks.
const QR_POLL_INTERVAL_MS: u64 = 1000;
const QR_POLL_TIMEOUT: Duration = Duration::from_secs(35);
/// auth.js `LOGIN_TIMEOUT_MS` (8 minutes).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(480);
/// auth.js `MAX_QR_REFRESH`.
const MAX_QR_REFRESH: u32 = 3;

/// MessageItemType.TEXT (weixin-ilink types.js).
pub const MESSAGE_ITEM_TYPE_TEXT: f64 = 1.0;
/// MessageItemType.IMAGE.
pub const MESSAGE_ITEM_TYPE_IMAGE: f64 = 2.0;
/// MessageItemType.VOICE.
pub const MESSAGE_ITEM_TYPE_VOICE: f64 = 3.0;
/// MessageItemType.FILE.
pub const MESSAGE_ITEM_TYPE_FILE: f64 = 4.0;
/// MessageItemType.VIDEO.
pub const MESSAGE_ITEM_TYPE_VIDEO: f64 = 5.0;
/// MessageType.BOT.
const MESSAGE_TYPE_BOT: f64 = 2.0;
/// MessageState.FINISH.
const MESSAGE_STATE_FINISH: f64 = 2.0;
/// TypingStatus.TYPING.
const TYPING_STATUS_TYPING: f64 = 1.0;

type WeixinConnector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

/// The shared rustls connector (`runtime::model::http`), not a private copy
/// (#1725 item 9: the copy dropped the connect-timeout setting). The CDN
/// client in [`super::media`] and the shared IM `http_client` use the same
/// construction.
fn weixin_connector() -> WeixinConnector {
    crate::runtime::model::http::https_connector(crate::runtime::model::http::webpki_client_config())
}

/// TS errors reaching `formatWeixinError` keep their `name`; the formatted
/// text is what `isRecoverablePollError` matches against.
#[derive(Clone, Debug)]
pub struct WeixinClientError {
    pub name: String,
    pub message: String,
}

impl WeixinClientError {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            name: "Error".into(),
            message: message.into(),
        }
    }
    /// Node `fetch` rejects with `TypeError: fetch failed` on transport-level
    /// failures; hyper's io errors fold into the same recoverable shape.
    pub fn fetch_failed() -> Self {
        Self {
            name: "TypeError".into(),
            message: "fetch failed".into(),
        }
    }
    pub fn abort() -> Self {
        Self {
            name: "AbortError".into(),
            message: "This operation was aborted".into(),
        }
    }
    /// TS `` `${name}: ${message}` ``.
    pub fn formatted(&self) -> String {
        format!("{}: {}", self.name, self.message)
    }
}

impl std::fmt::Display for WeixinClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.formatted())
    }
}
impl std::error::Error for WeixinClientError {}

/// WeixinChannel.ts `isRecoverablePollError` (substring set, lowercased).
pub fn is_recoverable_poll_error(detail: &str) -> bool {
    let detail = detail.to_lowercase();
    [
        "fetch failed",
        "econnreset",
        "enet",
        "etimedout",
        "und_err",
        "socket",
        "network",
        "timeout",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
}

/// The `WeixinIlinkClient` slice the channel depends on (WeixinChannel.ts
/// type of the same name), so tests can substitute a scripted mock.
pub trait WeixinIlinkClient: Send + Sync {
    fn cursor(&self) -> String;
    fn set_cursor(&self, cursor: &str);
    /// Long-poll; a client-side timeout returns the empty-response shape.
    fn poll(&self) -> BoxFuture<Result<JsValue, WeixinClientError>>;
    /// Returns the number of messages sent (chunk count).
    fn send_text_chunked(
        &self,
        to_user_id: &JsString,
        text: &JsString,
        context_token: &JsString,
        max_length: usize,
    ) -> BoxFuture<Result<usize, WeixinClientError>>;
    /// getconfig + conditional sendtyping.
    fn send_typing(
        &self,
        user_id: &JsString,
        context_token: &JsString,
    ) -> BoxFuture<Result<(), WeixinClientError>>;
}

fn string_field(value: &JsValue, name: &str) -> Option<JsString> {
    value.get(name).and_then(JsValue::as_string).cloned()
}

/// URL join: TS `new URL(endpoint, base.endsWith("/") ? base : base + "/")`.
fn endpoint_url(base_url: &str, endpoint: &str) -> String {
    if base_url.ends_with('/') {
        format!("{base_url}{endpoint}")
    } else {
        format!("{base_url}/{endpoint}")
    }
}

/// JS `encodeURIComponent` (unreserved set `A-Za-z0-9-_.!~*'()`); shared with
/// [`super::media`] rather than copied there.
pub(super) fn uri_encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// api.js `randomWechatUin`: base64 of a random uint32's decimal text.
fn random_wechat_uin() -> String {
    let bytes = uuid::Uuid::new_v4();
    let uint32 = u32::from_be_bytes([
        bytes.as_bytes()[0],
        bytes.as_bytes()[1],
        bytes.as_bytes()[2],
        bytes.as_bytes()[3],
    ]);
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(uint32.to_string())
}

/// client.js `generateClientId`: `ilink-{Date.now()}-{12 hex}`.
fn generate_client_id() -> String {
    let bytes = uuid::Uuid::new_v4();
    let hex: String = bytes.as_bytes()[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "ilink-{}-{hex}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default()
    )
}

/// The concrete hyper-backed client (ILinkClient + api.js). Cheap to clone:
/// the trait's boxed futures are `'static`, so each call takes an owned
/// handle (the cursor lock is shared, like the TS object identity).
#[derive(Clone)]
pub struct IlinkHttpClient {
    base_url: String,
    token: String,
    channel_version: String,
    long_poll_timeout: Duration,
    api_timeout: Duration,
    cursor: Arc<Mutex<String>>,
    http: Client<WeixinConnector, Full<Bytes>>,
}

impl IlinkHttpClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url,
            token,
            channel_version: DEFAULT_CHANNEL_VERSION.into(),
            long_poll_timeout: Duration::from_secs(35),
            api_timeout: Duration::from_secs(15),
            cursor: Arc::new(Mutex::new(String::new())),
            http: Client::builder(hyper_util::rt::TokioExecutor::new()).build(weixin_connector()),
        }
    }

    /// api.js `post` — JSON body with `base_info.channel_version`, Bearer
    /// auth, `AuthorizationType: ilink_bot_token`, random `X-WECHAT-UIN`.
    async fn post_json(
        &self,
        endpoint: &str,
        payload: &JsValue,
        timeout: Duration,
    ) -> Result<JsValue, WeixinClientError> {
        let mut body = payload.as_object().cloned().unwrap_or_default();
        let mut base_info = JsObject::new();
        base_info.insert(
            "channel_version".into(),
            JsValue::String(self.channel_version.clone().into()),
        );
        body.insert("base_info".into(), JsValue::Object(base_info));
        let body = stringify_json(&JsValue::Object(body))
            .map_err(|error| WeixinClientError::error(error.to_string()))?
            .ok_or_else(|| WeixinClientError::error("JSON body is undefined"))?;

        let request = Request::builder()
            .method(Method::POST)
            .uri(endpoint_url(&self.base_url, endpoint))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.token))
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", random_wechat_uin())
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| WeixinClientError::error(error.to_string()))?;

        let (status, bytes) = self.exchange(request, timeout).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(WeixinClientError::error(format!(
                "{endpoint} {code}: {text}",
                code = status.as_u16()
            )));
        }
        parse_json(&text).map_err(|error| WeixinClientError {
            name: "SyntaxError".into(),
            message: error.to_string(),
        })
    }

    /// One bounded request/response exchange. Transport failures and timeouts
    /// carry the Node fetch error shapes the channel's recovery matches on.
    async fn exchange(
        &self,
        request: Request<Full<Bytes>>,
        timeout: Duration,
    ) -> Result<(hyper::StatusCode, Bytes), WeixinClientError> {
        match tokio::time::timeout(timeout, async {
            let response = self
                .http
                .request(request)
                .await
                .map_err(|_| WeixinClientError::fetch_failed())?;
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| WeixinClientError::fetch_failed())?
                .to_bytes();
            Ok::<_, WeixinClientError>((status, bytes))
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(WeixinClientError::abort()),
        }
    }

    /// auth.js `pollStatus` / `fetchQRCode` GET helper.
    async fn get_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<JsValue, WeixinClientError> {
        let mut builder = Request::builder().method(Method::GET).uri(url);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Full::new(Bytes::new()))
            .map_err(|error| WeixinClientError::error(error.to_string()))?;
        let (status, bytes) = self.exchange(request, timeout).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(WeixinClientError::error(format!(
                "Failed to fetch QR code: {code}",
                code = status.as_u16()
            )));
        }
        parse_json(&text).map_err(|error| WeixinClientError {
            name: "SyntaxError".into(),
            message: error.to_string(),
        })
    }

    /// client.js `sendText` message envelope.
    fn text_message(to_user_id: &JsString, text: &JsString, context_token: &JsString) -> JsValue {
        let mut text_item = JsObject::new();
        text_item.insert("text".into(), JsValue::String(text.clone()));
        let mut item = JsObject::new();
        item.insert("type".into(), JsValue::Number(MESSAGE_ITEM_TYPE_TEXT));
        item.insert("text_item".into(), JsValue::Object(text_item));
        let mut message = JsObject::new();
        message.insert("from_user_id".into(), JsValue::from(""));
        message.insert("to_user_id".into(), JsValue::String(to_user_id.clone()));
        message.insert(
            "client_id".into(),
            JsValue::from(generate_client_id().as_str()),
        );
        message.insert("message_type".into(), JsValue::Number(MESSAGE_TYPE_BOT));
        message.insert(
            "message_state".into(),
            JsValue::Number(MESSAGE_STATE_FINISH),
        );
        message.insert(
            "context_token".into(),
            JsValue::String(context_token.clone()),
        );
        message.insert(
            "item_list".into(),
            JsValue::Array(vec![JsValue::Object(item)]),
        );
        JsValue::Object(message)
    }

    async fn send_text(
        &self,
        to_user_id: &JsString,
        text: &JsString,
        context_token: &JsString,
    ) -> Result<(), WeixinClientError> {
        let mut payload = JsObject::new();
        payload.insert(
            "msg".into(),
            Self::text_message(to_user_id, text, context_token),
        );
        // api.js sendMessage discards the response body.
        self.post_json(
            "ilink/bot/sendmessage",
            &JsValue::Object(payload),
            self.api_timeout,
        )
        .await
        .map(|_| ())
    }

    /// client.js `getConfig` — used by sendTyping for the ticket.
    async fn get_config(
        &self,
        user_id: &JsString,
        context_token: &JsString,
    ) -> Result<JsValue, WeixinClientError> {
        let mut payload = JsObject::new();
        payload.insert("ilink_user_id".into(), JsValue::String(user_id.clone()));
        payload.insert(
            "context_token".into(),
            JsValue::String(context_token.clone()),
        );
        self.post_json(
            "ilink/bot/getconfig",
            &JsValue::Object(payload),
            self.api_timeout,
        )
        .await
    }

    /// auth.js `fetchQRCode` (GET `get_bot_qrcode?bot_type=3`).
    async fn fetch_qr_code(&self, base_url: &str) -> Result<JsValue, WeixinClientError> {
        let root = base_url.trim_end_matches('/');
        self.get_json(
            &format!("{root}/ilink/bot/get_bot_qrcode?bot_type=3"),
            &[],
            QR_POLL_TIMEOUT,
        )
        .await
    }
}

impl WeixinIlinkClient for IlinkHttpClient {
    fn cursor(&self) -> String {
        self.cursor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_cursor(&self, cursor: &str) {
        *self.cursor.lock().unwrap_or_else(|e| e.into_inner()) = cursor.into();
    }

    fn poll(&self) -> BoxFuture<Result<JsValue, WeixinClientError>> {
        let this = self.clone();
        Box::pin(async move {
            let mut payload = JsObject::new();
            payload.insert(
                "get_updates_buf".into(),
                JsValue::String(JsString::from(this.cursor())),
            );
            // api.js getUpdates: an AbortError (client-side long-poll timeout)
            // becomes the empty success shape, echoing the requested buffer.
            let response = match this
                .post_json(
                    "ilink/bot/getupdates",
                    &JsValue::Object(payload),
                    this.long_poll_timeout,
                )
                .await
            {
                Ok(response) => response,
                Err(error) if error.name == "AbortError" => {
                    let mut synthesized = JsObject::new();
                    synthesized.insert("ret".into(), JsValue::Number(0.0));
                    synthesized.insert("msgs".into(), JsValue::Array(vec![]));
                    synthesized.insert(
                        "get_updates_buf".into(),
                        JsValue::String(JsString::from(this.cursor())),
                    );
                    return Ok(JsValue::Object(synthesized));
                }
                Err(error) => return Err(error),
            };
            // client.js:26-28 — `if (resp.get_updates_buf)`: an empty buffer is
            // falsy and must not clear the cursor.
            if let Some(buffer) =
                string_field(&response, "get_updates_buf").filter(|b| !b.units().is_empty())
            {
                *this.cursor.lock().unwrap_or_else(|e| e.into_inner()) =
                    String::from_utf16_lossy(buffer.units());
            }
            Ok(response)
        })
    }

    fn send_text_chunked(
        &self,
        to_user_id: &JsString,
        text: &JsString,
        context_token: &JsString,
        max_length: usize,
    ) -> BoxFuture<Result<usize, WeixinClientError>> {
        let this = self.clone();
        let to_user_id = to_user_id.clone();
        let text = text.clone();
        let context_token = context_token.clone();
        Box::pin(async move {
            // JS string length and slice are UTF-16 code units.
            let units = text.units();
            if units.len() <= max_length {
                this.send_text(&to_user_id, &text, &context_token).await?;
                return Ok(1);
            }
            let chunks: Vec<&[u16]> = units.chunks(max_length).collect();
            for chunk in &chunks {
                this.send_text(
                    &to_user_id,
                    &JsString::from_utf16(chunk.to_vec()),
                    &context_token,
                )
                .await?;
            }
            Ok(chunks.len())
        })
    }

    fn send_typing(
        &self,
        user_id: &JsString,
        context_token: &JsString,
    ) -> BoxFuture<Result<(), WeixinClientError>> {
        let this = self.clone();
        let user_id = user_id.clone();
        let context_token = context_token.clone();
        Box::pin(async move {
            let config = this.get_config(&user_id, &context_token).await?;
            // client.js:81 — `if (config.typing_ticket)`: an empty ticket is
            // falsy, so no sendtyping request is made.
            let Some(ticket) =
                string_field(&config, "typing_ticket").filter(|ticket| !ticket.units().is_empty())
            else {
                return Ok(());
            };
            let mut payload = JsObject::new();
            payload.insert("ilink_user_id".into(), JsValue::String(user_id.clone()));
            payload.insert("typing_ticket".into(), JsValue::String(ticket));
            payload.insert("status".into(), JsValue::Number(TYPING_STATUS_TYPING));
            this.post_json(
                "ilink/bot/sendtyping",
                &JsValue::Object(payload),
                this.api_timeout,
            )
            .await
            .map(|_| ())
        })
    }
}

/// auth.js `LoginResult` — the fields the channel consumes.
#[derive(Clone, Debug)]
pub struct WeixinLoginResult {
    pub bot_token: String,
    pub account_id: String,
    pub base_url: String,
}

/// The channel-facing QR-login seam (TS `loginWithQR` injection point).
pub struct WeixinLoginCallbacks {
    pub on_qr_code: Box<dyn Fn(&JsString) + Send + Sync>,
    pub on_status_change: Box<dyn Fn(&str) + Send + Sync>,
}

pub type WeixinLogin = Arc<
    dyn Fn(WeixinLoginCallbacks) -> BoxFuture<Result<WeixinLoginResult, WeixinClientError>>
        + Send
        + Sync,
>;

/// auth.js `loginWithQR` against the same base URL. Production traffic goes
/// to the real service; this unit's goldens only drive the loopback mock.
pub async fn login_with_qr(
    callbacks: WeixinLoginCallbacks,
    base_url: &str,
) -> Result<WeixinLoginResult, WeixinClientError> {
    let client = IlinkHttpClient::new(base_url.into(), String::new());
    let root = base_url.trim_end_matches('/');
    let mut qr = client.fetch_qr_code(base_url).await?;
    let mut refresh_count = 1u32;
    let qr_image = string_field(&qr, "qrcode_img_content").unwrap_or_else(|| JsString::from(""));
    (callbacks.on_qr_code)(&qr_image);
    (callbacks.on_status_change)("waiting");
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let qrcode = string_field(&qr, "qrcode").unwrap_or_else(|| JsString::from(""));
        let encoded = uri_encode_component(&String::from_utf16_lossy(qrcode.units()));
        let status = match client
            .get_json(
                &format!("{root}/ilink/bot/get_qrcode_status?qrcode={encoded}"),
                &[("iLink-App-ClientVersion", "1")],
                QR_POLL_TIMEOUT,
            )
            .await
        {
            Ok(status) => status,
            // auth.js pollStatus turns its own AbortError into `{wait}`.
            Err(error) if error.name == "AbortError" => {
                let mut wait = JsObject::new();
                wait.insert("status".into(), JsValue::from("wait"));
                JsValue::Object(wait)
            }
            Err(error) => return Err(error),
        };
        let state = string_field(&status, "status")
            .map(|value| String::from_utf16_lossy(value.units()))
            .unwrap_or_default();
        match state.as_str() {
            "wait" => (),
            "scaned" => (callbacks.on_status_change)("scanned"),
            "expired" => {
                refresh_count += 1;
                if refresh_count > MAX_QR_REFRESH {
                    return Err(WeixinClientError::error("QR code expired too many times"));
                }
                (callbacks.on_status_change)("refreshing");
                qr = client.fetch_qr_code(base_url).await?;
                let qr_image =
                    string_field(&qr, "qrcode_img_content").unwrap_or_else(|| JsString::from(""));
                (callbacks.on_qr_code)(&qr_image);
                (callbacks.on_status_change)("waiting");
            }
            "confirmed" => {
                // auth.js:81 — `if (!status.ilink_bot_id || !status.bot_token)`:
                // an empty string is falsy, so `""` counts as "not returned".
                let bot_token =
                    string_field(&status, "bot_token").filter(|token| !token.units().is_empty());
                let account_id =
                    string_field(&status, "ilink_bot_id").filter(|id| !id.units().is_empty());
                let (Some(bot_token), Some(account_id)) = (bot_token, account_id) else {
                    return Err(WeixinClientError::error(
                        "Login failed: server did not return required credentials",
                    ));
                };
                let returned_base = string_field(&status, "baseurl")
                    .map(|value| String::from_utf16_lossy(value.units()))
                    .unwrap_or_default();
                return Ok(WeixinLoginResult {
                    bot_token: String::from_utf16_lossy(bot_token.units()),
                    account_id: String::from_utf16_lossy(account_id.units()),
                    // `status.baseurl || baseUrl`.
                    base_url: if returned_base.is_empty() {
                        base_url.into()
                    } else {
                        returned_base
                    },
                });
            }
            _ => (),
        }
        tokio::time::sleep(Duration::from_millis(QR_POLL_INTERVAL_MS)).await;
    }
    Err(WeixinClientError::error("Login timed out"))
}

/// Default QR base URL (auth.js `DEFAULT_BASE_URL`).
pub fn default_login_base_url() -> String {
    DEFAULT_BASE_URL.into()
}

/// The `clientFactory` seam type (WeixinChannel.ts): fresh client per
/// credentials.
pub type WeixinClientFactory =
    Arc<dyn Fn(&SavedCredentials) -> Arc<dyn WeixinIlinkClient> + Send + Sync>;

/// The default injected seams: real hyper client + real QR login.
pub fn default_client_factory() -> WeixinClientFactory {
    Arc::new(|creds: &SavedCredentials| {
        Arc::new(IlinkHttpClient::new(
            creds.base_url.clone(),
            creds.bot_token.clone(),
        ))
    })
}

/// WeixinChannel.ts `SavedCredentials`.
#[derive(Clone, Debug)]
pub struct SavedCredentials {
    pub base_url: String,
    pub bot_token: String,
    pub account_id: String,
    pub cursor: Option<String>,
}
