//! Session-scoped live delivery, matching InProcessGateway.emitEvent.
//! Idle sessions have no sink; this is neither a WS broadcast nor a replay log.
use crate::runtime::turn::coordinator::TurnEventCoordinator;
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, OnceLock, Weak};

pub(super) type SessionEmit = Arc<dyn Fn(JsValue) -> bool + Send + Sync>;

#[derive(Default)]
pub(super) struct NativeSessionEvents {
    coordinator: OnceLock<Weak<TurnEventCoordinator>>,
}
impl NativeSessionEvents {
    pub(super) fn install(&self, coordinator: &Arc<TurnEventCoordinator>) {
        assert!(self.coordinator.set(Arc::downgrade(coordinator)).is_ok());
    }

    pub(super) fn bind(
        self: &Arc<Self>,
        session: JsString,
        is_open: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> SessionEmit {
        let events = Arc::downgrade(self);
        Arc::new(move |event| {
            if !is_open() {
                return false;
            }
            events
                .upgrade()
                .and_then(|events| events.coordinator.get().and_then(Weak::upgrade))
                .is_some_and(|coordinator| coordinator.emit(&session, &event))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::object, runtime::turn::publish::TurnPublisher, storage::RunRef};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn session_delivery_tracks_current_run_but_obeys_idle_and_session_close() {
        let events = Arc::new(NativeSessionEvents::default());
        let coordinator = Arc::new(TurnEventCoordinator::new());
        events.install(&coordinator);
        let open = Arc::new(AtomicBool::new(true));
        let gate = open.clone();
        let emit = events.bind(
            "parent".into(),
            Arc::new(move || gate.load(Ordering::SeqCst)),
        );
        let event = object([("type", "agent_status".into())]);
        assert!(!emit(event.clone()));
        let run = |id: &str| RunRef {
            session_key: "parent".into(),
            run_id: id.into(),
            project_key: None,
        };
        let old = Arc::new(TurnPublisher::start(run("old"), None, coordinator.clone()));
        let turn_emit = old.permission_emitter();
        assert!(emit(event.clone()));
        assert_eq!(old.next().await.unwrap().get("runId"), Some(&"old".into()));
        old.finish().await;
        drop(old);
        assert!(!emit(event.clone()));
        let current = Arc::new(TurnPublisher::start(run("new"), None, coordinator.clone()));
        assert!(
            !turn_emit(event.clone()),
            "per-turn callbacks must stay revoked"
        );
        assert!(emit(event.clone()));
        assert_eq!(
            current.next().await.unwrap().get("runId"),
            Some(&"new".into())
        );
        open.store(false, Ordering::SeqCst);
        assert!(
            !emit(event.clone()),
            "closed session retained delivery authority"
        );
        current.finish().await;
        drop(current);
        drop(coordinator);
        assert!(!emit(event));
    }
}
