//! Standalone apply control used when no discovery manager is enabled.
use super::{
    apply::ApplyPhase,
    control::{ControlFuture, StandaloneApply},
    cycles::WorkCycleStore,
    fire::ApplyPhaseInput,
    manager::ControlError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) struct StandaloneProject {
    pub store: WorkCycleStore,
    pub phase: ApplyPhase,
}

/// The native factory resolves one already-admitted project with bounded
/// marker I/O and constructs stores bound to that project generation.
pub(crate) trait StandaloneProjectFactory: Send + Sync {
    fn open<'a>(
        &'a self,
        project: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<StandaloneProject, ControlError>> + Send + 'a>>;
}

pub(crate) fn new_standalone(
    factory: Arc<dyn StandaloneProjectFactory>,
) -> Arc<dyn StandaloneApply> {
    Arc::new(Standalone { factory })
}
struct Standalone {
    factory: Arc<dyn StandaloneProjectFactory>,
}

fn failure(code: &str, message: impl Into<String>) -> ControlError {
    ControlError {
        code: code.into(),
        message: message.into(),
    }
}
fn envelope(session_key: &str, error: Option<ControlError>) -> JsValue {
    let mut result = JsObject::new();
    result.insert("sessionKey".into(), JsValue::String(session_key.into()));
    if let Some(error) = error {
        let mut value = JsObject::new();
        value.insert("code".into(), JsValue::String(error.code.as_str().into()));
        value.insert(
            "message".into(),
            JsValue::String(error.message.as_str().into()),
        );
        result.insert("error".into(), JsValue::Object(value));
    }
    JsValue::Object(result)
}

impl StandaloneApply for Standalone {
    fn apply<'a>(&'a self, project: &'a JsString, params: JsValue) -> ControlFuture<'a> {
        Box::pin(async move {
            if matches!(&params, JsValue::Null | JsValue::Undefined) {
                return Err(failure(
                    "gateway_request_failed",
                    "Cannot read projectKey from nullish input",
                ));
            }
            let selected = project.to_utf8().map_err(|_| {
                failure(
                    "invalid_project",
                    "Project path cannot be represented by the native filesystem",
                )
            })?;
            let undefined = JsValue::Undefined;
            let raw_id = params.get("workCycleId").unwrap_or(&undefined);
            let display_units = crate::js_value::to_string_units_checked(raw_id).map_err(|_| {
                failure(
                    "gateway_request_failed",
                    "Cannot convert workCycleId to string",
                )
            })?;
            let display = String::from_utf16(&display_units).map_err(|_| {
                failure(
                    "invalid_input",
                    "Cycle identifier cannot be represented in this error",
                )
            })?;
            // Store comparison is strict JS equality. Numeric or missing IDs
            // must never be coerced into matching a string record.
            let Some(id) = raw_id.as_string().cloned() else {
                return Ok(envelope(
                    "",
                    Some(failure(
                        "cycle_not_found",
                        format!("Work cycle {display} not found"),
                    )),
                ));
            };
            let backend = self.factory.open(project).await?;
            if &backend.phase.project_key != project || backend.phase.register_automation_tools {
                return Err(failure(
                    "invalid_project",
                    "Standalone apply factory returned an incompatible project scope",
                ));
            }
            let Some((cycle, plans)) = backend
                .store
                .read_apply(id)
                .await
                .map_err(|e| failure("gateway_request_failed", e.to_string()))?
            else {
                return Ok(envelope(
                    "",
                    Some(failure(
                        "cycle_not_found",
                        format!("Work cycle {display} not found"),
                    )),
                ));
            };
            let name = match params.get("projectName") {
                None | Some(JsValue::Undefined) => None,
                Some(value) => {
                    let units = crate::js_value::to_string_units_checked(value).map_err(|_| {
                        failure(
                            "gateway_request_failed",
                            "Cannot convert projectName to string",
                        )
                    })?;
                    Some(JsString::from_utf16(units))
                }
            };
            let run_id = uuid::Uuid::new_v4().to_string();
            let result = backend
                .phase
                .run(ApplyPhaseInput {
                    run_id: &run_id,
                    cycle: &cycle,
                    plans: &plans,
                    project_name: name.as_ref(),
                    project_root: &selected,
                })
                .await?;
            Ok(envelope(&result.session_key, result.error))
        })
    }
}
