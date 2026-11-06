//! `FeishuChannel.parseInbound` and the helpers it shares with the stream
//! path (baseline cc7c4e6): JSON parse, optional AES decrypt, the
//! `url_verification` handshake, the verify-token gate, the three inbound
//! shapes (direct / v2 / v1) and the text/group helpers.
//!
//! Everything here is a pure function of the body plus the channel's
//! `encryptKey` / `verifyToken`, so the golden fixture can pin it directly.
use pilotdeck_protocol::{JsValue, parse_json};

use super::crypto::decrypt_feishu_payload;

/// The TS `ParsedEvent` union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedEvent {
    UrlVerification { challenge: String },
    Message(InboundMessage),
    Ignore,
}

/// `FeishuInboundMessage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub chat_id: String,
    pub text: String,
    pub event_id: String,
    pub message_id: Option<String>,
    pub message_type: Option<String>,
    pub content: Option<String>,
}

/// `Date.now()` in milliseconds; direct/v1 shapes without an id use it in
/// their fallback event id, so tests inject a fixed clock.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> f64;
}

pub struct Parser<'a> {
    pub encrypt_key: Option<&'a str>,
    pub verify_token: Option<&'a str>,
    pub clock: &'a dyn Clock,
    /// TS `this.logger?.warn/error`; the messages are part of the parity
    /// record, so the sink receives the exact text.
    pub log: &'a dyn Fn(&str, &str),
}

impl Parser<'_> {
    /// `parseInbound(body)`.
    pub fn parse(&self, body: &str) -> ParsedEvent {
        let Ok(mut raw) = parse_json(body) else {
            return ParsedEvent::Ignore;
        };

        // A non-object JSON body has no `encrypt` own property in JS either,
        // so the decrypt gate simply does not fire. `null` would throw in TS
        // (`raw.encrypt` on null); the recorded divergence treats it as an
        // ignored event instead of an unhandled rejection.
        let encrypted = string_prop(&raw, "encrypt");
        if let (Some(encrypted), Some(key)) = (encrypted, self.encrypt_key) {
            match decrypt_feishu_payload(&encrypted, key)
                .ok()
                .and_then(|plain| parse_json(&plain).ok())
            {
                Some(decoded) => raw = decoded,
                None => {
                    (self.log)("error", "feishu: decrypt failed: Error: bad decrypt");
                    return ParsedEvent::Ignore;
                }
            }
        }

        if string_prop(&raw, "type").as_deref() == Some("url_verification")
            && let Some(challenge) = string_prop(&raw, "challenge")
        {
            // `if (this.verifyToken && raw.token !== this.verifyToken)`: a
            // non-string token never equals the configured string.
            if let Some(expected) = self.verify_token
                && string_prop(&raw, "token").as_deref() != Some(expected)
            {
                (self.log)("warn", "feishu: url_verification token mismatch");
            }
            return ParsedEvent::UrlVerification { challenge };
        }

        if let Some(expected) = self.verify_token {
            // `(raw.token as string | undefined) ?? raw.header?.token`: `??`
            // only falls back on null/undefined, so a `0` or `false` top-level
            // token shadows the header one and then fails the truthiness gate.
            let token = raw
                .get("token")
                .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
                .or_else(|| {
                    raw.get("header")
                        .and_then(JsValue::as_object)
                        .and_then(|header| header.get("token"))
                });
            // `if (token && token !== verifyToken)`: `token` is a JS truthiness
            // gate and the comparison is strict, so a non-string token (a
            // number, an object) never equals the configured string.
            if let Some(token) = token
                && js_truthy(token)
                && token
                    .as_string()
                    .map(|value| String::from_utf16_lossy(value.units()))
                    .as_deref()
                    != Some(expected)
            {
                (self.log)("warn", "feishu: verifyToken mismatch — ignoring event");
                return ParsedEvent::Ignore;
            }
        }

        if let Some(direct) = parse_direct_shape(&raw, self.clock) {
            return direct;
        }
        if let Some(v2) = parse_v2_event(&raw) {
            return v2;
        }
        if let Some(v1) = parse_v1_event(&raw, self.clock) {
            return v1;
        }
        ParsedEvent::Ignore
    }
}

fn string_prop(value: &JsValue, name: &str) -> Option<String> {
    string_prop_value(value.get(name))
}

fn string_prop_value(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .map(|text| String::from_utf16_lossy(text.units()))
}

