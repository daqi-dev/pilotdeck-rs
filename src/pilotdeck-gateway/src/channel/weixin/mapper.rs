//! Port of baseline `src/adapters/channel/weixin/WeixinSessionMapper.ts`
//! (chat → session-key mapping with `/new` session rotation and project
//! bindings). The state is the TS `WeixinSessionMapperState` object; property
//! order is insertion order, as in JS, so persisted JSON matches the TS byte
//! shape.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};

/// TS `randomUUID()` from node:crypto, injectable for deterministic tests.
pub type MapperUuid = Arc<dyn Fn() -> JsString + Send + Sync>;

pub fn random_uuid() -> JsString {
    JsString::from(uuid::Uuid::new_v4().to_string())
}

/// ``weixin:chat={chatId}:`` with the chat id's exact UTF-16 units.
fn chat_prefix(chat_id: &JsString) -> Vec<u16> {
    let mut units: Vec<u16> = "weixin:chat=".encode_utf16().collect();
    units.extend_from_slice(chat_id.units());
    units.push(u16::from(b':'));
    units
}

/// `weixin:chat={chatId}:{suffix}` (TS template literal, UTF-16 concatenation).
fn chat_key(chat_id: &JsString, suffix: &str) -> JsString {
    let mut units = chat_prefix(chat_id);
    units.extend(suffix.encode_utf16());
    JsString::from_utf16(units)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WeixinCommand {
    /// TS `command: "new"` — the only mapper command.
    New,
}

#[derive(Clone, Debug)]
pub struct WeixinResolveResult {
    pub session_key: JsString,
    pub project_key: Option<JsString>,
    pub command: Option<WeixinCommand>,
    pub message: JsString,
}

/// The TS `WeixinSessionMapperState` object, split into its two own fields.
#[derive(Default)]
struct MapperState {
    active_by_chat: JsObject,
    project_by_chat: JsObject,
}

pub struct WeixinSessionMapper {
    state: Mutex<MapperState>,
    uuid: MapperUuid,
}

impl WeixinSessionMapper {
    /// `new WeixinSessionMapper()` with fresh state.
    pub fn new() -> Self {
        Self::with_uuid(Arc::new(random_uuid))
    }

    /// Deterministic uuid source for tests and goldens.
    pub fn with_uuid(uuid: MapperUuid) -> Self {
        Self {
            state: Mutex::new(MapperState::default()),
            uuid,
        }
    }

    /// `new WeixinSessionMapper(saved)` — restores persisted state. The TS
    /// constructor backfills a missing `projectByChatId` with `{}`.
    pub fn from_saved(saved: &JsValue) -> Self {
        Self::from_saved_with_uuid(saved, Arc::new(random_uuid))
    }

    pub fn from_saved_with_uuid(saved: &JsValue, uuid: MapperUuid) -> Self {
        let state = MapperState {
            active_by_chat: saved
                .get("activeByChatId")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default(),
            project_by_chat: saved
                .get("projectByChatId")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default(),
        };
        Self {
            state: Mutex::new(state),
            uuid,
        }
    }

    pub fn resolve(&self, chat_id: &JsString, text: &JsString) -> WeixinResolveResult {
        let raw = String::from_utf16_lossy(text.units());
        let trimmed = crate::metadata::js_trim(&raw);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if trimmed == "/new" || trimmed.starts_with("/new ") {
            // `weixin:chat={chatId}:s_${uuid}` — a fresh uuid suffix.
            let mut units = chat_prefix(chat_id);
            units.extend_from_slice("s_".encode_utf16().collect::<Vec<_>>().as_slice());
            let fresh = (self.uuid)();
            units.extend_from_slice(fresh.units());
            let session_key = JsString::from_utf16(units);
            state.active_by_chat.insert(
                chat_id.clone(),
                JsValue::String(session_key.clone()),
            );
            return WeixinResolveResult {
                session_key,
                project_key: project_of(&state, chat_id),
                command: Some(WeixinCommand::New),
                // `trimmed.slice("/new".length).trim()`.
                message: JsString::from(crate::metadata::js_trim(&trimmed[4..])),
            };
        }
        WeixinResolveResult {
            session_key: state
                .active_by_chat
                .get_key(chat_id)
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| chat_key(chat_id, "general")),
            project_key: project_of(&state, chat_id),
            command: None,
            message: JsString::from(trimmed),
        }
    }

    pub fn bind_project(&self, chat_id: &JsString, project_key: &JsString) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .project_by_chat
            .insert(chat_id.clone(), JsValue::String(project_key.clone()));
    }

    pub fn get_project(&self, chat_id: &JsString) -> Option<JsString> {
        project_of(&self.state.lock().unwrap_or_else(|e| e.into_inner()), chat_id)
    }

    pub fn get_session(&self, chat_id: &JsString) -> JsString {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .active_by_chat
            .get_key(chat_id)
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| chat_key(chat_id, "general"))
    }

    /// `snapshot()` — only the two own fields, always both present, in the TS
    /// object-literal order.
    pub fn snapshot(&self) -> JsValue {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshot = JsObject::new();
        snapshot.insert(
            "activeByChatId".into(),
            JsValue::Object(state.active_by_chat.clone()),
        );
        snapshot.insert(
            "projectByChatId".into(),
            JsValue::Object(state.project_by_chat.clone()),
        );
        JsValue::Object(snapshot)
    }
}

impl Default for WeixinSessionMapper {
    fn default() -> Self {
        Self::new()
    }
}

fn project_of(state: &MapperState, chat_id: &JsString) -> Option<JsString> {
    state
        .project_by_chat
        .get_key(chat_id)
        .and_then(JsValue::as_string)
        .cloned()
}
