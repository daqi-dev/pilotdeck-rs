//! Stable control boundary shared by a live manager and standalone apply.
use std::{future::Future, pin::Pin, sync::Arc};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use super::manager::{AbortInput, AlwaysOnManager, ApplyInput, ControlError, RerunInput};

pub(crate) type ControlFuture<'a> = Pin<Box<dyn Future<Output = Result<JsValue, ControlError>> + Send + 'a>>;

pub(crate) trait AlwaysOnControl: Send + Sync {
    fn dispatch<'a>(&'a self, method: &'a str, project: &'a JsString, params: JsValue) -> ControlFuture<'a>;
}
pub(crate) trait StandaloneApply: Send + Sync {
    fn apply<'a>(&'a self, project: &'a JsString, params: JsValue) -> ControlFuture<'a>;
}

pub(crate) fn create_control(manager: Option<Arc<AlwaysOnManager>>,
    standalone: Arc<dyn StandaloneApply>) -> Arc<dyn AlwaysOnControl> {
    Arc::new(Control { manager, standalone })
}
struct Control { manager: Option<Arc<AlwaysOnManager>>, standalone: Arc<dyn StandaloneApply> }

fn err(code: &str, message: impl Into<String>) -> ControlError {
    ControlError { code: code.into(), message: message.into() }
}
fn read<'a>(params: &'a JsValue, key: &str) -> Result<Option<&'a JsValue>, ControlError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        return Err(err("gateway_request_failed", format!("Cannot read properties of {} (reading '{key}')",
            if matches!(params, JsValue::Null) { "null" } else { "undefined" })));
    }
    Ok(params.get(key))
}
fn text(value: Option<&JsValue>) -> Result<String, ControlError> {
    let missing = JsValue::Undefined;
    let units = crate::js_value::to_string_units_checked(value.unwrap_or(&missing))
        .map_err(|_| err("gateway_request_failed", "Cannot convert value to string"))?;
    String::from_utf16(&units).map_err(|_| err("invalid_input", "Value cannot be represented by the native filesystem"))
}
fn error_value(error: &ControlError) -> JsValue {
    let mut value = JsObject::new();
    value.insert("code".into(), JsValue::String(error.code.as_str().into()));
    value.insert("message".into(), JsValue::String(error.message.as_str().into()));
    JsValue::Object(value)
}
fn error_result(mut value: JsObject, error: Option<ControlError>) -> JsValue {
    if let Some(error) = error { value.insert("error".into(), error_value(&error)); }
    JsValue::Object(value)
}
impl AlwaysOnControl for Control {
    fn dispatch<'a>(&'a self, method: &'a str, project: &'a JsString, params: JsValue) -> ControlFuture<'a> {
        Box::pin(async move {
            let Some(manager) = &self.manager else {
                return match method {
                    "always_on_apply" => self.standalone.apply(project, params).await,
                    "always_on_abort" => {
                        let key = read(&params, "sessionKey")?;
                        let mut out = JsObject::new(); out.insert("aborted".into(), JsValue::Bool(false));
                        if let Some(key) = key.filter(|v| !v.is_undefined()) { out.insert("sessionKey".into(), key.clone()); }
                        Ok(error_result(out, Some(err("not_configured", "Always-On abort requires an enabled Always-On runtime."))))
                    },
                    "always_on_rerun_plan" => {
                        let mut out = JsObject::new(); out.insert("runId".into(), JsValue::String("".into()));
                        Ok(error_result(out, Some(err("not_configured", "Always-On plan rerun requires an enabled Always-On runtime."))))
                    },
                    _ => Err(err("method_not_found", "Unknown Always-On method")),
                };
            };
            let selected = project.to_utf8().map_err(|_| err("invalid_project", "Project path cannot be represented by the native filesystem"))?;
            // The manager reads projectKey before its method-specific fields.
            // Scope resolution normalizes a supplied string, but a missing or
            // non-string project must not silently choose the default.
            let raw_project = read(&params, "projectKey")?;
            let manager_project = if raw_project.and_then(JsValue::as_string).is_some() {
                selected
            } else {
                text(raw_project)?
            };
            match method {
                "always_on_apply" => {
                    let id = text(read(&params, "workCycleId")?)?;
                    let name = read(&params, "projectName")?.filter(|v| !v.is_undefined()).map(|v| {
                        crate::js_value::to_string_units_checked(v).map(JsString::from_utf16)
                            .map_err(|_| err("gateway_request_failed", "Cannot convert projectName to string"))
                    }).transpose()?;
                    let value = manager.apply_cycle(&ApplyInput { project_key: manager_project, work_cycle_id: id, project_name: name })
                        .await.map_err(|e| err("gateway_request_failed", e.to_string()))?;
                    let mut out = JsObject::new(); out.insert("sessionKey".into(), JsValue::String(value.session_key.as_str().into()));
                    Ok(error_result(out, value.error))
                },
                "always_on_abort" => {
                    let key = read(&params, "sessionKey")?;
                    let session = text(key)?;
                    let reason = read(&params, "reason")?.filter(|v| !v.is_undefined()).map(|v| text(Some(v))).transpose()?;
                    let value = manager.abort_run(&AbortInput { project_key: manager_project, session_key: session, reason }).await;
                    let mut out = JsObject::new(); out.insert("aborted".into(), JsValue::Bool(value.aborted));
                    if let Some(key) = key.filter(|v| !v.is_undefined()) { out.insert("sessionKey".into(), key.clone()); }
                    if let Some(run_id) = value.run_id { out.insert("runId".into(), JsValue::String(run_id.as_str().into())); }
                    Ok(error_result(out, value.error))
                },
                "always_on_rerun_plan" => {
                    let id = text(read(&params, "planId")?)?;
                    let value = manager.rerun_plan(&RerunInput { project_key: manager_project, plan_id: id }).await
                        .map_err(|e| err("gateway_request_failed", e.to_string()))?;
                    let mut out = JsObject::new(); out.insert("runId".into(), JsValue::String(value.run_id.as_str().into()));
                    Ok(error_result(out, value.error))
                },
                _ => Err(err("method_not_found", "Unknown Always-On method")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Apply(Mutex<Vec<JsString>>);
    impl StandaloneApply for Apply {
        fn apply<'a>(&'a self, project: &'a JsString, _params: JsValue) -> ControlFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(project.clone());
                let mut out = JsObject::new();
                out.insert("sessionKey".into(), JsValue::String("fixture".into()));
                Ok(JsValue::Object(out))
            })
        }
    }
    #[tokio::test]
    async fn disabled_runtime_dispatches_real_fallback_and_preserves_echo() {
        let fallback = Arc::new(Apply(Mutex::default()));
        let control = create_control(None, fallback.clone());
        let project: JsString = "/project".into();
        let result = control.dispatch("always_on_apply", &project, JsValue::Object(JsObject::new())).await.unwrap();
        assert!(result.get("sessionKey").and_then(JsValue::as_string).unwrap().equals_str("fixture"));
        assert_eq!(fallback.0.lock().unwrap().len(), 1);
        let mut params = JsObject::new(); params.insert("sessionKey".into(), JsValue::Number(9.0));
        let result = control.dispatch("always_on_abort", &project, JsValue::Object(params)).await.unwrap();
        assert_eq!(result.get("sessionKey"), Some(&JsValue::Number(9.0)));
        assert!(result.get("error").unwrap().get("code").and_then(JsValue::as_string).unwrap().equals_str("not_configured"));
        let result = control.dispatch("always_on_rerun_plan", &project, JsValue::Null).await.unwrap();
        assert!(result.get("runId").and_then(JsValue::as_string).unwrap().is_empty());
        assert_eq!(control.dispatch("always_on_abort", &project, JsValue::Null).await.unwrap_err().code, "gateway_request_failed");
    }
}