/// JavaScript truthiness of a present property.
fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => false,
        JsValue::Bool(true) | JsValue::Object(_) | JsValue::Array(_) => true,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.is_empty(),
    }
}

/// `raw.<name> === undefined` — property absent or literally `undefined`.
fn is_undefined(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined))
}

/// `Date.now()` rendered like a JS number: integers lose the `.0`.
fn now_string(clock: &dyn Clock) -> String {
    crate::js_value::number_string(clock.now_ms())
}

fn parse_direct_shape(raw: &JsValue, clock: &dyn Clock) -> Option<ParsedEvent> {
    let chat_id = string_prop(raw, "chatId")?;
    let text = string_prop(raw, "text")?;
    let event_id = string_prop(raw, "eventId")
        .unwrap_or_else(|| format!("direct:{chat_id}:{}", now_string(clock)));
    Some(ParsedEvent::Message(InboundMessage {
        chat_id,
        text,
        event_id,
        message_id: None,
        message_type: Some("text".to_owned()),
        content: None,
    }))
}

fn parse_v2_event(raw: &JsValue) -> Option<ParsedEvent> {
    let header = raw.get("header").and_then(JsValue::as_object)?;
    let event = raw.get("event").and_then(JsValue::as_object)?;
    let message = event.get("message").and_then(JsValue::as_object)?;
    // `if (!header?.event_id || !event?.message) return undefined;`: the
    // empty string is falsy, so it takes the "not this shape" exit rather
    // than becoming an id.
    let event_id = match string_prop_value(header.get("event_id")) {
        Some(value) if !value.is_empty() => value,
        _ => return None,
    };
    if string_prop_value(header.get("event_type")).as_deref() != Some("im.message.receive_v1") {
        return Some(ParsedEvent::Ignore);
    }
    let message_type = string_prop_value(message.get("message_type"));
    if !is_supported_type(message_type.as_deref()) {
        return Some(ParsedEvent::Ignore);
    }
    // `if (!chatId || content === undefined) return undefined;`: the empty
    // string is falsy, so `"chat_id": ""` takes the "not this shape" exit
    // instead of becoming `feishu:chat=:general`.
    let chat_id = match string_prop_value(message.get("chat_id")) {
        Some(value) if !value.is_empty() => value,
        _ => return None,
    };
    let content_value = message.get("content");
    if is_undefined(content_value) {
        return None;
    }
    let content = string_prop_value(content_value);
    let text = extract_message_text(message_type.as_deref(), content.as_deref());
    Some(ParsedEvent::Message(InboundMessage {
        chat_id,
        text,
        event_id,
        message_id: string_prop_value(message.get("message_id")),
        message_type,
        content,
    }))
}

fn parse_v1_event(raw: &JsValue, clock: &dyn Clock) -> Option<ParsedEvent> {
    let event = raw.get("event").and_then(JsValue::as_object)?;
    // `if (!event?.chat_id || event.text === undefined) return undefined;`:
    // the empty string is falsy here too.
    let chat_id = match string_prop_value(event.get("chat_id")) {
        Some(value) if !value.is_empty() => value,
        _ => return None,
    };
    if is_undefined(event.get("text")) {
        return None;
    }
    let event_id = string_prop(raw, "uuid")
        .or_else(|| string_prop_value(event.get("uuid")))
        .unwrap_or_else(|| format!("v1:{chat_id}:{}", now_string(clock)));
    Some(ParsedEvent::Message(InboundMessage {
        chat_id,
        text: string_prop_value(event.get("text")).unwrap_or_default(),
        event_id,
        message_id: None,
        message_type: Some("text".to_owned()),
        content: None,
    }))
}

/// `isSupportedFeishuInboundType`.
pub fn is_supported_type(message_type: Option<&str>) -> bool {
    matches!(message_type, Some("text" | "image" | "file"))
}

/// `shouldBatchFeishuMessage`.
pub fn should_batch(message: &InboundMessage) -> bool {
    matches!(message.message_type.as_deref(), Some("image" | "file"))
}

/// `groupFeishuInboundMessages`: a run of attachment messages becomes one
/// group, every text message its own.
pub fn group_inbound(messages: &[InboundMessage]) -> Vec<Vec<InboundMessage>> {
    let mut groups = Vec::new();
    let mut attachment_group: Vec<InboundMessage> = Vec::new();
    for message in messages {
        if should_batch(message) {
            attachment_group.push(message.clone());
            continue;
        }
        if !attachment_group.is_empty() {
            groups.push(std::mem::take(&mut attachment_group));
        }
        groups.push(vec![message.clone()]);
    }
    if !attachment_group.is_empty() {
        groups.push(attachment_group);
    }
    groups
}

