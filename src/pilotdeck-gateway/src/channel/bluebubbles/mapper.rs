//! `BlueBubblesSessionMapper.ts`: chat guid → `bluebubbles:chat=<id>:general`
//! or the `s_<uuid>` session created by the `/new` command. The
//! `activeByChatId` record persists across turns inside one channel instance,
//! exactly like the TS default-constructed mapper state.
use std::collections::HashMap;

use pilotdeck_protocol::JsString;

use super::sms::{concat_units, trim_units};

#[derive(Default)]
pub struct BlueBubblesSessionMapperState {
    active_by_chat_id: HashMap<JsString, JsString>,
}

pub struct BlueBubblesSessionMapper {
    state: BlueBubblesSessionMapperState,
    uuid: Box<dyn Fn() -> JsString + Send + Sync>,
}

pub struct MappedSession {
    pub session_key: JsString,
    /// `command: "new"` from the TS resolve result.
    pub command_new: bool,
    pub message: JsString,
}

impl Default for BlueBubblesSessionMapper {
    fn default() -> Self {
        // `randomUUID` (lowercase hyphenated v4) is the TS default.
        Self::new(
            BlueBubblesSessionMapperState::default(),
            Box::new(|| JsString::from(uuid::Uuid::new_v4().to_string())),
        )
    }
}

impl BlueBubblesSessionMapper {
    pub fn new(
        state: BlueBubblesSessionMapperState,
        uuid: Box<dyn Fn() -> JsString + Send + Sync>,
    ) -> Self {
        Self { state, uuid }
    }

    pub fn resolve(&mut self, chat_id: &JsString, text: &JsString) -> MappedSession {
        let trimmed = trim_units(text.units());
        let is_new = trimmed == "/new".encode_utf16().collect::<Vec<u16>>()
            || trimmed.starts_with("/new ".encode_utf16().collect::<Vec<u16>>().as_slice());
        if is_new {
            let session_key = concat_units(&[
                "bluebubbles:chat="
                    .encode_utf16()
                    .collect::<Vec<u16>>()
                    .as_slice(),
                chat_id.units(),
                ":s_".encode_utf16().collect::<Vec<u16>>().as_slice(),
                (self.uuid)().units(),
            ]);
            self.state
                .active_by_chat_id
                .insert(chat_id.clone(), session_key.clone());
            // `trimmed.slice("/new".length).trim()`
            let rest = trim_units(if trimmed.len() >= 4 {
                &trimmed[4..]
            } else {
                &[]
            });
            return MappedSession {
                session_key,
                command_new: true,
                message: JsString::from_utf16(rest.to_vec()),
            };
        }
        let session_key = self
            .state
            .active_by_chat_id
            .get(chat_id)
            .cloned()
            .unwrap_or_else(|| {
                concat_units(&[
                    "bluebubbles:chat="
                        .encode_utf16()
                        .collect::<Vec<u16>>()
                        .as_slice(),
                    chat_id.units(),
                    ":general".encode_utf16().collect::<Vec<u16>>().as_slice(),
                ])
            });
        MappedSession {
            session_key,
            command_new: false,
            message: JsString::from_utf16(trimmed.to_vec()),
        }
    }

    pub fn snapshot(&self) -> BlueBubblesSessionMapperState {
        BlueBubblesSessionMapperState {
            active_by_chat_id: self.state.active_by_chat_id.clone(),
        }
    }
}
