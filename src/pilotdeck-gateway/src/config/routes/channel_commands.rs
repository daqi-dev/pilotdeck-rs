//! `ChannelCommandRegistry.ts` system-level dispatch for native channel owners.
use crate::metadata::js_trim;
use crate::runtime::update::run_channel_update_with;
use pilotdeck_protocol::JsValue;

const UPDATE_ALIASES: &[&str] = &["升级", "更新"];

pub fn resolve_channel_command(text: &str) -> Option<(&'static str, String)> {
    let trimmed = js_trim(text);
    if !trimmed.starts_with('/') {
        return None;
    }
    let rest = &trimmed[1..];
    let (name, arg) = match rest.find(char::is_whitespace) {
        Some(at) => (&rest[..at], js_trim(&rest[at..]).to_owned()),
        None => (rest, String::new()),
    };
    let name = name.to_ascii_lowercase();
    if name == "update" || UPDATE_ALIASES.iter().any(|alias| alias == &name) {
        return Some(("update", arg));
    }
    None
}

/// Returns true when a system-level channel command was handled locally.
pub fn execute_channel_command(text: &str, reply: impl FnMut(&str)) -> bool {
    let Some(("update", arg)) = resolve_channel_command(text) else {
        return false;
    };
    crate::runtime::update::run_channel_update(&arg, reply);
    true
}

/// FeishuChannel.ts:487-500 — slash commands delegate to the shared registry.
pub fn dispatch_feishu_inbound_system_command(text: &str, reply: impl FnMut(&str)) -> bool {
    if !js_trim(text).starts_with('/') {
        return false;
    }
    execute_channel_command(text, reply)
}

/// WeComChannel.ts:526-544 — slash commands delegate to the shared registry.
pub fn dispatch_wecom_inbound_system_command(text: &str, reply: impl FnMut(&str)) -> bool {
    if !js_trim(text).starts_with('/') {
        return false;
    }
    execute_channel_command(text, reply)
}

pub fn execute_channel_command_with(
    text: &str,
    reply: impl FnMut(&str),
    run: impl FnOnce(&[String]) -> std::io::Result<crate::runtime::update::UpdateResult>,
) -> bool {
    let Some(("update", arg)) = resolve_channel_command(text) else {
        return false;
    };
    run_channel_update_with(&arg, reply, run);
    true
}

impl super::ConfigRoutes {
    /// Production entry for IM adapters (`executeChannelCommand` in TS).
    pub fn execute_inbound_channel_command(&self, text: &str, reply: impl FnMut(&str)) -> bool {
        let _ = self;
        execute_channel_command(text, reply)
    }

    /// Routes Feishu / WeCom / shared registry (`FeishuChannel.ts:487-500`, `WeComChannel.ts:526-544`).
    pub fn try_dispatch_channel_slash_command(
        &self,
        channel_key: &str,
        text: &str,
        reply: impl FnMut(&str),
    ) -> bool {
        let _ = self;
        match channel_key {
            "feishu" | "lark" => dispatch_feishu_inbound_system_command(text, reply),
            "wecom" | "work-weixin" | "workwechat" => {
                dispatch_wecom_inbound_system_command(text, reply)
            }
            _ => execute_channel_command(text, reply),
        }
    }

    pub(super) fn dispatch_im_inbound_slash_command(&self, params: &JsValue) -> pilotdeck_protocol::JsValue {
        use crate::metadata::text_field;
        use pilotdeck_protocol::JsString;
        let channel_key = text_field(params, "channelKey").unwrap_or_default();
        let message = text_field(params, "text").unwrap_or_default();
        let mut replies = Vec::<String>::new();
        let handled = self.try_dispatch_channel_slash_command(
            channel_key.as_str(),
            message.as_str(),
            |reply| replies.push(reply.to_owned()),
        );
        super::object([
            ("handled", pilotdeck_protocol::JsValue::Bool(handled)),
            (
                "replies",
                super::strings(replies.iter().map(|reply| JsString::from(reply.as_str()))),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::update::{UpdateResult, run_channel_update_with};

    #[test]
    fn resolves_update_aliases_and_arguments() {
        assert_eq!(
            resolve_channel_command("/update check"),
            Some(("update", "check".into()))
        );
        assert_eq!(
            resolve_channel_command("/升级"),
            Some(("update", String::new()))
        );
        assert!(resolve_channel_command("/help").is_none());
    }

    #[test]
    fn execute_channel_command_delegates_to_run_channel_update() {
        let mut via_execute = Vec::new();
        let mut via_run_channel = Vec::new();
        assert!(execute_channel_command_with(
            "/update check",
            |message| via_execute.push(message.to_owned()),
            |args| {
                assert_eq!(args, &["--check".to_string()]);
                Ok(UpdateResult {
                    code: 2,
                    output: "done".into(),
                })
            },
        ));
        run_channel_update_with(
            "check",
            |message| via_run_channel.push(message.to_owned()),
            |args| {
                assert_eq!(args, &["--check".to_string()]);
                Ok(UpdateResult {
                    code: 2,
                    output: "done".into(),
                })
            },
        );
        assert_eq!(via_execute, via_run_channel);
        assert!(!execute_channel_command("/help", |_| {}));
    }

    #[test]
    fn feishu_and_wecom_inbound_delegates_execute_channel_command() {
        use crate::config::routes::{feishu, wecom};
        assert!(!feishu::handle_inbound_text("hello", |_| {}));
        assert!(!feishu::handle_inbound_text("/help", |_| {}));
        assert!(!wecom::handle_inbound_text("/help", |_| {}));
    }

    #[test]
    fn inbound_channel_command_dispatches_shared_update_handler() {
        let mut replies = Vec::new();
        assert!(execute_channel_command_with(
            "/update check",
            |message| replies.push(message.to_owned()),
            |args| {
                assert_eq!(args, &["--check".to_string()]);
                Ok(UpdateResult {
                    code: 2,
                    output: "done".into(),
                })
            },
        ));
        assert_eq!(replies.last().unwrap(), "✅ 已是最新 Release，无需更新。");
    }
}
