use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

// ─────────────────────────────────────────────────────────── handler registry

/// `PilotDeckHookEvent` from the baseline's hook protocol. Variant order is
/// the single hand-maintained list; `as_str` reads the shared [`HOOK_EVENTS`]
/// table, and `tests::hook_event_enum_matches_shared_protocol_list` pins them
/// together so adding an event cannot silently drift.
///
/// [`HOOK_EVENTS`]: crate::hooks::protocol::HOOK_EVENTS
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PilotDeckHookEvent {
    PreToolUse = 0,
    PostToolUse = 1,
    PostToolUseFailure = 2,
    Notification = 3,
    UserPromptSubmit = 4,
    PreModelRequest = 5,
    SessionStart = 6,
    SessionEnd = 7,
    Stop = 8,
    StopFailure = 9,
    SubagentStart = 10,
    SubagentStop = 11,
    PreCompact = 12,
    PostCompact = 13,
    PermissionRequest = 14,
    PermissionDenied = 15,
    Setup = 16,
    ConfigChange = 17,
    InstructionsLoaded = 18,
    CwdChanged = 19,
    FileChanged = 20,
    WorktreeCreate = 21,
    WorktreeRemove = 22,
    Elicitation = 23,
    ElicitationResult = 24,
}

impl PilotDeckHookEvent {
    pub const ALL: [Self; crate::hooks::protocol::HOOK_EVENTS.len()] = [
        Self::PreToolUse,
        Self::PostToolUse,
        Self::PostToolUseFailure,
        Self::Notification,
        Self::UserPromptSubmit,
        Self::PreModelRequest,
        Self::SessionStart,
        Self::SessionEnd,
        Self::Stop,
        Self::StopFailure,
        Self::SubagentStart,
        Self::SubagentStop,
        Self::PreCompact,
        Self::PostCompact,
        Self::PermissionRequest,
        Self::PermissionDenied,
        Self::Setup,
        Self::ConfigChange,
        Self::InstructionsLoaded,
        Self::CwdChanged,
        Self::FileChanged,
        Self::WorktreeCreate,
        Self::WorktreeRemove,
        Self::Elicitation,
        Self::ElicitationResult,
    ];

    pub fn as_str(self) -> &'static str {
        crate::hooks::protocol::HOOK_EVENTS[self as usize]
    }
}

/// SDK config-change payload type (dispatch belongs to A6.7-pr6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SdkConfigChangePayload {
    pub changed_paths: Vec<String>,
    pub change_classes: Vec<String>,
}

pub type SdkConfigChangeHandler = Arc<dyn Fn(&SdkConfigChangePayload) + Send + Sync>;
pub type SessionHookHandler = Arc<dyn Fn(PilotDeckHookEvent, &JsValue) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerRegistration {
    session_key: JsString,
    id: u64,
}

struct ConfigChangeRegistration {
    id: u64,
    handler: SdkConfigChangeHandler,
}

struct SessionHookRegistration {
    id: u64,
    handler: SessionHookHandler,
}

/// Registration-only registry. The TS `dispatchSessionHook` and
/// `dispatchSdkConfigChange` loops are intentionally **not** implemented here;
/// A6.7-pr6 owns dispatch. Entries keep JS Map semantics: one registration per
/// session, later registration replaces it in place, and an old token cannot
/// remove a newer registration.
#[derive(Default)]
pub struct SessionHandlerRegistry {
    config_change: Vec<(JsString, ConfigChangeRegistration)>,
    session_hook: Vec<(JsString, SessionHookRegistration)>,
    next_id: u64,
}

impl SessionHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn register_config_change(
        &mut self,
        session_key: &JsString,
        handler: SdkConfigChangeHandler,
    ) -> HandlerRegistration {
        let id = self.take_id();
        let entry = ConfigChangeRegistration { id, handler };
        match self
            .config_change
            .iter_mut()
            .find(|(key, _)| key == session_key)
        {
            Some((_, current)) => *current = entry,
            None => self.config_change.push((session_key.clone(), entry)),
        }
        HandlerRegistration {
            session_key: session_key.clone(),
            id,
        }
    }

    pub fn register_session_hook(
        &mut self,
        session_key: &JsString,
        handler: SessionHookHandler,
    ) -> HandlerRegistration {
        let id = self.take_id();
        let entry = SessionHookRegistration { id, handler };
        match self
            .session_hook
            .iter_mut()
            .find(|(key, _)| key == session_key)
        {
            Some((_, current)) => *current = entry,
            None => self.session_hook.push((session_key.clone(), entry)),
        }
        HandlerRegistration {
            session_key: session_key.clone(),
            id,
        }
    }

    /// The TS unregister closure only deletes when the stored registration is
    /// the same object; a stale token is a no-op.
    pub fn unregister_config_change(&mut self, registration: &HandlerRegistration) {
        self.config_change.retain(|(key, current)| {
            !(key == &registration.session_key && current.id == registration.id)
        });
    }

    pub fn unregister_session_hook(&mut self, registration: &HandlerRegistration) {
        self.session_hook.retain(|(key, current)| {
            !(key == &registration.session_key && current.id == registration.id)
        });
    }

    pub fn has_config_change(&self, session_key: &JsString) -> bool {
        self.config_change.iter().any(|(key, _)| key == session_key)
    }

    pub fn has_session_hook(&self, session_key: &JsString) -> bool {
        self.session_hook.iter().any(|(key, _)| key == session_key)
    }

    /// The registered handler, exposed for A6.7-pr6 dispatch.
    pub fn config_change_handler(&self, session_key: &JsString) -> Option<&SdkConfigChangeHandler> {
        self.config_change
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entry)| &entry.handler)
    }

    /// The registered handler, exposed for A6.7-pr6 dispatch.
    pub fn session_hook_handler(&self, session_key: &JsString) -> Option<&SessionHookHandler> {
        self.session_hook
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entry)| &entry.handler)
    }

    pub fn config_change_sessions(&self) -> Vec<JsString> {
        self.config_change
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub fn session_hook_sessions(&self) -> Vec<JsString> {
        self.session_hook
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub fn clear_session(&mut self, session_key: &JsString) {
        self.config_change.retain(|(key, _)| key != session_key);
        self.session_hook.retain(|(key, _)| key != session_key);
    }

    pub fn clear(&mut self) {
        self.config_change.clear();
        self.session_hook.clear();
    }
}
