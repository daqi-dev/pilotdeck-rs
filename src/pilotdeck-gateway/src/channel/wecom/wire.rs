//! Request framing and reply correlation for WeCom's websocket protocol.
//! This module has no socket, session owner or ambient credentials.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::VecDeque;

const DEDUP_TTL_MS: u64 = 5 * 60 * 1000;
const DEDUP_MAX_SIZE: usize = 1000;
fn object(values: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (key, value) in values {
        o.insert(key.into(), value);
    }
    JsValue::Object(o)
}
fn header(req_id: JsString) -> JsValue {
    object([("req_id", JsValue::String(req_id))])
}
pub(crate) fn subscribe(
    req_id: JsString,
    bot: JsString,
    secret: JsString,
    device: JsString,
) -> JsValue {
    object([
        ("cmd", "aibot_subscribe".into()),
        ("headers", header(req_id)),
        (
            "body",
            object([
                ("bot_id", JsValue::String(bot)),
                ("secret", JsValue::String(secret)),
                ("device_id", JsValue::String(device)),
            ]),
        ),
    ])
}
pub(crate) fn response(req_id: JsString, text: JsString) -> JsValue {
    // JS String.slice(0, 4000) counts UTF-16 units, including split surrogates.
    let units = text.units();
    let text = JsString::from_utf16(units[..units.len().min(4000)].to_vec());
    object([
        ("cmd", "aibot_respond_msg".into()),
        ("headers", header(req_id)),
        (
            "body",
            object([
                ("msgtype", "markdown".into()),
                ("markdown", object([("content", JsValue::String(text))])),
            ]),
        ),
    ])
}
/// A proactive markdown message is valid for direct chats only; the caller
/// must enforce the chat-type restriction before constructing this frame.
pub(crate) fn proactive_markdown(req_id: JsString, chat_id: JsString, text: JsString) -> JsValue {
    let units = text.units();
    let text = JsString::from_utf16(units[..units.len().min(4000)].to_vec());
    object([
        ("cmd", "aibot_send_msg".into()),
        ("headers", header(req_id)),
        (
            "body",
            object([
                ("chatid", JsValue::String(chat_id)),
                ("msgtype", "markdown".into()),
                ("markdown", object([("content", JsValue::String(text))])),
            ]),
        ),
    ])
}

