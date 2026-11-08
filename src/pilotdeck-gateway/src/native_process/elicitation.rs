//! Turn-owned question delivery using the Gateway's existing pending/reconnect bus.
use crate::runtime::{
    interaction::{GatewayInteractionCoordinator, PendingEntry, Registration},
    tools::{ToolError, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct Elicitation {
    pub profile: Arc<std::sync::OnceLock<crate::runtime::project::InteractionProfile>>,
    pub coordinator: Arc<GatewayInteractionCoordinator>,
    pub session: JsString,
    pub can_prompt: bool,
    pub timeout: std::time::Duration,
    pub emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
}
struct Pending(Registration);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.dispose();
    }
}
fn object<const N: usize>(pairs: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in pairs {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}
impl Elicitation {
    pub async fn ask(
        &self,
        tool_call_id: JsString,
        questions: JsValue,
        metadata: Option<JsValue>,
        context: &ToolRunContext,
    ) -> Result<JsValue, ToolError> {
        let profile = self.profile.get().ok_or_else(|| {
            ToolError::new("tool_execution_failed", "Interaction profile is not bound.")
        })?;
        if profile.question_provider == "disabled" {
            return Err(ToolError::new(
                "unsupported_tool",
                "ask_user_question requires a host elicitation channel (none registered).",
            ));
        }
        if !self.can_prompt || !profile.can_prompt {
            return Ok(object([
                ("type", "cancelled".into()),
                ("reason", "Prompts are disabled for this session.".into()),
            ]));
        }
        if profile.question_provider == "deterministic" {
            let mut answers = JsObject::new();
            if let JsValue::Array(items) = &questions {
                for question in items {
                    let Some(key) = question.get("question").and_then(JsValue::as_string) else {
                        continue;
                    };
                    let answer = match question.get("options") {
                        Some(JsValue::Array(options)) if !options.is_empty() => options[0]
                            .get("label")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                        _ => "yes".into(),
                    };
                    let answer = if question.get("multiSelect") == Some(&JsValue::Bool(true)) {
                        JsValue::Array(vec![answer])
                    } else {
                        answer
                    };
                    answers.insert(key.clone(), answer);
                }
            }
            return Ok(object([
                ("type", "answered".into()),
                ("answers", JsValue::Object(answers)),
            ]));
        }
        let request_id: JsString = uuid::Uuid::new_v4().to_string().as_str().into();
        let (send, receive) = oneshot::channel::<Result<JsValue, JsString>>();
        let sender = Arc::new(Mutex::new(Some(send)));
        let on_answer = sender.clone();
        let on_reject = sender;
        let entry = PendingEntry {
            request_id: request_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: "ask_user_question".into(),
            resolve: Arc::new(move |answer| {
                if let Some(tx) = on_answer.lock().unwrap().take() {
                    let _ = tx.send(Ok(answer));
                }
            }),
            reject: Arc::new(move |error| {
                if let Some(tx) = on_reject.lock().unwrap().take() {
                    let _ = tx.send(Err(error));
                }
            }),
        };
        let mut payload = JsObject::new();
        payload.insert("toolCallId".into(), JsValue::String(tool_call_id));
        payload.insert("toolName".into(), "ask_user_question".into());
        payload.insert("questions".into(), questions);
        if let Some(metadata) = metadata {
            payload.insert("metadata".into(), metadata);
        }
        let registration = self
            .coordinator
            .elicitation_bus()
            .register(&self.session, entry, Some(JsValue::Object(payload.clone())))
            .map_err(|e| ToolError::new("tool_execution_failed", e))?;
        let _pending = Pending(registration);
        payload.insert("type".into(), "elicitation_request".into());
        payload.insert("requestId".into(), JsValue::String(request_id.clone()));
        if !(self.emit)(JsValue::Object(payload)) {
            return Err(ToolError::new(
                "tool_execution_failed",
                "Elicitation delivery failed.",
            ));
        }
        let mut receive = receive;
        let reason = tokio::select! {
            biased;
            result = &mut receive => return match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(ToolError::new("tool_execution_failed", error)),
                Err(_) => Err(ToolError::new("tool_execution_failed", "Elicitation channel closed.")),
            },
            _ = context.cancelled() => "aborted",
            _ = tokio::time::sleep(self.timeout) => "timeout",
        };
        if self
            .coordinator
            .elicitation_bus()
            .consume(&self.session, &request_id, None)
            .is_none()
        {
            return match receive.await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(ToolError::new("tool_execution_failed", error)),
                Err(_) => Err(ToolError::new(
                    "tool_execution_failed",
                    "Elicitation channel closed.",
                )),
            };
        }
        let _ = (self.emit)(object([
            ("type", "elicitation_cancelled".into()),
            ("requestId", JsValue::String(request_id)),
            ("reason", reason.into()),
        ]));
        Ok(object([
            ("type", "cancelled".into()),
            ("reason", reason.into()),
        ]))
    }
}

