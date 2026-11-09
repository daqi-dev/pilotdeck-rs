//! Session-scoped projection of HookRuntime execution transitions.
//!
//! Mirrors `GatewayHookExecutionProjection.ts`: regular sessions receive the
//! compact `agent_status` vocabulary, while SDK sessions retain the public
//! `hook_started`/`hook_response` payload including stdout, stderr and deferred
//! invocation metadata.

use super::runtime::{
    AsyncHookRegistration, AsyncHookRegistrationHandler, HookExecutionEventHandler,
};
use crate::runtime::async_hooks::{
    AsyncHookRegistration as GatewayAsyncHookRegistration, AsyncHookRegistry,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

type EventSink = dyn Fn(JsObject) -> bool + Send + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEventProjection {
    Gateway,
    Sdk,
}

pub struct GatewayHookExecutionProjection {
    session_key: JsString,
    mode: HookEventProjection,
    emit: Arc<EventSink>,
}

impl GatewayHookExecutionProjection {
    pub fn new(
        session_key: JsString,
        mode: HookEventProjection,
        emit: impl Fn(JsObject) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            session_key,
            mode,
            emit: Arc::new(emit),
        }
    }
}

impl HookExecutionEventHandler for GatewayHookExecutionProjection {
    fn handle(&self, event: &JsObject) {
        if event.get("sessionId").and_then(JsValue::as_string) != Some(&self.session_key) {
            return;
        }
        let Some(projected) = project_hook_execution_event(event, self.mode) else {
            return;
        };
        (self.emit)(projected);
    }
}

pub fn project_hook_execution_event(
    event: &JsObject,
    mode: HookEventProjection,
) -> Option<JsObject> {
    let kind = event.get("type")?.as_string()?;
    if !kind.equals_str("started") && !kind.equals_str("response") {
        return None;
    }
    let mut output = JsObject::new();
    match (mode, kind.equals_str("started")) {
        (HookEventProjection::Sdk, true) => {
            copy(&mut output, event, "type", Some("hook_started".into()));
            copy(&mut output, event, "hookName", None);
            copy(&mut output, event, "hookEvent", None);
        }
        (HookEventProjection::Sdk, false) => {
            copy(&mut output, event, "type", Some("hook_response".into()));
            for key in [
                "hookName",
                "hookEvent",
                "stdout",
                "stderr",
                "exitCode",
                "outcome",
                "asyncInvocationId",
                "asyncTimeoutMs",
            ] {
                copy(&mut output, event, key, None);
            }
        }
        (HookEventProjection::Gateway, started) => {
            output.insert("type".into(), "agent_status".into());
            output.insert(
                "event".into(),
                if started {
                    "hook_execution_started".into()
                } else {
                    "hook_execution_completed".into()
                },
            );
            let mut detail = JsObject::new();
            copy(&mut detail, event, "hookName", None);
            copy(&mut detail, event, "hookEvent", None);
            if !started {
                copy(&mut detail, event, "outcome", None);
                copy(&mut detail, event, "exitCode", None);
            }
            output.insert("detail".into(), JsValue::Object(detail));
        }
    }
    Some(output)
}

fn copy(output: &mut JsObject, input: &JsObject, key: &str, replacement: Option<JsValue>) {
    if let Some(value) = replacement.or_else(|| input.get(key).cloned())
        && !value.is_undefined()
    {
        output.insert(key.into(), value);
    }
}

/// Connects HookRuntime's external async IDs to the Gateway-owned registry.
/// The registry resolves the active turn at registration time and owns expiry,
/// idempotence, steer delivery and `hook_async_result` emission.
pub struct GatewayAsyncHookRegistrationBridge {
    pub registry: AsyncHookRegistry,
    pub session_key: JsString,
    pub include_hook_events: bool,
}

impl AsyncHookRegistrationHandler for GatewayAsyncHookRegistrationBridge {
    fn handle(&self, input: AsyncHookRegistration) {
        let _ = self.registry.register(GatewayAsyncHookRegistration {
            session_key: self.session_key.clone(),
            invocation_id: input.invocation_id,
            hook_name: input.hook_name,
            hook_event: input.hook_event,
            timeout_ms: input.timeout_ms,
            include_hook_events: self.include_hook_events,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: &str) -> JsObject {
        let mut event = JsObject::new();
        event.insert("type".into(), kind.into());
        event.insert("sessionId".into(), "session".into());
        event.insert("hookName".into(), "plugin:http".into());
        event.insert("hookEvent".into(), "PostToolUse".into());
        if kind == "response" {
            event.insert("stdout".into(), "out".into());
            event.insert("stderr".into(), "err".into());
            event.insert("exitCode".into(), JsValue::Number(0.0));
            event.insert("outcome".into(), "success".into());
            event.insert("asyncInvocationId".into(), "async-1".into());
            event.insert("asyncTimeoutMs".into(), JsValue::Number(500.0));
        }
        event
    }

    #[test]
    fn sdk_projection_preserves_public_hook_payload() {
        let response = project_hook_execution_event(&source("response"), HookEventProjection::Sdk)
            .expect("known event");
        assert_eq!(response.get("type"), Some(&"hook_response".into()));
        assert_eq!(response.get("stdout"), Some(&"out".into()));
        assert_eq!(response.get("stderr"), Some(&"err".into()));
        assert_eq!(response.get("asyncInvocationId"), Some(&"async-1".into()));
        assert_eq!(
            response.get("asyncTimeoutMs"),
            Some(&JsValue::Number(500.0))
        );
    }

    #[test]
    fn normal_projection_hides_output_and_uses_agent_status() {
        let response =
            project_hook_execution_event(&source("response"), HookEventProjection::Gateway)
                .expect("known event");
        assert_eq!(response.get("type"), Some(&"agent_status".into()));
        assert_eq!(
            response.get("event"),
            Some(&"hook_execution_completed".into())
        );
        let detail = response.get("detail").and_then(JsValue::as_object).unwrap();
        assert_eq!(detail.get("outcome"), Some(&"success".into()));
        assert!(!detail.contains_key("stdout"));
        assert!(!detail.contains_key("stderr"));
    }
}
