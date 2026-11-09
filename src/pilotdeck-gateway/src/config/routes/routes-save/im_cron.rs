//! Shared cron IM delivery helpers (`ImCronDelivery.ts:4-27`).
use serde_json::Value;

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
    let (chat, tail) = suffix.rsplit_once(':')?;
    if chat.is_empty() || chat.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    if tail == "general"
        || tail
            .strip_prefix("s_")
            .is_some_and(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
    {
        Some(chat)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{cron_delivery_lookup_key, cron_protocol_channel_key, parse_chat_id};
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
}
