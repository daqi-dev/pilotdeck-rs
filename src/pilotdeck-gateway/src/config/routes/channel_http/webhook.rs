use super::*;
use crate::config::channel_commands::ChannelOrderedEventSink;
use pilotdeck_protocol::{JsString, JsValue};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct WebhookOptions {
    pub host: String,
    pub port: u16,
    pub secret: String,
    pub routes: HashMap<String, Value>,
    pub rate_limit: usize,
    pub max_body_bytes: usize,
}
impl Default for WebhookOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8643,
            secret: String::new(),
            routes: Default::default(),
            rate_limit: 30,
            max_body_bytes: 1_048_576,
        }
    }
}
#[derive(Default)]
struct State {
    seen: HashMap<String, f64>,
    rates: HashMap<String, Vec<f64>>,
    deliveries: HashMap<String, (JsValue, f64)>,
}
/// TS logs webhook replies. Non-log delivery types are reported as unsupported;
/// the cc7c4e6 baseline does not perform network callbacks.
pub trait WebhookLogger: Send + Sync {
    fn info(&self, message: &str);
}
struct StderrLogger;
impl WebhookLogger for StderrLogger {
    fn info(&self, message: &str) {
        eprintln!("{message}");
    }
}
pub struct WebhookChannel {
    options: WebhookOptions,
    gateway: Arc<dyn ChannelGateway>,
    sessions: Sessions,
    state: Mutex<State>,
    active: Arc<Mutex<HashSet<String>>>,
    work: Arc<Work>,
    logger: Arc<dyn WebhookLogger>,
}
impl WebhookChannel {
    pub fn new(options: WebhookOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_logger(options, gateway, Arc::new(StderrLogger))
    }
    pub fn with_logger(
        options: WebhookOptions,
        gateway: Arc<dyn ChannelGateway>,
        logger: Arc<dyn WebhookLogger>,
    ) -> Arc<Self> {
        Arc::new(Self {
            options,
            gateway,
            logger,
            sessions: Default::default(),
            state: Default::default(),
            active: Default::default(),
            work: Default::default(),
        })
    }
    pub async fn start(self: &Arc<Self>) -> Result<HttpChannelHandle> {
        for (name, route) in &self.options.routes {
            if self.secret(route).is_empty() {
                return Err(RouteError::new(
                    "WEBHOOK_SECRET_REQUIRED",
                    format!(
                        "webhook: route '{name}' has no HMAC secret. Set 'secret' on the route or globally. For testing without auth, set secret to '__INSECURE_NO_AUTH__'."
                    ),
                ));
            }
        }
        listen(
            &self.options.host,
            self.options.port,
            self.clone(),
            self.work.clone(),
        )
        .await
    }
    fn secret<'a>(&'a self, route: &'a Value) -> &'a str {
        route["secret"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.options.secret)
    }
    fn error(&self, status: u16, event: &str, code: &str, message: &str, hint: &str) -> Reply {
        json_response(
            status,
            http_error("webhook", status, event, code, message, hint, "http"),
        )
    }
    async fn handle(self: &Arc<Self>, request: Request<Incoming>) -> Reply {
        let path = request.uri().path();
        if path == "/health" {
            return json_response(200, json!({"status":"ok","platform":"webhook"}));
        }
        let Some(name) = path
            .strip_prefix("/webhooks/")
            .filter(|s| !s.is_empty() && !s.contains('/'))
        else {
            return response(404, "Not Found", "text/plain");
        };
        if request.method() != "POST" {
            return response(404, "Not Found", "text/plain");
        }
        let name = name.to_owned();
        let Some(route) = self.options.routes.get(&name) else {
            return self.error(
                404,
                "webhook_unknown_route",
                "unknown_route",
                &format!("Unknown route: {name}"),
                "Check the webhook route name and retry.",
            );
        };
        let now = now_ms() as f64 / 1000.0;
        {
            let mut state = self.state.lock().unwrap();
            let rates = state.rates.entry(name.clone()).or_default();
            rates.retain(|t| *t > now - 60.0);
            if rates.len() >= self.options.rate_limit {
                return self.error(
                    429,
                    "webhook_rate_limited",
                    "rate_limit_exceeded",
                    "Rate limit exceeded",
                    "Wait for the webhook rate limit window to reset, then retry.",
                );
            }
            rates.push(now);
        }
        let signature = request
            .headers()
            .get("x-hub-signature-256")
            .or_else(|| request.headers().get("x-signature-256"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let delivery_id = request
            .headers()
            .get("x-delivery-id")
            .or_else(|| request.headers().get("x-github-delivery"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{name}-{now}"));
        let body_text = match read_body(request.into_body(), self.options.max_body_bytes).await {
            Ok(body) => body,
            Err(error) => {
                return self.error(
                    413,
                    "webhook_request_too_large",
                    "request_too_large",
                    &format!("Request too large or unreadable: {error}"),
                    "Reduce the webhook payload size and retry.",
                );
            }
        };
        let secret = self.secret(route);
        if secret != "__INSECURE_NO_AUTH__" && !verify_hmac(&body_text, secret, &signature) {
            return self.error(
                401,
                "webhook_invalid_signature",
                "invalid_signature",
                "Invalid signature",
                "Check the webhook signing secret and signature header.",
            );
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.seen.contains_key(&delivery_id) {
                return json_response(200, json!({"status":"duplicate","delivery_id":delivery_id}));
            }
            state.seen.insert(delivery_id.clone(), now);
            state.seen.retain(|_, time| *time >= now - 3600.0);
        }
        // Keep JSON property order and JavaScript null/undefined semantics for
        // payload-to-message conversion. Delivery metadata remains ordinary JSON.
        let ordered_body = pilotdeck_protocol::parse_json(&body_text).unwrap_or_else(|_| {
            let mut object = pilotdeck_protocol::JsObject::new();
            object.insert("raw".into(), body_text.as_str().into());
            pilotdeck_protocol::JsValue::Object(object)
        });
        let Some(input) = extract_text(&ordered_body).filter(|s| !s.is_empty()) else {
            return json_response(200, json!({"status":"ignored","reason":"no text content"}));
        };
        let chat = format!("webhook:{name}:{delivery_id}");
        let mut delivery = pilotdeck_protocol::JsObject::new();
        delivery.insert(
            "deliver".into(),
            pilotdeck_protocol::parse_json(
                &route
                    .get("deliver")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!("log"))
                    .to_string(),
            )
            .unwrap(),
        );
        delivery.insert("route".into(), name.as_str().into());
        if let Some(body) = ordered_body.as_object() {
            for (key, value) in body.entries() {
                delivery.insert(key.clone(), value.clone());
            }
        }
        let delivery = JsValue::Object(delivery);
        {
            let mut state = self.state.lock().unwrap();
            state.deliveries.insert(chat.clone(), (delivery, now));
            state
                .deliveries
                .retain(|_, (_, time)| *time >= now - 3600.0);
        }
        let owner = self.clone();
        self.work.spawn(async move {
            owner.incoming(&chat, &input).await;
        });
        json_response(200, json!({"status":"accepted","delivery_id":delivery_id}))
    }
    async fn incoming(&self, chat: &str, input: &JsString) {
        let Some(_active) = ActiveGuard::acquire(&self.active, chat) else {
            return;
        };
        let (session, message, new) = self.sessions.resolve_ordered("webhook", chat, input);
        if new && message.is_empty() {
            self.reply(chat, "已创建新会话。");
            return;
        }
        if message.is_empty() {
            return;
        }
        let sink = OrderedWebhookSink::default();
        let mut params = pilotdeck_protocol::JsObject::new();
        params.insert("sessionKey".into(), session.as_str().into());
        params.insert("channelKey".into(), "webhook".into());
        params.insert("message".into(), JsValue::String(message));
        let result = self
            .gateway
            .stream_turn_ordered(JsValue::Object(params), &sink)
            .await;
        let text = if result.is_err() {
            "⚠️ Failed to process this message. Please retry.".to_owned()
        } else {
            String::from_utf16_lossy(&sink.state.lock().unwrap().1)
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.reply(chat, text);
        }
    }
    fn reply(&self, chat: &str, text: &str) -> bool {
        let kind = self
            .state
            .lock()
            .unwrap()
            .deliveries
            .get(chat)
            .and_then(|(value, _)| value.get("deliver"))
            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
            .map(|value| pilotdeck_protocol::js_value::display(value).unwrap_or_default())
            .unwrap_or_else(|| "log".into());
        let limit = if kind == "log" { 200 } else { 100 };
        let text = String::from_utf16_lossy(&text.encode_utf16().take(limit).collect::<Vec<_>>());
        if kind == "log" {
            self.logger
                .info(&format!("webhook: response for {chat}: {text}"));
            true
        } else {
            self.logger.info(&format!(
                "webhook: deliver type '{kind}' for {chat}: {text}"
            ));
            false
        }
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use crate::config::routes::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "webhook") {
            return Ok(false);
        }
        let Some(session) = delivery["originSessionKey"]
            .as_str()
            .or_else(|| delivery["sessionKey"].as_str())
        else {
            return Ok(false);
        };
        let Some((chat, suffix)) = session
            .strip_prefix("webhook:chat=")
            .and_then(|rest| rest.rsplit_once(':'))
        else {
            return Ok(false);
        };
        if chat.is_empty()
            || chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            || !(suffix == "general"
                || suffix.strip_prefix("s_").is_some_and(|id| {
                    id.len() == 36 && id.bytes().all(|ch| ch.is_ascii_hexdigit() || ch == b'-')
                }))
        {
            return Ok(false);
        };
        Ok(self.reply(chat, delivery["text"].as_str().unwrap_or("")))
    }
}
impl Service for WebhookChannel {
    fn request(self: Arc<Self>, request: Request<Incoming>) -> RequestCall {
        Box::pin(async move { self.handle(request).await })
    }
}
#[derive(Default)]
struct OrderedWebhookSink {
    state: Mutex<(RenderState, Vec<u16>)>,
}
impl ChannelOrderedEventSink for OrderedWebhookSink {
    fn event_ordered(&self, event: JsValue) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if event
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("assistant_text_delta"))
            {
                if let Some(text) = event.get("text").and_then(JsValue::as_string) {
                    state.1.extend_from_slice(text.units());
                }
            } else {
                let fragment = ordered_status_fragment(&mut state.0, &event);
                if let Some(fragment) = fragment {
                    state.1.extend(fragment);
                }
            }
            Ok(())
        })
    }
}
// Preserve arbitrary JS string fields until the final logger boundary.
pub(super) fn ordered_status_fragment(
    state: &mut RenderState,
    event: &JsValue,
) -> Option<Vec<u16>> {
    let undef = JsValue::Undefined;
    let field = |name| event.get(name).unwrap_or(&undef);
    let detail = field("detail");
    let kind = field("type").as_string()?.to_utf8().ok()?;
    let text =
        |value: &JsValue| pilotdeck_protocol::js_value::to_string_units(value).unwrap_or_default();
    let mut output = Vec::new();
    let mut add = |s: &str| output.extend(s.encode_utf16());
    let visible = detail.get("visible") != Some(&JsValue::Bool(false));
    let failure = kind == "agent_status"
        && visible
        && detail
            .get("severity")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("error"));
    state.visible |= failure;
    if kind == "error" && state.visible {
        return None;
    }
    match kind.as_str() {
        "assistant_thinking_delta" => {}
        "tool_call_started" => {
            add("\n[");
            output.extend(text(field("name")));
            output.extend(" running]\n".encode_utf16());
        }
        "tool_call_finished" => {
            add("\n[");
            let name = match field("toolName") {
                JsValue::Null | JsValue::Undefined => field("toolCallId"),
                value => value,
            };
            output.extend(text(name));
            let ok = match field("ok") {
                JsValue::Null | JsValue::Undefined | JsValue::Bool(false) => false,
                JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
                JsValue::String(s) => !s.is_empty(),
                _ => true,
            };
            output.extend(if ok { " done]\n" } else { " failed]\n" }.encode_utf16());
        }
        "error" => {
            add("\nError: ");
            output.extend(text(field("message")));
            output.push(10);
        }
        "agent_status" => {
            let name = field("event")
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .unwrap_or_default();
            let known = matches!(
                name.as_str(),
                "model_empty_response_exhausted"
                    | "max_turns_reached"
                    | "max_output_recovery_exhausted"
                    | "model_request_failed"
                    | "tool_call_recovery_exhausted"
                    | "tool_error_loop"
                    | "lifecycle_blocked"
                    | "turn_failed"
                    | "turn_timeout"
                    | "gateway_submit_failed"
                    | "session_busy"
                    | "gateway_bridge_error"
                    | "gateway_stream_ended_without_completion"
                    | "channel_submit_failed"
                    | "subagent_failed"
                    | "content_filter_stop"
                    | "unknown_finish_reason"
            );
            if !visible || !(known || failure) {
                return None;
            }
            add("\n⚠️ ");
            let message = detail
                .get("message")
                .and_then(JsValue::as_string)
                .filter(|s| {
                    s.units().iter().any(|u| {
                        !char::from_u32(u32::from(*u))
                            .is_some_and(pilotdeck_protocol::js_value::js_space)
                    })
                });
            output.extend(message.map(|s| s.units().to_vec()).unwrap_or_else(|| {
                "Agent execution stopped before producing a complete response."
                    .encode_utf16()
                    .collect()
            }));
            output.push(10);
        }
        _ => return None,
    }
    Some(output)
}
fn extract_text(body: &JsValue) -> Option<JsString> {
    let undefined = JsValue::Undefined;
    let field = |key| body.get(key).unwrap_or(&undefined);
    let string =
        |value: &JsValue| pilotdeck_protocol::js_value::to_string_units(value).unwrap_or_default();
    let truthy = |value: &JsValue| match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    };
    let combine =
        |parts: Vec<Vec<u16>>| JsString::from_utf16(parts.into_iter().flatten().collect());
    let literal = |s: &str| s.encode_utf16().collect::<Vec<_>>();
    for key in ["text", "message", "content", "body"] {
        if let JsValue::String(value) = field(key) {
            return Some(value.clone());
        }
    }
    let action = field("action");
    let issue = field("issue");
    let pr = field("pull_request");
    let comment = field("comment");
    let property =
        |value: &JsValue, key: &str| value.get(key).cloned().unwrap_or(JsValue::Undefined);
    if truthy(action) && truthy(issue) && property(issue, "body").as_string().is_some() {
        return Some(combine(vec![
            literal("GitHub "),
            string(action),
            literal(": "),
            string(&property(issue, "title")),
            literal("\n"),
            string(&property(issue, "body")),
        ]));
    }
    if truthy(action) && truthy(pr) {
        let content = property(pr, "body");
        let content = if matches!(content, JsValue::Undefined | JsValue::Null) {
            vec![]
        } else {
            string(&content)
        };
        return Some(combine(vec![
            literal("GitHub PR "),
            string(action),
            literal(": "),
            string(&property(pr, "title")),
            literal("\n"),
            content,
        ]));
    }
    if property(comment, "body").as_string().is_some() {
        return Some(combine(vec![
            literal("Comment: "),
            string(&property(comment, "body")),
        ]));
    }
    let value = pilotdeck_protocol::stringify_json(body).ok().flatten()?;
    (value.encode_utf16().count() > 5 && value != "{}").then(|| value.into())
}
pub(super) fn hmac(body: &str, secret: &str) -> Vec<u8> {
    // RFC 2104 SHA-256, using the workspace's RustCrypto digest implementation.
    let mut key = [0u8; 64];
    if secret.len() > 64 {
        key[..32].copy_from_slice(&Sha256::digest(secret.as_bytes()));
    } else {
        key[..secret.len()].copy_from_slice(secret.as_bytes());
    }
    let mut inner = Sha256::new();
    inner.update(key.map(|byte| byte ^ 0x36));
    inner.update(body.as_bytes());
    let mut outer = Sha256::new();
    outer.update(key.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}
pub(super) fn verify_hmac(body: &str, secret: &str, signature: &str) -> bool {
    if signature.is_empty() {
        return false;
    }
    let signature = signature.strip_prefix("sha256=").unwrap_or(signature);
    // Buffer.from(hex) stops at the first invalid pair and discards an odd tail.
    let decoded = signature
        .as_bytes()
        .chunks_exact(2)
        .map_while(|pair| {
            Some((pair[0] as char).to_digit(16)? as u8 * 16 + (pair[1] as char).to_digit(16)? as u8)
        })
        .collect::<Vec<_>>();
    constant_equal(&decoded, &hmac(body, secret))
}
