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
    automation: HashMap<JsString, AutomationEntry>,
    permission_modes: HashMap<JsString, JsString>,
    thinking: HashMap<JsString, JsValue>,
    output_styles: HashMap<JsString, JsString>,
    output_style_order: Vec<JsString>,
}

pub(crate) type SdkStyleFuture<'a, T> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<T, crate::runtime::sdk_session::SdkSessionError>>
            + Send
            + 'a,
    >,
>;
/// Production uses the committed SDK registry; the legacy map is only a
/// synchronous observation cache and the generic no-port implementation.
pub(crate) trait SdkOutputStylePort: Send + Sync {
    fn get<'a>(&'a self, key: &'a JsString) -> SdkStyleFuture<'a, Option<JsString>>;
    fn plugins<'a>(
        &'a self,
        key: &'a JsString,
    ) -> SdkStyleFuture<'a, Vec<std::sync::Arc<crate::runtime::plugins::LoadedPlugin>>>;
    fn set<'a>(
        &'a self,
        key: &'a JsString,
        project: &'a JsString,
        name: Option<JsString>,
    ) -> SdkStyleFuture<'a, ()>;
}
/// Host-owned overrides for an unattended automation session. These are not
/// accepted from a public turn request.
#[derive(Clone)]
pub(crate) struct AutomationOverrides {
    pub cwd: std::path::PathBuf,
    pub permission_mode: JsString,
    pub bypass_available: bool,
    pub can_prompt: bool,
    pub exclude_tools: Vec<JsString>,
    pub permission_deny: Vec<crate::config::permission::PermissionRule>,
    pub context_registry:
        Option<std::sync::Arc<crate::runtime::always_on::AlwaysOnRunContextRegistry>>,
}

struct AutomationEntry {
    identity: std::sync::Arc<()>,
    value: AutomationOverrides,
}

/// Revoke only the generation installed by this lease, even if an old task
/// finishes after its slot has been replaced.
pub(crate) struct AutomationOverrideLease {
    owner: std::sync::Weak<SessionOverrides>,
    key: JsString,
    identity: std::sync::Arc<()>,
}

impl Drop for AutomationOverrideLease {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.inner.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .automation
                .get(&self.key)
                .is_some_and(|entry| std::sync::Arc::ptr_eq(&entry.identity, &self.identity))
            {
                state.automation.remove(&self.key);
            }
        }
    }
}

/// The minimal equivalent registry #1839 allows: one shared instance per
/// gateway process (created in `assembly`, handed to both `Requests` and
/// the session factory's prepare closure).
#[derive(Default)]
pub struct SessionOverrides {
    inner: std::sync::Mutex<Inner>,
    sdk_styles: std::sync::Mutex<Option<std::sync::Weak<dyn SdkOutputStylePort>>>,
}

impl SessionOverrides {
    pub(crate) fn bind_sdk_styles(&self, port: std::sync::Weak<dyn SdkOutputStylePort>) {
        *self.sdk_styles.lock().unwrap_or_else(|e| e.into_inner()) = Some(port);
    }
    pub(crate) async fn effective_output_style(
        &self,
        key: &JsString,
    ) -> Result<Option<JsString>, crate::runtime::sdk_session::SdkSessionError> {
        let port = self
            .sdk_styles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match port {
            None => Ok(self.output_style(key)),
            Some(port) => {
                port.upgrade()
                    .ok_or_else(|| {
                        crate::runtime::sdk_session::SdkSessionError::raw(
                            "Error",
                            "SDK output-style owner is closed.",
                        )
                    })?
                    .get(key)
                    .await
            }
        }
    }
    pub(crate) async fn output_style_plugins(
        &self,
        key: &JsString,
    ) -> Result<
        Vec<std::sync::Arc<crate::runtime::plugins::LoadedPlugin>>,
        crate::runtime::sdk_session::SdkSessionError,
    > {
        let port = self
            .sdk_styles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match port {
            None => Ok(vec![]),
            Some(port) => {
                port.upgrade()
                    .ok_or_else(|| {
                        crate::runtime::sdk_session::SdkSessionError::raw(
                            "Error",
                            "SDK output-style owner is closed.",
                        )
                    })?
                    .plugins(key)
                    .await
            }
        }
    }
    pub(crate) async fn select_output_style(
        &self,
        key: &JsString,
        project: &JsString,
        name: Option<JsString>,
    ) -> Result<(), crate::runtime::sdk_session::SdkSessionError> {
        let port = self
            .sdk_styles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(port) = port {
            port.upgrade()
                .ok_or_else(|| {
                    crate::runtime::sdk_session::SdkSessionError::raw(
                        "Error",
                        "SDK output-style owner is closed.",
                    )
                })?
                .set(key, project, name)
                .await
        } else {
            match name {
                Some(name) => self.set_output_style(key, name),
                None => self.clear_output_style(key),
            }
            Ok(())
        }
    }

