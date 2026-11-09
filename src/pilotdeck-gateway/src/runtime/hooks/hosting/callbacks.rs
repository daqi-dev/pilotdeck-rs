//! Ordered SDK callbacks (`packages/sdk/src/hook-server.ts:169-207,263-353`).
use super::{
    Cancellation, HostedHookError, HostedHookEvents, HostedHookFuture, HostedHookHandler,
    HostedHookMatcher, validate_events,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{sync::Arc, time::Duration};

pub trait HostedHookCallback: Send + Sync {
    fn call(
        &self,
        input: JsObject,
        tool_use_id: Option<JsString>,
        signal: Cancellation,
        async_hook_id: JsString,
    ) -> HostedHookFuture;
}

pub struct HostedHookCallbackMatcher {
    pub matcher: Option<JsString>,
    pub timeout_seconds: Option<f64>,
    pub callbacks: Vec<Arc<dyn HostedHookCallback>>,
}
pub type HostedHookCallbackEvents = Vec<(JsString, Vec<HostedHookCallbackMatcher>)>;

/// Pass `events()` and this handler to `HostedHookSession::connect`, or use
/// `HostedHookServer::from_callbacks` for an independently hosted endpoint.
pub struct HostedHookCallbacks(Arc<HostedHookCallbackEvents>);
impl HostedHookCallbacks {
    pub fn new(events: HostedHookCallbackEvents) -> Result<Self, HostedHookError> {
        let callbacks = Self(Arc::new(events));
        validate_events(&callbacks.events())?;
        Ok(callbacks)
    }
    pub fn events(&self) -> HostedHookEvents {
        self.0
            .iter()
            .map(|(event, matchers)| {
                (
                    event.clone(),
                    matchers
                        .iter()
                        .map(|matcher| HostedHookMatcher {
                            matcher: matcher.matcher.clone(),
                            timeout_seconds: matcher.timeout_seconds,
                        })
                        .collect(),
                )
            })
            .collect()
    }
}
impl HostedHookHandler for HostedHookCallbacks {
    fn manages_callback_timeouts(&self) -> bool {
        true
    }
    fn call(
        &self,
        event: JsString,
        matcher: usize,
        input: JsObject,
        signal: Cancellation,
        async_hook_id: JsString,
    ) -> HostedHookFuture {
        let events = self.0.clone();
        Box::pin(async move {
            let matcher = events
                .iter()
                .find(|(name, _)| name == &event)
                .and_then(|(_, matchers)| matchers.get(matcher))
                .ok_or_else(|| {
                    HostedHookError::validation("Invalid SDK hook endpoint selector.")
                })?;
            let tool_use_id = input
                .get("tool_use_id")
                .and_then(JsValue::as_string)
                .or_else(|| input.get("tool_call_id").and_then(JsValue::as_string))
                .cloned();
            let mut merged = JsObject::new();
            let mut deferred = None;
            for callback in &matcher.callbacks {
                let output = invoke(
                    callback,
                    input.clone(),
                    tool_use_id.clone(),
                    matcher.timeout_seconds,
                    signal.clone(),
                    async_hook_id.clone(),
                )
                .await?;
                if !truthy(&output) {
                    continue;
                }
                if output.get("async").and_then(JsValue::as_bool) == Some(true) {
                    if deferred.is_some() || !merged.is_empty() {
                        return Err(mixed_output());
                    }
                    if output
                        .get("asyncTimeout")
                        .filter(|v| !v.is_undefined())
                        .is_some_and(|v| v.as_number().is_none_or(|n| !n.is_finite() || n <= 0.0))
                    {
                        return Err(HostedHookError::validation(
                            "asyncTimeout must be a positive finite number of seconds.",
                        ));
                    }
                    deferred = Some(output);
                } else {
                    if deferred.is_some() {
                        return Err(mixed_output());
                    }
                    merged = merge(merged, output);
                }
            }
            Ok(deferred.unwrap_or(JsValue::Object(merged)))
        })
    }
}

fn mixed_output() -> HostedHookError {
    HostedHookError::validation(
        "A hook matcher cannot combine an async output with synchronous hook effects.",
    )
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn spread(value: &JsValue) -> JsObject {
    match value {
        JsValue::Object(object) => object.clone(),
        JsValue::Array(values) => {
            let mut object = JsObject::new();
            for (i, v) in values.iter().enumerate() {
                object.insert(i.to_string().into(), v.clone());
            }
            object
        }
        JsValue::String(value) => {
            let mut object = JsObject::new();
            for (i, unit) in value.units().iter().enumerate() {
                object.insert(
                    i.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
            object
        }
        _ => JsObject::new(),
    }
}
fn merge(previous: JsObject, next: JsValue) -> JsObject {
    let previous_specific = previous.get("hookSpecificOutput");
    let next_specific = next.get("hookSpecificOutput");
    let specific = if previous_specific.is_some_and(truthy) {
        if next_specific.is_some_and(truthy) {
            let mut specific = spread(previous_specific.unwrap());
            for (key, value) in spread(next_specific.unwrap()).entries() {
                specific.insert(key.clone(), value.clone());
            }
            Some(JsValue::Object(specific))
        } else {
            previous_specific.cloned()
        }
    } else {
        next_specific.cloned()
    };
    let blocked = previous.get("continue").and_then(JsValue::as_bool) == Some(false)
        || next.get("continue").and_then(JsValue::as_bool) == Some(false);
    let decision = previous
        .get("decision")
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str("block"))
        || next
            .get("decision")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str("block"));
    let system_message = match (
        previous.get("systemMessage").filter(|v| truthy(v)),
        next.get("systemMessage").filter(|v| truthy(v)),
    ) {
        (Some(left), Some(right)) => {
            let mut units = crate::js_value::to_string_units(left).unwrap_or_default();
            units.push(10);
            units.extend(crate::js_value::to_string_units(right).unwrap_or_default());
            Some(JsValue::String(JsString::from_utf16(units)))
        }
        _ => None,
    };
    let mut merged = previous;
    for (key, value) in spread(&next).entries() {
        merged.insert(key.clone(), value.clone());
    }
    if blocked {
        merged.insert("continue".into(), JsValue::Bool(false));
    }
    if decision {
        merged.insert("decision".into(), "block".into());
    }
    if let Some(message) = system_message {
        merged.insert("systemMessage".into(), message);
    }
    if let Some(specific) = specific.filter(truthy) {
        merged.insert("hookSpecificOutput".into(), specific);
    }
    merged
}

async fn invoke(
    callback: &Arc<dyn HostedHookCallback>,
    input: JsObject,
    tool_use_id: Option<JsString>,
    seconds: Option<f64>,
    parent: Cancellation,
    async_hook_id: JsString,
) -> Result<JsValue, HostedHookError> {
    if parent.is_aborted() {
        return Err(aborted());
    }
    if seconds.is_some_and(|n| !n.is_finite() || n <= 0.0) {
        return Err(HostedHookError::validation(
            "Hook matcher timeout must be a positive finite number of seconds.",
        ));
    }
    let child = Cancellation::new();
    let signal = child.clone();
    let parent_signal = parent.clone();
    let bridge = tokio::spawn(async move {
        parent_signal.cancelled().await;
        signal.abort();
    });
    struct Guard {
        bridge: tokio::task::JoinHandle<()>,
        signal: Cancellation,
        complete: bool,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.bridge.abort();
            if !self.complete {
                self.signal.abort();
            }
        }
    }
    let mut guard = Guard {
        bridge,
        signal: child.clone(),
        complete: false,
    };
    let future = callback.call(input, tool_use_id, child.clone(), async_hook_id);
    let result = if let Some(seconds) = seconds {
        let delay = pilotdeck_protocol::timer_delay_ms(seconds * 1000.0);
        tokio::select! {
            result=future=>result,
            _=parent.cancelled()=>{ child.abort(); Err(aborted()) },
            _=tokio::time::sleep(Duration::from_millis(delay))=>{ child.abort(); Err(HostedHookError { code:"timeout", message:format!("SDK hook callback timed out after {seconds}s.") }) },
        }
    } else {
        future.await
    };
    guard.complete = true;
    result
}
fn aborted() -> HostedHookError {
    HostedHookError {
        code: "transport_error",
        message: "SDK hook callback was aborted.".into(),
    }
}

#[cfg(test)]
#[path = "callbacks_tests.rs"]
mod tests;
