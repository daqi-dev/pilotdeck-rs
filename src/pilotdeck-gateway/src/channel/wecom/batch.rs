//! Coalesce split text callbacks per interaction key, matching TS timers.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Batches {
    generation: u64,
    pending: HashMap<String, (u64, JsValue, JsString)>,
}
impl Batches {
    pub(crate) fn insert(&mut self, key: String, callback: JsValue, message: JsString) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let mut units = self
            .pending
            .remove(&key)
            .map(|(_, _, text)| text.units().to_vec())
            .unwrap_or_default();
        if !units.is_empty() {
            units.push('\n' as u16);
        }
        units.extend_from_slice(message.units());
        self.pending
            .insert(key, (generation, callback, JsString::from_utf16(units)));
        generation
    }
    pub(crate) fn take(&mut self, key: &str, generation: u64) -> Option<JsValue> {
        if self
            .pending
            .get(key)
            .is_none_or(|(current, _, _)| *current != generation)
        {
            return None;
        }
        let (_, callback, text) = self.pending.remove(key)?;
        let mut outer = callback.as_object()?.clone();
        let mut body = outer.get("body")?.as_object()?.clone();
        let mut content = JsObject::new();
        content.insert("content".into(), JsValue::String(text));
        body.insert("msgtype".into(), "text".into());
        body.insert("text".into(), JsValue::Object(content));
        outer.insert("body".into(), JsValue::Object(body));
        Some(JsValue::Object(outer))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newest_timer_flushes_joined_text_and_latest_reply_target() {
        let mut b = Batches::default();
        let a = pilotdeck_protocol::parse_json(r#"{"headers":{"req_id":"1"},"body":{}}"#).unwrap();
        let c = pilotdeck_protocol::parse_json(r#"{"headers":{"req_id":"2"},"body":{}}"#).unwrap();
        let old = b.insert("k".into(), a, "first".into());
        let latest = b.insert("k".into(), c, "second".into());
        assert!(b.take("k", old).is_none());
        let value = b.take("k", latest).unwrap();
        assert_eq!(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .and_then(JsValue::as_string),
            Some(&JsString::from("2"))
        );
        assert_eq!(
            value
                .get("body")
                .unwrap()
                .get("text")
                .unwrap()
                .get("content")
                .and_then(JsValue::as_string),
            Some(&JsString::from("first\nsecond"))
        );
    }
}
