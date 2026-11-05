//! `ApiServerSessionMapper`: `/new` creates a per-chat session key, otherwise
//! the chat keeps its previous key or falls back to the `:general` key.
use crate::metadata::js_trim;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub session_key: String,
    pub command_new: bool,
    pub message: String,
}

pub struct SessionMapper {
    active_by_chat_id: Mutex<HashMap<String, String>>,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Default for SessionMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionMapper {
    pub fn new() -> Self {
        Self::with_uuid(Arc::new(|| Uuid::new_v4().to_string()))
    }

    pub fn with_uuid(uuid: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        Self {
            active_by_chat_id: Mutex::new(HashMap::new()),
            uuid,
        }
    }

    pub fn resolve(&self, chat_id: &str, text: &str) -> Mapped {
        let trimmed = js_trim(text);
        if trimmed == "/new" || trimmed.starts_with("/new ") {
            let session_key = format!("api_server:chat={chat_id}:s_{}", (self.uuid)());
            self.active_by_chat_id
                .lock()
                .expect("api_server mapper state")
                .insert(chat_id.to_owned(), session_key.clone());
            return Mapped {
                session_key,
                command_new: true,
                message: js_trim(&trimmed[4..]).to_owned(),
            };
        }
        let session_key = self
            .active_by_chat_id
            .lock()
            .expect("api_server mapper state")
            .get(chat_id)
            .cloned()
            .unwrap_or_else(|| format!("api_server:chat={chat_id}:general"));
        Mapped {
            session_key,
            command_new: false,
            message: trimmed.to_owned(),
        }
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.active_by_chat_id
            .lock()
            .expect("api_server mapper state")
            .clone()
    }
}
