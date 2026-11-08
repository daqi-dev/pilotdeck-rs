//! Native host-only scope/stop regressions; core scheduler/fire oracle stays #2381.
use super::{
    CronAgentGateway, CronError, CronManager, CronManagerConfig, CronManagerOptions,
    CronTurnSubmit, resolve_cron_paths,
};
use crate::test_scratch::Scratch;
use pilotdeck_protocol::{JsString, parse_json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
const NOW: f64 = 1_767_225_600_000.0;

struct Gateway(mpsc::Sender<String>);
impl CronAgentGateway for Gateway {
    fn submit_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.0
            .send(input.project_key.as_ref().unwrap().to_utf8().unwrap())
            .unwrap();
        Ok(())
    }
    fn abort_turn(
        &self,
        _: &JsString,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> Result<(), CronError> {
        Ok(())
    }
    fn close_session(&self, _: &JsString, _: Option<&JsString>) -> Result<(), CronError> {
        Ok(())
    }
}
fn manager(
    home: &Path,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
) -> (Arc<CronManager>, mpsc::Receiver<String>) {
    let manager = CronManager::new(CronManagerOptions {
        config: CronManagerConfig {
            max_concurrent_runs: 2.0,
            ..Default::default()
        },
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(now),
        uuid: None,
        logger: None,
        scheduler_idle_poll_ms: Some(250),
    })
    .unwrap();
    let (send, receive) = mpsc::channel();
    manager.bind_agent_gateway(Arc::new(Gateway(send))).unwrap();
    (manager, receive)
}
fn seed(home: &Path, project: &str, due: bool) -> super::CronPaths {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project).unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(paths.project_dir.join(".cwd"), format!("{project}\n")).unwrap();
    let date = if due {
        "2025-12-31T00:00:00.000Z"
    } else {
        "2027-01-01T00:00:00.000Z"
    };
    let task = serde_json::json!({"schemaVersion":1,"taskId":"task","message":"seeded","schedule":{"type":"once","runAt":date},"status":"scheduled","sessionKey":format!("cron:{project}"),"channelKey":"cron","projectKey":project,"mode":"default","timezone":"UTC","createdAt":"2025-12-31T00:00:00.000Z","updatedAt":"2025-12-31T00:00:00.000Z","nextRunAt":date,"revision":0});
    fs::write(
        &paths.tasks_file,
        serde_json::json!({"schemaVersion":1,"tasks":[task]}).to_string(),
    )
    .unwrap();
    paths
}
fn wait_finished(paths: &super::CronPaths) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let tasks = fs::read(&paths.tasks_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let history = fs::read_to_string(&paths.run_history_file).unwrap_or_default();
        if tasks
            .as_ref()
            .is_some_and(|value| value["tasks"].as_array().is_some_and(Vec::is_empty))
            && history.contains("\"outcome\":\"completed\"")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fire did not persist terminal state and remove the once task"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn host_scope_preserves_unserved_due_once_and_runs_served_project() {
    let scratch = Scratch::new("cron-host-scope");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let served = seed(&home, "/served", true);
    let excluded = seed(&home, "/unserved", true);
    let before_tasks = fs::read(&excluded.tasks_file).unwrap();
    let before_marker = fs::read(excluded.project_dir.join(".cwd")).unwrap();
    let (manager, calls) = manager(&home, Arc::new(|| NOW));
    manager
        .set_execution_project_filter(Arc::new(|project| project == "/served"))
        .unwrap();
    manager.start().unwrap();
    assert_eq!(
        manager.runtime_count(),
        1,
        "discovery must not load an excluded runtime"
    );
    assert_eq!(
        calls.recv_timeout(Duration::from_secs(4)).unwrap(),
        "/served"
    );
    wait_finished(&served);
    // An explicit readonly lookup can load the runtime later. Neither direct
    // start nor the public test tick may bypass the host execution scope.
    manager
        .list_tasks(&parse_json(r#"{"projectKey":"/unserved"}"#).unwrap())
        .unwrap();
    let excluded_runtime = manager.loaded_runtime("/unserved").unwrap();
    excluded_runtime.start().unwrap();
    let scheduler = excluded_runtime.bound_scheduler().unwrap();
    scheduler.start().unwrap();
    scheduler.run_tick_once().unwrap();
    assert_eq!(scheduler.armed_tick_delay_ms(), None);
    manager.stop().unwrap();
    assert!(calls.try_recv().is_err());
    assert_eq!(fs::read(&excluded.tasks_file).unwrap(), before_tasks);
    assert_eq!(
        fs::read(excluded.project_dir.join(".cwd")).unwrap(),
        before_marker
    );
    assert!(!excluded.run_history_file.exists());
    assert_eq!(excluded_runtime.active_run_count(), 0);
}
#[test]
fn default_manager_still_executes_all_discovered_projects() {
    let scratch = Scratch::new("cron-host-default");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let one = seed(&home, "/one", true);
    let two = seed(&home, "/two", true);
    let (manager, calls) = manager(&home, Arc::new(|| NOW));
    manager.start().unwrap();
    let mut projects = vec![
        calls.recv_timeout(Duration::from_secs(4)).unwrap(),
        calls.recv_timeout(Duration::from_secs(4)).unwrap(),
    ];
    projects.sort();
    assert_eq!(projects, vec!["/one".to_owned(), "/two".to_owned()]);
    wait_finished(&one);
    wait_finished(&two);
    manager.stop().unwrap();
}
#[test]
fn host_scope_must_be_fixed_before_loading_runtimes() {
    let scratch = Scratch::new("cron-host-scope-order");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let (manager, _) = manager(&home, Arc::new(|| NOW));
    manager
        .list_tasks(&parse_json(r#"{"projectKey":"/project"}"#).unwrap())
        .unwrap();
    assert!(
        manager
            .set_execution_project_filter(Arc::new(|_| false))
            .is_err()
    );
    manager.stop().unwrap();
    manager
        .set_execution_project_filter(Arc::new(|_| false))
        .unwrap();
    assert!(
        manager
            .set_execution_project_filter(Arc::new(|_| true))
            .is_err()
    );
    manager.start().unwrap();
    assert!(
        manager
            .set_execution_project_filter(Arc::new(|_| true))
            .is_err()
    );
    manager.stop().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn host_async_stop_keeps_runtime_progressing_and_serializes_repeat_callers() {
    let scratch = Scratch::new("cron-host-stop");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    seed(&home, "/served", false);
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let now = {
        let entered = entered.clone();
        let gate = gate.clone();
        Arc::new(move || {
            if thread::current().name() == Some("cron-scheduler") {
                entered.store(true, Ordering::SeqCst);
                let guard = gate.0.lock().unwrap();
                let _guard = gate
                    .1
                    .wait_timeout_while(guard, Duration::from_secs(2), |released| !*released)
                    .unwrap();
            }
            NOW
        })
    };
    let (manager, _) = manager(&home, now);
    manager.start().unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        Default::default(),
    ));
    let first = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.stop_async().await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while !handle.is_stopped() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let released_on_runtime = Arc::new(AtomicBool::new(false));
    let release = {
        let gate = gate.clone();
        let observed = released_on_runtime.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            observed.store(true, Ordering::SeqCst);
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
        })
    };
    // The old flag alone returned immediately here although the first join
    // was still in progress. Both awaiters must now observe completion.
    handle.stop_async().await.unwrap();
    assert!(
        released_on_runtime.load(Ordering::SeqCst),
        "repeat stop returned before the timer released the tick, or blocked its runtime"
    );
    first.await.unwrap().unwrap();
    release.await.unwrap();
    assert_eq!(manager.runtime_count(), 0);
}

#[test]
fn scoped_migration_preserves_mixed_legacy_store_before_any_data_write() {
    let scratch = Scratch::new("cron-scoped-mixed-migration");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let served = seed(&home, "/served", false);
    let excluded = seed(&home, "/unserved", false);
    let served_before = fs::read(&served.tasks_file).unwrap();
    let excluded_before = fs::read(&excluded.tasks_file).unwrap();
    let one: serde_json::Value = serde_json::from_slice(&served_before).unwrap();
    let two: serde_json::Value = serde_json::from_slice(&excluded_before).unwrap();
    let legacy = home.join("cron/projects/legacy");
    fs::create_dir(&legacy).unwrap();
    let legacy_before =
        serde_json::json!({"schemaVersion":1,"tasks":[one["tasks"][0],two["tasks"][0]]})
            .to_string();
    fs::write(legacy.join("tasks.json"), &legacy_before).unwrap();
    let (manager, calls) = manager(&home, Arc::new(|| NOW));
    manager
        .set_execution_project_filter(Arc::new(|key| key == "/served"))
        .unwrap();
    let error = manager.start().unwrap_err();
    assert!(
        String::from_utf16_lossy(error.message().units())
            .contains("mixed-project or unknown legacy store")
    );
    assert_eq!(fs::read(&served.tasks_file).unwrap(), served_before);
    assert_eq!(fs::read(&excluded.tasks_file).unwrap(), excluded_before);
    assert_eq!(
        fs::read_to_string(legacy.join("tasks.json")).unwrap(),
        legacy_before
    );
    assert!(!home.join("cron/store-migration-v1.json").exists());
    assert_eq!(manager.runtime_count(), 0);
    assert!(calls.try_recv().is_err());
    manager.stop().unwrap();
}

#[test]
fn scoped_migration_routes_wholly_served_legacy_source_without_claiming_global_completion() {
    let scratch = Scratch::new("cron-scoped-served-migration");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let served = seed(&home, "/served", false);
    let legacy_bytes = fs::read(&served.tasks_file).unwrap();
    fs::remove_dir_all(&served.project_dir).unwrap();
    let excluded = seed(&home, "/unserved", false);
    let excluded_before = fs::read(&excluded.tasks_file).unwrap();
    let legacy = home.join("cron/projects/legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("tasks.json"), legacy_bytes).unwrap();
    let (manager, calls) = manager(&home, Arc::new(|| NOW));
    manager
        .set_execution_project_filter(Arc::new(|key| key == "/served"))
        .unwrap();
    manager.start().unwrap();
    let tasks: serde_json::Value =
        serde_json::from_slice(&fs::read(&served.tasks_file).unwrap()).unwrap();
    assert_eq!(tasks["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(tasks["tasks"][0]["projectKey"], "/served");
    let old: serde_json::Value =
        serde_json::from_slice(&fs::read(legacy.join("tasks.json")).unwrap()).unwrap();
    assert!(old["tasks"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(&excluded.tasks_file).unwrap(), excluded_before);
    assert!(!home.join("cron/store-migration-v1.json").exists());
    assert!(calls.try_recv().is_err());
    manager.stop().unwrap();
}

#[test]
fn scoped_migration_rejects_destination_belonging_to_excluded_project_before_staging() {
    let scratch = Scratch::new("cron-scoped-destination-migration");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let served = seed(&home, "/served", false);
    let excluded = seed(&home, "/unserved", false);
    let served_bytes = fs::read(&served.tasks_file).unwrap();
    let excluded_before = fs::read(&excluded.tasks_file).unwrap();
    let legacy = home.join("cron/projects/legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("tasks.json"), &served_bytes).unwrap();
    // A legacy directory collision is not proof of ownership: the target
    // contains another project's records and marker, so preserve it exactly.
    fs::write(&served.tasks_file, &excluded_before).unwrap();
    fs::write(served.project_dir.join(".cwd"), "/unserved\n").unwrap();
    let (manager, calls) = manager(&home, Arc::new(|| NOW));
    manager
        .set_execution_project_filter(Arc::new(|key| key == "/served"))
        .unwrap();
    assert!(manager.start().is_err());
    assert_eq!(fs::read(&served.tasks_file).unwrap(), excluded_before);
    assert_eq!(fs::read(&excluded.tasks_file).unwrap(), excluded_before);
    assert_eq!(fs::read(legacy.join("tasks.json")).unwrap(), served_bytes);
    assert_eq!(
        fs::read_to_string(served.project_dir.join(".cwd")).unwrap(),
        "/unserved\n"
    );
    assert!(calls.try_recv().is_err());
    manager.stop().unwrap();
}
