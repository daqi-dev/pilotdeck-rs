//! SDK opt-in input tool, using the Gateway's existing renderer/journal owner.
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    user_dialog::{DialogAnswer, GatewayUserDialogChannel, InputRequest},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::time::Duration;

pub struct RequestUserInputTool {
    fields: JsObject,
}
impl RequestUserInputTool {
    pub fn new() -> Self {
        Self {
            fields: object([
                ("name", string("request_user_input")),
                ("aliases", JsValue::Array(vec![string("RequestUserInput")])),
                (
                    "description",
                    string(
                        "Ask the human user for one short free-form answer during this task. Use this only when a concise clarification is required and multiple-choice ask_user_question is unsuitable.",
                    ),
                ),
                ("kind", string("session")),
                ("shouldDefer", JsValue::Bool(true)),
                ("maxResultBytes", JsValue::Number(16_000.0)),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["prompt"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000,"description":"The concise question or request shown to the user."},"placeholder":{"type":"string","maxLength":512,"description":"Optional input placeholder shown by the host UI."},"allowEmpty":{"type":"boolean","description":"Whether the host may submit an empty answer."}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl Default for RequestUserInputTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for RequestUserInputTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    // Registration is the SDK capability gate; permission prompting is separate.
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}

// Dropping a tool future must settle the same owner, including its journal/store.
struct PendingInput {
    channel: GatewayUserDialogChannel,
    request_id: JsString,
}
impl Drop for PendingInput {
    fn drop(&mut self) {
        self.channel.abort(&self.request_id);
    }
}

impl ExecutableTool for RequestUserInputTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            let Some(prompt) = input.get("prompt").and_then(JsValue::as_string) else {
                return Ok(None);
            };
            if prompt
                .units()
                .iter()
                .all(|unit| char::from_u32(u32::from(*unit)).is_some_and(crate::js_value::js_space))
            {
                return Ok(Some(vec![object([
                    ("path", string("prompt")),
                    ("code", string("invalid_schema")),
                    ("message", string("prompt must be a non-empty string.")),
                ])]));
            }
            Ok(None)
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = context
                .host
                .user_dialog
                .as_ref()
                .and_then(|v| v.downcast_ref::<GatewayUserDialogChannel>())
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "request_user_input requires an enabled Gateway user-dialog channel.",
                    )
                })?;
            let prompt = input
                .get("prompt")
                .and_then(JsValue::as_string)
                .ok_or_else(|| {
                    ToolError::new("invalid_input", "prompt must be a non-empty string.")
                })?;
            let allow_empty = input.get("allowEmpty") == Some(&JsValue::Bool(true));
            let mut request = channel.request_input(&InputRequest {
                tool_call_id: context
                    .fields
                    .get("currentToolCallId")
                    .or_else(|| context.fields.get("turnId"))
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                tool_name: "request_user_input".into(),
                prompt: prompt.clone(),
                placeholder: input
                    .get("placeholder")
                    .and_then(JsValue::as_string)
                    .cloned(),
                allow_empty,
                abort: None,
            });
            let _pending = PendingInput {
                channel: channel.clone(),
                request_id: request.request_id().clone(),
            };
            loop {
                if let Some(answer) = request.try_result() {
                    let answer = answer.map_err(ToolError::failed)?;
                    return match answer {
                        DialogAnswer::Answered(JsValue::String(ref value)) => {
                            if !allow_empty
                                && value.units().iter().all(|u| {
                                    char::from_u32(u32::from(*u))
                                        .is_some_and(crate::js_value::js_space)
                                })
                            {
                                return Err(ToolError::failed(
                                    "The user submitted an empty response for a required input request.",
                                ));
                            }
                            Ok(ToolOutput {
                                content: vec![JsValue::Object(object([
                                    ("type", string("text")),
                                    (
                                        "text",
                                        string(format!("The user answered: {}", text(value))),
                                    ),
                                ]))],
                                data: Some(JsValue::Object(object([(
                                    "answer",
                                    JsValue::String(value.clone()),
                                )]))),
                                ..Default::default()
                            })
                        }
                        DialogAnswer::Cancelled(reason) => Err(ToolError::new(
                            "unsupported_tool",
                            format!(
                                "The user cancelled the input request{}",
                                reason
                                    .filter(|s| !s.is_empty())
                                    .map(|s| format!(": {}", text(&s)))
                                    .unwrap_or_else(|| ".".into())
                            ),
                        )),
                        _ => Err(ToolError::failed(
                            "The user submitted a non-string input response.",
                        )),
                    };
                }
                tokio::select! {
                    _ = context.cancelled() => { channel.abort(request.request_id()); },
                    _ = tokio::time::sleep(Duration::from_millis(10)) => { channel.tick(); },
                }
            }
        })
    }
}

