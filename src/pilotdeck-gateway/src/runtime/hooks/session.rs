//! Session hook sources and the serializable SDK HTTP-hook adapter.
//! TS `SessionInteractionBundle.ts:325-365` appends SDK matchers after the
//! project/plugin settings; the HTTP executor owns the native hook contract.
use crate::hooks::protocol::{HookCommand, HookCommon, HookMatcher, HooksSettings};
use pilotdeck_protocol::{JsString, JsValue};

#[derive(Clone)]
pub struct FileUpdateHook {
    pub lifecycle: super::LifecycleRuntime,
    pub session_key: JsString,
    pub cwd: JsString,
}
impl FileUpdateHook {
    pub async fn did_save(
        &self,
        absolute: &JsString,
        relative: &JsString,
        root: &JsString,
        previous: Option<&str>,
    ) {
        let mut base = pilotdeck_protocol::JsObject::new();
        base.insert(
            "sessionId".into(),
            JsValue::String(self.session_key.clone()),
        );
        base.insert("transcriptPath".into(), "".into());
        base.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        let mut payload = pilotdeck_protocol::JsObject::new();
        payload.insert("filePath".into(), JsValue::String(relative.clone()));
        payload.insert("absolutePath".into(), JsValue::String(absolute.clone()));
        payload.insert("root".into(), JsValue::String(root.clone()));
        payload.insert(
            "changeType".into(),
            if previous.is_none() {
                "created".into()
            } else {
                "updated".into()
            },
        );
        let _ = self
            .lifecycle
            .dispatch(super::LifecycleDispatchInput {
                event: "FileChanged".into(),
                base_input: base,
                payload: Some(payload),
                match_query: Some(relative.clone()),
                cwd: self.cwd.clone(),
                env: None,
                signal: None,
            })
            .await;
    }
}

pub async fn notify_file_save(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
    relative: &JsString,
    root: &JsString,
    previous: Option<&str>,
) {
    if let Some(notifier) = context
        .host
        .file_update_notifier
        .as_ref()
        .and_then(|port| port.downcast_ref::<FileUpdateHook>())
    {
        notifier.did_save(absolute, relative, root, previous).await;
    }
}

pub fn external_sdk_settings(config: Option<&JsValue>, event: &str) -> HooksSettings {
    sdk_hook_settings(config)
        .into_iter()
        .filter(|(key, _)| key.equals_str(event))
        .collect()
}

pub fn merge_settings(base: &mut HooksSettings, extra: HooksSettings) {
    for (event, matchers) in extra {
        match base.iter_mut().find(|(key, _)| key == &event) {
            Some((_, existing)) => existing.extend(matchers),
            None => base.push((event, matchers)),
        }
    }
}

