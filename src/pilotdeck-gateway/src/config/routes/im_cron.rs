//! Shared cron IM delivery helpers (`ImCronDelivery.ts:4-27`).
use serde_json::Value;

static CHAT_ID_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(.+):(general|s_[0-9a-fA-F-]{36})$").expect("valid chat id regex")
});

/// TS `delivery.originChannelKey ?? delivery.channelKey` (`pilotdeckServer.ts:305-306`).
pub fn cron_delivery_lookup_key(delivery: &Value) -> Option<&str> {
    match delivery.get("originChannelKey") {
        None | Some(Value::Null) => delivery["channelKey"].as_str(),
        Some(Value::String(key)) => Some(key.as_str()),
        _ => delivery["channelKey"].as_str(),
    }
}

/// Config / host adapter keys → protocol `channelKey` used in `deliver()`.
pub fn cron_protocol_channel_key(key: &str) -> &str {
    match key {
        "home-assistant" => "homeassistant",
        "wecom-callback" => "wecom_callback",
        _ => key,
    }
}

pub fn origin_channel_matches(delivery: &Value, channel: &str) -> bool {
    match delivery.get("originChannelKey") {
        None | Some(Value::Null) => true,
        Some(Value::String(key)) if key.is_empty() => false,
        Some(Value::String(key)) => key == channel,
        _ => false,
    }
}

pub(crate) fn session_field<'a>(delivery: &'a Value) -> Option<&'a str> {
    delivery
        .get("originSessionKey")
        .filter(|v| !v.is_null())
        .or_else(|| delivery.get("sessionKey"))
        .and_then(|v| v.as_str())
}

pub(crate) fn parse_chat_id<'a>(session: &'a str, channel: &'a str) -> Option<&'a str> {
    let prefix = format!("{channel}:chat=");
    let suffix = session.strip_prefix(&prefix)?;
    let captures = CHAT_ID_RE.captures(suffix)?;
    let chat = captures.get(1)?.as_str();
    if chat.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some(chat)
}

#[cfg(test)]
mod tests {
    use super::{
        cron_delivery_lookup_key, cron_protocol_channel_key, origin_channel_matches, parse_chat_id,
        session_field,
    };
    use serde_json::json;

    #[test]
    fn cron_delivery_lookup_key_matches_ts_nullish_coalescing() {
        assert_eq!(
            cron_delivery_lookup_key(&json!({"channelKey":"telegram"})),
            Some("telegram")
        );
        assert_eq!(
            cron_delivery_lookup_key(&json!({"originChannelKey":"","channelKey":"telegram"})),
            Some("")
        );
        assert_eq!(
            cron_delivery_lookup_key(&json!({"originChannelKey":null,"channelKey":"feishu"})),
            Some("feishu")
        );
    }

    #[test]
    fn cron_protocol_channel_key_maps_host_adapter_aliases() {
        assert_eq!(cron_protocol_channel_key("home-assistant"), "homeassistant");
        assert_eq!(cron_protocol_channel_key("wecom-callback"), "wecom_callback");
        assert_eq!(cron_protocol_channel_key("telegram"), "telegram");
    }

    #[test]
    fn parse_chat_id_matches_im_cron_delivery_ts_contract() {
        assert_eq!(
            parse_chat_id("telegram:chat=42:general", "telegram"),
            Some("42")
        );
        assert!(parse_chat_id("telegram:chat=42:s_not-a-uuid", "telegram").is_none());
    }

