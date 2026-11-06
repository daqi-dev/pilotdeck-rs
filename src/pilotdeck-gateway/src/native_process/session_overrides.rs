//! #1864: the per-gateway session selection store the three selector
//! methods write and the next session composes from — the native
//! counterpart of the TS registry maps (`sessionOverrides`,
//! `sdkThinkingOverrides`, `sdkSessionConfigs.outputStyle`,
//! createLocalGateway.ts:622-744). Keyed by the session key's UTF-16
//! units; presence is the override, so a stored JS `null` thinking value
//! is a value (TS deletes only on `undefined`,
//! ProjectRuntimeRegistry.ts:906-912).
use pilotdeck_protocol::{JsString, JsValue};
use std::collections::HashMap;

#[derive(Default)]
struct Inner {
    permission_modes: HashMap<String, JsString>,
    thinking: HashMap<String, JsValue>,
    output_styles: HashMap<String, JsString>,
}

fn key_units(key: &JsString) -> String {
    String::from_utf16_lossy(key.units())
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
            .insert(key_units(key), mode);
    }

    pub fn permission_mode(&self, key: &JsString) -> Option<JsString> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permission_modes
            .get(&key_units(key))
            .cloned()
    }

    pub fn set_thinking(&self, key: &JsString, thinking: JsValue) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .insert(key_units(key), thinking);
    }

    pub fn clear_thinking(&self, key: &JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .remove(&key_units(key));
    }

    pub fn thinking(&self, key: &JsString) -> Option<JsValue> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking
            .get(&key_units(key))
            .cloned()
    }

    pub fn set_output_style(&self, key: &JsString, name: JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .insert(key_units(key), name);
    }

    pub fn clear_output_style(&self, key: &JsString) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .remove(&key_units(key));
    }

    pub fn output_style(&self, key: &JsString) -> Option<JsString> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_styles
            .get(&key_units(key))
            .cloned()
    }
}
