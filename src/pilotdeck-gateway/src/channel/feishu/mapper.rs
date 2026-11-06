//! `src/adapters/channel/feishu/FeishuSessionMapper.ts` (baseline cc7c4e6):
//! per-chat session/project bookkeeping for the feishu channel. The record
//! semantics are the JS ones every mapper in this component keeps — first
//! assignment fixes a chat's position in the snapshot.
use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// The three mapper commands TS emits. `/projects` and `/switch-project` are
/// delegated to the system command registry there; the mapper only shapes the
/// result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeishuCommand {
    New,
    Projects,
    SwitchProject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub session_key: String,
    pub project_key: Option<String>,
    pub command: Option<FeishuCommand>,
    pub command_arg: Option<String>,
    pub message: String,
}

/// `FeishuSessionMapperState`: both `Record<string, string>` fields, key order
/// included (a JS object keeps first-assignment order, and `snapshot()`
/// spreads it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeishuSessionMapperState {
    pub active_by_chat_id: Vec<(String, String)>,
    pub project_by_chat_id: Vec<(String, String)>,
}

pub type Uuid = Arc<dyn Fn() -> JsString + Send + Sync>;

pub struct FeishuSessionMapper {
    state: FeishuSessionMapperState,
    uuid: Uuid,
}

impl FeishuSessionMapper {
    pub fn new(uuid: Uuid) -> Self {
        Self {
            state: FeishuSessionMapperState::default(),
            uuid,
        }
    }

    /// `randomUUID`, the TS default constructor argument.
    pub fn random_uuid() -> Uuid {
        Arc::new(|| JsString::from(uuid::Uuid::new_v4().to_string()))
    }

    /// A mapper seeded with recorded state (the TS constructor's first
    /// argument; used by the replay fixture to pin a snapshot round-trip).
    pub fn with_state(state: FeishuSessionMapperState, uuid: Uuid) -> Self {
        Self { state, uuid }
    }

    /// The `ChannelStatePersistence.load` result handed to the constructor as
    /// `new FeishuSessionMapper(saved)`.
    pub fn from_saved(saved: &JsValue) -> Self {
        Self::from_saved_with_uuid(saved, Self::random_uuid())
    }

    pub fn from_saved_with_uuid(saved: &JsValue, uuid: Uuid) -> Self {
        Self {
            state: state_from_js(saved),
            uuid,
        }
    }

    /// `resolve({chatId, text})`.
    pub fn resolve(&mut self, chat_id: &str, text: &str) -> Resolved {
        let trimmed = crate::metadata::js_trim(text);
        let project_key = self.get_project(chat_id);

        if trimmed == "/new" || trimmed.starts_with("/new ") {
            let uuid = String::from_utf16_lossy((self.uuid)().units());
            let session_key = format!("feishu:chat={chat_id}:s_{uuid}");
            self.assign_active(chat_id, session_key.clone());
            return Resolved {
                session_key,
                project_key,
                command: Some(FeishuCommand::New),
                command_arg: None,
                message: crate::metadata::js_trim(&trimmed["/new".len()..]).to_owned(),
            };
        }

        if trimmed == "/projects" {
            return Resolved {
                session_key: self.current_session_key(chat_id),
                project_key,
                command: Some(FeishuCommand::Projects),
                command_arg: None,
                message: String::new(),
            };
        }

        if trimmed == "/switch-project" || trimmed.starts_with("/switch-project ") {
            let arg = crate::metadata::js_trim(&trimmed["/switch-project".len()..]).to_owned();
            return Resolved {
                session_key: self.current_session_key(chat_id),
                project_key,
                command: Some(FeishuCommand::SwitchProject),
                // `arg || undefined`: the empty string is falsy in JS.
                command_arg: (!arg.is_empty()).then_some(arg),
                message: String::new(),
            };
        }

        Resolved {
            session_key: self.current_session_key(chat_id),
            project_key,
            command: None,
            command_arg: None,
            message: trimmed.to_owned(),
        }
    }

    pub fn bind_project(&mut self, chat_id: &str, project_key: &str) {
        put(&mut self.state.project_by_chat_id, chat_id, project_key);
    }

    pub fn get_project(&self, chat_id: &str) -> Option<String> {
        get(&self.state.project_by_chat_id, chat_id)
    }

    /// `getSession(chatId)`: the current key, i.e. the general fallback when
    /// the chat never ran `/new`.
    pub fn get_session(&self, chat_id: &str) -> String {
        self.current_session_key(chat_id)
    }

    /// `snapshot()`: the copied records, key order included.
    pub fn snapshot(&self) -> FeishuSessionMapperState {
        self.state.clone()
    }

    /// `snapshot()` as the persisted JSON value
    /// (`{activeByChatId, projectByChatId}`).
    pub fn snapshot_js(&self) -> JsValue {
        state_to_js(&self.state)
    }

    fn current_session_key(&self, chat_id: &str) -> String {
        get(&self.state.active_by_chat_id, chat_id)
            .unwrap_or_else(|| format!("feishu:chat={chat_id}:general"))
    }

    fn assign_active(&mut self, chat_id: &str, session_key: String) {
        put(&mut self.state.active_by_chat_id, chat_id, &session_key);
    }
}

