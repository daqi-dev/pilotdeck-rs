//! Session tools call the same durable task owner as gateway RPCs.
use super::{native, runtime::CronRuntime};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{
        ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext,
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use serde_json::{Value, json};
use std::sync::Arc;

struct CronTool {
    fields: JsObject,
    method: &'static str,
    runtime: Arc<CronRuntime>,
}
impl ToolDefinition for CronTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.method == "cron_list"))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.method == "cron_list"))
    }
}
impl ExecutableTool for CronTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let runtime = self.runtime.clone();
        let method = self.method;
        Box::pin(async move {
            let convert =
                |error: super::always_on::AlwaysOnError| ToolError::new(error.code, error.message);
            let mut params = native::from_js(&input).map_err(convert)?;
            if !params.is_object() {
                return Err(ToolError::new(
                    "invalid_params",
                    "Cron tool input must be an object.",
                ));
            }
            params["projectKey"] = native::from_js(
                context
                    .fields
                    .get("cwd")
                    .ok_or_else(|| ToolError::failed("Missing tool cwd."))?,
            )
            .map_err(convert)?;
            if method == "cron_create" {
                if params.get("sessionKey").is_none() {
                    params["sessionKey"] = native::from_js(
                        context
                            .fields
                            .get("sessionId")
                            .ok_or_else(|| ToolError::failed("Missing tool sessionId."))?,
                    )
                    .map_err(convert)?;
                }
                if params.get("channelKey").is_none()
                    && let Some((channel, _)) = context
                        .fields
                        .get("sessionId")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .as_deref()
                        .and_then(|session| session.split_once(':'))
                        .filter(|(channel, _)| !channel.is_empty())
                {
                    params["channelKey"] = channel.to_owned().into();
                }
            }
            let result = runtime
                .tasks()
                .handle(method, &params, runtime.as_ref())
                .expect("registered cron method")
                .await
                .map_err(|error| ToolError::new(error.code, error.message))?;
            let content = if method == "cron_create" {
                result["task"].clone()
            } else {
                result.clone()
            };
            Ok(ToolOutput {
                content: vec![
                    native::to_js(&json!({"type":"json","value":content})).map_err(convert)?,
                ],
                data: Some(native::to_js(&result).map_err(convert)?),
                ..Default::default()
            })
        })
    }
}

pub fn register(registry: &ToolRegistry, runtime: Arc<CronRuntime>) -> std::io::Result<()> {
    // TS CronRuntime.getTools() exposes no model tools while Cron is disabled.
    // RPC handlers remain installed so callers receive the domain's disabled
    // error instead of a transport capability error.
    if !runtime.tasks().config().enabled {
        return Ok(());
    }
    let once = json!({"type":"object","required":["type","runAt"],"additionalProperties":false,"properties":{"type":{"type":"string","const":"once"},"runAt":{"type":"string"}}});
    let recurring = json!({"type":"object","required":["type","expression"],"additionalProperties":false,"properties":{"type":{"type":"string","const":"cron"},"expression":{"type":"string","description":"Five numeric fields: minute hour day-of-month month day-of-week (0 or 7 = Sunday). If neither day field starts with '*', they match with OR; if either starts with '*' (including */n), they match with AND. Explicit full ranges are not wildcards. '0 9 1 * 1' means the 1st of each month OR every Monday, not their intersection. Do not use '0 9 1-7 * 1' for the first Monday; schedule explicit one-time dates instead."},"timezone":{"type":"string"}}});
    let delay = json!({"type":"object","required":["type","amount","unit"],"additionalProperties":false,"properties":{"type":{"type":"string","const":"delay"},"amount":{"type":"number","exclusiveMinimum":0},"unit":{"type":"string","enum":["second","minute","hour","day"]}}});
    for (method, title, description, required, properties) in [
        (
            "cron_create",
            "Create Cron Task",
            "Create a one-time or recurring background Cron task that submits future work back into a session. For relative reminders like 'in 10 minutes', use schedule.type='delay'. For absolute natural-language times like tonight, tomorrow morning, or next Monday, call get_current_time first to resolve the current local time/timezone, then pass a future schedule.type='once' runAt. Recurring expressions use Unix cron day matching: if neither day field starts with '*', day-of-month and day-of-week match with OR, not AND; see the expression schema for examples.",
            json!(["message", "schedule"]),
            json!({"message":{"type":"string"},"schedule":{"anyOf":[once.clone(),recurring.clone(),delay]},"sessionKey":{"type":"string"},"channelKey":{"type":"string"},"mode":{"type":"string"},"timezone":{"type":"string"}}),
        ),
        (
            "cron_list",
            "List Cron Tasks",
            "List scheduled Cron tasks and optionally recent Cron run history.",
            json!([]),
            json!({"includeHistory":{"type":"boolean"},"limit":{"type":"number"}}),
        ),
        (
            "cron_update",
            "Update Cron Task",
            "Update the message or schedule of a scheduled Cron task. Use cron_list first to obtain the task revision. Updates to running tasks are rejected.",
            json!(["taskId", "expectedRevision", "message", "schedule"]),
            json!({"taskId":{"type":"string"},"expectedRevision":{"type":"number","minimum":0},"message":{"type":"string"},"schedule":{"anyOf":[once,recurring]},"timezone":{"type":"string"}}),
        ),
        (
            "cron_delete",
            "Delete Cron Task",
            "Delete a scheduled Cron task and cancel its future triggers.",
            json!(["taskId"]),
            json!({"taskId":{"type":"string"},"stopRunning":{"type":"boolean"}}),
        ),
        (
            "cron_stop",
            "Stop Cron Run",
            "Stop a currently running Cron run. One-time tasks are removed after being stopped.",
            json!([]),
            json!({"taskId":{"type":"string"},"runId":{"type":"string"}}),
        ),
        (
            "cron_run_now",
            "Run Cron Task Now",
            "Start one immediate run of a scheduled Cron task without changing its future schedule.",
            json!(["taskId"]),
            json!({"taskId":{"type":"string"}}),
        ),
    ] {
        let mut schema =
            json!({"type":"object","additionalProperties":false,"properties":properties});
        if required.as_array().is_some_and(|items| !items.is_empty()) {
            schema["required"] = required;
        }
        let mut definition = json!({"name":method,"title":title,"description":description,"kind":"session","inputSchema":schema});
        if method == "cron_delete" {
            definition["isDestructive"] = Value::Bool(true);
        }
        let fields = native::to_js(&definition)
            .map_err(std::io::Error::other)?
            .as_object()
            .cloned()
            .expect("object");
        registry
            .register(Arc::new(CronTool {
                fields,
                method,
                runtime: runtime.clone(),
            }))
            .map_err(|error| std::io::Error::other(error.message))?;
    }
    Ok(())
}
