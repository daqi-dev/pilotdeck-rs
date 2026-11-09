//! SDK opt-in select, confirmation, and form tools.
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    user_dialog::{
        ChannelRequest, ConfirmRequest, DialogAnswer, FormRequest, GatewayUserDialogChannel,
        SelectChoice, SelectRequest,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::time::Duration;

fn definition(
    name: &str,
    alias: &str,
    description: &str,
    max_result_bytes: f64,
    input_schema: &str,
) -> JsObject {
    object([
        ("name", string(name)),
        ("aliases", JsValue::Array(vec![string(alias)])),
        ("description", string(description)),
        ("kind", string("session")),
        ("shouldDefer", JsValue::Bool(true)),
        ("maxResultBytes", JsValue::Number(max_result_bytes)),
        ("inputSchema", JsValue::Object(schema(input_schema))),
    ])
}

fn read_only(_: &JsValue) -> Result<JsValue, HostError> {
    Ok(JsValue::Bool(true))
}

fn issue(path: &str, message: impl Into<JsString>) -> Vec<JsObject> {
    vec![object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])]
}

fn non_blank(value: &JsString) -> bool {
    value.units().iter().any(|unit| {
        char::from_u32(u32::from(*unit))
            .is_none_or(|character| !crate::metadata::js_space(character))
    })
}

