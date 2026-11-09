//! SDK HTTP hook validation and conversion; no second hook runtime or bus.
use crate::{
    hooks::protocol::{HooksSettings, is_hook_event},
    runtime::turn::submit::SubmitError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn invalid(message: &str) -> SubmitError {
    SubmitError::new("INVALID_SDK_SESSION_CONFIG", message)
}
fn object<const N: usize>(pairs: [(&str, JsValue); N]) -> JsObject {
    let mut value = JsObject::new();
    for (key, item) in pairs {
        value.insert(key.into(), item);
    }
    value
}
pub(super) fn validate(config: &JsValue) -> Result<(), SubmitError> {
    if let Some(value) = config
        .get("includeHookEvents")
        .filter(|v| !v.is_undefined())
        && !matches!(value, JsValue::Bool(_))
    {
        return Err(invalid("includeHookEvents must be a boolean."));
    }
    let Some(hooks) = config.get("hooks").filter(|v| !v.is_undefined()) else {
        return Ok(());
    };
    let hooks = hooks
        .as_object()
        .ok_or_else(|| invalid("hooks must be an object."))?;
    let url_value = hooks
        .get("url")
        .and_then(JsValue::as_string)
        .ok_or_else(|| invalid("hooks.url must be an absolute HTTP(S) URL."))?;
    if url_value.units().len() > 32 * 1024 {
        return Err(SubmitError::new(
            "SDK_HOOK_RESOURCE_LIMIT",
            "SDK hook URL exceeds the native limit.",
        ));
    }
    let raw = String::from_utf16_lossy(url_value.units());
    let mut header_cost = 0usize;
    let url_cost = url_value
        .units()
        .len()
        .saturating_mul(12)
        .saturating_add(512);
    let url =
        url::Url::parse(&raw).map_err(|_| invalid("hooks.url must be an absolute HTTP(S) URL."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("hooks.url must use http: or https:."));
    }
    if let Some(headers) = hooks.get("headers").filter(|v| !v.is_undefined()) {
        let headers = headers
            .as_object()
            .ok_or_else(|| invalid("hooks.headers must be a string-to-string map."))?;
        if headers.entries().len() > 256 {
            return Err(SubmitError::new(
                "SDK_HOOK_RESOURCE_LIMIT",
                "Too many SDK hook headers.",
            ));
        }
        for (key, value) in headers.entries() {
            let JsValue::String(value) = value else {
                return Err(invalid("hooks.headers must be a string-to-string map."));
            };
            if crate::runtime::memory::js_trim_units(key.units()).is_empty() {
                return Err(invalid("hooks.headers must be a string-to-string map."));
            }
            header_cost = header_cost
                .saturating_add(
                    key.units()
                        .len()
                        .saturating_add(value.units().len())
                        .saturating_mul(8),
                )
                .saturating_add(128);
            if header_cost > 8 * 1024 * 1024 {
                return Err(SubmitError::new(
                    "SDK_HOOK_RESOURCE_LIMIT",
                    "SDK hook headers exceed the native expansion budget.",
                ));
            }
        }
    }
    let events=hooks.get("events").and_then(JsValue::as_object).ok_or_else(||invalid("hooks.events must map event names to matcher objects with optional string matcher and positive timeout."))?;
    if events.entries().len() > crate::hooks::protocol::HOOK_EVENTS.len() {
        return Err(SubmitError::new(
            "SDK_HOOK_RESOURCE_LIMIT",
            "Too many SDK hook event entries.",
        ));
    }
    let mut total_cost = 0usize;
    let mut matcher_count = 0usize;
    for (event, matchers) in events.own_entries() {
        if !is_hook_event(event) {
            return Err(invalid(
                "hooks.events must map event names to matcher objects with optional string matcher and positive timeout.",
            ));
        }
        let JsValue::Array(matchers) = matchers else {
            return Err(invalid(
                "hooks.events must map event names to matcher objects with optional string matcher and positive timeout.",
            ));
        };
        for matcher in matchers {
            let Some(matcher) = matcher.as_object() else {
                return Err(invalid(
                    "hooks.events must map event names to matcher objects with optional string matcher and positive timeout.",
                ));
            };
            matcher_count = matcher_count.saturating_add(1);
            if matcher_count > 256 {
                return Err(SubmitError::new(
                    "SDK_HOOK_RESOURCE_LIMIT",
                    "Too many SDK hook matchers.",
                ));
            }
            let matcher_cost = matcher
                .get("matcher")
                .and_then(JsValue::as_string)
                .map_or(0, |v| v.units().len().saturating_mul(8));
            total_cost = total_cost
                .saturating_add(url_cost)
                .saturating_add(header_cost)
                .saturating_add(matcher_cost);
            if total_cost > 8 * 1024 * 1024 {
                return Err(SubmitError::new(
                    "SDK_HOOK_RESOURCE_LIMIT",
                    "SDK hook configuration exceeds the native expansion budget.",
                ));
            }
            if matcher
                .get("matcher")
                .is_some_and(|v| !v.is_undefined() && !matches!(v, JsValue::String(_)))
                || matcher.get("timeout").is_some_and(|v| {
                    !v.is_undefined() && !v.as_number().is_some_and(|n| n.is_finite() && n > 0.0)
                })
            {
                return Err(invalid(
                    "hooks.events must map event names to matcher objects with optional string matcher and positive timeout.",
                ));
            }
        }
    }
    Ok(())
}
fn selector(mut endpoint: url::Url, event: &JsString, index: usize) -> url::Url {
    let event = String::from_utf16_lossy(event.units());
    let index = index.to_string();
    let mut event_seen = false;
    let mut matcher_seen = false;
    let mut pairs = Vec::<(String, String)>::new();
    for (key, value) in endpoint.query_pairs() {
        match key.as_ref() {
            "event" => {
                if !event_seen {
                    pairs.push(("event".into(), event.clone()));
                    event_seen = true;
                }
            }
            "matcher" => {
                if !matcher_seen {
                    pairs.push(("matcher".into(), index.clone()));
                    matcher_seen = true;
                }
            }
            _ => pairs.push((key.into_owned(), value.into_owned())),
        }
    }
    if !event_seen {
        pairs.push(("event".into(), event));
    }
    if !matcher_seen {
        pairs.push(("matcher".into(), index));
    }
    {
        let mut query = endpoint.query_pairs_mut();
        query.clear();
        query.extend_pairs(pairs);
    }
    endpoint
}
pub(super) fn settings(config: &JsValue) -> Result<HooksSettings, SubmitError> {
    validate(config)?;
    let Some(hooks) = config.get("hooks").and_then(JsValue::as_object) else {
        return Ok(Vec::new());
    };
    let raw = String::from_utf16_lossy(
        hooks
            .get("url")
            .and_then(JsValue::as_string)
            .expect("validated")
            .units(),
    );
    let base = url::Url::parse(&raw).expect("validated");
    let events = hooks
        .get("events")
        .and_then(JsValue::as_object)
        .expect("validated");
    let mut settings = JsObject::new();
    for (event, matchers) in events.own_entries() {
        let JsValue::Array(matchers) = matchers else {
            unreachable!()
        };
        let mut entries = Vec::new();
        // Fixed TS SessionInteractionBundle.ts:325-341 validates matcher.timeout
        // at admission but does not copy it into the generated HTTP hook.
        for (index, matcher) in matchers.iter().enumerate() {
            let endpoint = selector(base.clone(), event, index);
            let mut hook = object([("type", "http".into()), ("url", endpoint.as_str().into())]);
            if let Some(headers) = hooks.get("headers") {
                hook.insert("headers".into(), headers.clone());
            }
            let mut entry = object([("hooks", JsValue::Array(vec![JsValue::Object(hook)]))]);
            if let Some(JsValue::String(value)) = matcher.get("matcher")
                && !value.is_empty()
            {
                entry.insert("matcher".into(), JsValue::String(value.clone()));
            }
            entries.push(JsValue::Object(entry));
        }
        settings.insert(event.clone(), JsValue::Array(entries));
    }
    Ok(crate::hooks::config::parse_hooks_config(&JsValue::Object(settings)).settings)
}

/// Uses the same Gateway-owned pending registry as hook_async_result.
pub(in crate::native_process) struct NativeAsyncRegistration {
    pub session: JsString,
    pub include_events: bool,
    pub registry: crate::runtime::turn::interaction::SdkAsyncRegistrySlot,
    pub steer: Option<crate::runtime::project::interaction_steer::InteractionSteer>,
    pub current_turn: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    pub native_turn: Option<Option<crate::runtime::session::CapturedAsyncTurn>>,
}
impl crate::runtime::hooks::runtime::AsyncHookRegistrationHandler for NativeAsyncRegistration {
    fn handle(
        &self,
        registration: crate::runtime::hooks::runtime::AsyncHookRegistration,
    ) -> Result<(), crate::host::HostError> {
        let registry = self
            .registry
            .get()
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                crate::host::ModuleError::new("SDK async hook registry is unavailable.")
            })?;
        if let Some(native) = &self.native_turn {
            let Some(native) = native else { return Ok(()) };
            let slot = self.current_turn.clone().ok_or_else(|| {
                crate::host::ModuleError::new("SDK async hook has no admission guard.")
            })?;
            let completion = native.guard.clone();
            let guard = std::sync::Arc::new(move || slot() && completion());
            return registry.register_pinned(
                crate::runtime::async_hooks::AsyncHookRegistration {
                    session_key: self.session.clone(),
                    invocation_id: registration.invocation_id,
                    hook_name: registration.hook_name,
                    hook_event: registration.hook_event,
                    timeout_ms: registration.timeout_ms,
                    include_hook_events: self.include_events,
                },
                native.turn.clone(),
                guard,
            );
        }
        let info = self
            .steer
            .as_ref()
            .and_then(|steer| steer.active_steer_info(&self.session))
            .ok_or_else(|| {
                crate::host::ModuleError::new("SDK async hook has no active pinned turn.")
            })?;
        let guard = self.current_turn.clone().ok_or_else(|| {
            crate::host::ModuleError::new("SDK async hook has no admission guard.")
        })?;
        registry.register_pinned(
            crate::runtime::async_hooks::AsyncHookRegistration {
                session_key: self.session.clone(),
                invocation_id: registration.invocation_id,
                hook_name: registration.hook_name,
                hook_event: registration.hook_event,
                timeout_ms: registration.timeout_ms,
                include_hook_events: self.include_events,
            },
            crate::runtime::async_hooks::AsyncHookTurn::new(info),
            guard,
        )
    }
}

