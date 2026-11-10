//! `src/adapters/channel/protocol/ImCronDelivery.ts` (baseline cc7c4e6).
use pilotdeck_protocol::JsString;

use super::protocol::{BoxFuture, ChannelKey};

/// The subset of the TS `CronResultDelivery` the delivery path reads.
#[derive(Clone, Debug)]
pub struct CronResultDelivery {
    pub task_id: JsString,
    pub run_id: JsString,
    pub session_key: JsString,
    pub channel_key: ChannelKey,
    pub origin_session_key: Option<JsString>,
    pub origin_channel_key: Option<ChannelKey>,
    pub text: JsString,
}

/// `parseChatIdFromSessionKey`: `${channelKey}:chat=` prefix, then the
/// anchored `^(.+):(general|s_[0-9a-fA-F-]{36})$` split. The JS `.` excludes
/// the four line terminators and, without `/u`, matches one UTF-16 unit, so
/// the prefix must be free of `\n`, `\r`, U+2028 and U+2029; greedy `.+`
/// takes the longest prefix, which is the last colon whose suffix matches.
pub fn parse_chat_id_from_session_key(
    session_key: Option<&JsString>,
    channel_key: &ChannelKey,
) -> Option<JsString> {
    let key = session_key?;
    let mut prefix = channel_key.as_js().units().to_vec();
    prefix.extend(":chat=".encode_utf16());
    let units = key.units();
    if !units.starts_with(&prefix) || units.len() <= prefix.len() {
        return None;
    }
    let general: Vec<u16> = "general".encode_utf16().collect();
    let suffix = &units[prefix.len()..];
    // Candidate split colons, right to left: longest `.+` first.
    let mut index = suffix.len();
    while let Some(colon) = suffix[..index]
        .iter()
        .rposition(|&unit| unit == u16::from(b':'))
    {
        let tail = &suffix[colon + 1..];
        let chat = &suffix[..colon];
        if (tail == general.as_slice() || is_session_suffix(tail))
            && !chat.is_empty()
            && !chat.iter().any(is_line_terminator)
        {
            return Some(JsString::from_utf16(chat.to_vec()));
        }
        index = colon;
    }
    None
}

/// `s_` + exactly 36 units of `[0-9a-fA-F-]` (the TS `{36}` counts the
/// charset class, so the whole suffix is `s_` plus 36 characters).
fn is_session_suffix(tail: &[u16]) -> bool {
    if crate::mutants::on("webhook-cron-suffix") {
        return false;
    }
    tail.len() == 38
        && tail.starts_with(&[u16::from(b's'), u16::from(b'_')])
        && tail[2..].iter().all(|&unit| {
            u8::try_from(unit).is_ok_and(|byte| byte == b'-' || byte.is_ascii_hexdigit())
        })
}