    #[test]
    fn parse_chat_id_handles_various_valid_and_invalid_chat_ids() {
        // Valid cases: general suffix, uuid suffix (lowercase/uppercase/mixed)
        assert_eq!(
            parse_chat_id("telegram:chat=42:general", "telegram"),
            Some("42")
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=42:s_12345678-1234-1234-1234-123456789abc",
                "telegram"
            ),
            Some("42")
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=user_abc:s_ABCDEF01-2345-6789-ABCD-EF0123456789",
                "telegram"
            ),
            Some("user_abc")
        );

        // Valid cases: special characters, colons in chat id, unicode/Chinese, spaces
        assert_eq!(
            parse_chat_id("feishu:chat=oc_123:sub_456:general", "feishu"),
            Some("oc_123:sub_456")
        );
        assert_eq!(
            parse_chat_id("slack:chat=C123-456.789_xyz:general", "slack"),
            Some("C123-456.789_xyz")
        );
        assert_eq!(
            parse_chat_id("wecom:chat=room name 123:general", "wecom"),
            Some("room name 123")
        );
        assert_eq!(
            parse_chat_id("weixin:chat=用户张三:general", "weixin"),
            Some("用户张三")
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=user@domain.com#chat:general",
                "telegram"
            ),
            Some("user@domain.com#chat")
        );

        // Invalid cases: channel prefix mismatch
        assert_eq!(parse_chat_id("discord:chat=42:general", "telegram"), None);
        assert_eq!(parse_chat_id("telegram:topic=42:general", "telegram"), None);
        assert_eq!(parse_chat_id("telegram:chat=", "telegram"), None);

        // Invalid cases: empty chat id
        assert_eq!(parse_chat_id("telegram:chat=:general", "telegram"), None);
        assert_eq!(
            parse_chat_id(
                "telegram:chat=:s_12345678-1234-1234-1234-123456789abc",
                "telegram"
            ),
            None
        );

        // Invalid cases: invalid suffixes
        assert_eq!(parse_chat_id("telegram:chat=42:other", "telegram"), None);
        assert_eq!(parse_chat_id("telegram:chat=42:s_short", "telegram"), None);
        assert_eq!(
            parse_chat_id(
                "telegram:chat=42:s_12345678-1234-1234-1234-123456789abcz",
                "telegram"
            ),
            None
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=42:s_12345678-1234-1234-1234-123456789abc-extra",
                "telegram"
            ),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=42:generall", "telegram"),
            None
        );
        assert_eq!(parse_chat_id("telegram:chat=42", "telegram"), None);
        assert_eq!(parse_chat_id("telegram:chat=42:", "telegram"), None);

        // Invalid cases: newline in chat id (TS regex single line match rejects \n)
        assert_eq!(
            parse_chat_id("telegram:chat=42\nsecond_line:general", "telegram"),
            None
        );
        assert_eq!(parse_chat_id("telegram:chat=\n:general", "telegram"), None);

        // Invalid cases: carriage return and JS line terminators
        assert_eq!(
            parse_chat_id("telegram:chat=42\rsecond_line:general", "telegram"),
            None
        );
        assert_eq!(parse_chat_id("telegram:chat=\r:general", "telegram"), None);
        assert_eq!(
            parse_chat_id("telegram:chat=42\u{2028}:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=42\u{2029}:general", "telegram"),
            None
        );
    }

    #[test]
    fn parse_chat_id_rejects_js_line_terminators() {
        // TS regex `.` in single-line mode rejects \r, \n, \u2028, \u2029
        assert_eq!(
            parse_chat_id("telegram:chat=42\r:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=42\rsecond_line:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=42\r\nsecond_line:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=\r:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=user\u{2028}name:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id("telegram:chat=user\u{2029}name:general", "telegram"),
            None
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=42\u{2028}:s_12345678-1234-1234-1234-123456789abc",
                "telegram"
            ),
            None
        );
        assert_eq!(
            parse_chat_id(
                "telegram:chat=42\u{2029}:s_12345678-1234-1234-1234-123456789abc",
                "telegram"
            ),
            None
        );
    }

    #[test]
    fn origin_channel_matches_ts_contract() {
        // Empty string behavior: empty originChannelKey matches no channel
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": ""}),
            "telegram"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": ""}),
            "weixin"
        ));

        // Missing / None originChannelKey behavior: does not reject
        assert!(origin_channel_matches(&json!({}), "telegram"));
        assert!(origin_channel_matches(
            &json!({"sessionKey": "foo"}),
            "telegram"
        ));
        assert!(origin_channel_matches(
            &json!({"channelKey": "feishu"}),
            "telegram"
        ));

        // Null originChannelKey behavior: does not reject
        assert!(origin_channel_matches(
            &json!({"originChannelKey": null}),
            "telegram"
        ));
        assert!(origin_channel_matches(
            &json!({"originChannelKey": null}),
            "weixin"
        ));

        // Matching channelKey: returns true
        assert!(origin_channel_matches(
            &json!({"originChannelKey": "telegram"}),
            "telegram"
        ));
        assert!(origin_channel_matches(
            &json!({"originChannelKey": "weixin"}),
            "weixin"
        ));

        // Mismatched channelKey: returns false
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": "telegram"}),
            "weixin"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": "discord"}),
            "telegram"
        ));

        // Non-string originChannelKey: returns false
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": 123}),
            "telegram"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": false}),
            "telegram"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": true}),
            "telegram"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": {}}),
            "telegram"
        ));
        assert!(!origin_channel_matches(
            &json!({"originChannelKey": []}),
            "telegram"
        ));
    }

    #[test]
    fn session_field_resolves_origin_session_priority() {
        assert_eq!(
            session_field(&json!({
                "originSessionKey": "orig:chat=1:general",
                "sessionKey": "fallback:chat=2:general"
            })),
            Some("orig:chat=1:general")
        );
        assert_eq!(
            session_field(&json!({
                "originSessionKey": null,
                "sessionKey": "fallback:chat=2:general"
            })),
            Some("fallback:chat=2:general")
        );
        assert_eq!(
            session_field(&json!({"sessionKey": "fallback:chat=2:general"})),
            Some("fallback:chat=2:general")
        );
        assert_eq!(session_field(&json!({})), None);
    }
}
