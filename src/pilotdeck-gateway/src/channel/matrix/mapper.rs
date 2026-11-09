//! Fixed cc7c4e6 Matrix session selection, preserving UTF-16 and JS trim.
use crate::channel::sms::{concat_units, trim_units};
use pilotdeck_protocol::JsString;
use std::collections::HashMap;
#[derive(Default)]
pub struct MatrixSessionMapperState {
    pub active_by_chat_id: HashMap<JsString, JsString>,
}
pub struct MatrixSessionMapper {
    state: MatrixSessionMapperState,
    uuid: Box<dyn Fn() -> JsString + Send + Sync>,
}
pub struct MappedSession {
    pub session_key: JsString,
    pub command_new: bool,
    pub message: JsString,
}
impl Default for MatrixSessionMapper {
    fn default() -> Self {
        Self::new(
            MatrixSessionMapperState::default(),
            Box::new(|| uuid::Uuid::new_v4().to_string().into()),
        )
    }
}
impl MatrixSessionMapper {
    pub fn new(
        state: MatrixSessionMapperState,
        uuid: Box<dyn Fn() -> JsString + Send + Sync>,
    ) -> Self {
        Self { state, uuid }
    }
    pub fn resolve(&mut self, chat: &JsString, text: &JsString) -> MappedSession {
        let input = trim_units(text.units());
        let new = input == [47, 110, 101, 119] || input.starts_with(&[47, 110, 101, 119, 32]);
        let prefix: Vec<u16> = "matrix:chat=".encode_utf16().collect();
        let session_key = if new {
            let id = (self.uuid)();
            let key = concat_units(&[&prefix, chat.units(), &[58, 115, 95], id.units()]);
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
                    concat_units(&[
                        &prefix,
                        chat.units(),
                        &":general".encode_utf16().collect::<Vec<_>>(),
                    ])
                })
        };
        let message =
            JsString::from_utf16(trim_units(if new { &input[4..] } else { input }).to_vec());
        MappedSession {
            session_key,
            command_new: new,
            message,
        }
    }
    pub fn snapshot(&self) -> MatrixSessionMapperState {
        MatrixSessionMapperState {
            active_by_chat_id: self.state.active_by_chat_id.clone(),
        }
    }
}