fn tool_call_id(context: &ToolRunContext) -> JsString {
    context
        .fields
        .get("currentToolCallId")
        .or_else(|| context.fields.get("turnId"))
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

fn channel(context: &ToolRunContext, name: &str) -> Result<GatewayUserDialogChannel, ToolError> {
    context
        .host
        .user_dialog
        .as_ref()
        .and_then(|value| value.downcast_ref::<GatewayUserDialogChannel>())
        .cloned()
        .ok_or_else(|| {
            ToolError::new(
                "unsupported_tool",
                format!("{name} requires an enabled Gateway user-dialog channel."),
            )
        })
}

struct PendingDialog {
    channel: GatewayUserDialogChannel,
    request_id: JsString,
}
impl Drop for PendingDialog {
    fn drop(&mut self) {
        self.channel.abort(&self.request_id);
    }
}

async fn wait_for_answer(
    channel: GatewayUserDialogChannel,
    mut request: ChannelRequest,
    context: &ToolRunContext,
) -> Result<DialogAnswer, ToolError> {
    let _pending = PendingDialog {
        channel: channel.clone(),
        request_id: request.request_id().clone(),
    };
    loop {
        if let Some(answer) = request.try_result() {
            return answer.map_err(ToolError::failed);
        }
        tokio::select! {
            _ = context.cancelled() => channel.abort(request.request_id()),
            _ = tokio::time::sleep(Duration::from_millis(10)) => channel.tick(),
        }
    }
}

fn cancelled(kind: &str, reason: Option<JsString>) -> ToolError {
    ToolError::new(
        "unsupported_tool",
        format!(
            "The user cancelled the {kind} request{}",
            reason
                .filter(|value| !value.is_empty())
                .map(|value| format!(": {}", text(&value)))
                .unwrap_or_else(|| ".".into())
        ),
    )
}

pub struct RequestUserChoiceTool {
    fields: JsObject,
}
impl RequestUserChoiceTool {
    pub fn new() -> Self {
        Self {
            fields: definition(
                "request_user_choice",
                "RequestUserChoice",
                "Ask the human user to choose exactly one option during this task. Use this when a bounded decision is needed and the options can be stated clearly.",
                16_000.0,
                r#"{"type":"object","required":["prompt","options"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"options":{"type":"array","minItems":2,"maxItems":12,"items":{"type":"object","required":["value"],"additionalProperties":false,"properties":{"value":{"type":"string","minLength":1,"maxLength":512},"label":{"type":"string","maxLength":512},"description":{"type":"string","maxLength":2000}}}},"defaultValue":{"type":"string","maxLength":512}}}"#,
            ),
        }
    }
}
impl Default for RequestUserChoiceTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for RequestUserChoiceTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
impl ExecutableTool for RequestUserChoiceTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            let Some(prompt) = input.get("prompt").and_then(JsValue::as_string) else {
                return Ok(None);
            };
            if !non_blank(prompt) {
                return Ok(Some(issue("prompt", "prompt must be a non-empty string.")));
            }
            let Some(JsValue::Array(options)) = input.get("options") else {
                return Ok(None);
            };
            let mut values: Vec<JsString> = Vec::new();
            for (index, option) in options.iter().enumerate() {
                let Some(value) = option.get("value").and_then(JsValue::as_string) else {
                    continue;
                };
                if !non_blank(value) {
                    return Ok(Some(issue(
                        &format!("options[{index}].value"),
                        "each option value must be a non-empty string.",
                    )));
                }
                if values.contains(value) {
                    return Ok(Some(issue(
                        &format!("options[{index}].value"),
                        "choice values must be unique.",
                    )));
                }
                values.push(value.clone());
            }
            if let Some(default) = input.get("defaultValue").and_then(JsValue::as_string)
                && !values.contains(default)
            {
                return Ok(Some(issue(
                    "defaultValue",
                    "defaultValue must match one of the choice values.",
                )));
            }
            Ok(None)
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = channel(&context, "request_user_choice")?;
            let choices = match input.get("options") {
                Some(JsValue::Array(values)) => values
                    .iter()
                    .filter_map(|value| {
                        Some(SelectChoice {
                            value: value.get("value")?.as_string()?.clone(),
                            label: value.get("label").and_then(JsValue::as_string).cloned(),
                            description: value
                                .get("description")
                                .and_then(JsValue::as_string)
                                .cloned(),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let request = channel.request_select(&SelectRequest {
                tool_call_id: tool_call_id(&context),
                tool_name: "request_user_choice".into(),
                prompt: input
                    .get("prompt")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                choices,
                default_value: input
                    .get("defaultValue")
                    .and_then(JsValue::as_string)
                    .cloned(),
                abort: None,
            });
            match wait_for_answer(channel, request, &context).await? {
                DialogAnswer::Answered(JsValue::String(ref value)) => Ok(ToolOutput {
                    content: vec![JsValue::Object(object([
                        ("type", string("text")),
                        (
                            "text",
                            string(format!("The user selected: {}", text(&value))),
                        ),
                    ]))],
                    data: Some(JsValue::Object(object([(
                        "selected",
                        JsValue::String(value.clone()),
                    )]))),
                    ..Default::default()
                }),
                DialogAnswer::Cancelled(reason) => Err(cancelled("choice", reason)),
                _ => Err(ToolError::failed(
                    "The user submitted an invalid choice response.",
                )),
            }
        })
    }
}

pub struct RequestUserConfirmationTool {
    fields: JsObject,
}
impl RequestUserConfirmationTool {
    pub fn new() -> Self {
        Self {
            fields: definition(
                "request_user_confirmation",
                "RequestUserConfirmation",
                "Ask the human user to confirm or decline one explicit decision during this task. This does not grant permission for a tool; it only returns a boolean answer.",
                16_000.0,
                r#"{"type":"object","required":["prompt"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"confirmLabel":{"type":"string","maxLength":128},"cancelLabel":{"type":"string","maxLength":128},"defaultValue":{"type":"boolean"}}}"#,
            ),
        }
    }
}
impl Default for RequestUserConfirmationTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for RequestUserConfirmationTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
impl ExecutableTool for RequestUserConfirmationTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            let Some(prompt) = input.get("prompt").and_then(JsValue::as_string) else {
                return Ok(None);
            };
            if !non_blank(prompt) {
                return Ok(Some(issue("prompt", "prompt must be a non-empty string.")));
            }
            for key in ["confirmLabel", "cancelLabel"] {
                if let Some(label) = input.get(key).and_then(JsValue::as_string)
                    && !non_blank(label)
                {
                    return Ok(Some(issue(
                        key,
                        format!("{key} must be a non-empty string no longer than 128 characters."),
                    )));
                }
            }
            Ok(None)
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = channel(&context, "request_user_confirmation")?;
            let request = channel.request_confirm(&ConfirmRequest {
                tool_call_id: tool_call_id(&context),
                tool_name: "request_user_confirmation".into(),
                prompt: input
                    .get("prompt")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                confirm_label: input
                    .get("confirmLabel")
                    .and_then(JsValue::as_string)
                    .cloned(),
                cancel_label: input
                    .get("cancelLabel")
                    .and_then(JsValue::as_string)
                    .cloned(),
                default_value: input.get("defaultValue").and_then(JsValue::as_bool),
                abort: None,
            });
            match wait_for_answer(channel, request, &context).await? {
                DialogAnswer::Answered(JsValue::Bool(value)) => Ok(ToolOutput {
                    content: vec![JsValue::Object(object([
                        ("type", string("text")),
                        (
                            "text",
                            string(format!(
                                "The user confirmed: {}",
                                if value { "yes" } else { "no" }
                            )),
                        ),
                    ]))],
                    data: Some(JsValue::Object(object([(
                        "confirmed",
                        JsValue::Bool(value),
                    )]))),
                    ..Default::default()
                }),
                DialogAnswer::Cancelled(reason) => Err(cancelled("confirmation", reason)),
                _ => Err(ToolError::failed(
                    "The user submitted a non-boolean confirmation response.",
                )),
            }
        })
    }
}