    /// One active automation generation per session; no silent replacement.
    pub(crate) fn acquire_automation(
        self: &std::sync::Arc<Self>,
        key: JsString,
        value: AutomationOverrides,
    ) -> Result<AutomationOverrideLease, &'static str> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.automation.contains_key(&key) {
            return Err("automation session override already active");
        }
        let identity = std::sync::Arc::new(());
        state.automation.insert(
            key.clone(),
            AutomationEntry {
                identity: identity.clone(),
                value,
            },
        );
        Ok(AutomationOverrideLease {
            owner: std::sync::Arc::downgrade(self),
            key,
            identity,
        })
    }

    pub(crate) fn automation(&self, key: &JsString) -> Option<AutomationOverrides> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .automation
            .get(key)
            .map(|entry| entry.value.clone())
    }

    pub fn clear_sdk_state(&self, key: &JsString) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.thinking.remove(key);
        state.output_styles.remove(key);
        state.output_style_order.retain(|item| item != key);
    }

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
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !state.output_styles.contains_key(key) {
            state.output_style_order.push(key.clone());
        }
        state.output_styles.insert(key.clone(), name);
    }

    pub fn clear_output_style(&self, key: &JsString) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.output_styles.remove(key);
        state.output_style_order.retain(|item| item != key);
    }

    pub(crate) fn output_style_sessions(&self) -> Vec<JsString> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_style_order
            .clone()
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

/// The flag-settings wire method writes the exact store read at native turn
/// preparation, shared with set_permission_mode and set_session_thinking.
impl crate::runtime::settings::ApplyFlagSettings for SessionOverrides {
    fn apply(
        &self,
        input: &JsValue,
    ) -> Result<crate::runtime::settings::FlagSettings, crate::runtime::settings::SettingsError>
    {
        use crate::runtime::settings::{FlagSettings, SettingsError};
        let session = input
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .ok_or_else(|| {
                SettingsError::gateway("INVALID_SESSION_KEY", "sessionKey is required.")
            })?;
        let settings = input
            .get("settings")
            .and_then(JsValue::as_object)
            .ok_or_else(|| {
                SettingsError::gateway("INVALID_FLAG_SETTINGS", "settings must be an object.")
            })?;
        let mut result = FlagSettings::default();
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(effort) = settings.get("effortLevel") {
            if matches!(effort, JsValue::Null) {
                state.thinking.remove(session);
                result.cleared.push("effortLevel".into());
            } else {
                let mut thinking = pilotdeck_protocol::JsObject::new();
                thinking.insert("enabled".into(), JsValue::Bool(true));
                thinking.insert("mode".into(), effort.clone());
                state
                    .thinking
                    .insert(session.clone(), JsValue::Object(thinking));
                result.applied.push("effortLevel".into());
            }
        }
        if let Some(permissions) = settings.get("permissions") {
            match permissions.get("defaultMode").and_then(JsValue::as_string) {
                Some(mode) => {
                    state.permission_modes.insert(session.clone(), mode.clone());
                    result.applied.push("permissions.defaultMode".into());
                }
                None => {
                    state.permission_modes.remove(session);
                    result.cleared.push("permissions.defaultMode".into());
                }
            }
        }
        Ok(result)
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

#[cfg(test)]
mod automation_override_tests {
    use super::*;
    use std::sync::Arc;

    fn config(path: &str) -> AutomationOverrides {
        AutomationOverrides {
            cwd: path.into(),
            permission_mode: "bypassPermissions".into(),
            bypass_available: true,
            can_prompt: false,
            exclude_tools: vec!["ask_user_question".into()],
            context_registry: None,
            permission_deny: vec![
                crate::config::permission::PermissionRule::new("policy", "deny", "bash")
                    .with_pattern("git push*"),
            ],
        }
    }

    #[test]
    fn automation_lease_rejects_overlap_and_preserves_regular_selection() {
        let owner = Arc::new(SessionOverrides::default());
        let key = JsString::from("auto-session");
        owner.set_permission_mode(&key, "plan".into());
        let lease = owner
            .acquire_automation(key.clone(), config("/project/a"))
            .unwrap();
        assert!(
            owner
                .acquire_automation(key.clone(), config("/project/b"))
                .is_err()
        );
        let got = owner.automation(&key).unwrap();
        assert_eq!(got.cwd, std::path::PathBuf::from("/project/a"));
        assert!(got.bypass_available);
        assert!(!got.can_prompt);
        assert_eq!(got.exclude_tools.len(), 1);
        assert_eq!(got.permission_deny.len(), 1);
        assert!(got.permission_mode.equals_str("bypassPermissions"));
        drop(lease);
        assert!(owner.automation(&key).is_none());
        assert!(owner.permission_mode(&key).unwrap().equals_str("plan"));
        let next = owner
            .acquire_automation(key.clone(), config("/project/b"))
            .unwrap();
        assert_eq!(
            owner.automation(&key).unwrap().cwd,
            std::path::PathBuf::from("/project/b")
        );
        drop(next);
    }
}
