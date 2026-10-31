use super::Options;
use crate::{
    metadata::{Metadata, NativeFiles, object},
    runtime::{
        connection::{Call, RequestError},
        refresh::RefreshRouter,
        session::SessionOwner,
        turn::{
            coordinator::TurnEventCoordinator,
            gateway::GatewayRequests,
            publish::FileRegistry,
            read::{self, RunEventsInput},
            replay::ActiveTurnSnapshotInput,
        },
    },
    storage::RunRef,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

pub struct Requests {
    pub options: Options,
    pub env: crate::config::Env,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub runs: Arc<FileRegistry>,
    pub owner: SessionOwner,
}
fn field(value: &JsValue, name: &str) -> JsValue {
    value.get(name).cloned().unwrap_or(JsValue::Undefined)
}
fn string(value: &JsValue, name: &str) -> Result<JsString, RequestError> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| RequestError::new(format!("{name} must be a string.")))
}
fn read_error(error: read::TurnReadError) -> RequestError {
    RequestError {
        code: error
            .code
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message,
        details: None,
        validation: None,
    }
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            Ok(object([
                ("mode", "in_process".into()),
                (
                    "sessionCount",
                    JsValue::Number(self.owner.cached_session_count() as f64),
                ),
                (
                    "capabilities",
                    JsValue::Array(
                        [
                            "run_get",
                            "run_events",
                            "run_reattach",
                            "project_files_list",
                            "project_file_read",
                            "model_catalog_list",
                            "session_model_get",
                            "supported_agents",
                        ]
                        .into_iter()
                        .map(JsValue::from)
                        .collect(),
                    ),
                ),
            ]))
        })
    }
    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            if let Some(project) = params.get("projectKey").filter(|v| !v.is_undefined())
                && !project
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .is_some_and(|p| self.options.serves_project(&p))
            {
                return Err(RequestError::new(
                    "The isolated native process serves only its configured project.",
                ));
            }
            let method = method
                .to_utf8()
                .map_err(|_| RequestError::new("Unknown native gateway method."))?;
            if method == "active_turn_snapshot" {
                return Ok(read::active_turn_snapshot(
                    &self.coordinator,
                    &ActiveTurnSnapshotInput {
                        session_key: string(&params, "sessionKey")?,
                        include_events: params.get("includeEvents") != Some(&JsValue::Bool(false)),
                    },
                )
                .to_json());
            }
            if ["run_get", "run_events", "run_reattach"].contains(&method.as_str()) {
                let run = RunRef {
                    project_key: params
                        .get("projectKey")
                        .and_then(JsValue::as_string)
                        .cloned(),
                    session_key: string(&params, "sessionKey")?,
                    run_id: string(&params, "runId")?,
                };
                return if method == "run_events" {
                    read::run_events(
                        Some(self.runs.as_ref()),
                        &RunEventsInput {
                            run,
                            after_seq: params.get("afterSeq").cloned(),
                            limit: params.get("limit").cloned(),
                        },
                    )
                    .await
                    .map_err(read_error)
                } else {
                    read::run_get(Some(self.runs.as_ref()), &run)
                        .await
                        .map(|v| v.unwrap_or(JsValue::Null))
                        .map_err(read_error)
                };
            }
            let metadata = Metadata {
                home: self.options.home.clone(),
                cwd: std::path::PathBuf::new(),
                files: NativeFiles,
            };
            let request = object([
                ("type", "request".into()),
                ("id", "native".into()),
                ("method", JsValue::String(method.clone().into())),
                ("params", params),
            ]);
            match crate::dispatch::route_with_environment(
                &request,
                Some(&metadata),
                Some(&self.env),
            ) {
                crate::dispatch::Route::LocalResponse(bytes) => {
                    let response = crate::dispatch::parse(&bytes)
                        .ok_or_else(|| RequestError::new("Native method returned invalid JSON."))?;
                    if response.get("ok") == Some(&JsValue::Bool(true)) {
                        Ok(field(&response, "result"))
                    } else {
                        let error = field(&response, "error");
                        Err(RequestError {
                            code: error
                                .get("code")
                                .and_then(JsValue::as_string)
                                .and_then(|s| s.to_utf8().ok())
                                .unwrap_or_else(|| "gateway_request_failed".into()),
                            message: error
                                .get("message")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_else(|| "Native request failed.".into()),
                            details: error.get("details").cloned().map(Box::new),
                            validation: None,
                        })
                    }
                }
                crate::dispatch::Route::Forward => Err(RequestError {
                    code: "CAPABILITY_UNAVAILABLE".into(),
                    message: format!("Native gateway method {method} is not configured.").into(),
                    details: None,
                    validation: None,
                }),
            }
        })
    }
}
