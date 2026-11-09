use pilotdeck_protocol::JsString;
use std::{collections::HashMap, sync::Arc};
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct HomeAssistantSessionMapperState {
    pub active_by_chat_id: HashMap<JsString, JsString>,
}
pub struct MappedSession {
    pub session_key: JsString,
    pub new_session: bool,
    pub message: JsString,
}
pub struct HomeAssistantSessionMapper {
    state: HomeAssistantSessionMapperState,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
}
impl Default for HomeAssistantSessionMapper {
    fn default() -> Self {
        Self::new(
            Default::default(),
            Arc::new(|| uuid::Uuid::new_v4().to_string()),
        )
    }
}
impl HomeAssistantSessionMapper {
    pub fn new(
        state: HomeAssistantSessionMapperState,
        uuid: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self { state, uuid }
    }
    pub fn snapshot(&self) -> HomeAssistantSessionMapperState {
        self.state.clone()
    }
    pub fn resolve(&mut self, chat: &JsString, text: &JsString) -> MappedSession {
        let trimmed = super::super::sms::trim_units(text.units());
        let command: Vec<u16> = "/new".encode_utf16().collect();
        let new_session =
            trimmed == command || (trimmed.starts_with(&command) && trimmed.get(4) == Some(&32));
        let session_key = if new_session {
            let mut units: Vec<u16> = "homeassistant:chat=".encode_utf16().collect();
            units.extend_from_slice(chat.units());
            units.extend(format!(":s_{}", (self.uuid)()).encode_utf16());
            let key = JsString::from_utf16(units);
            self.state
                .active_by_chat_id
                .insert(chat.clone(), key.clone());
            key
        } else {
            self.state
                .active_by_chat_id
                .get(chat)
                .cloned()
                .unwrap_or_else(|| {
                    let mut units: Vec<u16> = "homeassistant:chat=".encode_utf16().collect();
                    units.extend_from_slice(chat.units());
                    units.extend(":general".encode_utf16());
                    JsString::from_utf16(units)
                })
        };
        let message = if new_session {
            super::super::sms::trim_units(&trimmed[4..])
        } else {
            trimmed
        };
        MappedSession {
            session_key,
            new_session,
            message: JsString::from_utf16(message.to_vec()),
        }
    }
}
