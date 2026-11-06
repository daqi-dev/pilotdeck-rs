//! #1864: the per-gateway session selection store the three selector
//! methods write and the next session composes from — the native
//! counterpart of the TS registry maps (`sessionOverrides`,
//! `sdkThinkingOverrides`, `sdkSessionConfigs.outputStyle`,
//! createLocalGateway.ts:622-744). Keyed by the session key itself
//! (`JsString` hashes its UTF-16 units, so a lone surrogate never folds
//! onto U+FFFD and shares a slot with a different key); presence is the
//! override, so a stored JS `null` thinking value is a value (TS deletes
//! only on `undefined`, ProjectRuntimeRegistry.ts:906-912).
use pilotdeck_protocol::{JsString, JsValue};
use std::collections::HashMap;

#[derive(Default)]
struct Inner {
    permission_modes: HashMap<JsString, JsString>,
    thinking: HashMap<JsString, JsValue>,
    output_styles: HashMap<JsString, JsString>,
}

/// The minimal equivalent registry #1839 allows: one shared instance per
/// gateway process (created in `assembly`, handed to both `Requests` and
/// the session factory's prepare closure).
#[derive(Default)]
pub struct SessionOverrides {
    inner: std::sync::Mutex<Inner>,
}

impl SessionOverrides {
    pub fn set_permission_mode(&self, key: &JsString, mode: JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permission_modes
            .insert(key.clone(), mode);
    }

    pub fn permission_mode(&self, key: &JsString) -> Option<JsString> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permission_modes
            .get(key)
            .cloned()
    }

    pub fn set_thinking(&self, key: &JsString, thinking: JsValue) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .insert(key.clone(), thinking);
    }

    pub fn clear_thinking(&self, key: &JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .remove(key);
    }

    pub fn thinking(&self, key: &JsString) -> Option<JsValue> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .get(key)
            .cloned()
    }

    pub fn set_output_style(&self, key: &JsString, name: JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .insert(key.clone(), name);
    }

    pub fn clear_output_style(&self, key: &JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .remove(key);
    }

    pub fn output_style(&self, key: &JsString) -> Option<JsString> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .get(key)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;

    /// #1864 fix-r1: the store keys on the raw `JsString`, so a session key
    /// carrying a lone surrogate and the key that holds the U+FFFD it would
    /// lossily fold onto keep separate slots (the pre-fix
    /// `String::from_utf16_lossy` keying made both land in one).
    #[test]
    fn lone_surrogate_keys_do_not_fold_onto_replacement_chars() {
        let store = SessionOverrides::default();
        let surrogate = JsString::from_utf16(vec![0xD800]);
        let replacement = JsString::from_utf16(vec![0xFFFD]);
        store.set_permission_mode(&surrogate, "plan".into());
        store.set_permission_mode(&replacement, "bypassPermissions".into());
        negative_assert_eq!(
            store.permission_mode(&surrogate).as_ref(),
            Some(&JsString::from("plan")),
            "#1864 the lone-surrogate key keeps its own slot"
        );
        negative_assert_eq!(
            store.permission_mode(&replacement).as_ref(),
            Some(&JsString::from("bypassPermissions")),
            "#1864 the replacement-character key keeps its own slot"
        );
    }
}
