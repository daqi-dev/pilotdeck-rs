//! `WeComCallbackSessionMapper.ts` — the `/new` command and per-chat active
//! session key. Insertion order is object property order, so the snapshot
//! keeps a Vec rather than a map.

use std::sync::Arc;

pub(crate) type Uuid = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub session_key: String,
    pub is_new: bool,
    pub message: String,
}

pub(crate) struct SessionMapper {
    active_by_chat: Vec<(String, String)>,
    uuid: Uuid,
}

impl SessionMapper {
    pub(crate) fn new(uuid: Uuid) -> Self {
        Self {
            active_by_chat: Vec::new(),
            uuid,
        }
    }

    pub(crate) fn resolve(&mut self, chat_id: &str, text: &str) -> Resolved {
        let trimmed = crate::metadata::js_trim(text);
        if trimmed == "/new" || trimmed.starts_with("/new ") {
            let session_key = format!("wecom_callback:chat={chat_id}:s_{}", (self.uuid)());
            self.set(chat_id, session_key.clone());
            return Resolved {
                session_key,
                is_new: true,
                message: crate::metadata::js_trim(&trimmed["/new".len()..]).to_owned(),
            };
        }
        let session_key = self
            .get(chat_id)
            .unwrap_or_else(|| format!("wecom_callback:chat={chat_id}:general"));
        Resolved {
            session_key,
            is_new: false,
            message: trimmed.to_owned(),
        }
    }

    fn get(&self, chat_id: &str) -> Option<String> {
        self.active_by_chat
            .iter()
            .rev()
            .find(|(chat, _)| chat == chat_id)
            .map(|(_, key)| key.clone())
    }

    /// JavaScript property assignment: an existing key keeps its position.
    fn set(&mut self, chat_id: &str, session_key: String) {
        if let Some((_, key)) = self
            .active_by_chat
            .iter_mut()
            .find(|(chat, _)| chat == chat_id)
        {
            *key = session_key;
        } else {
            self.active_by_chat.push((chat_id.to_owned(), session_key));
        }
    }

    /// `snapshot()`. Reached from the parity tests and by callers that persist
    /// the mapper state; the channel itself only needs `resolve`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn snapshot(&self) -> Vec<(String, String)> {
        self.active_by_chat.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn mapper() -> SessionMapper {
        let counter = Arc::new(AtomicUsize::new(0));
        SessionMapper::new(Arc::new(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            format!("uuid-{n}")
        }))
    }

    #[test]
    fn new_command_rotates_and_keeps_message() {
        let mut mapper = mapper();
        let first = mapper.resolve("chat", "hello ");
        assert_eq!(first.session_key, "wecom_callback:chat=chat:general");
        assert_eq!(first.message, "hello");
        assert!(!first.is_new);

        let created = mapper.resolve("chat", " /new  a b ");
        assert_eq!(created.session_key, "wecom_callback:chat=chat:s_uuid-0");
        assert_eq!(created.message, "a b");
        assert!(created.is_new);

        let next = mapper.resolve("chat", "next");
        assert_eq!(next.session_key, "wecom_callback:chat=chat:s_uuid-0");
        assert_eq!(next.message, "next");
        assert!(!next.is_new);
        assert_eq!(
            mapper.snapshot(),
            vec![(
                "chat".to_owned(),
                "wecom_callback:chat=chat:s_uuid-0".to_owned()
            )]
        );
    }

    #[test]
    fn new_prefix_requires_a_space_and_trim_uses_js_whitespace() {
        let mut mapper = mapper();
        assert_eq!(
            mapper.resolve("c", "/newx").session_key,
            "wecom_callback:chat=c:general"
        );
        assert_eq!(mapper.resolve("c", "\u{feff}/new").message, "");
        assert!(mapper.resolve("c", "\u{feff}/new").is_new);
    }
}