#[cfg(test)]
pub(crate) fn test_profile() -> Arc<std::sync::OnceLock<crate::runtime::project::InteractionProfile>>
{
    let cell = Arc::new(std::sync::OnceLock::new());
    let profile =
        crate::runtime::project::RuntimeProfile::resolve(&JsValue::Undefined, &Default::default())
            .interaction;
    cell.set(profile).expect("fresh profile");
    cell
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use crate::runtime::network::Cancellation;
    use std::time::Duration;
    fn context(signal: Cancellation) -> ToolRunContext {
        let checkpoint = HostToolCheckpoint::new(None, &[]).unwrap();
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: checkpoint.tool_context_state(),
            abort_signal: Some(Arc::new(signal)),
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn same_bus_response_cancel_and_timeout_cleanup() {
        for action in ["answer", "abort", "timeout"] {
            let coordinator = Arc::new(GatewayInteractionCoordinator::default());
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let channel = Elicitation {
                profile: test_profile(),
                coordinator: coordinator.clone(),
                session: "session".into(),
                can_prompt: true,
                emit: Arc::new(move |value| tx.send(value).is_ok()),
                timeout: if action == "timeout" {
                    Duration::from_millis(1)
                } else {
                    Duration::from_secs(5)
                },
            };
            let signal = Cancellation::new();
            let ctx = context(signal.clone());
            let task = tokio::spawn(async move {
                channel
                    .ask("turn".into(), JsValue::Array(vec![]), None, &ctx)
                    .await
            });
            let event = rx.recv().await.unwrap();
            let id = event.get("requestId").unwrap().as_string().unwrap().clone();
            if action == "answer" {
                assert!(coordinator.respond_elicitation(
                    &"session".into(),
                    &id,
                    object([("type", "answered".into())]),
                    None
                ));
            } else if action == "abort" {
                signal.cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                result.get("type"),
                Some(&JsValue::from(if action == "answer" {
                    "answered"
                } else {
                    "cancelled"
                }))
            );
            if action != "answer" {
                assert_eq!(
                    rx.recv().await.unwrap().get("type"),
                    Some(&JsValue::from("elicitation_cancelled"))
                );
            }
            assert_eq!(
                coordinator
                    .elicitation_bus()
                    .pending_count(&"session".into()),
                0
            );
            assert!(!coordinator.respond_elicitation(&"session".into(), &id, JsValue::Null, None));
        }
    }
    #[tokio::test]
    async fn no_answerer_and_failed_delivery_do_not_leave_pending() {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        for can_prompt in [false, true] {
            let channel = Elicitation {
                profile: test_profile(),
                coordinator: coordinator.clone(),
                session: "session".into(),
                can_prompt,
                emit: Arc::new(|_| false),
                timeout: Duration::from_secs(2),
            };
            let ctx = context(Cancellation::new());
            let result = channel
                .ask("turn".into(), JsValue::Array(vec![]), None, &ctx)
                .await;
            if can_prompt {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result.unwrap().get("reason"),
                    Some(&JsValue::from("Prompts are disabled for this session."))
                );
            }
            assert_eq!(
                coordinator
                    .elicitation_bus()
                    .pending_count(&"session".into()),
                0
            );
        }
    }

    #[tokio::test]
    async fn reconnect_replays_pending_and_rejects_old_binding() {
        use crate::runtime::interaction::InteractionBinding;
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let old = InteractionBinding {
            connection_id: "old".into(),
            generation: 1,
        };
        let new = InteractionBinding {
            connection_id: "new".into(),
            generation: 2,
        };
        coordinator
            .reconnect(&"session".into(), Some(old.clone()), None)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let channel = Elicitation {
            profile: test_profile(),
            coordinator: coordinator.clone(),
            session: "session".into(),
            can_prompt: true,
            emit: Arc::new(move |event| tx.send(event).is_ok()),
            timeout: Duration::from_secs(2),
        };
        let ctx = context(Cancellation::new());
        let task = tokio::spawn(async move {
            channel
                .ask("turn".into(), JsValue::Array(vec![]), None, &ctx)
                .await
        });
        let event = rx.recv().await.unwrap();
        let id = event.get("requestId").unwrap().as_string().unwrap().clone();
        let disconnected = coordinator.disconnect(&"session".into(), &old).unwrap();
        assert_eq!(disconnected.get("preserveTurn"), Some(&JsValue::Bool(true)));
        let replay = coordinator
            .reconnect(&"session".into(), Some(new.clone()), Some(&old))
            .unwrap();
        assert_eq!(replay.get("outcome"), Some(&JsValue::from("reconnected")));
        let Some(JsValue::Array(requests)) = replay.get("requests") else {
            panic!("missing replay")
        };
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].get("toolName"),
            Some(&JsValue::from("ask_user_question"))
        );
        assert!(!coordinator.respond_elicitation(
            &"session".into(),
            &id,
            JsValue::Null,
            Some(&old)
        ));
        let expected = object([("type", "answered".into())]);
        assert!(coordinator.respond_elicitation(
            &"session".into(),
            &id,
            expected.clone(),
            Some(&new)
        ));
        assert_eq!(task.await.unwrap().unwrap(), expected);
        assert_eq!(
            coordinator
                .elicitation_bus()
                .pending_count(&"session".into()),
            0
        );
    }

    #[tokio::test]
    async fn leased_headless_profile_uses_ts_deterministic_answers() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/elicitation/oracle-profile.json"
        )))
        .unwrap();
        let Some(JsValue::Array(results)) = fixture.get("results") else {
            panic!("profile oracle missing")
        };
        let questions = pilotdeck_protocol::parse_json(r#"[{"question":"One?","header":"A","options":[{"label":"first","description":"x"},{"label":"second","description":"y"}]},{"question":"Many?","header":"B","multiSelect":true,"options":[{"label":"alpha","description":"x"},{"label":"beta","description":"y"}]}]"#).unwrap();
        let agent = pilotdeck_protocol::parse_json(r#"{"interactionProfile":"headless"}"#).unwrap();
        let profile = crate::runtime::project::RuntimeProfile::resolve(&agent, &Default::default())
            .interaction;
        assert_eq!(profile.question_provider, "deterministic");
        for (can_prompt, expected) in [(true, &results[0]), (false, &results[1])] {
            let cell = Arc::new(std::sync::OnceLock::new());
            cell.set(profile).unwrap();
            let channel = Elicitation {
                profile: cell,
                coordinator: Arc::new(GatewayInteractionCoordinator::default()),
                session: "s".into(),
                can_prompt,
                emit: Arc::new(|_| panic!("deterministic answer must not emit")),
                timeout: Duration::from_secs(1),
            };
            let result = channel
                .ask(
                    "turn".into(),
                    questions.clone(),
                    None,
                    &context(Cancellation::new()),
                )
                .await
                .unwrap();
            assert_eq!(&result, expected.get("result").unwrap());
        }
    }
}