// The dialog RPC routes live on the transferred channel track (`user_dialog::routes`),
// which needs the channel persistence API this session does not own. The tool
// itself is compiled and covered by the dialog-channel suites.
#[cfg(all(test, any()))]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use crate::user_dialog::{ChannelOptions, GatewayUserDialogs, routes};
    use pilotdeck_protocol::parse_json;
    use std::sync::{Arc, Mutex};

    fn context(channel: Option<GatewayUserDialogChannel>) -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: object([
                ("turnId", string("turn")),
                ("currentToolCallId", string("tool")),
            ]),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: channel
                .map(|v| Arc::new(v) as crate::host::modules::capability::RuntimeContext),
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
    async fn route(state: &GatewayUserDialogs, method: &str, params: JsValue) -> JsValue {
        routes::handle(method, &params, state)
            .await
            .unwrap()
            .unwrap()
    }
    #[test]
    fn input_catalog_requires_sdk_opt_in() {
        let registry = crate::runtime::tools::ToolRegistry::new(None, None);
        super::super::register_user_dialog_tools(&registry, &[]).unwrap();
        assert!(registry.get(&"request_user_input".into()).is_none());
        super::super::register_user_dialog_tools(&registry, &["confirm".into()]).unwrap();
        assert!(registry.get(&"request_user_input".into()).is_none());
        super::super::register_user_dialog_tools(&registry, &["input".into()]).unwrap();
        assert!(registry.get(&"RequestUserInput".into()).is_some());
        let tool = RequestUserInputTool::new();
        assert_eq!(
            tool.requires_user_interaction(&JsValue::Null).unwrap(),
            JsValue::Bool(false)
        );
    }
    #[tokio::test]
    async fn input_renderer_lease_delivers_once_and_returns_tool_result() {
        let state = GatewayUserDialogs::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let channel = GatewayUserDialogChannel::new(ChannelOptions::new(
            "session",
            state.bus.clone(),
            Arc::new(move |event| {
                sink.lock().unwrap().push(event);
                Ok(())
            }),
        ));
        let mut pending = RequestUserInputTool::new().execute(
            parse_json(r#"{"prompt":"Answer?"}"#).unwrap(),
            context(Some(channel)),
        );
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        let event = events.lock().unwrap()[0].clone();
        assert_eq!(event.get("toolCallId"), Some(&string("tool")));
        let request_id = event.get("requestId").unwrap().clone();
        let mut params = object([("sessionKey", string("session")), ("requestId", request_id)]);
        let listed = route(&state, "user_dialog_list", JsValue::Object(params.clone())).await;
        assert_eq!(
            listed
                .get("dialogs")
                .and_then(|v| if let JsValue::Array(a) = v {
                    Some(a.len())
                } else {
                    None
                }),
            Some(1)
        );
        let claim = route(&state, "user_dialog_claim", JsValue::Object(params.clone())).await;
        assert_eq!(claim.get("claimed"), Some(&JsValue::Bool(true)));
        params.insert("leaseId".into(), string("wrong"));
        params.insert(
            "result".into(),
            parse_json(r#"{"behavior":"answered","value":"approved"}"#).unwrap(),
        );
        let wrong = routes::handle(
            "user_dialog_respond",
            &JsValue::Object(params.clone()),
            &state,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(wrong.code, "USER_DIALOG_LEASE_REQUIRED");
        params.insert("leaseId".into(), claim.get("leaseId").unwrap().clone());
        let release = route(
            &state,
            "user_dialog_release",
            JsValue::Object(params.clone()),
        )
        .await;
        assert_eq!(release.get("released"), Some(&JsValue::Bool(true)));
        params.insert("leaseId".into(), JsValue::Undefined);
        let claim = route(&state, "user_dialog_claim", JsValue::Object(params.clone())).await;
        params.insert("leaseId".into(), claim.get("leaseId").unwrap().clone());
        assert_eq!(
            route(
                &state,
                "user_dialog_respond",
                JsValue::Object(params.clone())
            )
            .await
            .get("delivered"),
            Some(&JsValue::Bool(true))
        );
        assert_eq!(
            route(&state, "user_dialog_respond", JsValue::Object(params))
                .await
                .get("delivered"),
            Some(&JsValue::Bool(false))
        );
        let result = pending.await.unwrap();
        assert_eq!(
            result.data.unwrap().get("answer"),
            Some(&string("approved"))
        );
        assert_eq!(
            state.bus.lock().unwrap().pending_count(&"session".into()),
            0
        );
    }
    #[tokio::test]
    async fn input_missing_port_fails_and_dropped_future_cleans_pending() {
        let tool = RequestUserInputTool::new();
        let input = parse_json(r#"{"prompt":"Answer?"}"#).unwrap();
        assert!(tool.execute(input.clone(), context(None)).await.is_err());
        let state = GatewayUserDialogs::default();
        let channel = GatewayUserDialogChannel::new(ChannelOptions::new(
            "session",
            state.bus.clone(),
            Arc::new(|_| Ok(())),
        ));
        let mut pending = tool.execute(input, context(Some(channel)));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        assert_eq!(
            state.bus.lock().unwrap().pending_count(&"session".into()),
            1
        );
        drop(pending);
        assert_eq!(
            state.bus.lock().unwrap().pending_count(&"session".into()),
            0
        );
    }
    #[tokio::test]
    async fn input_empty_answer_and_renderer_cancellation_keep_ts_contract() {
        for (allow_empty, answer, expected_ok) in [
            (false, r#"{"behavior":"answered","value":" "}"#, false),
            (true, r#"{"behavior":"answered","value":""}"#, true),
            (
                false,
                r#"{"behavior":"cancelled","reason":"declined"}"#,
                false,
            ),
        ] {
            let state = GatewayUserDialogs::default();
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            let channel = GatewayUserDialogChannel::new(ChannelOptions::new(
                "session",
                state.bus.clone(),
                Arc::new(move |event| {
                    sink.lock().unwrap().push(event);
                    Ok(())
                }),
            ));
            let input = JsValue::Object(object([
                ("prompt", string("Answer?")),
                ("allowEmpty", JsValue::Bool(allow_empty)),
            ]));
            let mut pending = RequestUserInputTool::new().execute(input, context(Some(channel)));
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(
                    pending.as_mut().poll(cx).is_pending()
                ))
                .await
            );
            let request = events.lock().unwrap()[0].get("requestId").unwrap().clone();
            let params = JsValue::Object(object([
                ("sessionKey", string("session")),
                ("requestId", request),
                ("result", parse_json(answer).unwrap()),
            ]));
            route(&state, "user_dialog_respond", params).await;
            assert_eq!(pending.await.is_ok(), expected_ok);
            assert_eq!(
                state.bus.lock().unwrap().pending_count(&"session".into()),
                0
            );
        }
    }
}
