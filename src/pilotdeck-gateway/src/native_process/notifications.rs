//! Late binding avoids a settings/automation -> host -> services ownership cycle.
use crate::runtime::{always_on::event_sink::TurnEventBroadcast, refresh::NotificationServer};
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, OnceLock, Weak};

#[derive(Default)]
pub(super) struct NativeNotifications {
    server: OnceLock<Weak<dyn NotificationServer>>,
}
impl NativeNotifications {
    pub(super) fn bind(&self, server: Arc<dyn NotificationServer>) {
        assert!(
            self.server.set(Arc::downgrade(&server)).is_ok(),
            "notification server already bound"
        );
    }
    pub(super) fn worktree_events(self: &Arc<Self>) -> crate::runtime::always_on::WorktreeObserver {
        let notifications = self.clone();
        Arc::new(move |name, payload| notifications.broadcast_notification(name, &payload))
    }
    pub(super) fn turn_events(self: &Arc<Self>) -> TurnEventBroadcast {
        let notifications = self.clone();
        Arc::new(move |session, channel, event| {
            notifications.broadcast_notification(
                "always-on:turn-event",
                &crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("channelKey", JsValue::String(channel.clone())),
                    ("event", event.clone()),
                ]),
            );
        })
    }
}
impl NotificationServer for NativeNotifications {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        if let Some(server) = self.server.get().and_then(Weak::upgrade) {
            server.broadcast_notification(name, payload);
        }
    }
}