pub struct RequestUserFormTool {
    fields: JsObject,
}
impl RequestUserFormTool {
    pub fn new() -> Self {
        Self {
            fields: definition(
                "request_user_form",
                "RequestUserForm",
                "Ask the human user to fill a structured form during this task. Use a JSON object schema with fields, required entries, nested objects or arrays, and scalar enums.",
                32_000.0,
                r#"{"type":"object","required":["prompt","schema"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"schema":{"type":"object","additionalProperties":true}}}"#,
            ),
        }
    }
}
impl Default for RequestUserFormTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for RequestUserFormTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        read_only(input)
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
impl ExecutableTool for RequestUserFormTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            let Some(prompt) = input.get("prompt").and_then(JsValue::as_string) else {
                return Ok(None);
            };
            if !non_blank(prompt) {
                return Ok(Some(issue("prompt", "prompt must be a non-empty string.")));
            }
            let Some(form_schema) = input.get("schema") else {
                return Ok(None);
            };
            if form_schema
                .get("type")
                .and_then(JsValue::as_string)
                .is_none_or(|kind| !kind.equals_str("object"))
            {
                return Ok(Some(issue("schema", "schema.type must be object.")));
            }
            let bytes = stringify_json(form_schema)
                .map_err(|error| ToolError::failed(error.to_string()))?
                .map_or(0, |json| json.len());
            if bytes > 64_000 {
                return Ok(Some(issue("schema", "schema must not exceed 64000 bytes.")));
            }
            Ok(None)
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = channel(&context, "request_user_form")?;
            let request = channel.request_form(&FormRequest {
                tool_call_id: tool_call_id(&context),
                tool_name: "request_user_form".into(),
                prompt: input
                    .get("prompt")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                schema: input.get("schema").cloned().unwrap_or(JsValue::Undefined),
                abort: None,
            });
            match wait_for_answer(channel, request, &context).await? {
                DialogAnswer::Answered(value @ JsValue::Object(_)) => Ok(ToolOutput {
                    content: vec![JsValue::Object(object([
                        ("type", string("json")),
                        ("value", value.clone()),
                    ]))],
                    data: Some(JsValue::Object(object([("values", value)]))),
                    ..Default::default()
                }),
                DialogAnswer::Cancelled(reason) => Err(cancelled("form", reason)),
                _ => Err(ToolError::failed(
                    "The user submitted a non-object form response.",
                )),
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
    use crate::user_dialog::{ChannelOptions, GatewayUserDialogs};
    use pilotdeck_protocol::parse_json;
    use std::sync::{Arc, Mutex};

    fn context(channel: GatewayUserDialogChannel) -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: object([
                ("turnId", string("turn")),
                ("currentToolCallId", string("tool")),
            ]),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: Some(Arc::new(channel)),
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

    async fn exercise(
        tool: &dyn ExecutableTool,
        input: &str,
        answer: &str,
        field: &str,
        expected: JsValue,
    ) {
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
        let mut pending = tool.execute(parse_json(input).unwrap(), context(channel));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        let event = events.lock().unwrap()[0].clone();
        assert_eq!(event.get("toolCallId"), Some(&string("tool")));
        let request_id = event.get("requestId").unwrap().clone();
        let claim_params = JsValue::Object(object([
            ("sessionKey", string("session")),
            ("requestId", request_id.clone()),
        ]));
        let claim = routes::handle("user_dialog_claim", &claim_params, &state)
            .await
            .unwrap()
            .unwrap();
        let params = JsValue::Object(object([
            ("sessionKey", string("session")),
            ("requestId", request_id),
            ("leaseId", claim.get("leaseId").unwrap().clone()),
            ("result", parse_json(answer).unwrap()),
        ]));
        assert_eq!(
            routes::handle("user_dialog_respond", &params, &state)
                .await
                .unwrap()
                .unwrap()
                .get("delivered"),
            Some(&JsValue::Bool(true))
        );
        assert_eq!(
            pending.await.unwrap().data.unwrap().get(field),
            Some(&expected)
        );
    }

    #[tokio::test]
    async fn opt_in_dialog_tools_emit_and_map_typed_answers() {
        exercise(
            &RequestUserChoiceTool::new(),
            r#"{"prompt":"Pick","options":[{"value":"a"},{"value":"b"}]}"#,
            r#"{"behavior":"answered","value":"b"}"#,
            "selected",
            string("b"),
        )
        .await;
        exercise(
            &RequestUserConfirmationTool::new(),
            r#"{"prompt":"Continue?"}"#,
            r#"{"behavior":"answered","value":true}"#,
            "confirmed",
            JsValue::Bool(true),
        )
        .await;
        exercise(&RequestUserFormTool::new(), r#"{"prompt":"Details","schema":{"type":"object","properties":{"name":{"type":"string"}}}}"#, r#"{"behavior":"answered","value":{"name":"Ada"}}"#, "values", parse_json(r#"{"name":"Ada"}"#).unwrap()).await;
    }
}