/// Projects only this session's hook execution events to the active Gateway turn.
pub(in crate::native_process) struct NativeHookProjection {
    pub session: JsString,
    pub sdk: bool,
    pub emit: std::sync::Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
    /// Budget errors remain fenced to the turn that owns this quota.
    pub error_emit: std::sync::Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
    pub signal: crate::runtime::hooks::Cancellation,
    pub budget: std::sync::Arc<HookProjectionBudget>,
}
/// Shared by the turn and session scopes created for one submit.
#[derive(Default)]
pub(in crate::native_process) struct HookProjectionBudget {
    pub bytes: std::sync::atomic::AtomicUsize,
    pub count: std::sync::atomic::AtomicUsize,
    pub tripped: std::sync::atomic::AtomicBool,
}
impl crate::runtime::hooks::runtime::HookProjectionHandler for NativeHookProjection {
    #[allow(deprecated)] // fetch_update supports the workspace MSRV (1.93).
    fn project(&self, event: &JsObject) -> Result<(), crate::host::HostError> {
        if event.get("sessionId").and_then(JsValue::as_string) != Some(&self.session) {
            return Ok(());
        }
        if self.sdk {
            use std::sync::atomic::Ordering;
            let estimate = event
                .entries()
                .iter()
                .fold(512usize, |total, (key, value)| {
                    total
                        .saturating_add(key.units().len().saturating_mul(6))
                        .saturating_add(
                            value
                                .as_string()
                                .map_or(32, |text| text.units().len().saturating_mul(6)),
                        )
                });
            let over_count = self
                .budget
                .count
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    n.checked_add(1).filter(|n| *n <= 1024)
                })
                .is_err();
            let over_bytes = self
                .budget
                .bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    n.checked_add(estimate).filter(|n| *n <= 64 * 1024 * 1024)
                })
                .is_err();
            if over_count || over_bytes {
                if !self.budget.tripped.swap(true, Ordering::SeqCst) {
                    (self.error_emit)(JsValue::Object(object([
                        ("type", "error".into()),
                        ("code", "SDK_HOOK_EVENT_LIMIT".into()),
                        (
                            "message",
                            "SDK hook event output exceeded the per-turn budget.".into(),
                        ),
                        ("recoverable", JsValue::Bool(false)),
                    ])));
                }
                self.signal.abort();
                let mut error = crate::host::ModuleError::new(
                    "SDK hook event output exceeded the per-turn budget.",
                );
                error.code = Some("SDK_HOOK_EVENT_LIMIT".into());
                return Err(error.into());
            }
        }
        let started = event
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.equals_str("started"));
        let mut output = JsObject::new();
        let name = event.get("hookName").cloned().unwrap_or(JsValue::Undefined);
        let hook_event = event
            .get("hookEvent")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        if self.sdk {
            output.insert(
                "type".into(),
                if started {
                    "hook_started".into()
                } else {
                    "hook_response".into()
                },
            );
            output.insert("hookName".into(), name);
            output.insert("hookEvent".into(), hook_event);
            if !started {
                for key in [
                    "stdout",
                    "stderr",
                    "exitCode",
                    "outcome",
                    "asyncInvocationId",
                    "asyncTimeoutMs",
                ] {
                    if let Some(value) = event.get(key).filter(|value| !value.is_undefined()) {
                        output.insert(key.into(), value.clone());
                    }
                }
            }
        } else {
            output.insert("type".into(), "agent_status".into());
            output.insert(
                "event".into(),
                if started {
                    "hook_execution_started".into()
                } else {
                    "hook_execution_completed".into()
                },
            );
            let mut detail = object([("hookName", name), ("hookEvent", hook_event)]);
            if !started {
                for key in ["outcome", "exitCode"] {
                    if let Some(value) = event.get(key).filter(|value| !value.is_undefined()) {
                        detail.insert(key.into(), value.clone());
                    }
                }
            }
            output.insert("detail".into(), JsValue::Object(detail));
        }
        (self.emit)(JsValue::Object(output));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_selectors_retain_other_query_parameters_and_matcher_order() {
        let value=pilotdeck_protocol::parse_json(r#"{"hooks":{"url":"http://127.0.0.1:8123/h?x=1&event=old&matcher=8&event=other","events":{"PreToolUse":[{"matcher":"read_file"},{"matcher":"write_file"}]}}}"#).unwrap();
        let actual = settings(&value).unwrap();
        assert_eq!(actual.len(), 1);
        assert!(actual[0].0.equals_str("PreToolUse"));
        assert_eq!(actual[0].1.len(), 2);
        let first = selector(
            url::Url::parse("http://127.0.0.1:8123/h?x=1&event=old&matcher=8&event=other").unwrap(),
            &"PreToolUse".into(),
            0,
        );
        assert_eq!(
            first.as_str(),
            "http://127.0.0.1:8123/h?x=1&event=PreToolUse&matcher=0"
        );
    }
    #[test]
    fn matcher_count_and_header_expansion_are_bounded_before_conversion() {
        let many = vec!["{}"; 257].join(",");
        let text = format!(
            r#"{{"hooks":{{"url":"http://localhost/h","events":{{"PreToolUse":[{many}]}}}}}}"#
        );
        assert_eq!(
            validate(&pilotdeck_protocol::parse_json(&text).unwrap())
                .unwrap_err()
                .code,
            "SDK_HOOK_RESOURCE_LIMIT"
        );
        let header = "x".repeat(20_000);
        let many = vec!["{}"; 100].join(",");
        let text = format!(
            r#"{{"hooks":{{"url":"http://localhost/h","headers":{{"fixture":"{header}"}},"events":{{"PreToolUse":[{many}]}}}}}}"#
        );
        assert_eq!(
            validate(&pilotdeck_protocol::parse_json(&text).unwrap())
                .unwrap_err()
                .code,
            "SDK_HOOK_RESOURCE_LIMIT"
        );
    }
    #[test]
    fn sdk_projection_preserves_hook_events_and_isolates_session() {
        use crate::runtime::hooks::runtime::HookProjectionHandler;
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        let projection = NativeHookProjection {
            session: "owner".into(),
            sdk: true,
            emit: std::sync::Arc::new(move |event| {
                sink.lock().unwrap().push(event);
                true
            }),
            error_emit: std::sync::Arc::new(|_| true),
            signal: crate::runtime::hooks::Cancellation::new(),
            budget: Default::default(),
        };
        projection
            .project(&object([
                ("type", "started".into()),
                ("sessionId", "other".into()),
                ("hookName", "name".into()),
                ("hookEvent", "PreToolUse".into()),
            ]))
            .unwrap();
        projection
            .project(&object([
                ("type", "response".into()),
                ("sessionId", "owner".into()),
                ("hookName", "name".into()),
                ("hookEvent", "PreToolUse".into()),
                ("stdout", "hello".into()),
                ("stderr", "".into()),
                ("outcome", "success".into()),
            ]))
            .unwrap();
        let seen = events.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].get("type"), Some(&"hook_response".into()));
        assert_eq!(seen[0].get("stdout"), Some(&"hello".into()));
    }
    #[test]
    fn hook_event_budget_reports_and_cancels_once() {
        use crate::runtime::hooks::runtime::HookProjectionHandler;
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let target = sent.clone();
        let signal = crate::runtime::hooks::Cancellation::new();
        let projection = NativeHookProjection {
            session: "s".into(),
            sdk: true,
            emit: {
                let target = target.clone();
                std::sync::Arc::new(move |event| {
                    target.lock().unwrap().push(event);
                    true
                })
            },
            error_emit: std::sync::Arc::new(move |event| {
                target.lock().unwrap().push(event);
                true
            }),
            signal: signal.clone(),
            budget: Default::default(),
        };
        let event = object([
            ("type", "started".into()),
            ("sessionId", "s".into()),
            ("hookName", "h".into()),
            ("hookEvent", "PreToolUse".into()),
        ]);
        for _ in 0..1024 {
            projection.project(&event).unwrap();
        }
        assert!(projection.project(&event).is_err());
        assert!(projection.project(&event).is_err());
        assert!(signal.is_cancelled());
        let events = sent.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|item| item.get("code") == Some(&"SDK_HOOK_EVENT_LIMIT".into()))
                .count(),
            1
        );
    }
    #[test]
    fn turn_and_session_projection_share_one_submit_budget_without_touching_next_turn() {
        use crate::runtime::hooks::runtime::HookProjectionHandler;
        let budget = std::sync::Arc::new(HookProjectionBudget::default());
        let current_signal = crate::runtime::hooks::Cancellation::new();
        let next_signal = crate::runtime::hooks::Cancellation::new();
        let sink = std::sync::Arc::new(|_: JsValue| true);
        let turn = NativeHookProjection {
            session: "s".into(),
            sdk: true,
            emit: sink.clone(),
            error_emit: sink.clone(),
            signal: current_signal.clone(),
            budget: budget.clone(),
        };
        let session = NativeHookProjection {
            session: "s".into(),
            sdk: true,
            emit: sink.clone(),
            error_emit: sink.clone(),
            signal: current_signal.clone(),
            budget,
        };
        let next = NativeHookProjection {
            session: "s".into(),
            sdk: true,
            emit: sink.clone(),
            error_emit: sink,
            signal: next_signal.clone(),
            budget: Default::default(),
        };
        let event = object([
            ("type", "started".into()),
            ("sessionId", "s".into()),
            ("hookName", "h".into()),
            ("hookEvent", "PreToolUse".into()),
        ]);
        for _ in 0..512 {
            turn.project(&event).unwrap();
            session.project(&event).unwrap();
        }
        assert!(session.project(&event).is_err());
        assert!(current_signal.is_cancelled());
        assert!(!next_signal.is_cancelled());
        assert!(next.project(&event).is_ok());
    }
    #[test]
    fn session_budget_error_uses_owning_turn_emitter_not_current_session_emitter() {
        use crate::runtime::hooks::runtime::HookProjectionHandler;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let session_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let turn_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let session_sink = session_calls.clone();
        let turn_sink = turn_calls.clone();
        let budget = std::sync::Arc::new(HookProjectionBudget::default());
        budget.count.store(1024, Ordering::SeqCst);
        let projection = NativeHookProjection {
            session: "s".into(),
            sdk: true,
            emit: std::sync::Arc::new(move |_| {
                session_sink.fetch_add(1, Ordering::SeqCst);
                true
            }),
            error_emit: std::sync::Arc::new(move |_| {
                turn_sink.fetch_add(1, Ordering::SeqCst);
                true
            }),
            signal: crate::runtime::hooks::Cancellation::new(),
            budget,
        };
        let event = object([
            ("type", "started".into()),
            ("sessionId", "s".into()),
            ("hookName", "h".into()),
            ("hookEvent", "ConfigChange".into()),
        ]);
        assert!(projection.project(&event).is_err());
        assert_eq!(session_calls.load(Ordering::SeqCst), 0);
        assert_eq!(turn_calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn async_response_without_gateway_registry_fails_explicitly() {
        use crate::runtime::hooks::runtime::{AsyncHookRegistration, AsyncHookRegistrationHandler};
        let slot = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(slot.set(None).is_ok());
        let bridge = NativeAsyncRegistration {
            session: "s".into(),
            include_events: true,
            registry: slot,
            steer: None,
            current_turn: None,
            native_turn: None,
        };
        assert!(
            bridge
                .handle(AsyncHookRegistration {
                    hook_name: "h".into(),
                    hook_event: "PreToolUse".into(),
                    invocation_id: "i".into(),
                    timeout_ms: None
                })
                .is_err()
        );
    }

    #[tokio::test]
    async fn native_completion_capture_still_obeys_the_inner_handle_slot_guard() {
        use crate::runtime::{
            async_hooks::{
                AsyncHookRegistry, AsyncHookStatus, AsyncHookTurn, AsyncHookTurnHandle,
                TokioHookClock,
            },
            hooks::runtime::{AsyncHookRegistration, AsyncHookRegistrationHandler},
            steer::{EnqueueResult, SteerMessage},
        };
        use std::sync::{
            Arc, OnceLock,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        struct Turn {
            run: JsString,
            identity: Arc<()>,
            calls: Arc<AtomicUsize>,
        }
        impl AsyncHookTurnHandle for Turn {
            fn run_id(&self) -> &JsString {
                &self.run
            }
            fn identity(&self) -> &Arc<()> {
                &self.identity
            }
            fn is_live(&self) -> bool {
                true
            }
            fn enqueue(&self, _: SteerMessage) -> crate::host::HostFuture<EnqueueResult> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(EnqueueResult {
                        accepted: true,
                        reason: None,
                    })
                })
            }
        }
        let registry = Arc::new(AsyncHookRegistry::new(
            Arc::new(TokioHookClock),
            |_| None,
            |_, _| {},
        ));
        let slot = Arc::new(OnceLock::new());
        assert!(slot.set(Some(registry.clone())).is_ok());
        let inner = Arc::new(AtomicBool::new(true));
        let guard = inner.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let capture = crate::runtime::session::CapturedAsyncTurn {
            turn: AsyncHookTurn::new(Turn {
                run: "same".into(),
                identity: Arc::new(()),
                calls: calls.clone(),
            }),
            guard: Arc::new(|| true),
            bind_steer: Arc::new(|_| Ok(())),
        };
        let bridge = NativeAsyncRegistration {
            session: "session".into(),
            include_events: false,
            registry: slot,
            steer: None,
            current_turn: Some(Arc::new(move || guard.load(Ordering::SeqCst))),
            native_turn: Some(Some(capture)),
        };
        bridge
            .handle(AsyncHookRegistration {
                hook_name: "fixture".into(),
                hook_event: "SubagentStart".into(),
                invocation_id: "invocation".into(),
                timeout_ms: Some(30_000.0),
            })
            .unwrap();
        // The outer Gateway run remains admitted while inner turn cleanup runs.
        inner.store(false, Ordering::SeqCst);
        let output=pilotdeck_protocol::parse_json(r#"{"hookSpecificOutput":{"hookEventName":"SubagentStart","additionalContext":"must-not-enqueue"}}"#).unwrap();
        let result = registry
            .submit("session".into(), "invocation".into(), output)
            .await
            .unwrap();
        assert_eq!(result.status, AsyncHookStatus::Expired);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "closed inner slot cannot enqueue through still-live Gateway completion"
        );
    }
    #[test]
    fn malformed_sdk_hook_settings_are_rejected_before_admission() {
        for text in [
            r#"{"hooks":{"url":"file:///tmp/h","events":{}}}"#,
            r#"{"hooks":{"url":"http://localhost/h","events":{"PreToolUse":[{"timeout":0}]}}}"#,
            r#"{"includeHookEvents":"yes"}"#,
        ] {
            assert!(
                validate(&pilotdeck_protocol::parse_json(text).unwrap()).is_err(),
                "{text}"
            );
        }
    }
}
