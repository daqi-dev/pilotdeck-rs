//! SDK opt-in user-dialog tools over the Gateway's existing dialog channel.
use std::{sync::{Arc, OnceLock}, time::Duration};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::{host::{HostError, modules::capability::ToolDefinition}, runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistration, ToolRegistry, ToolRunContext}, user_dialog::{AbortSignal, ChannelOptions, ChannelRequest, ConfirmRequest, DialogAnswer, FormRequest, GatewayUserDialogChannel, InputRequest, SelectChoice, SelectRequest}};
use super::{object, schema, string};
use super::form_schema;

#[derive(Clone)]
pub(crate) struct NativeUserDialogPort { options: ChannelOptions }
impl NativeUserDialogPort { pub(crate) fn new(options: ChannelOptions) -> Self { Self { options } } }
static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
fn slots() -> Arc<tokio::sync::Semaphore> { WORKERS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4))).clone() }
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ToolError> {
    let permit = slots().acquire_owned().await.map_err(|_| ToolError::failed("Dialog worker unavailable"))?;
    tokio::task::spawn_blocking(move || { let _permit = permit; f() }).await.map_err(|e| ToolError::failed(e.to_string().as_str()))
}
struct AbortGuard(Option<AbortSignal>);
impl Drop for AbortGuard {
    fn drop(&mut self) {
        if let Some(signal) = self.0.take() && let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { let _ = blocking(move || signal.abort()).await; });
        }
    }
}
#[derive(Clone, Copy)]
enum Kind { Input, Choice, Confirmation, Form }
impl Kind {
    fn name(self) -> &'static str { match self { Self::Input => "request_user_input", Self::Choice => "request_user_choice", Self::Confirmation => "request_user_confirmation", Self::Form => "request_user_form" } }
    fn alias(self) -> &'static str { match self { Self::Input => "RequestUserInput", Self::Choice => "RequestUserChoice", Self::Confirmation => "RequestUserConfirmation", Self::Form => "RequestUserForm" } }
    fn schema(self) -> &'static str { match self {
        Self::Input => r#"{"type":"object","required":["prompt"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"placeholder":{"type":"string","maxLength":512},"allowEmpty":{"type":"boolean"}}}"#,
        Self::Choice => r#"{"type":"object","required":["prompt","options"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"options":{"type":"array","minItems":2,"maxItems":12,"items":{"type":"object","required":["value"],"additionalProperties":false,"properties":{"value":{"type":"string","minLength":1,"maxLength":512},"label":{"type":"string","maxLength":512},"description":{"type":"string","maxLength":2000}}}},"defaultValue":{"type":"string","maxLength":512}}}"#,
        Self::Confirmation => r#"{"type":"object","required":["prompt"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"confirmLabel":{"type":"string","maxLength":128},"cancelLabel":{"type":"string","maxLength":128},"defaultValue":{"type":"boolean"}}}"#,
        Self::Form => r#"{"type":"object","required":["prompt","schema"],"additionalProperties":false,"properties":{"prompt":{"type":"string","minLength":1,"maxLength":4000},"schema":{"type":"object","additionalProperties":true}}}"#,
    } }
}
struct DialogTool { kind: Kind, fields: JsObject }
impl DialogTool {
    fn new(kind: Kind) -> Self {
        let fields = object([("name", string(kind.name())), ("aliases", JsValue::Array(vec![string(kind.alias())])),
            ("description", string("Request a response from the user through the active session dialog.")),
            ("kind", string("session")), ("shouldDefer", JsValue::Bool(true)),
            ("maxResultBytes", JsValue::Number(if matches!(kind, Kind::Form) { 32000.0 } else { 16000.0 })),
            ("inputSchema", JsValue::Object(schema(kind.schema())))]);
        Self { kind, fields }
    }
}
impl ToolDefinition for DialogTool {
    fn fields(&self) -> &JsObject { &self.fields }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(true)) }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(true)) }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(false)) }
}
fn blank(s: &JsString) -> bool { s.units().iter().all(|c| matches!(*c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)) }
fn issue(path: &str, message: impl Into<JsString>) -> Option<Vec<JsObject>> {
    Some(vec![object([("path", string(path)), ("code", string("invalid_schema")), ("message", string(message))])])
}
fn validate(kind: Kind, input: &JsValue) -> Option<Vec<JsObject>> {
    if !matches!(kind, Kind::Input) && input.get("prompt").and_then(JsValue::as_string).is_none_or(blank) { return issue("prompt", "prompt must be a non-empty string."); }
    match kind {
        Kind::Choice => {
            let Some(JsValue::Array(options)) = input.get("options") else { return issue("options", "options must contain 2-12 choices."); };
            if !(2..=12).contains(&options.len()) { return issue("options", "options must contain 2-12 choices."); }
            let mut seen = std::collections::HashSet::new();
            for (index, option) in options.iter().enumerate() {
                let path = format!("options[{index}]");
                let Some(value) = option.get("value").and_then(JsValue::as_string).filter(|v| !blank(v)) else { return issue(&format!("{path}.value"), "each option value must be a non-empty string."); };
                if value.len_utf16() > 512 || [("label",512), ("description",2000)].into_iter().any(|(key, max)| option.get(key).is_some_and(|v| !v.is_undefined() && v.as_string().is_none_or(|s| s.len_utf16() > max))) {
                    return issue(&path, "choice values, labels, and descriptions exceed their limits.");
                }
                if !seen.insert(value.clone()) { return issue(&format!("{path}.value"), "choice values must be unique."); }
            }
            if let Some(value) = input.get("defaultValue").filter(|v| !v.is_undefined()) {
                if value.as_string().is_none_or(|s| s.len_utf16() > 512 || !seen.contains(s)) { return issue("defaultValue", "defaultValue must match one of the choice values."); }
            }
        }
        Kind::Confirmation => {
            for key in ["confirmLabel", "cancelLabel"] {
                if let Some(value) = input.get(key).filter(|v| !v.is_undefined()) {
                    if value.as_string().is_none_or(|s| blank(s) || s.len_utf16() > 128) { return issue(key, format!("{key} must be a non-empty string no longer than 128 characters.").as_str()); }
                }
            }
            if input.get("defaultValue").is_some_and(|v| !v.is_undefined() && v.as_bool().is_none()) { return issue("defaultValue", "defaultValue must be a boolean."); }
        }
        Kind::Form => if let Err(message) = form_schema::validate_schema(input.get("schema").unwrap_or(&JsValue::Undefined)) { return issue("schema", message.as_str()); },
        Kind::Input => {},
    }
    None
}
fn required(input: &JsValue, key: &str) -> Result<JsString, ToolError> { input.get(key).and_then(JsValue::as_string).cloned().ok_or_else(|| ToolError::new("invalid_input", format!("{key} must be a string").as_str())) }
fn optional(input: &JsValue, key: &str) -> Option<JsString> { input.get(key).and_then(JsValue::as_string).cloned() }
fn cancellation(kind: Kind, reason: Option<JsString>) -> ToolError {
    let label = match kind { Kind::Input => "input", Kind::Choice => "choice", Kind::Confirmation => "confirmation", Kind::Form => "form" };
    let mut value = format!("The user cancelled the {label} request").encode_utf16().collect::<Vec<_>>();
    if let Some(reason) = reason.filter(|s| !s.is_empty()) { value.extend(": ".encode_utf16()); value.extend_from_slice(reason.units()); }
    else { value.push(u16::from(b'.')); }
    ToolError::new("unsupported_tool", JsString::from_utf16(value))
}
fn make_request(kind: Kind, channel: &GatewayUserDialogChannel, input: &JsValue,
    call: JsString, abort: AbortSignal) -> Result<ChannelRequest, ToolError> {
    let prompt = required(input, "prompt")?; let tool_name = kind.name().into();
    Ok(match kind {
        Kind::Input => channel.request_input(&InputRequest { tool_call_id: call, tool_name, prompt,
            placeholder: optional(input, "placeholder").filter(|s| !s.is_empty()),
            allow_empty: input.get("allowEmpty").and_then(JsValue::as_bool) == Some(true), abort: Some(abort) }),
        Kind::Choice => {
            let Some(JsValue::Array(options)) = input.get("options") else { return Err(ToolError::new("invalid_input", "options must be an array")); };
            let choices = options.iter().map(|o| Ok(SelectChoice { value: required(o, "value")?, label: optional(o,"label"), description: optional(o,"description") })).collect::<Result<Vec<_>, ToolError>>()?;
            channel.request_select(&SelectRequest { tool_call_id: call, tool_name, prompt, choices,
                default_value: optional(input, "defaultValue"), abort: Some(abort) })
        }
        Kind::Confirmation => channel.request_confirm(&ConfirmRequest { tool_call_id: call, tool_name, prompt,
            confirm_label: optional(input,"confirmLabel"), cancel_label: optional(input,"cancelLabel"),
            default_value: input.get("defaultValue").and_then(JsValue::as_bool), abort: Some(abort) }),
        Kind::Form => channel.request_form(&FormRequest { tool_call_id: call, tool_name, prompt,
            schema: input.get("schema").cloned().unwrap_or(JsValue::Undefined), abort: Some(abort) }),
    })
}
fn output(kind: Kind, answer: JsValue, allow_empty: bool) -> Result<ToolOutput, ToolError> {
    let (data_key, content) = match kind {
        Kind::Input | Kind::Choice => {
            let value = answer.as_string().ok_or_else(|| ToolError::failed("Dialog returned an invalid answer."))?;
            if matches!(kind, Kind::Input) && !allow_empty && blank(value) { return Err(ToolError::failed("The user submitted an empty response for a required input request.")); }
            let prefix = if matches!(kind, Kind::Input) { "The user answered: " } else { "The user selected: " };
            let mut text = prefix.encode_utf16().collect::<Vec<_>>(); text.extend_from_slice(value.units());
            (if matches!(kind, Kind::Input) { "answer" } else { "selected" }, object([("type", string("text")), ("text", JsValue::String(JsString::from_utf16(text)))]))
        }
        Kind::Confirmation => {
            let value = answer.as_bool().ok_or_else(|| ToolError::failed("Dialog returned an invalid answer."))?;
            ("confirmed", object([("type", string("text")), ("text", string(if value { "The user confirmed: yes" } else { "The user confirmed: no" }))]))
        }
        Kind::Form => {
            if answer.as_object().is_none() { return Err(ToolError::failed("Dialog returned an invalid form answer.")); }
            ("values", object([("type", string("json")), ("value", answer.clone())]))
        }
    };
    Ok(ToolOutput { content: vec![JsValue::Object(content)], data: Some(JsValue::Object(object([(data_key, answer)]))), ..Default::default() })
}
impl ExecutableTool for DialogTool {
    fn validate_input(&self, input: JsValue, _: ToolRunContext) -> ToolFuture<Option<Vec<JsObject>>> {
        let kind = self.kind; Box::pin(async move { Ok(validate(kind, &input)) })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let kind = self.kind;
        Box::pin(async move {
            let port = context.host.user_dialog.as_ref().and_then(|p| p.downcast_ref::<NativeUserDialogPort>()).cloned()
                .ok_or_else(|| ToolError::new("unsupported_tool", format!("{} requires an enabled Gateway user-dialog channel.", kind.name()).as_str()))?;
            if context.is_aborted() { return Err(ToolError::aborted()); }
            let mut options = port.options;
            if matches!(kind, Kind::Form) { options.form_accepts = Some(Arc::new(form_schema::accepts_form_dialog_answer)); }
            let channel = GatewayUserDialogChannel::new(options);
            let signal = AbortSignal::new(); let mut guard = AbortGuard(Some(signal.clone()));
            let call = context.fields.get("currentToolCallId").and_then(JsValue::as_string).or_else(|| context.fields.get("turnId").and_then(JsValue::as_string)).cloned().unwrap_or_default();
            let channel_for_begin = channel.clone(); let input_for_begin = input.clone();
            let begin = blocking(move || make_request(kind, &channel_for_begin, &input_for_begin, call, signal));
            let mut request = tokio::select! { result = begin => result??, _ = context.cancelled() => return Err(ToolError::aborted()) };
            loop {
                if let Some(answer) = request.try_result() {
                    guard.0.take();
                    return match answer {
                        Ok(DialogAnswer::Answered(value)) => output(kind, value, input.get("allowEmpty").and_then(JsValue::as_bool) == Some(true)),
                        Ok(DialogAnswer::Cancelled(reason)) => Err(cancellation(kind, reason)),
                        Err(error) => Err(ToolError::failed(error.as_str())),
                    };
                }
                tokio::select! {
                    _ = context.cancelled() => return Err(ToolError::aborted()),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        let channel = channel.clone();
                        tokio::select! { result = blocking(move || channel.tick()) => { result?; }, _ = context.cancelled() => return Err(ToolError::aborted()) }
                    }
                }
            }
        })
    }
}
/// Registers only explicitly enabled dialog kinds. On failure, rolls back the
/// registrations made by this call without affecting other session tools.
pub(crate) fn register_user_dialog_tools(registry: &ToolRegistry, kinds: &[JsString]) -> Result<Vec<ToolRegistration>, HostError> {
    let mut registrations: Vec<ToolRegistration> = Vec::new();
    for (name, kind) in [("input", Kind::Input), ("select", Kind::Choice), ("confirm", Kind::Confirmation), ("form", Kind::Form)] {
        if !kinds.iter().any(|k| k.equals_str(name)) { continue; }
        match registry.register(Arc::new(DialogTool::new(kind))) {
            Ok(handle) => registrations.push(handle),
            Err(error) => { for handle in &registrations { handle.dispose(); } return Err(error); }
        }
    }
    Ok(registrations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext}, user_dialog::{DialogRuntime, GatewayUserDialogBus}};
    use std::sync::Mutex;
    fn setup() -> (Arc<Mutex<GatewayUserDialogBus>>, ToolRunContext) {
        let bus=Arc::new(Mutex::new(GatewayUserDialogBus::connected(DialogRuntime::default())));
        let mut options=ChannelOptions::new("session",bus.clone(),Arc::new(|_|Ok(())));
        options.uuid=Arc::new(|| "dialog-id".into());
        let mut fields=JsObject::new(); fields.insert("currentToolCallId".into(), "tool-1".into()); fields.insert("turnId".into(), "turn-1".into());
        let host=Arc::new(ToolRuntimeContext { fields, files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal:None, model:None, elicitation:None, user_dialog:Some(Arc::new(NativeUserDialogPort::new(options))),
            audit_recorder:None, file_history:None, file_update_notifier:None, now:None, progress:None,
            subagent:None, plan_todo:None, goal:None, plan_directory:None });
        (bus, ToolRunContext::new(host))
    }
    async fn pending(bus:&Arc<Mutex<GatewayUserDialogBus>>, want:usize) {
        tokio::time::timeout(Duration::from_secs(3),async { loop {
            if bus.lock().unwrap().pending_count(&"session".into())==want {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }}).await.expect("dialog lifecycle timed out");
    }
    #[tokio::test]
    async fn real_channel_delivers_answer_and_dropped_future_cancels() {
        let (bus,context)=setup(); let tool=DialogTool::new(Kind::Input);
        let input=JsValue::Object(object([("prompt",string("Question?"))]));
        let task=tokio::spawn(tool.execute(input.clone(),context.clone())); pending(&bus,1).await;
        let value=JsValue::String("answer".into());
        let taken={ let mut bus=bus.lock().unwrap();
            assert!((bus.peek(&"session".into(),&"dialog-id".into()).unwrap().accepts)(&value));
            bus.consume(&"session".into(),&"dialog-id".into(),"answered").unwrap() };
        if let Some(settled)=taken.entry.on_settled {settled().unwrap();}
        (taken.entry.resolve)(DialogAnswer::Answered(value));
        let result=tokio::time::timeout(Duration::from_secs(3),task).await.unwrap().unwrap().unwrap();
        assert!(result.data.unwrap().get("answer").and_then(JsValue::as_string).unwrap().equals_str("answer"));
        let task=tokio::spawn(tool.execute(input,context)); pending(&bus,1).await;
        task.abort(); let _=task.await; pending(&bus,0).await;
    }
    #[test]
    fn choice_confirmation_and_registry_enforce_their_contract() {
        let bad=pilotdeck_protocol::parse_json(r#"{"prompt":"choose","options":[{"value":"same"},{"value":"same"}]}"#).unwrap();
        assert!(validate(Kind::Choice,&bad).is_some());
        let bad=pilotdeck_protocol::parse_json(r#"{"prompt":"ok","confirmLabel":" "}"#).unwrap();
        assert!(validate(Kind::Confirmation,&bad).is_some());
        let registry=ToolRegistry::default();
        let handles=register_user_dialog_tools(&registry,&["input".into(),"form".into()]).unwrap();
        assert!(registry.get(&"request_user_input".into()).is_some());
        assert!(registry.get(&"request_user_form".into()).is_some());
        assert!(registry.get(&"request_user_choice".into()).is_none());
        for handle in handles {handle.dispose();}
    }
}