pub fn sdk_hook_settings(config: Option<&JsValue>) -> HooksSettings {
    let Some(config) = config else {
        return Vec::new();
    };
    let Some(endpoint) = config
        .get("url")
        .and_then(JsValue::as_string)
        .and_then(|v| v.to_utf8().ok())
        .and_then(|v| url::Url::parse(&v).ok())
    else {
        return Vec::new();
    };
    let Some(events) = config.get("events").and_then(JsValue::as_object) else {
        return Vec::new();
    };
    let headers = config.get("headers").and_then(JsValue::as_object).cloned();
    let mut settings = Vec::new();
    for (event, matchers) in events.entries() {
        if !crate::hooks::protocol::is_hook_event(event) {
            continue;
        }
        let JsValue::Array(matchers) = matchers else {
            continue;
        };
        let matchers = matchers
            .iter()
            .enumerate()
            .map(|(index, matcher)| {
                let mut url = endpoint.clone();
                let pairs: Vec<(String, String)> = url
                    .query_pairs()
                    .filter(|(key, _)| key != "event" && key != "matcher")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                url.set_query(None);
                url.query_pairs_mut()
                    .extend_pairs(pairs)
                    .append_pair("event", &String::from_utf16_lossy(event.units()))
                    .append_pair("matcher", &index.to_string());
                HookMatcher {
                    matcher: matcher
                        .get("matcher")
                        .and_then(JsValue::as_string)
                        .filter(|v| !v.is_empty())
                        .cloned(),
                    hooks: vec![HookCommand::Http {
                        url: JsString::from(url.as_str()),
                        headers: headers.clone(),
                        allowed_env_vars: None,
                        common: HookCommon::default(),
                    }],
                    ..Default::default()
                }
            })
            .collect();
        settings.push((event.clone(), matchers));
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn sdk_matchers_append_after_native_hooks_and_replace_selectors() {
        let config = parse_json(r#"{"url":"http://localhost/hooks?event=old&x=1&matcher=9","headers":{"authorization":"Bearer fixture"},"events":{"PreToolUse":[{"matcher":"read_file","timeout":2},{"matcher":""}],"not-an-event":[]}}"#).unwrap();
        let mut merged = vec![(
            "PreToolUse".into(),
            vec![HookMatcher {
                plugin_name: Some("native".into()),
                ..Default::default()
            }],
        )];
        merge_settings(&mut merged, sdk_hook_settings(Some(&config)));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].1.len(), 3);
        assert_eq!(merged[0].1[0].plugin_name, Some("native".into()));
        assert_eq!(merged[0].1[1].matcher, Some("read_file".into()));
        assert_eq!(merged[0].1[2].matcher, None);
        let HookCommand::Http {
            url,
            headers,
            common,
            ..
        } = &merged[0].1[1].hooks[0]
        else {
            panic!("HTTP adapter")
        };
        assert_eq!(
            url,
            &"http://localhost/hooks?x=1&event=PreToolUse&matcher=0".into()
        );
        assert_eq!(
            headers.as_ref().unwrap().get("authorization"),
            Some(&"Bearer fixture".into())
        );
        // The SDK callback host, not the native HTTP-hook executor, owns this timeout.
        assert_eq!(common.timeout, None);
    }

    #[tokio::test]
    async fn serialized_sdk_config_runs_native_http_hook_and_preserves_sdk_projection() {
        use crate::runtime::async_hooks::{AsyncHookStatus, AsyncHookTurn, LiveAsyncHookBindings};
        use crate::runtime::hooks::{
            GatewayAsyncHookRegistrationBridge, GatewayHookExecutionProjection,
            HookEventProjection, HookRuntime, HookRuntimeOptions, HookRuntimeRunInput,
            HostedHookFuture, HostedHookHandler, HostedHookMatcher, HostedHookServer,
        };
        use std::sync::{Arc, Mutex};
        struct Callback;
        impl HostedHookHandler for Callback {
            fn call(
                &self,
                _: JsString,
                _: usize,
                input: pilotdeck_protocol::JsObject,
                _: crate::runtime::hooks::Cancellation,
                _: JsString,
            ) -> HostedHookFuture {
                assert_eq!(input.get("session_id"), Some(&"sdk-session".into()));
                Box::pin(async { Ok(parse_json(r#"{"async":true,"asyncTimeout":2.5}"#).unwrap()) })
            }
        }
        let server = HostedHookServer::new(
            vec![(
                "PreModelRequest".into(),
                vec![HostedHookMatcher {
                    matcher: None,
                    timeout_seconds: None,
                }],
            )],
            Arc::new(Callback),
        )
        .unwrap();
        let config = server.start(Default::default()).await.unwrap();
        let bindings = LiveAsyncHookBindings::default();
        let mailbox = crate::runtime::steer::SteerMailbox::default();
        mailbox.start("run".into());
        let events = Arc::new(Mutex::new(Vec::new()));
        let delivery = events.clone();
        let guard = bindings.bind(
            "sdk-session".into(),
            AsyncHookTurn {
                run_id: "run".into(),
                mailbox,
            },
            move |event| delivery.lock().unwrap().push(event),
        );
        let runtime = HookRuntime::with_options(
            sdk_hook_settings(Some(&config.to_value())),
            HookRuntimeOptions {
                on_async_registered: Some(Arc::new(GatewayAsyncHookRegistrationBridge {
                    registry: bindings.registry.clone(),
                    session_key: "sdk-session".into(),
                    include_hook_events: true,
                })),
                ..Default::default()
            },
        );
        let projection = events.clone();
        let subscription = runtime
            .subscribe_execution_events(Arc::new(GatewayHookExecutionProjection::new(
                "sdk-session".into(),
                HookEventProjection::Sdk,
                move |event| {
                    projection.lock().unwrap().push(event);
                    true
                },
            )))
            .unwrap();
        runtime.run(HookRuntimeRunInput { event: "PreModelRequest".into(), hook_input: parse_json(r#"{"hookEventName":"PreModelRequest","sessionId":"sdk-session","transcriptPath":"","cwd":"/tmp"}"#).unwrap().as_object().unwrap().clone(), match_query: None, cwd: "/tmp".into(), env: None, signal: None }).await.unwrap();
        let invocation = {
            let events = events.lock().unwrap();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].get("type"), Some(&"hook_started".into()));
            assert_eq!(events[1].get("type"), Some(&"hook_response".into()));
            assert_eq!(events[1].get("outcome"), Some(&"success".into()));
            assert_eq!(
                events[1].get("asyncTimeoutMs"),
                Some(&JsValue::Number(2500.0))
            );
            events[1]
                .get("asyncInvocationId")
                .and_then(JsValue::as_string)
                .unwrap()
                .clone()
        };
        assert_eq!(
            bindings
                .registry
                .submit("sdk-session".into(), invocation, parse_json("{}").unwrap())
                .await
                .unwrap()
                .status,
            AsyncHookStatus::Delivered
        );
        assert_eq!(
            events.lock().unwrap()[2].get("type"),
            Some(&"hook_async_result".into())
        );
        subscription.dispose();
        drop(guard);
        runtime.dispose().await;
        server.close().await;
    }

    #[tokio::test]
    async fn successful_write_and_edit_dispatch_file_changed_after_commit() {
        use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
        use crate::runtime::{
            hooks::*,
            tools::{
                ExecutableTool, ToolRunContext,
                builtin::{edit_file::EditFileTool, write_file::WriteFileTool},
            },
        };
        use std::sync::Arc;
        struct Calls(tokio::sync::mpsc::UnboundedSender<pilotdeck_protocol::JsObject>);
        impl HostedHookHandler for Calls {
            fn call(
                &self,
                _: JsString,
                _: usize,
                input: pilotdeck_protocol::JsObject,
                _: Cancellation,
                _: JsString,
            ) -> HostedHookFuture {
                let absolute = input
                    .get("absolute_path")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                assert!(
                    std::fs::read_to_string(absolute).is_ok(),
                    "notification follows durable file write"
                );
                let _ = self.0.send(input);
                Box::pin(async { Ok(parse_json("{}").unwrap()) })
            }
        }
        let scratch = crate::scratch::Scratch::new("file-changed-hook");
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let server = HostedHookServer::new(
            vec![(
                "FileChanged".into(),
                vec![HostedHookMatcher {
                    matcher: Some("note.txt".into()),
                    timeout_seconds: None,
                }],
            )],
            Arc::new(Calls(sent)),
        )
        .unwrap();
        let endpoint = server.start(Default::default()).await.unwrap();
        let lifecycle = LifecycleRuntime::new(HookRuntime::new(sdk_hook_settings(Some(
            &endpoint.to_value(),
        ))));
        let cwd_path = scratch.path().join("primary");
        let additional_path = scratch.path().join("additional");
        std::fs::create_dir_all(&cwd_path).unwrap();
        std::fs::create_dir_all(&additional_path).unwrap();
        let cwd: JsString = cwd_path.to_string_lossy().as_ref().into();
        let root: JsString = additional_path.to_string_lossy().as_ref().into();
        let absolute = additional_path
            .join("note.txt")
            .to_string_lossy()
            .to_string();
        let fields = parse_json(&format!(
            r#"{{"sessionId":"s","turnId":"t","cwd":{},"permissionContext":{{"additionalWorkingDirectories":[{}]}}}}"#,
            serde_json::to_string(&cwd.to_utf8().unwrap()).unwrap(),
            serde_json::to_string(&root.to_utf8().unwrap()).unwrap()
        ))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
        let context = ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields,
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: Some(Arc::new(FileUpdateHook {
                lifecycle: lifecycle.clone(),
                session_key: "s".into(),
                cwd: cwd.clone(),
            })),
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }));
        WriteFileTool::new()
            .execute(
                parse_json(&format!(
                    r#"{{"file_path":{},"content":"before"}}"#,
                    serde_json::to_string(&absolute).unwrap()
                ))
                .unwrap(),
                context.clone(),
            )
            .await
            .unwrap();
        let created = received.recv().await.unwrap();
        assert_eq!(created.get("change_type"), Some(&"created".into()));
        assert_eq!(created.get("file_path"), Some(&"note.txt".into()));
        assert_eq!(
            created.get("absolute_path"),
            Some(&JsValue::String(absolute.as_str().into()))
        );
        assert_eq!(created.get("root"), Some(&JsValue::String(root.clone())));
        assert_eq!(created.get("cwd"), Some(&JsValue::String(cwd)));
        // The shared write checkpoint already represents the successful write.
        EditFileTool::new()
            .execute(
                parse_json(&format!(
                    r#"{{"file_path":{},"old_string":"before","new_string":"after"}}"#,
                    serde_json::to_string(&absolute).unwrap()
                ))
                .unwrap(),
                context,
            )
            .await
            .unwrap();
        let updated = received.recv().await.unwrap();
        assert_eq!(updated.get("change_type"), Some(&"updated".into()));
        assert_eq!(updated.get("root"), Some(&JsValue::String(root)));
        assert_eq!(std::fs::read_to_string(absolute).unwrap(), "after");
        lifecycle.dispose().await;
        server.close().await;
    }
}
