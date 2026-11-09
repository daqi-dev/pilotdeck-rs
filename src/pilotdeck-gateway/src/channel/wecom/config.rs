//! Constructor settings needed for establishing a WeCom websocket.
use super::policy::Policy;
use pilotdeck_protocol::JsValue;

#[derive(Clone)]
pub(crate) struct Config {
    pub bot_id: String,
    pub secret: String,
    pub websocket_url: String,
    pub dm_policy: Policy,
    pub group_policy: Policy,
    pub allow_from: Vec<String>,
    pub group_allow_from: Vec<String>,
    pub groups: JsValue,
    pub group_sessions_per_user: bool,
    pub text_batch_delay_ms: u64,
    pub text_batch_split_delay_ms: u64,
}
fn first<'a>(values: impl IntoIterator<Item = Option<&'a JsValue>>) -> Option<&'a JsValue> {
    values
        .into_iter()
        .flatten()
        .find(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
}
fn string(value: &JsValue) -> String {
    crate::js_value::to_string_units(value)
        .map(|s| String::from_utf16_lossy(&s))
        .unwrap_or_default()
}
fn setting<'a>(
    values: impl IntoIterator<Item = Option<&'a JsValue>>,
    env: &str,
    default: &str,
) -> String {
    first(values)
        .map(string)
        .unwrap_or_else(|| std::env::var(env).unwrap_or_else(|_| default.into()))
        .trim()
        .to_string()
}
fn list(value: Option<&JsValue>, env: Option<&str>) -> Vec<String> {
    match value {
        Some(JsValue::Array(items)) => items
            .iter()
            .map(string)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(JsValue::String(s)) => String::from_utf16_lossy(s.units())
            .split(",")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => env
            .and_then(|key| std::env::var(key).ok())
            .unwrap_or_default()
            .split(",")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}
fn duration(value: Option<&JsValue>, fallback: u64) -> u64 {
    let Some(value) = value else { return fallback };
    match value {
        JsValue::Number(n) if n.is_finite() && *n >= 0.0 => (*n).min(u64::MAX as f64) as u64,
        JsValue::Number(_) => fallback,
        _ => {
            let raw = string(value);
            let raw = raw.trim_start();
            let (sign, digits) = if let Some(rest) = raw.strip_prefix("-") {
                (-1, rest)
            } else {
                (1, raw.strip_prefix("+").unwrap_or(raw))
            };
            let digits: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
            match digits.parse::<u64>() {
                Ok(value) if sign > 0 || value == 0 => value,
                _ => fallback,
            }
        }
    }
}

impl Config {
    pub(crate) fn from_options(options: &JsValue) -> Self {
        let extra = options.get("extra");
        let get = |key| extra.and_then(|v| v.get(key));
        let bot_id = setting(
            [options.get("botKey"), get("bot_id"), get("botId")],
            "WECOM_BOT_ID",
            "",
        );
        let secret = setting([get("botSecret"), get("secret")], "WECOM_SECRET", "");
        let websocket_url = setting(
            [get("websocket_url"), get("websocketUrl")],
            "WECOM_WEBSOCKET_URL",
            "",
        );
        let dm = setting(
            [get("dm_policy"), get("dmPolicy")],
            "WECOM_DM_POLICY",
            "pairing",
        );
        let group = setting(
            [get("group_policy"), get("groupPolicy")],
            "WECOM_GROUP_POLICY",
            "pairing",
        );
        Self {
            bot_id,
            secret,
            websocket_url: if websocket_url.is_empty() {
                "wss://openws.work.weixin.qq.com".into()
            } else {
                websocket_url
            },
            dm_policy: Policy::parse(&dm),
            group_policy: Policy::parse(&group),
            allow_from: list(
                first([get("allow_from"), get("allowFrom")]),
                Some("WECOM_ALLOWED_USERS"),
            ),
            group_allow_from: list(
                first([get("group_allow_from"), get("groupAllowFrom")]),
                None,
            ),
            groups: get("groups")
                .filter(|v| v.as_object().is_some())
                .cloned()
                .unwrap_or_else(|| JsValue::Object(Default::default())),
            text_batch_delay_ms: duration(
                first([get("text_batch_delay_ms"), get("textBatchDelayMs")]),
                600,
            ),
            text_batch_split_delay_ms: duration(
                first([
                    get("text_batch_split_delay_ms"),
                    get("textBatchSplitDelayMs"),
                ]),
                2000,
            ),
            group_sessions_per_user: match get("group_sessions_per_user") {
                Some(JsValue::Bool(value)) => *value,
                _ => match get("groupSessionsPerUser") {
                    Some(JsValue::Bool(value)) => *value,
                    _ => true,
                },
            },
        }
    }
    pub(crate) fn group_sender_allow(&self, chat: &str) -> Vec<String> {
        let Some(groups) = self.groups.as_object() else {
            return Vec::new();
        };
        let value = groups
            .get(chat)
            .filter(|v| v.as_object().is_some())
            .or_else(|| {
                groups
                    .own_entries()
                    .into_iter()
                    .find(|(key, value)| {
                        String::from_utf16_lossy(key.units()).to_lowercase() == chat.to_lowercase()
                            && value.as_object().is_some()
                    })
                    .map(|(_, value)| value)
            })
            .or_else(|| groups.get("*").filter(|v| v.as_object().is_some()));
        list(
            value.and_then(|value| first([value.get("allow_from"), value.get("allowFrom")])),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn per_group_rules_match_case_insensitively_before_wildcard() {
        let options = parse_json(r#"{"extra":{"groups":{"ROOM":{"allow_from":["alice"]},"*":{"allowFrom":["bob"]}},"group_sessions_per_user":false}}"#).unwrap();
        let config = Config::from_options(&options);
        assert_eq!(config.group_sender_allow("room"), ["alice"]);
        assert_eq!(config.group_sender_allow("other"), ["bob"]);
        assert!(!config.group_sessions_per_user);
    }
    #[test]
    fn explicit_settings_use_ts_alias_precedence_and_trim() {
        let options = parse_json(r#"{"botKey":" bot ","extra":{"bot_id":"ignored","botSecret":" secret ","websocket_url":"ws://127.0.0.1:9","dm_policy":" ALLOWLIST ","allow_from":"wecom:user:alice, bob ","group_policy":"open","group_allow_from":["Room", "*"]}}"#).unwrap();
        let c = Config::from_options(&options);
        assert_eq!(c.bot_id, "bot");
        assert_eq!(c.secret, "secret");
        assert_eq!(c.websocket_url, "ws://127.0.0.1:9");
        assert_eq!(c.dm_policy, Policy::Allowlist);
        assert_eq!(c.group_policy, Policy::Open);
        assert_eq!(c.allow_from, ["wecom:user:alice", "bob"]);
        assert_eq!(c.group_allow_from, ["Room", "*"]);
    }
}
