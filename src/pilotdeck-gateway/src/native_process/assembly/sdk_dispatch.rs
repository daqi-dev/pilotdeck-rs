//! Shared SDK config notification; snapshots callbacks before invoking them.
use crate::runtime::{
    refresh::{ConfigChangeNotice, SdkConfigChange},
    sdk_session::{SdkConfigChangePayload, SessionHandlerRegistry},
};
use std::sync::{Arc, Mutex};
pub(super) struct NativeSdkConfigDispatch(pub Arc<Mutex<SessionHandlerRegistry>>);
impl SdkConfigChange for NativeSdkConfigDispatch {
    fn dispatch_sdk_config_change(&self, notice: &ConfigChangeNotice) {
        let handlers = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .config_change_handlers();
        let payload = SdkConfigChangePayload {
            changed_paths: notice.changed_paths.clone(),
            change_classes: notice.change_classes.clone(),
        };
        for handler in handlers {
            handler(&payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn dispatch_uses_original_registry_and_never_invokes_under_its_lock() {
        let handlers = Arc::new(Mutex::new(SessionHandlerRegistry::new()));
        let unlocked = Arc::new(AtomicBool::new(false));
        let seen = unlocked.clone();
        let nested = handlers.clone();
        handlers.lock().unwrap().register_config_change(
            &"s".into(),
            Arc::new(move |payload| {
                seen.store(
                    nested.try_lock().is_ok()
                        && payload.changed_paths == vec!["agent.model".into()],
                    Ordering::SeqCst,
                );
            }),
        );
        NativeSdkConfigDispatch(handlers).dispatch_sdk_config_change(&ConfigChangeNotice {
            changed_paths: vec!["agent.model".into()],
            change_classes: vec!["next-request".into()],
        });
        assert!(unlocked.load(Ordering::SeqCst));
    }
}