fn get(record: &[(String, String)], key: &str) -> Option<String> {
    record
        .iter()
        .find(|(entry, _)| entry == key)
        .map(|(_, value)| value.clone())
}

fn put(record: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(entry) = record.iter_mut().find(|(entry, _)| entry == key) {
        entry.1 = value.to_owned();
    } else {
        record.push((key.to_owned(), value.to_owned()));
    }
}

/// `Object.entries(record)` for one persisted record; a non-object value has
/// no entries, which is what `{ ...undefined }` would produce for the missing
/// field.
fn record_from_js(value: Option<&JsValue>) -> Vec<(String, String)> {
    let Some(object) = value.and_then(JsValue::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in object.entries() {
        let Some(text) = value
            .as_string()
            .map(|text| String::from_utf16_lossy(text.units()))
        else {
            continue;
        };
        out.push((String::from_utf16_lossy(key.units()), text));
    }
    out
}

fn state_from_js(saved: &JsValue) -> FeishuSessionMapperState {
    FeishuSessionMapperState {
        active_by_chat_id: record_from_js(saved.get("activeByChatId")),
        project_by_chat_id: record_from_js(saved.get("projectByChatId")),
    }
}

fn record_to_js(record: &[(String, String)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in record {
        object.insert(key.as_str().into(), JsValue::String(JsString::from(value.as_str())));
    }
    JsValue::Object(object)
}

fn state_to_js(state: &FeishuSessionMapperState) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "activeByChatId".into(),
        record_to_js(&state.active_by_chat_id),
    );
    object.insert(
        "projectByChatId".into(),
        record_to_js(&state.project_by_chat_id),
    );
    JsValue::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn fixed_uuid(seed: char) -> (Arc<Mutex<usize>>, Uuid) {
        let issued = Arc::new(Mutex::new(0usize));
        let counter = issued.clone();
        let uuid: Uuid = Arc::new(move || {
            let mut count = counter.lock().expect("#1329 uuid mutex");
            *count += 1;
            JsString::from(seed.to_string().repeat(36))
        });
        (issued, uuid)
    }

    #[test]
    fn new_assigns_and_reuses_session_keys() {
        let (issued, uuid) = fixed_uuid('a');
        let mut mapper = FeishuSessionMapper::new(uuid);
        let first = mapper.resolve("c1", "/new");
        assert_eq!(first.command, Some(FeishuCommand::New));
        assert_eq!(first.message, "");
        assert_eq!(
            first.session_key,
            format!("feishu:chat=c1:s_{}", "a".repeat(36))
        );
        let followup = mapper.resolve("c1", "hello");
        assert_eq!(followup.session_key, first.session_key);
        assert_eq!(followup.command, None);
        assert_eq!(followup.message, "hello");
        assert_eq!(
            mapper.resolve("c2", "hi").session_key,
            "feishu:chat=c2:general"
        );
        assert_eq!(*issued.lock().expect("#1329 uuid mutex"), 1);
    }

    #[test]
    fn new_trims_payload_once_and_keeps_project() {
        let (_, uuid) = fixed_uuid('b');
        let mut mapper = FeishuSessionMapper::new(uuid);
        mapper.bind_project("c", "proj");
        let with_payload = mapper.resolve("c", "  /new   hello world  ");
        assert_eq!(with_payload.command, Some(FeishuCommand::New));
        assert_eq!(with_payload.message, "hello world");
        assert_eq!(with_payload.project_key.as_deref(), Some("proj"));
        assert_eq!(mapper.resolve("c", "/newer").command, None);
    }

    #[test]
    fn projects_and_switch_project_shape() {
        let (_, uuid) = fixed_uuid('c');
        let mut mapper = FeishuSessionMapper::new(uuid);
        mapper.bind_project("c", "p1");
        let projects = mapper.resolve("c", "/projects");
        assert_eq!(projects.command, Some(FeishuCommand::Projects));
        assert_eq!(projects.message, "");
        assert_eq!(projects.project_key.as_deref(), Some("p1"));
        let switch = mapper.resolve("c", "/switch-project  p2 ");
        assert_eq!(switch.command, Some(FeishuCommand::SwitchProject));
        assert_eq!(switch.command_arg.as_deref(), Some("p2"));
        // `arg || undefined`: an empty argument is undefined, not "".
        let bare = mapper.resolve("c", "/switch-project");
        assert_eq!(bare.command_arg, None);
        assert_eq!(bare.command, Some(FeishuCommand::SwitchProject));
    }

    #[test]
    fn snapshot_copies_key_order() {
        let (_, uuid) = fixed_uuid('d');
        let mut mapper = FeishuSessionMapper::new(uuid);
        mapper.resolve("b", "/new");
        mapper.resolve("a", "/new");
        mapper.resolve("b", "/new");
        mapper.bind_project("b", "p");
        let snapshot = mapper.snapshot();
        assert_eq!(
            snapshot
                .active_by_chat_id
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(snapshot.project_by_chat_id, vec![("b".into(), "p".into())]);
    }

    #[test]
    fn js_trim_includes_bom() {
        let (_, uuid) = fixed_uuid('e');
        let mut mapper = FeishuSessionMapper::new(uuid);
        assert_eq!(
            mapper.resolve("c", "\u{feff}/new\u{feff}").command,
            Some(FeishuCommand::New)
        );
    }
}