fn is_line_terminator(unit: &u16) -> bool {
    matches!(unit, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// `deliverChatCronResult`: origin-channel gate, chat-id resolution, then the
/// sender's boolean with `false` as the only failure value.
pub fn deliver_chat_cron_result<F>(
    delivery: &CronResultDelivery,
    channel_key: &ChannelKey,
    send_text: F,
) -> BoxFuture<bool>
where
    F: FnOnce(JsString, JsString) -> BoxFuture<bool> + Send + 'static,
{
    // TS: `if (delivery.originChannelKey && delivery.originChannelKey !== channelKey)`.
    // `""` is falsy in JS, so an empty origin channel means "not set" and the
    // delivery proceeds; only a non-empty foreign channel aborts it.
    // #1699: the mutant counts the empty string as a set foreign channel —
    // the cron `originChannelKey: ''` oracle cell records a delivered `true`
    // there, so it goes red instead of relying on the direct unit test alone.
    let origin_set = delivery.origin_channel_key.as_ref().filter(|origin| {
        crate::mutants::on("webhook-cron-origin-empty") || !origin.as_js().is_empty()
    });
    let chat_id = if origin_set.is_some_and(|origin| origin != channel_key) {
        None
    } else {
        parse_chat_id_from_session_key(
            delivery
                .origin_session_key
                .as_ref()
                .or(Some(&delivery.session_key)),
            channel_key,
        )
    };
    let Some(chat_id) = chat_id else {
        return Box::pin(async { false });
    };
    let text = delivery.text.clone();
    Box::pin(async move { send_text(chat_id, text).await })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> ChannelKey {
        ChannelKey::new(text)
    }
    fn js(text: &str) -> JsString {
        JsString::from(text)
    }

    #[test]
    fn parse_extracts_general_and_new_session_chat_ids() {
        let channel = key("webhook");
        assert_eq!(
            parse_chat_id_from_session_key(Some(&js("webhook:chat=a:b:general")), &channel),
            Some(js("a:b"))
        );
        assert_eq!(
            parse_chat_id_from_session_key(
                Some(&js("webhook:chat=x:s_0123456789abcdef0123456789abcdef0123")),
                &channel
            ),
            Some(js("x"))
        );
    }

    #[test]
    fn parse_rejects_missing_prefix_empty_and_malformed_suffixes() {
        let channel = key("webhook");
        assert_eq!(parse_chat_id_from_session_key(None, &channel), None);
        assert_eq!(
            parse_chat_id_from_session_key(Some(&js("telegram:chat=a:general")), &channel),
            None
        );
        assert_eq!(
            parse_chat_id_from_session_key(Some(&js("webhook:chat=:general")), &channel),
            None
        );
        assert_eq!(
            parse_chat_id_from_session_key(
                Some(&js("webhook:chat=x:s_0123456789abcdef0123456789abcdef012")),
                &channel
            ),
            None
        );
        // The greedy prefix would need `.+` to cross a line terminator.
        assert_eq!(
            parse_chat_id_from_session_key(Some(&js("webhook:chat=a\nb:general")), &channel),
            None
        );
        // A 36-char but non-hex tail leaves the shorter colon match invalid.
        assert_eq!(
            parse_chat_id_from_session_key(
                Some(&js("webhook:chat=x:s_0123456789abcdef0123456789abcdef012g")),
                &channel
            ),
            None
        );
    }

    #[tokio::test]
    async fn delivery_gates_on_origin_channel_and_resolves_chat() {
        let channel = key("webhook");
        let base = CronResultDelivery {
            task_id: js("t"),
            run_id: js("r"),
            session_key: js("webhook:chat=alpha:general"),
            channel_key: channel.clone(),
            origin_session_key: None,
            origin_channel_key: None,
            text: js("done"),
        };
        assert!(deliver_chat_cron_result(&base, &channel, |_, _| Box::pin(async { true })).await);
        // `false` from the sender is the only failure value.
        assert!(!deliver_chat_cron_result(&base, &channel, |_, _| Box::pin(async { false })).await);
        let foreign = CronResultDelivery {
            origin_channel_key: Some(key("telegram")),
            ..base.clone()
        };
        assert!(
            !deliver_chat_cron_result(&foreign, &channel, |_, _| Box::pin(async { true })).await
        );
        // #1681: TS gates on `originChannelKey && originChannelKey !== channelKey`;
        // the empty string is falsy, so it means "not set" and the delivery runs.
        let empty_origin = CronResultDelivery {
            origin_channel_key: Some(key("")),
            ..base.clone()
        };
        assert!(
            deliver_chat_cron_result(&empty_origin, &channel, |chat, _| {
                assert_eq!(chat, js("alpha"));
                Box::pin(async { true })
            })
            .await
        );
        let unparseable = CronResultDelivery {
            session_key: js("webhook:no-chat-marker"),
            ..base.clone()
        };
        assert!(
            !deliver_chat_cron_result(&unparseable, &channel, |_, _| Box::pin(async { true }))
                .await
        );
        // originSessionKey wins over sessionKey when present.
        let origin = CronResultDelivery {
            session_key: js("other"),
            origin_session_key: Some(js(
                "webhook:chat=beta:s_0123456789abcdef0123456789abcdef0123",
            )),
            ..base
        };
        assert!(
            deliver_chat_cron_result(&origin, &channel, |chat, _| {
                assert_eq!(chat, js("beta"));
                Box::pin(async { true })
            })
            .await
        );
    }
}
