use super::{Options, methods};
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        refresh::RefreshRouter,
        session::SessionOwner,
        turn::{
            coordinator::TurnEventCoordinator, gateway::GatewayRequests, publish::FileRegistry,
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub struct Requests {
    pub options: Options,
    pub env: crate::config::Env,
    pub snapshot_enabled: bool,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub runs: Arc<FileRegistry>,
    pub owner: SessionOwner,
    pub project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
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
                            "snapshot_list",
                            "snapshot_get",
                            "snapshot_restore",
                            "manager_sessions",
                            "commands_list",
                        ]
                        .into_iter()
                        .filter(|capability| {
                            self.snapshot_enabled
                                || !matches!(
                                    *capability,
                                    "snapshot_list" | "snapshot_get" | "snapshot_restore"
                                )
                        })
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
            // createLocalGateway's archive callbacks use the configured fallback
            // project for nullish keys. Fill it before isolated admission, while
            // leaving Metadata's cwd lazy for explicit paths and other RPCs.
            let mut params = params;
            if let Ok(name) = method.to_utf8()
                && crate::native_archive::ARCHIVE_METHODS.contains(&name.as_str())
            {
                crate::native_archive::fill_default_project_key(&mut params, &self.options.project);
            }
            if let Ok(name) = method.to_utf8()
                && let Some(result) = methods::before_admission(self, &name, &params).await
            {
                return result;
            }
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
            methods::request(self, method, params).await
        })
    }
}
