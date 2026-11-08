//! One-shot observer admission uses the same detached fork owner and body.
use super::observer::{ObserverActivity, ObserverInput, ObserverOutcome};
use super::*;

pub(super) struct ObserverLaunch {
    pub factory: Arc<ProjectSessionFactory>,
    pub definition: String,
    pub subagent: JsString,
    pub observer: JsObject,
    pub directive: JsString,
    pub observer_message: Option<JsString>,
    pub activity: ObserverActivity,
    pub outcome: ObserverOutcome,
    pub depth: f64,
    pub max: f64,
    pub context: ToolRunContext,
}

/// Match the OneShot caller catch: a rejected success-path observer admission
/// is retried once with failure outcome. A failed observed run already entered
/// that catch and therefore gets only one observer admission attempt.
pub(super) fn launch_with_retry(
    outcome: ObserverOutcome,
    mut launch: impl FnMut(ObserverOutcome) -> Result<(), ToolError>,
) -> Result<(), ToolError> {
    let observed_success = outcome.success;
    let observed_error = outcome.error.clone();
    let first = match launch(outcome) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if !observed_success {
        return Err(observer_launch_aggregate(
            observed_error.unwrap_or_else(|| "Subagent failed.".into()),
            first.message,
        ));
    }
    let retry = ObserverOutcome {
        success: false,
        markdown: None,
        error: Some(first.message.clone()),
    };
    match launch(retry) {
        Ok(()) => Err(first),
        Err(second) => Err(observer_launch_aggregate(first.message, second.message)),
    }
}
fn observer_launch_aggregate(first: JsString, second: JsString) -> ToolError {
    let mut failure = error("Subagent failed and its observer could not be launched.");
    failure.details = Some(
        object([
            ("name", "AggregateError".into()),
            (
                "errors",
                JsValue::Array(
                    [first, second]
                        .into_iter()
                        .map(|message| {
                            object([
                                ("name", "Error".into()),
                                ("message", JsValue::String(message)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
        .as_object()
        .unwrap()
        .clone(),
    );
    failure
}

impl ForkApi {
    pub(super) fn observer_definition(
        &self,
        spec: &JsObject,
        definition: &str,
    ) -> Result<Option<JsObject>, ToolError> {
        let Some(value) = spec
            .get("observer")
            .filter(|value| crate::runtime::mcp::sdk::js_truthy(value))
        else {
            return Ok(None);
        };
        let id = match value {
            JsValue::String(value) => value.clone(),
            _ => JsString::from_utf16(
                crate::js_value::to_string_units_checked(value)
                    .map_err(|_| error("Cannot convert observer definition to a property key."))?,
            ),
        };
        let available = definitions(&self.turn.config);
        let selected = available
            .get_key(&id)
            .and_then(JsValue::as_object)
            .cloned()
            .ok_or_else(|| {
                error(concat(&[
                    &"Observer definition ".into(),
                    &id,
                    &" for ".into(),
                    &definition.into(),
                    &" is not configured.".into(),
                ]))
            })?;
        if !self
            .turn
            .owner
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|owner| owner.is_open())
        {
            let name = string(&selected, "id").unwrap_or(id);
            return Err(error(concat(&[
                &"Observer definition ".into(),
                &name,
                &" requires a host observer-subagent launcher.".into(),
            ])));
        }
        Ok(Some(selected))
    }
    pub(super) fn launch_observer(
        self: &Arc<Self>,
        launch: ObserverLaunch,
    ) -> Result<(), ToolError> {
        let ObserverLaunch {
            factory,
            definition,
            subagent,
            observer,
            directive,
            observer_message,
            activity,
            outcome,
            depth,
            max,
            context,
        } = launch;
        let observer_type = string(&observer, "id")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| error("Observer definition must have a nonempty id."))?;
        let description = string(&observer, "description").unwrap_or_else(|| observer_type.clone());
        let timeout = context
            .fields
            .get("subagentTimeoutMs")
            .and_then(JsValue::as_number);
        let input = ObserverInput {
            session_id: self.turn.parent.session_id.clone(),
            parent_turn_id: self.turn.parent.turn_id.clone(),
            observed_subagent_id: subagent,
            observed_subagent_type: definition.as_str().into(),
            observer_subagent_id: uuid::Uuid::new_v4().to_string().into(),
            definition: observer.clone(),
            directive,
            activity,
            observer_message,
            timeout_ms: timeout,
            outcome,
        };
        self.launch_detached(super::background::DetachedFork {
            factory,
            permit: None,
            definition: String::from_utf16_lossy(observer_type.units()),
            spec: observer,
            description,
            directive: JsString::default(),
            parent_mode: "plan".into(),
            parent_ask: true,
            depth,
            max,
            context,
            observer: Some(input),
        })
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
    fn make_api(config: JsObject) -> Arc<ForkApi> {
        let turn = ForkTurn::new(
            NativeForkRegistry::new(),
            ExecutionIdentity {
                request_id: "request".into(),
                run_id: "run".into(),
                operation_id: "operation".into(),
                session_id: "parent".into(),
                turn_id: "turn".into(),
            },
            config,
            crate::runtime::turn::live::test_recorder(),
            RuleSetHandle::empty(),
            None,
            None,
            ToolRegistry::new(None, None),
            super::super::super::skills::NativeSkills::default(),
            None,
            #[cfg(unix)]
            None,
        );
        Arc::new(ForkApi {
            turn,
            read: Arc::default(),
            write: Arc::default(),
        })
    }
    #[test]
    fn observer_preflight_rejects_missing_definition_and_launcher_before_fork() {
        let api = make_api(JsObject::new());
        let spec = object([("observer", "missing".into())])
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            api.observer_definition(&spec, "research")
                .unwrap_err()
                .message,
            "Observer definition missing for research is not configured.".into()
        );
        assert!(lock(&api.turn.registry.scopes).is_empty());
        assert!(lock(&api.turn.jobs).is_empty());
        let definition = object([("id", "watcher".into())]);
        let config = object([("subagentDefinitions", object([("watcher", definition)]))])
            .as_object()
            .unwrap()
            .clone();
        let api = make_api(config);
        let spec = object([("observer", "watcher".into())])
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            api.observer_definition(&spec, "research")
                .unwrap_err()
                .message,
            "Observer definition watcher requires a host observer-subagent launcher.".into()
        );
        assert!(lock(&api.turn.registry.scopes).is_empty());
    }
    #[tokio::test]
    async fn observer_scope_consumes_host_identity_and_keeps_empty_tools_plan_memory_disabled() {
        let api = make_api(
            object([
                ("cwd", "/fixture-observer".into()),
                ("permissionMode", "bypassPermissions".into()),
            ])
            .as_object()
            .unwrap()
            .clone(),
        );
        let resources = ProjectRuntimeRegistry::new(
            "/fixture-observer",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        api.turn
            .bind_generation(Arc::new(resources.acquire(None).unwrap()))
            .unwrap();
        let spec = object([
            ("id", "watcher".into()),
            ("allowedTools", JsValue::Array(vec![])),
            ("disallowedTools", JsValue::Array(vec![])),
            ("permissionMode", "plan".into()),
            ("isReadOnly", JsValue::Bool(true)),
            ("memory", "disabled".into()),
            ("nativeObserver", JsValue::Bool(true)),
            ("mcpServers", JsValue::Undefined),
        ])
        .as_object()
        .unwrap()
        .clone();
        let (scope, context, _) = api
            .compose_child(
                "watcher",
                &spec,
                &"plan".into(),
                true,
                0.0,
                4.0,
                &"child-session".into(),
                &"child-id".into(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(scope.permission_mode, "plan".into());
        assert_eq!(context.allowed_tools, Some(vec![]));
        for key in ["nativeObserver", "nativeMemoryDisabled"] {
            assert_eq!(scope.config.get(key), Some(&JsValue::Bool(true)));
        }
        assert_eq!(
            scope.config.get("runMode"),
            Some(&JsValue::String("ask".into()))
        );
        assert!(
            scope
                .config
                .get("nativeForkMcpServers")
                .is_some_and(JsValue::is_undefined)
        );
        let normal = object([("allowedTools", JsValue::Array(vec![]))])
            .as_object()
            .unwrap()
            .clone();
        let (normal, _, _) = api
            .compose_child(
                "research",
                &normal,
                &"default".into(),
                false,
                0.0,
                4.0,
                &"normal-session".into(),
                &"normal-id".into(),
                None,
                false,
            )
            .unwrap();
        assert!(normal.config.get("nativeObserver").is_none());
        drop(scope);
        drop(normal);
        drop(api);
        resources.dispose().await.unwrap();
    }
}

#[cfg(test)]
mod caller_retry_tests {
    use super::*;
    use serde_json::Value;
    #[test]
    fn actual_ts_caller_retries_only_success_path_launch_failure_and_keeps_tool_failure() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/native-process/observer-caller-oracle.json"
        ))
        .unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let mode = case["mode"].as_str().unwrap();
            let observed = case["completed"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["subagentId"] == "observed-fixed")
                .unwrap();
            let observed_success = observed["errored"] == false;
            let failures = match mode {
                "observer-launch-fails" | "observer-key-only" => 2,
                "observer-first-launch-fails-once" => 1,
                _ => 0,
            };
            let messages: Vec<String> = case["result"]["errors"]
                .as_array()
                .map(|errors| {
                    errors
                        .iter()
                        .map(|error| error["message"].as_str().unwrap().to_owned())
                        .collect()
                })
                .unwrap_or_else(|| vec!["oracle launcher rejection".to_owned()]);
            let mut attempts = vec![];
            let result = launch_with_retry(
                ObserverOutcome {
                    success: observed_success,
                    markdown: observed_success.then(|| "SELF_WRITTEN_CONTROLLED_REPORT".into()),
                    error: (!observed_success)
                        .then(|| JsString::from(case["result"]["message"].as_str().unwrap())),
                },
                |outcome| {
                    attempts.push(outcome);
                    let index = attempts.len() - 1;
                    if index < failures {
                        Err(error(messages[index.min(messages.len() - 1)].as_str()))
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(
                attempts.len(),
                case["launches"].as_array().unwrap().len(),
                "{mode}"
            );
            assert_eq!(
                observed_success && result.is_ok(),
                case["result"]["ok"].as_bool().unwrap(),
                "{mode}"
            );
            if let Err(error) = result {
                assert_eq!(
                    error.message.to_utf8().unwrap(),
                    case["result"]["message"].as_str().unwrap(),
                    "{mode}"
                );
            }
            assert_eq!(attempts[0].success, observed_success, "{mode}");
            if attempts.len() == 2 {
                assert!(!attempts[1].success, "{mode}");
                assert_eq!(
                    attempts[1].error.as_ref().unwrap().to_utf8().unwrap(),
                    messages[0],
                    "{mode}"
                );
            }
        }
    }
}