pub(crate) enum Frame<'a> {
    Callback,
    Response {
        request_id: JsString,
        payload: &'a JsValue,
    },
    Ignored,
}
pub(crate) fn classify(payload: &JsValue) -> Result<Frame<'_>, &'static str> {
    let cmd = payload.get("cmd").and_then(JsValue::as_string);
    if cmd.is_some_and(|s| s.equals_str("aibot_msg_callback") || s.equals_str("aibot_callback")) {
        return Ok(Frame::Callback);
    }
    if cmd.is_some_and(|s| s.equals_str("aibot_event_callback")) {
        return Ok(Frame::Ignored);
    }
    let missing = JsValue::Undefined;
    let id = payload
        .get("headers")
        .and_then(|h| h.get("req_id"))
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or(&missing);
    let units = crate::js_value::to_string_units_checked(id).map_err(|_| "invalid request id")?;
    if units.is_empty() || id.is_undefined() {
        return Ok(Frame::Ignored);
    }
    Ok(Frame::Response {
        request_id: JsString::from_utf16(units),
        payload,
    })
}
#[derive(Default)]
pub(crate) struct Dedupe {
    seen: VecDeque<(JsString, u64)>,
}
impl Dedupe {
    pub(crate) fn duplicate(&mut self, id: JsString, now_ms: u64) -> bool {
        self.seen
            .retain(|(_, then)| now_ms.saturating_sub(*then) <= DEDUP_TTL_MS);
        if self.seen.iter().any(|(seen, _)| seen == &id) {
            return true;
        }
        self.seen.push_back((id, now_ms));
        while self.seen.len() > DEDUP_MAX_SIZE {
            self.seen.pop_front();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn subscribe_response_and_callback_match_ts_wire() {
        let fixture = parse_json(include_str!("fixtures/wire.json")).unwrap();
        let JsValue::Array(sent) = fixture.get("sent").unwrap() else {
            panic!("sent")
        };
        let id = "00000000000040008000000000000001";
        assert_eq!(
            subscribe(
                format!("subscribe-{id}").into(),
                "bot-fixture".into(),
                "secret-fixture".into(),
                id.into()
            ),
            sent[0]
        );
        assert_eq!(response("r-1".into(), "answer".into()), sent[1]);
        assert!(matches!(
            classify(fixture.get("callback").unwrap()).unwrap(),
            Frame::Callback
        ));
        let ack = parse_json(r#"{"headers":{"req_id":"r-1"},"body":{"errcode":0}}"#).unwrap();
        match classify(&ack).unwrap() {
            Frame::Response {
                request_id,
                payload,
            } => {
                assert_eq!(request_id, JsString::from("r-1"));
                assert_eq!(payload, &ack);
            }
            _ => panic!("response"),
        }
    }
    #[test]
    fn duplicate_expires_after_boundary_and_evicts_oldest() {
        let mut d = Dedupe::default();
        assert!(!d.duplicate("a".into(), 0));
        assert!(d.duplicate("a".into(), DEDUP_TTL_MS));
        assert!(!d.duplicate("a".into(), DEDUP_TTL_MS + 1));
        let mut d = Dedupe::default();
        for i in 0..1001 {
            assert!(!d.duplicate(i.to_string().into(), 0));
        }
        assert!(!d.duplicate("0".into(), 0));
    }
}

pub(crate) fn ping(req_id: JsString) -> JsValue {
    object([
        ("cmd", "ping".into()),
        ("headers", header(req_id)),
        ("body", object([])),
    ])
}

/// Interpret both documented response envelopes, preserving a nonzero error.
pub(crate) fn response_error(payload: &JsValue) -> Result<Option<String>, String> {
    let non_null = |v: &&JsValue| !matches!(v, JsValue::Null | JsValue::Undefined);
    let code = payload
        .get("body")
        .and_then(|body| body.get("errcode"))
        .filter(non_null)
        .or_else(|| payload.get("errcode"))
        .filter(non_null);
    let Some(code) = code else { return Ok(None) };
    if *code == JsValue::Number(0.0) {
        return Ok(None);
    }
    let message = payload
        .get("body")
        .and_then(|body| body.get("errmsg"))
        .filter(non_null)
        .or_else(|| payload.get("errmsg"))
        .filter(non_null);
    let code = crate::js_value::to_string_units_checked(code)
        .map_err(|e| format!("{}: {}", e.name(), e.message()))?;
    let message = match message {
        Some(v) => crate::js_value::to_string_units_checked(v)
            .map_err(|e| format!("{}: {}", e.name(), e.message()))?,
        None => "error".encode_utf16().collect(),
    };
    Ok(Some(format!(
        "WeCom errcode {}: {}",
        String::from_utf16_lossy(&code),
        String::from_utf16_lossy(&message)
    )))
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    #[test]
    fn response_errors_follow_the_two_platform_envelopes() {
        for (source, expected) in [
            (r#"{"body":{"errcode":0}}"#, None),
            (
                r#"{"body":{"errcode":7,"errmsg":"bad"},"errcode":8}"#,
                Some("WeCom errcode 7: bad"),
            ),
            (
                r#"{"body":{"errcode":null},"errcode":"0"}"#,
                Some("WeCom errcode 0: error"),
            ),
            (r#"{}"#, None),
        ] {
            assert_eq!(
                response_error(&parse_json(source).unwrap())
                    .unwrap()
                    .as_deref(),
                expected
            );
        }
    }
}
