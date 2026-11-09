//! One native ledger is shared by SDK configuration, admission and terminal
//! settlement. Blocking disk work owns its permit and continues after caller
//! cancellation, so a successful retry cannot skip an unfinished append.
use super::sdk_adapter::NativeSdkPort;
use crate::{
    metadata::object,
    runtime::{
        sdk_session::{SdkBudgetFuture, SdkTaskBudget},
        turn::{
            admission::SubmitTaskBudgetSnapshot,
            submit::{SubmitError, SubmitFuture},
        },
    },
    storage::{TaskBudgetError, TaskBudgetScope, TaskBudgetStore},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(super) struct NativeBudgetLedger {
    home: PathBuf,
    store: Arc<Mutex<Option<TaskBudgetStore>>>,
    slots: Arc<tokio::sync::Semaphore>,
}
impl NativeBudgetLedger {
    pub(super) fn new(home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            home,
            store: Arc::default(),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut TaskBudgetStore) -> Result<T, TaskBudgetError> + Send + 'static,
    ) -> Result<T, TaskBudgetError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| TaskBudgetError::Invalid(e.to_string()))?;
        let store = self.store.clone();
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = store.lock().map_err(|_| {
                TaskBudgetError::Invalid(
                    "Native task budget store is unavailable after a worker failure.".into(),
                )
            })?;
            if guard.is_none() {
                *guard = Some(TaskBudgetStore::new(&home, None).with_native_home(&home)?);
            }
            f(guard.as_mut().expect("initialized above"))
        })
        .await
        .map_err(|e| TaskBudgetError::Invalid(e.to_string()))?
    }
}
impl SdkTaskBudget for NativeBudgetLedger {
    fn configure_project(
        &self,
        _: &JsString,
        _: &JsString,
        _: f64,
        _: Option<f64>,
        _: f64,
    ) -> Result<(), TaskBudgetError> {
        Err(TaskBudgetError::Invalid(
            "Native task-budget writes require the asynchronous SDK path.".into(),
        ))
    }
    fn clear_session(&self, _: &JsString, _: &JsString) -> Result<(), TaskBudgetError> {
        Err(TaskBudgetError::Invalid(
            "Native task-budget clearing requires the asynchronous SDK path.".into(),
        ))
    }
    fn configure_project_async<'a>(
        &'a self,
        root: &'a JsString,
        session: &'a JsString,
        total: f64,
        retention: Option<f64>,
        now: f64,
    ) -> SdkBudgetFuture<'a> {
        let root = root.clone();
        let session = session.clone();
        Box::pin(async move {
            self.run(move |store| store.configure_project(&root, &session, total, retention, now))
                .await
        })
    }
    fn clear_session_async<'a>(
        &'a self,
        root: &'a JsString,
        session: &'a JsString,
    ) -> SdkBudgetFuture<'a> {
        let root = root.clone();
        let session = session.clone();
        Box::pin(async move {
            self.run(move |store| {
                store.load()?;
                store.clear_session(&root, &session)
            })
            .await
        })
    }
}
pub(super) struct NativeTaskBudget {
    pub ledger: Arc<NativeBudgetLedger>,
    pub sdk: Arc<NativeSdkPort>,
    pub fallback: PathBuf,
}
impl NativeTaskBudget {
    async fn config(
        &self,
        session: &JsString,
        project: Option<&JsString>,
    ) -> Result<Option<(JsString, TaskBudgetScope, f64)>, SubmitError> {
        let registry = self.sdk.registry.lock().await;
        let Some(budget) = registry
            .session_config(session)
            .and_then(|v| v.get("taskBudget"))
            .filter(|v| !v.is_undefined())
        else {
            return Ok(None);
        };
        let total = budget
            .get("total")
            .and_then(JsValue::as_number)
            .ok_or_else(|| {
                SubmitError::new(
                    "INVALID_SDK_SESSION_CONFIG",
                    "taskBudget.total must be a positive finite USD amount.",
                )
            })?;
        let scope = if budget
            .get("scope")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("project"))
        {
            TaskBudgetScope::Project
        } else {
            TaskBudgetScope::Session
        };
        let root = project
            .cloned()
            .or_else(|| registry.session_project(session))
            .unwrap_or_else(|| self.fallback.to_string_lossy().into_owned().into());
        // Native admission already binds projectKey to a registered canonical
        // owner. Preserve that same identity across snapshot and settlement.
        let root = TaskBudgetStore::resolve_project_root(&root).map_err(submit_error)?;
        Ok(Some((root, scope, total)))
    }
}
impl SubmitTaskBudgetSnapshot for NativeTaskBudget {
    fn snapshot<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            let Some((root, scope, total)) = self.config(session, project).await? else {
                return Ok(JsValue::Undefined);
            };
            let session = session.clone();
            self.ledger
                .run(move |store| {
                    let state = store.get(&root, scope, &session)?;
                    let total = if scope == TaskBudgetScope::Project {
                        state.and_then(|s| s.total_usd.as_number()).unwrap_or(total)
                    } else {
                        total
                    };
                    Ok(object([
                        ("totalUsd", JsValue::Number(total)),
                        (
                            "spentUsd",
                            JsValue::Number(state.map_or(0.0, |s| s.spent_usd)),
                        ),
                    ]))
                })
                .await
                .map_err(submit_error)
        })
    }
    fn settle<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
        run: &'a JsString,
        spent: f64,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async move {
            if !spent.is_finite() || spent < 0.0 {
                return Ok(());
            }
            let Some((root, scope, _)) = self.config(session, project).await? else {
                return Ok(());
            };
            let session = session.clone();
            let run = run.clone();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as f64;
            self.ledger
                .run(move |store| store.settle(&root, scope, &session, &run, spent, now))
                .await
                .map_err(submit_error)
        })
    }
}
fn submit_error(error: TaskBudgetError) -> SubmitError {
    SubmitError::new(
        error.code().unwrap_or("gateway_request_failed"),
        error.to_string(),
    )
}
pub(super) fn validate(config: &JsValue) -> Result<(), SubmitError> {
    let Some(value) = config.get("taskBudget").filter(|v| !v.is_undefined()) else {
        return Ok(());
    };
    let invalid = |message| SubmitError::new("INVALID_SDK_SESSION_CONFIG", message);
    if !value
        .get("total")
        .and_then(JsValue::as_number)
        .is_some_and(|v| v.is_finite() && v > 0.0)
    {
        return Err(invalid(
            "taskBudget.total must be a positive finite USD amount.",
        ));
    }
    if let Some(scope) = value.get("scope").filter(|v| !v.is_undefined())
        && !scope
            .as_string()
            .is_some_and(|v| v.equals_str("session") || v.equals_str("project"))
    {
        return Err(invalid(
            "taskBudget.scope must be session or project when provided.",
        ));
    }
    if let Some(retention) = value
        .get("projectRetentionMs")
        .filter(|v| !v.is_undefined())
    {
        if !retention.as_number().is_some_and(|v| {
            v.is_finite() && v > 0.0 && v <= 9_007_199_254_740_991.0 && v.fract() == 0.0
        }) {
            return Err(invalid(
                "taskBudget.projectRetentionMs must be a positive safe integer in milliseconds.",
            ));
        }
        if !value
            .get("scope")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str("project"))
        {
            return Err(invalid(
                "taskBudget.projectRetentionMs requires taskBudget.scope to be project.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_budget_is_durable_deduplicated_and_project_clear_preserves_others() {
        let home = std::env::temp_dir().join(format!("pdrs-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let ledger = NativeBudgetLedger::new(home.clone());
        ledger
            .configure_project_async(&"/project".into(), &"one".into(), 3.0, Some(1000.0), 10.0)
            .await
            .unwrap();
        assert!(matches!(
            ledger
                .configure_project_async(&"/project".into(), &"two".into(), 4.0, None, 11.0)
                .await,
            Err(TaskBudgetError::TotalConflict(_))
        ));
        for _ in 0..2 {
            ledger
                .run(|store| {
                    store.settle(
                        &"/project".into(),
                        TaskBudgetScope::Project,
                        &"one".into(),
                        &"run".into(),
                        1.5,
                        20.0,
                    )
                })
                .await
                .unwrap();
        }
        ledger
            .run(|store| {
                store.settle(
                    &"/project".into(),
                    TaskBudgetScope::Session,
                    &"two".into(),
                    &"second".into(),
                    0.25,
                    21.0,
                )
            })
            .await
            .unwrap();
        ledger
            .clear_session_async(&"/project".into(), &"one".into())
            .await
            .unwrap();
        drop(ledger);
        let restored = NativeBudgetLedger::new(home.clone());
        restored
            .run(|store| {
                assert_eq!(
                    store
                        .get(
                            &"/project".into(),
                            TaskBudgetScope::Project,
                            &"other".into()
                        )?
                        .unwrap()
                        .spent_usd,
                    1.5
                );
                assert_eq!(
                    store
                        .get(&"/project".into(), TaskBudgetScope::Session, &"two".into())?
                        .unwrap()
                        .spent_usd,
                    0.25
                );
                assert!(
                    store
                        .get(&"/project".into(), TaskBudgetScope::Session, &"one".into())?
                        .is_none()
                );
                Ok(())
            })
            .await
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
    #[tokio::test]
    async fn native_budget_refuses_fifo_symlink_and_never_acknowledges_failed_settlement() {
        use std::os::unix::fs::symlink;
        let home = std::env::temp_dir().join(format!("pdrs-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("gateway")).unwrap();
        let path = home.join("gateway/sdk-task-budget-ledger.jsonl");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let ledger = NativeBudgetLedger::new(home.clone());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), ledger.run(|s| s.load()))
                .await
                .unwrap()
                .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        drop(ledger);
        let outside = home.join("untouched");
        std::fs::write(&outside, b"original").unwrap();
        symlink(&outside, &path).unwrap();
        let ledger = NativeBudgetLedger::new(home.clone());
        assert!(ledger.run(|s| s.load()).await.is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        std::fs::remove_file(&path).unwrap();
        drop(ledger);
        let ledger = NativeBudgetLedger::new(home.clone());
        ledger.run(|s| s.load()).await.unwrap();
        symlink(&outside, &path).unwrap();
        let settle = |s: &mut TaskBudgetStore| {
            s.settle(
                &"/p".into(),
                TaskBudgetScope::Session,
                &"s".into(),
                &"r".into(),
                1.0,
                1.0,
            )
        };
        assert!(ledger.run(settle).await.is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(
            ledger.run(settle).await.is_err(),
            "failed in-memory settlement cannot become successful duplicate"
        );
        assert!(!path.exists());
        assert_eq!(std::fs::read(outside).unwrap(), b"original");
        std::fs::remove_dir_all(home).unwrap();
    }
}
