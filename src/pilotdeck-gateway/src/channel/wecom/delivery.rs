//! Route replies through the callback request or proactive direct-message API.
use super::{transport::ActiveTransport, wire};
use pilotdeck_protocol::{JsString, JsValue};
use std::time::Duration;

#[derive(Clone)]
pub(crate) enum ReplyTarget {
    Callback(JsString),
    Direct(JsString),
    Unavailable,
}
pub(crate) async fn reply(
    transport: &ActiveTransport,
    target: ReplyTarget,
    text: JsString,
) -> bool {
    let (request_id, frame) = match target {
        ReplyTarget::Callback(request_id) => {
            let frame = wire::response(request_id.clone(), text);
            (request_id, frame)
        }
        ReplyTarget::Direct(chat_id) => {
            let request_id: JsString =
                format!("aibot_send_msg-{}", uuid::Uuid::new_v4().simple()).into();
            let frame = wire::proactive_markdown(request_id.clone(), chat_id, text);
            (request_id, frame)
        }
        ReplyTarget::Unavailable => return false,
    };
    let Ok(response) = transport
        .request(request_id, frame, Duration::from_secs(15))
        .await
    else {
        return false;
    };
    matches!(wire::response_error(&response), Ok(None))
}
pub(crate) fn request_id(value: &JsValue) -> Option<JsString> {
    value
        .get("headers")
        .and_then(|v| v.get("req_id"))
        .and_then(JsValue::as_string)
        .map(|s| JsString::from_utf16(crate::runtime::memory::js_trim_units(s.units()).to_vec()))
        .filter(|s| !s.units().is_empty())
}

#[derive(Default)]
pub(crate) struct ReplyHistory {
    order: std::collections::VecDeque<String>,
    by_chat: std::collections::HashMap<String, JsString>,
}
impl ReplyHistory {
    pub(crate) fn target(
        &mut self,
        chat: &str,
        group: bool,
        incoming: Option<JsString>,
    ) -> ReplyTarget {
        if let Some(id) = incoming.filter(|s| !s.units().is_empty()) {
            if !self.by_chat.contains_key(chat) {
                self.order.push_back(chat.to_string());
            }
            self.by_chat.insert(chat.to_string(), id.clone());
            while self.by_chat.len() > 1000 {
                if let Some(key) = self.order.pop_front() {
                    self.by_chat.remove(&key);
                } else {
                    break;
                }
            }
            ReplyTarget::Callback(id)
        } else if let Some(id) = self.by_chat.get(chat) {
            ReplyTarget::Callback(id.clone())
        } else if group {
            ReplyTarget::Unavailable
        } else {
            ReplyTarget::Direct(chat.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_callback_id_uses_prior_reply_or_dm_proactive_only() {
        let mut history = ReplyHistory::default();
        assert!(matches!(
            history.target("room", true, None),
            ReplyTarget::Unavailable
        ));
        assert!(matches!(
            history.target("alice", false, None),
            ReplyTarget::Direct(_)
        ));
        assert!(matches!(
            history.target("room", true, Some("r".into())),
            ReplyTarget::Callback(_)
        ));
        match history.target("room", true, None) {
            ReplyTarget::Callback(id) => assert_eq!(id, JsString::from("r")),
            _ => panic!("prior callback"),
        }
    }
}
