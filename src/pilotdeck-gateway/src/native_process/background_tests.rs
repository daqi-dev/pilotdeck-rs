use super::*;
use crate::native_process::{Options, assembly};
use crate::runtime::background::{
    StartTaskSpec, TaskStatus, background_tasks_in_registry, stop_background_task_in_registry,
};
use pilotdeck_protocol::JsValue;
use std::{path::PathBuf, time::Duration};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn native_project_background_owner_is_shared_with_sdk_and_drained_on_dispose() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("pdrs-native-background-{}", uuid::Uuid::new_v4())),
    );
    let project = scratch.0.join("project");
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n").unwrap();
    std::fs::write(
        project.join("owned.sh"),
        "printf STARTED > started.txt\nsleep 30\nprintf LATE > never.txt\n",
    )
    .unwrap();
    let options = Options {
        home,
        project: project.clone(),
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: scratch.0.join("unused-token"),
        sidecar: scratch.0.join("unused-sidecar"),
        sidecar_args: vec![],
        drain_budget: Duration::from_secs(2),
        storage_root: None,
    };
    let registry =
        assembly::project_registry(&options, &options.environment(crate::config::Env::new()));
    let runtime = registry.resolve(None).unwrap();
    // Different spellings of one project share the held archive lock. The
    // alternate runtime must not fail staging as if another process owned it.
    let alias = scratch.0.join("project-alias");
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    let alternate = registry.resolve(Some(&alias)).unwrap();
    assert!(!Arc::ptr_eq(&runtime, &alternate));
    assert!(
        !project.join(".pilotdeck/background-tasks").exists(),
        "staging two aliases without tasks must not create an archive"
    );

    let resource = runtime
        .resources()
        .get::<crate::runtime::project::ProjectResources>()
        .unwrap()
        .context()
        .resource(Resource::ExecutionWorld)
        .unwrap();
    let resources = resource
        .as_any()
        .downcast_ref::<crate::runtime::background::BackgroundExecutionWorld>()
        .unwrap()
        .resources
        .clone();
    let task = resources
        .tasks
        .start(StartTaskSpec {
            command: "bash owned.sh".into(),
            cwd: project.to_string_lossy().into_owned().into(),
            env: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
            session_id: "owner".into(),
            agent_id: JsValue::Undefined,
            kind: JsValue::Undefined,
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !project.join("started.txt").exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Record assertions before cleanup, but evaluate only after the registry
    // has drained the process even when an observation is wrong.
    let started = project.join("started.txt").exists();
    let owner = background_tasks_in_registry(
        &registry,
        None,
        &"owner".into(),
        &JsValue::String(task.task_id.clone()),
    );
    let other = background_tasks_in_registry(
        &registry,
        None,
        &"other".into(),
        &JsValue::String(task.task_id.clone()),
    );
    let denied =
        stop_background_task_in_registry(&registry, None, &"other".into(), &task.task_id).await;
    let was_running = resources
        .tasks
        .get(&task.task_id, None)
        .is_some_and(|task| task.status == TaskStatus::Running);
    let cleaned = tokio::time::timeout(Duration::from_secs(10), registry.dispose()).await;
    assert!(cleaned.unwrap().is_ok());
    assert!(started, "owned child never reached its witness");
    // This SDK method backgrounds foreground work; shell tasks are already
    // detached. Its exact shell result is not a task-list envelope. Selecting
    // the task distinguishes ownership without inventing a taskIds field.
    let owner = owner.unwrap();
    assert_eq!(
        JsValue::Object(owner),
        crate::metadata::object([
            ("backgrounded", JsValue::Bool(false)),
            ("reason", "no_foreground_tasks".into()),
        ])
    );
    let other = other.unwrap();
    assert_eq!(
        JsValue::Object(other),
        crate::metadata::object([
            ("backgrounded", JsValue::Bool(false)),
            ("reason", "task_not_found".into()),
        ])
    );
    let denied = denied.unwrap_err();
    assert_eq!(denied.code, Some("BACKGROUND_TASK_NOT_FOUND".into()));
    assert!(
        was_running,
        "cross-session stop affected the owner's process"
    );
    assert_eq!(
        resources.tasks.get(&task.task_id, None).unwrap().status,
        TaskStatus::Cancelled
    );
    assert!(!project.join("never.txt").exists());
    assert!(
        project
            .join(".pilotdeck/background-tasks/state.json")
            .is_file()
    );
}

#[tokio::test]
async fn readonly_unregistered_project_stage_leaves_no_archive_and_existing_foreign_lock_is_rejected() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("pdrs-native-background-readonly-{}", uuid::Uuid::new_v4())),
    );
    let project = scratch.0.join("project-a");
    let other = scratch.0.join("unregistered-b");
    let locked = scratch.0.join("unregistered-locked");
    let home = scratch.0.join("home");
    for path in [&project, &other, &locked, &home] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n").unwrap();
    let options = Options {
        home,
        project: project.clone(),
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: scratch.0.join("unused-token"),
        sidecar: scratch.0.join("unused-sidecar"),
        sidecar_args: vec![],
        drain_budget: Duration::from_secs(2),
        storage_root: None,
    };
    let registry =
        assembly::project_registry(&options, &options.environment(crate::config::Env::new()));
    let other_runtime = registry.resolve(Some(&other));
    let untouched_after_stage = !other.join(".pilotdeck").exists();
    // This archive existed before the runtime was staged, so staging must
    // retain eager foreign-lock admission rather than pretending it is empty.
    let foreign = super::super::background_archive::NativeArchive::open(&locked).unwrap();
    let locked_result = registry.resolve(Some(&locked));
    let closed = tokio::time::timeout(Duration::from_secs(10), registry.dispose()).await;
    assert!(closed.unwrap().is_ok());
    assert!(other_runtime.is_ok(), "an unregistered read-only project must stage normally");
    assert!(untouched_after_stage, "read-only staging wrote into the unregistered project");
    assert!(!other.join(".pilotdeck").exists(), "read-only disposal created an archive");
    let error = locked_result.err().expect("existing foreign archive lock must reject staging");
    assert!(error.to_string().contains("Another process owns this project's background archive."));
    drop(foreign);
}
