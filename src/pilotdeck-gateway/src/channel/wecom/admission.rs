//! Per-session exclusion mirrors WeComChannel.activeChats and releases on cancellation.
use pilotdeck_protocol::JsString;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub(crate) struct ActiveSessions(Arc<Mutex<HashSet<JsString>>>);
pub(crate) struct ActiveTurn {
    sessions: ActiveSessions,
    key: JsString,
}
impl ActiveSessions {
    pub(crate) fn try_enter(&self, key: JsString) -> Option<ActiveTurn> {
        let mut sessions = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !sessions.insert(key.clone()) {
            return None;
        }
        Some(ActiveTurn {
            sessions: self.clone(),
            key,
        })
    }
}
impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.sessions
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_turn_per_session_and_cancellation_releases_it() {
        let sessions = ActiveSessions::default();
        let active = sessions.try_enter("one".into()).unwrap();
        assert!(sessions.try_enter("one".into()).is_none());
        assert!(sessions.try_enter("two".into()).is_some());
        drop(active);
        assert!(sessions.try_enter("one".into()).is_some());
    }
}
