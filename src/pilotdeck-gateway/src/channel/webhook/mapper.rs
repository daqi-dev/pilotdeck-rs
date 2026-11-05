//! `src/adapters/channel/webhook/WebhookSessionMapper.ts` (baseline cc7c4e6).
use std::sync::Arc;

use pilotdeck_protocol::JsString;

/// The `/new` command, the only command the mapper emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebhookCommand {
    New,
}

pub struct MappedTurn {
    pub session_key: String,
    pub command: Option<WebhookCommand>,
    pub message: String,
}

/// `WebhookSessionMapperState.activeByChatId`: first assignment owns the key
/// order, like every JS record here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebhookSessionMapperState {
    active_by_chat_id: Vec<(String, String)>,
}

pub type Uuid = Arc<dyn Fn() -> JsString + Send + Sync>;

pub struct WebhookSessionMapper {
    state: WebhookSessionMapperState,
    uuid: Uuid,
}

impl WebhookSessionMapper {
    pub fn new(uuid: Uuid) -> Self {
        Self {
            state: WebhookSessionMapperState::default(),
            uuid,
        }
    }

    /// `randomUUID`, the TS default constructor argument.
    pub fn random_uuid() -> Uuid {
        Arc::new(|| JsString::from(uuid::Uuid::new_v4().to_string()))
    }

    /// `resolve({chatId, text})`.
    pub fn resolve(&mut self, chat_id: &str, text: &str) -> MappedTurn {
        let trimmed = crate::metadata::js_trim(text);
        if trimmed == "/new" || trimmed.starts_with("/new ") {
            let uuid = String::from_utf16_lossy((self.uuid)().units());
            let session_key = format!("webhook:chat={chat_id}:s_{uuid}");
            self.assign(chat_id, session_key.clone());
            return MappedTurn {
                session_key,
                command: Some(WebhookCommand::New),
                message: crate::metadata::js_trim(&trimmed["/new".len()..]).to_owned(),
            };
        }
        let session_key = self
            .state
            .active_by_chat_id
            .iter()
            .find(|(key, _)| key == chat_id)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| format!("webhook:chat={chat_id}:general"));
        MappedTurn {
            session_key,
            command: None,
            message: trimmed.to_owned(),
        }
    }

    /// `snapshot()`: the copied record, key order included.
    pub fn snapshot(&self) -> WebhookSessionMapperState {
        self.state.clone()
    }

    fn assign(&mut self, chat_id: &str, session_key: String) {
        if let Some(entry) = self
            .state
            .active_by_chat_id
            .iter_mut()
            .find(|(key, _)| key == chat_id)
        {
            entry.1 = session_key;
        } else {
            self.state
                .active_by_chat_id
                .push((chat_id.to_owned(), session_key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn fixed_uuid(seed: char) -> (Arc<Mutex<Vec<String>>>, Uuid) {
        let issued = Arc::new(Mutex::new(Vec::new()));
        let recorded = issued.clone();
        let uuid: Uuid = Arc::new(move || {
            let mut issued = recorded.lock().expect("#1334 uuid mutex");
            let value = seed.to_string().repeat(36);
            issued.push(value.clone());
            JsString::from(value)
        });
        (issued, uuid)
    }

    #[test]
    fn new_command_assigns_and_reuses_session_keys() {
        let (_, uuid) = fixed_uuid('a');
        let mut mapper = WebhookSessionMapper::new(uuid);
        let first = mapper.resolve("webhook:r:d1", "/new");
        assert_eq!(
            first.command,
            Some(WebhookCommand::New),
            "#1334 /new command"
        );
        assert_eq!(first.message, "", "#1334 /new without payload");
        assert_eq!(
            first.session_key,
            format!("webhook:chat=webhook:r:d1:s_{}", "a".repeat(36))
        );
        let followup = mapper.resolve("webhook:r:d1", "hello");
        assert_eq!(
            followup.session_key,
            format!("webhook:chat=webhook:r:d1:s_{}", "a".repeat(36))
        );
        assert_eq!(followup.command, None, "#1334 plain turn has no command");
        assert_eq!(followup.message, "hello");
        let fresh = mapper.resolve("webhook:r:d2", "hi");
        assert_eq!(
            fresh.session_key, "webhook:chat=webhook:r:d2:general",
            "#1334 unmapped chat falls back to general"
        );
    }

    #[test]
    fn new_command_trims_payload_once() {
        let (_, uuid) = fixed_uuid('b');
        let mut mapper = WebhookSessionMapper::new(uuid);
        let with_payload = mapper.resolve("c", "  /new   hello world  ");
        assert_eq!(with_payload.command, Some(WebhookCommand::New));
        assert_eq!(with_payload.message, "hello world");
        // Prefix match is on the trimmed text, and `/new` alone is exact.
        let spaced = mapper.resolve("c", "/new next");
        assert_eq!(spaced.message, "next");
        // A different chat: no prior session, so the general fallback applies.
        let almost = mapper.resolve("c2", "/newer");
        assert_eq!(almost.command, None, "#1334 only /new is a command");
        assert_eq!(almost.session_key, "webhook:chat=c2:general");
    }

    #[test]
    fn js_trim_includes_bom_and_line_terminators() {
        // The shared metadata::js_trim is the ECMAScript trim; a BOM-only
        // padded /new is still a command after it.
        let (_, uuid) = fixed_uuid('c');
        let mut mapper = WebhookSessionMapper::new(uuid);
        let bom = mapper.resolve("c", "\u{feff}/new\u{feff}");
        assert_eq!(bom.command, Some(WebhookCommand::New));
    }

    #[test]
    fn snapshot_copies_key_order() {
        let (issued, uuid) = fixed_uuid('d');
        let mut mapper = WebhookSessionMapper::new(uuid);
        mapper.resolve("b", "/new");
        mapper.resolve("a", "/new");
        mapper.resolve("b", "/new");
        assert_eq!(
            mapper.snapshot(),
            WebhookSessionMapperState {
                active_by_chat_id: vec![
                    ("b".into(), format!("webhook:chat=b:s_{}", "d".repeat(36))),
                    ("a".into(), format!("webhook:chat=a:s_{}", "d".repeat(36))),
                ]
            }
        );
        assert_eq!(issued.lock().expect("#1334 uuid mutex").len(), 3);
    }
}
