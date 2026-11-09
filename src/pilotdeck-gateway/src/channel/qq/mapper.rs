//! QQ group:user mapper; C2C deliberately bypasses /new in the channel.
use crate::channel::sms;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{collections::HashMap, sync::Arc};
#[derive(Clone, Default)]
pub struct QQSessionMapperState {
    pub active_by_chat_key: HashMap<JsString, JsString>,
}
impl QQSessionMapperState {
    pub fn to_value(&self) -> JsValue {
        let mut state = JsObject::new();
        for (k, v) in &self.active_by_chat_key {
            state.insert(k.clone(), JsValue::String(v.clone()));
        }
        super::object([("activeByChatKey", JsValue::Object(state))])
    }
    pub fn from_value(value: &JsValue) -> Self {
        let mut result = Self::default();
        if let Some(state) = value.get("activeByChatKey").and_then(JsValue::as_object) {
            for (k, v) in state.entries() {
                if let Some(value) = v.as_string() {
                    result.active_by_chat_key.insert(k.clone(), value.clone());
                }
            }
        }
        result
    }
}
pub struct QQSessionMapper {
    state: QQSessionMapperState,
    uuid: Arc<dyn Fn() -> JsString + Send + Sync>,
}
pub struct MappedSession {
    pub session_key: JsString,
    pub command_new: bool,
    pub message: JsString,
}
impl Default for QQSessionMapper {
    fn default() -> Self {
        Self::new(
            QQSessionMapperState::default(),
            Arc::new(|| uuid::Uuid::new_v4().to_string().into()),
        )
    }
}
impl QQSessionMapper {
    pub fn new(state: QQSessionMapperState, uuid: Arc<dyn Fn() -> JsString + Send + Sync>) -> Self {
        Self { state, uuid }
    }
    pub fn resolve(&mut self, group: &JsString, user: &JsString, text: &JsString) -> MappedSession {
        let key = sms::concat_units(&[group.units(), &[58], user.units()]);
        let trimmed = sms::trim_units(text.units());
        let new = trimmed == [47, 110, 101, 119] || trimmed.starts_with(&[47, 110, 101, 119, 32]);
        let prefix = sms::concat_units(&[
            &"qq:group=".encode_utf16().collect::<Vec<_>>(),
            group.units(),
            &":user=".encode_utf16().collect::<Vec<_>>(),
            user.units(),
        ]);
        let session_key = if new {
            let id = (self.uuid)();
            let value = sms::concat_units(&[
                prefix.units(),
                &":s_".encode_utf16().collect::<Vec<_>>(),
                id.units(),
            ]);
            self.state.active_by_chat_key.insert(key, value.clone());
            value
        } else {
            self.state
                .active_by_chat_key
                .get(&key)
                .cloned()
                .unwrap_or_else(|| {
                    sms::concat_units(&[
                        prefix.units(),
                        &":general".encode_utf16().collect::<Vec<_>>(),
                    ])
                })
        };
        MappedSession {
            session_key,
            command_new: new,
            message: JsString::from_utf16(
                sms::trim_units(if new { &trimmed[4..] } else { trimmed }).to_vec(),
            ),
        }
    }
    pub fn snapshot(&self) -> QQSessionMapperState {
        self.state.clone()
    }
}