/// `extractFeishuMessageText`: only `text` messages carry text, and a payload
/// that is not valid JSON falls back to the raw content string.
pub fn extract_message_text(message_type: Option<&str>, content: Option<&str>) -> String {
    if message_type != Some("text") {
        return String::new();
    }
    let Some(content) = content else {
        return String::new();
    };
    match parse_json(content) {
        Ok(parsed) => string_prop(&parsed, "text").unwrap_or_default(),
        Err(_) => content.to_owned(),
    }
}

/// `mergeTextAndDiagnostics`.
pub fn merge_text_and_diagnostics(text: &str, diagnostics: &[String]) -> String {
    if diagnostics.is_empty() {
        return text.to_owned();
    }
    let mut parts = Vec::with_capacity(diagnostics.len() + 1);
    let trimmed = crate::metadata::js_trim(text);
    if !trimmed.is_empty() {
        parts.push(trimmed.to_owned());
    }
    parts.extend(diagnostics.iter().cloned());
    parts.join("\n\n")
}

/// `maskAppId`: `abcd…wxyz`, unchanged when the id is 8 units or shorter.
pub fn mask_app_id(id: &str) -> String {
    let units: Vec<u16> = id.encode_utf16().collect();
    if units.len() <= 8 {
        return id.to_owned();
    }
    let head = String::from_utf16_lossy(&units[..4]);
    let tail = String::from_utf16_lossy(&units[units.len() - 4..]);
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock(f64);
    impl Clock for FixedClock {
        fn now_ms(&self) -> f64 {
            self.0
        }
    }

    fn parse<'a>(body: &str, clock: &'a FixedClock, log: &'a dyn Fn(&str, &str)) -> ParsedEvent {
        Parser {
            encrypt_key: None,
            verify_token: None,
            clock,
            log,
        }
        .parse(body)
    }

    fn no_log() -> impl Fn(&str, &str) {
        |_, _| {}
    }

    #[test]
    fn direct_shape_carries_text_and_event_id() {
        let clock = FixedClock(1_700_000_000_000.0);
        let log = no_log();
        let parsed = parse(r#"{"chatId":"c1","text":"hi"}"#, &clock, &log);
        let ParsedEvent::Message(message) = parsed else {
            panic!("#1329 direct message");
        };
        assert_eq!(message.chat_id, "c1");
        assert_eq!(message.text, "hi");
        assert_eq!(message.event_id, "direct:c1:1700000000000");
        assert_eq!(message.message_type.as_deref(), Some("text"));
    }

    #[test]
    fn v2_shape_extracts_json_text_and_ignores_unsupported_types() {
        let clock = FixedClock(0.0);
        let log = no_log();
        let body = r#"{"header":{"event_id":"e1","event_type":"im.message.receive_v1"},"event":{"message":{"chat_id":"c","message_type":"text","message_id":"m","content":"{\"text\":\"hello\"}"}}}"#;
        let ParsedEvent::Message(message) = parse(body, &clock, &log) else {
            panic!("#1329 v2 message");
        };
        assert_eq!(message.text, "hello");
        assert_eq!(message.event_id, "e1");

        let sticker = r#"{"header":{"event_id":"e2","event_type":"im.message.receive_v1"},"event":{"message":{"chat_id":"c","message_type":"sticker","content":"{}"}}}"#;
        assert_eq!(parse(sticker, &clock, &log), ParsedEvent::Ignore);

        let other = r#"{"header":{"event_id":"e3","event_type":"im.chat.updated_v1"},"event":{"message":{"chat_id":"c","message_type":"text","content":"{}"}}}"#;
        assert_eq!(parse(other, &clock, &log), ParsedEvent::Ignore);
    }

    #[test]
    fn empty_chat_id_is_not_a_message_in_either_event_shape() {
        // FeishuChannel.ts:287 `if (!chatId || message.content === undefined)
        // return undefined;` and :1174 `if (!event?.chat_id || event.text ===
        // undefined) return undefined;`: the empty string is falsy, so neither
        // shape dispatches a turn for `feishu:chat=:general`.
        let clock = FixedClock(0.0);
        let log = no_log();
        let v2 = r#"{"header":{"event_id":"e1","event_type":"im.message.receive_v1"},"event":{"message":{"chat_id":"","message_type":"text","content":"{\"text\":\"hi\"}"}}}"#;
        assert_eq!(parse(v2, &clock, &log), ParsedEvent::Ignore);
        let v1 = r#"{"event":{"chat_id":"","text":"x"}}"#;
        assert_eq!(parse(v1, &clock, &log), ParsedEvent::Ignore);
        // The direct shape keeps the baseline `typeof raw.chatId === "string"`
        // gate, which does not exclude the empty string.
        let ParsedEvent::Message(message) = parse(r#"{"chatId":"","text":"hi"}"#, &clock, &log)
        else {
            panic!("#1329 direct empty chat id");
        };
        assert_eq!(message.chat_id, "");
    }

    #[test]
    fn v1_shape_uses_uuid_fallbacks() {
        let clock = FixedClock(42.0);
        let log = no_log();
        let ParsedEvent::Message(message) =
            parse(r#"{"event":{"chat_id":"c","text":"x"}}"#, &clock, &log)
        else {
            panic!("#1329 v1 message");
        };
        assert_eq!(message.event_id, "v1:c:42");
        let ParsedEvent::Message(with_uuid) = parse(
            r#"{"uuid":"u1","event":{"chat_id":"c","text":"x"}}"#,
            &clock,
            &log,
        ) else {
            panic!("#1329 v1 uuid");
        };
        assert_eq!(with_uuid.event_id, "u1");
    }

    #[test]
    fn url_verification_returns_challenge_even_when_token_mismatches() {
        let clock = FixedClock(0.0);
        let seen = std::sync::Mutex::new(Vec::new());
        let log = |level: &str, message: &str| {
            seen.lock()
                .expect("#1329 log mutex")
                .push(format!("{level}:{message}"));
        };
        let parser = Parser {
            encrypt_key: None,
            verify_token: Some("t"),
            clock: &clock,
            log: &log,
        };
        assert_eq!(
            parser.parse(r#"{"type":"url_verification","challenge":"c","token":"other"}"#),
            ParsedEvent::UrlVerification {
                challenge: "c".into()
            }
        );
        assert_eq!(
            seen.lock().expect("#1329 log mutex").as_slice(),
            ["warn:feishu: url_verification token mismatch"]
        );
        assert_eq!(
            parser.parse(r#"{"type":"url_verification","challenge":"c","token":"t"}"#),
            ParsedEvent::UrlVerification {
                challenge: "c".into()
            }
        );
    }

    #[test]
    fn verify_token_gate_reads_top_level_then_header() {
        let clock = FixedClock(0.0);
        let log = no_log();
        let parser = Parser {
            encrypt_key: None,
            verify_token: Some("t"),
            clock: &clock,
            log: &log,
        };
        assert_eq!(
            parser.parse(r#"{"chatId":"c","text":"x","token":"bad"}"#),
            ParsedEvent::Ignore
        );
        assert_eq!(
            parser.parse(
                r#"{"header":{"event_id":"e","token":"bad"},"event":{"message":{"chat_id":"c","message_type":"text","content":"{}"}}}"#
            ),
            ParsedEvent::Ignore
        );
        assert!(matches!(
            parser.parse(r#"{"chatId":"c","text":"x","token":"t"}"#),
            ParsedEvent::Message(_)
        ));
    }

    #[test]
    fn text_extraction_falls_back_to_raw_content() {
        assert_eq!(
            extract_message_text(Some("text"), Some(r#"{"text":"a"}"#)),
            "a"
        );
        assert_eq!(
            extract_message_text(Some("text"), Some("not json")),
            "not json"
        );
        assert_eq!(extract_message_text(Some("image"), Some("{}")), "");
        assert_eq!(extract_message_text(Some("text"), None), "");
    }

    #[test]
    fn grouping_batches_only_attachment_runs() {
        let text = |id: &str| InboundMessage {
            chat_id: "c".into(),
            text: id.into(),
            event_id: id.into(),
            message_id: None,
            message_type: Some("text".into()),
            content: None,
        };
        let image = |id: &str| InboundMessage {
            message_type: Some("image".into()),
            ..text(id)
        };
        let groups = group_inbound(&[text("a"), image("b"), image("c"), text("d"), image("e")]);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[1].len(), 2);
        assert_eq!(groups[3][0].event_id, "e");
    }

    #[test]
    fn mask_app_id_keeps_short_ids() {
        assert_eq!(mask_app_id("short"), "short");
        assert_eq!(mask_app_id("cli_1234567890"), "cli_…7890");
        assert_eq!(mask_app_id(""), "");
    }
}
