use super::RuntimeError;
use crate::cron::always_on::storage::absolute;
use crate::runtime::background::{
    BackgroundResources, BackgroundTaskRuntime, background_tasks_for_sdk,
    stop_background_task_for_sdk,
};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

#[derive(Default)]
struct State {
    stopped: bool,
    projects: HashMap<PathBuf, BackgroundResources>,
}

/// The shell and subagent tool factories must use resources() from this owner.
pub struct BackgroundService {
    cwd: PathBuf,
    state: Mutex<State>,
}
impl BackgroundService {
    pub fn new(cwd: PathBuf) -> Result<Self, RuntimeError> {
        Ok(Self {
            cwd: absolute(&cwd)?,
            state: Mutex::new(State::default()),
        })
    }
    pub fn resources(&self, project: Option<&str>) -> Result<BackgroundResources, RuntimeError> {
        let key =
            absolute(&project.map_or_else(|| self.cwd.clone(), |project| self.cwd.join(project)))?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return Err(RuntimeError::new(
                "gateway_disposed",
                "Background runtime is stopped.",
            ));
        }
        if let Some(resources) = state.projects.get(&key) {
            return Ok(resources.clone());
        }
        let tasks = BackgroundTaskRuntime::open_persistent(&key).map_err(|error| {
            RuntimeError::new(
                "background_store_failed",
                String::from_utf16_lossy(error.message.units()),
            )
        })?;
        let resources = BackgroundResources {
            tasks,
            subagents: Default::default(),
        };
        state.projects.insert(key, resources.clone());
        Ok(resources)
    }
    pub fn handles(method: &str) -> bool {
        matches!(method, "background_tasks" | "background_task_stop")
    }
    pub async fn handle(&self, method: &str, params: &Value) -> Result<Value, RuntimeError> {
        if !Self::handles(method) {
            return Err(RuntimeError::new("unknown_method", method));
        }
        let session = required(params, "sessionKey", "INVALID_SESSION_KEY")?;
        let task = if method == "background_task_stop" {
            Some(required(params, "taskId", "INVALID_TASK_ID")?)
        } else {
            None
        };
        let project = match params.get("projectKey") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.as_str()),
            _ => {
                return Err(RuntimeError::new(
                    "INVALID_PROJECT_KEY",
                    "projectKey must be a string.",
                ));
            }
        };
        let resources = self.resources(project)?;
        let value = if let Some(task) = task {
            stop_background_task_for_sdk(&resources, &session.into(), &task.into())
                .await
                .map_err(|e| {
                    RuntimeError::new(
                        e.code
                            .unwrap_or_else(|| "gateway_request_failed".into()),
                        String::from_utf16_lossy(e.message.units()),
                    )
                })?
        } else {
            let task = params
                .get("taskId")
                .map(|v| parse_json(&v.to_string()))
                .transpose()
                .map_err(|e| RuntimeError::new("invalid_params", e.to_string()))?
                .unwrap_or(JsValue::Undefined);
            background_tasks_for_sdk(&resources, &session.into(), &task)
        };
        let encoded = stringify_json(&JsValue::Object(value))
            .map_err(|e| RuntimeError::new("serialization_failed", e.to_string()))?
            .ok_or_else(|| {
                RuntimeError::new("serialization_failed", "Background result is undefined.")
            })?;
        serde_json::from_str(&encoded)
            .map_err(|e| RuntimeError::new("serialization_failed", e.to_string()))
    }
    pub async fn stop(&self) {
        let resources = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            state.projects.values().cloned().collect::<Vec<_>>()
        };
        for resource in resources {
            resource.subagents.shutdown();
            resource.tasks.dispose().await;
        }
    }
}
fn required<'a>(params: &'a Value, field: &str, code: &str) -> Result<&'a str, RuntimeError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RuntimeError::new(code, format!("{field} is required.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::background::{StartTaskSpec, TaskStatus};
    use serde_json::json;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("pdrs-background-rpc-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn background_rpc_uses_live_owned_process_and_rejects_other_session() {
        let fixture = Fixture::new();
        let service = BackgroundService::new(fixture.0.clone()).unwrap();
        let resources = service.resources(Some("project")).unwrap();
        let task = resources
            .tasks
            .start(StartTaskSpec {
                command: "sleep 30".into(),
                cwd: fixture.0.to_string_lossy().as_ref().into(),
                env: None,
                session_id: "owner".into(),
                agent_id: JsValue::Undefined,
                kind: "bash".into(),
            })
            .await
            .unwrap();
        let id = task.task_id.to_utf8().unwrap();
        let error = service
            .handle(
                "background_task_stop",
                &json!({"sessionKey":"other","taskId":id,"projectKey":"project"}),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "BACKGROUND_TASK_NOT_FOUND");
        assert_eq!(
            resources.tasks.get(&task.task_id, None).unwrap().status,
            TaskStatus::Running
        );
        let stopped = service
            .handle(
                "background_task_stop",
                &json!({"sessionKey":"owner","taskId":id,"projectKey":fixture.0.join("project")}),
            )
            .await
            .unwrap();
        assert_eq!(stopped["stopped"], true);
        assert!(
            resources
                .tasks
                .get(&task.task_id, None)
                .unwrap()
                .status
                .is_terminal()
        );
        service.stop().await;
        assert!(service.resources(None).is_err());
    }

    #[tokio::test]
    async fn background_rpc_retains_baseline_projection_and_validation() {
        let fixture = Fixture::new();
        let service = BackgroundService::new(fixture.0.clone()).unwrap();
        assert_eq!(
            service
                .handle("background_tasks", &json!({}))
                .await
                .unwrap_err()
                .code,
            "INVALID_SESSION_KEY"
        );
        assert_eq!(
            service
                .handle("background_task_stop", &json!({"sessionKey":"s"}))
                .await
                .unwrap_err()
                .code,
            "INVALID_TASK_ID"
        );
        assert_eq!(
            service
                .handle("background_tasks", &json!({"sessionKey":"s"}))
                .await
                .unwrap(),
            json!({"backgrounded":false,"reason":"no_foreground_tasks"})
        );
        assert_eq!(
            service
                .handle(
                    "background_tasks",
                    &json!({"sessionKey":"s","taskId":"missing"})
                )
                .await
                .unwrap(),
            json!({"backgrounded":false,"reason":"task_not_found"})
        );
        service.stop().await;
    }

    #[tokio::test]
    async fn background_subagent_is_listed_and_stopped_through_the_same_resources() {
        use crate::runtime::background::StartBackgroundSubagentInput;
        let fixture = Fixture::new();
        let service = BackgroundService::new(fixture.0.clone()).unwrap();
        let resources = service.resources(None).unwrap();
        let task = resources.subagents.start(StartBackgroundSubagentInput {
            kind: None,
            session_id: "owner".into(),
            parent_turn_id: "parent".into(),
            subagent_id: "worker".into(),
            subagent_type: "general".into(),
            observed_subagent_id: None,
            run: Box::new(|signal| {
                Box::pin(async move {
                    signal.cancelled().await;
                    Ok(())
                })
            }),
        });
        let id = task.task_id.to_utf8().unwrap();
        assert_eq!(
            service
                .handle(
                    "background_tasks",
                    &json!({"sessionKey":"owner","taskId":id})
                )
                .await
                .unwrap(),
            json!({"backgrounded":true,"taskIds":[id]})
        );
        assert_eq!(
            service
                .handle(
                    "background_tasks",
                    &json!({"sessionKey":"other","taskId":id})
                )
                .await
                .unwrap()["reason"],
            "task_not_found"
        );
        assert_eq!(
            service
                .handle(
                    "background_task_stop",
                    &json!({"sessionKey":"owner","taskId":id})
                )
                .await
                .unwrap()["stopped"],
            true
        );
        assert!(
            resources
                .subagents
                .get(&task.task_id)
                .unwrap()
                .status
                .is_terminal()
        );
        service.stop().await;
    }

    #[test]
    fn background_persistent_store_error_is_not_an_empty_runtime() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("file"), "not a directory").unwrap();
        let service = BackgroundService::new(fixture.0.clone()).unwrap();
        assert_eq!(
            service.resources(Some("file")).err().unwrap().code,
            "background_store_failed"
        );
    }
}
