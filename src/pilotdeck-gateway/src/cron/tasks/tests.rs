use super::*;
use std::sync::Barrier;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-cron-methods-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn service(&self) -> CronTasks {
        self.service_with(CronConfig::default())
    }
    fn service_with(&self, config: CronConfig) -> CronTasks {
        let count = AtomicUsize::new(0);
        CronTasks::with_sources(
            self.0.clone(),
            config,
            Arc::new(now),
            Arc::new(move || format!("task-{}", count.fetch_add(1, Ordering::SeqCst))),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Host {
    prepared: Mutex<Vec<Value>>,
    released: Mutex<Vec<Value>>,
    active: Mutex<Option<Value>>,
    fail_abort: bool,
    pokes: AtomicUsize,
}
impl CronTaskHost for Host {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            self.prepared.lock().unwrap().push(task.clone());
            Ok(())
        })
    }
    fn release_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            self.released.lock().unwrap().push(task.clone());
            Ok(())
        })
    }
    fn stop_run<'a>(
        &'a self,
        _project: &'a str,
        task: Option<&'a str>,
        run: Option<&'a str>,
    ) -> TaskFuture<'a, Option<Value>> {
        Box::pin(async move {
            if self.fail_abort {
                return Err(CronError::new("abort_test", "abort failed"));
            }
            Ok(self.active.lock().unwrap().clone().filter(|active| {
                task.is_none_or(|id| text(active, "taskId") == id)
                    && run.is_none_or(|id| text(active, "runId") == id)
            }))
        })
    }
    fn poke(&self) {
        self.pokes.fetch_add(1, Ordering::SeqCst);
    }
}
fn now() -> i64 {
    schedule::parse_date("2026-06-02T00:00:00Z")
        .unwrap()
        .unwrap()
}
fn input(project: &str) -> Value {
    json!({"projectKey":project,"message":" Review project ","sessionKey":"cli:origin","channelKey":"cli","schedule":{"type":"cron","expression":"0 9 * * 1","timezone":"Asia/Shanghai"}})
}
fn record(task: &Value, id: &str) -> Value {
    json!({"schemaVersion":1,"runId":id,"taskId":task["taskId"],"sessionKey":task["sessionKey"],"projectKey":task["projectKey"],"startedAt":"2026-06-01T00:00:00.000Z","finishedAt":"2026-06-01T00:01:00.000Z","outcome":"completed"})
}

#[tokio::test]
async fn six_routes_create_update_list_delete_and_local_errors() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/pdrs-cron-tests/project";
    assert!(service.handle("unrelated", &json!({}), &host).is_none());
    let created = service
        .handle("cron_create", &input(project), &host)
        .unwrap()
        .await
        .unwrap();
    let task = &created["task"];
    let id = text(task, "taskId");
    assert_eq!(task["revision"], 0);
    assert_eq!(task["scheduleComputationVersion"], 3);
    assert_eq!(task["sessionKey"], format!("cron:{id}"));
    assert_eq!(task["channelKey"], "cron");
    assert_eq!(task["originSessionKey"], "cli:origin");
    assert_eq!(task["message"], " Review project ");
    let list = service
        .handle("cron_list", &json!({"projectKey":project}), &host)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 1);
    assert!(list.get("recentRuns").is_none());
    let store = service.store(project).unwrap();
    store.append_run(&record(task, "historic")).unwrap();
    let update = json!({"taskId":id,"projectKey":project,"expectedRevision":0,"message":"Updated prompt","timezone":"America/New_York","schedule":{"type":"once","runAt":"2026-07-01T13:30:00Z"}});
    let updated = service
        .handle("cron_update", &update, &host)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(updated["updated"], true);
    assert_eq!(updated["task"]["revision"], 1);
    for field in ["taskId", "createdAt", "sessionKey", "originSessionKey"] {
        assert_eq!(updated["task"][field], task[field]);
    }
    assert!(updated["task"].get("scheduleComputationVersion").is_none());
    assert_eq!(
        service.update_task(&update, &host).await.unwrap(),
        json!({"updated":false,"reason":"conflict"})
    );
    let history = service
        .list_tasks(&json!({"includeHistory":true,"limit":1}))
        .unwrap();
    assert_eq!(history["recentRuns"][0]["runId"], "historic");
    let removed = service
        .handle(
            "cron_delete",
            &json!({"taskId":id,"projectKey":project}),
            &host,
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(removed, json!({"deleted":true}));
    assert_eq!(
        service
            .delete_task(&json!({"taskId":id}), &host)
            .await
            .unwrap(),
        json!({"deleted":false})
    );
    assert_eq!(store.list_runs(None).unwrap().len(), 1);
    assert_eq!(host.released.lock().unwrap().len(), 1);
    for method in ["cron_create", "cron_update", "cron_delete", "cron_run_now"] {
        assert!(
            service
                .handle(method, &json!({"taskId":15}), &host)
                .unwrap()
                .await
                .is_err(),
            "{method}"
        );
    }
}

#[tokio::test]
async fn create_delay_defaults_future_validation_and_disabled() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let created=service.create_task(&json!({"projectKey":"/tmp/cron-delay","message":"","schedule":{"type":"delay","amount":0.5,"unit":"minute"}}),&host).await.unwrap();
    assert_eq!(
        created["task"]["schedule"],
        json!({"type":"once","runAt":schedule::iso(now()+30_000)})
    );
    assert_eq!(created["task"]["timezone"], "UTC");
    assert!(created["task"].get("originSessionKey").is_none());
    for bad in [
        json!({"message":"a","schedule":{"type":"once","runAt":schedule::iso(now())}}),
        json!({"projectKey":"/tmp/cron-delay","message":"a","schedule":{"type":"once","runAt":schedule::iso(now()-1)}}),
        json!({"projectKey":"/tmp/cron-delay","message":"a","schedule":{"type":"cron","expression":"broken"}}),
        json!({"projectKey":"/tmp/cron-delay","message":"a","schedule":{"type":"delay","amount":0,"unit":"minute"}}),
    ] {
        assert!(service.create_task(&bad, &host).await.is_err());
    }
    let disabled = fixture.service_with(CronConfig {
        enabled: false,
        ..Default::default()
    });
    assert!(
        disabled
            .create_task(&input("/tmp/disabled"), &host)
            .await
            .unwrap_err()
            .message
            .contains("disabled")
    );
}

#[tokio::test]
async fn update_original_negatives_and_running_precedes_conflict() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-update";
    let task = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let id = text(&task, "taskId");
    let update = json!({"projectKey":project,"taskId":id,"expectedRevision":0,"message":"changed","schedule":{"type":"once","runAt":"2026-07-01T00:00:00Z"}});
    for (key, value) in [
        ("message", json!(" ")),
        ("expectedRevision", json!(-1)),
        ("expectedRevision", json!(0.5)),
        ("expectedRevision", json!(9_007_199_254_740_992_u64)),
        ("timezone", json!("Not/AZone")),
    ] {
        let mut bad = update.clone();
        bad[key] = value;
        assert!(service.update_task(&bad, &host).await.is_err());
        assert_eq!(
            service
                .store(project)
                .unwrap()
                .get_task(id)
                .unwrap()
                .unwrap()["revision"],
            0
        );
    }
    let mut wrong = update.clone();
    wrong["projectKey"] = json!("/tmp/other");
    assert_eq!(
        service.update_task(&wrong, &host).await.unwrap()["reason"],
        "not_found"
    );
    let store = service.store(project).unwrap();
    let claimed = store.claim(&task, "run", now()).unwrap().unwrap();
    assert_eq!(
        service.update_task(&update, &host).await.unwrap()["reason"],
        "running"
    );
    assert_eq!(claimed["revision"], 1);
}

#[tokio::test]
async fn disabled_update_precedes_unknown_task_lookup() {
    let fixture = Fixture::new();
    let disabled = fixture.service_with(CronConfig {
        enabled: false,
        ..Default::default()
    });
    let error = disabled
        .update_task(
            &json!({
                "projectKey":"/tmp/cron-disabled-update",
                "taskId":"missing",
                "expectedRevision":0,
                "message":"changed",
                "schedule":{"type":"once","runAt":"2026-07-01T00:00:00Z"}
            }),
            &Host::default(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("disabled"));
}

#[tokio::test]
async fn run_now_copies_task_without_mutating_original_or_claiming_started() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-now";
    let original = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let id = text(&original, "taskId");
    let result = service
        .handle(
            "cron_run_now",
            &json!({"projectKey":project,"taskId":id}),
            &host,
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(result["started"], true);
    assert_ne!(result["taskId"], original["taskId"]);
    let store = service.store(project).unwrap();
    let copy = store.get_task(text(&result, "taskId")).unwrap().unwrap();
    assert_eq!(
        copy["schedule"],
        json!({"type":"once","runAt":schedule::iso(now())})
    );
    assert_eq!(copy["timezone"], "UTC");
    assert_eq!(copy["originSessionKey"], original["originSessionKey"]);
    assert_eq!(copy["status"], "scheduled");
    assert_eq!(store.get_task(id).unwrap(), Some(original.clone()));
    store.claim(&original, "running", now()).unwrap();
    assert_eq!(
        service
            .run_task_now(&json!({"taskId":id}), &host)
            .await
            .unwrap()["reason"],
        "already_running"
    );
    assert_eq!(
        service
            .run_task_now(&json!({"taskId":"missing"}), &host)
            .await
            .unwrap(),
        json!({"started":false,"reason":"not_found"})
    );
}

#[tokio::test]
async fn stop_and_delete_once_double_delete_and_abort_failure() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-stop";
    let task=service.create_task(&json!({"projectKey":project,"message":"one","schedule":{"type":"once","runAt":schedule::iso(now())}}),&host).await.unwrap()["task"].clone();
    let store = service.store(project).unwrap();
    let id = text(&task, "taskId");
    store.claim(&task, "run", now()).unwrap();
    *host.active.lock().unwrap() = Some(json!({"taskId":id,"runId":"run","scheduleType":"once"}));
    assert_eq!(
        service
            .stop_task(
                &json!({"projectKey":project,"taskId":id,"runId":"wrong"}),
                &host
            )
            .await
            .unwrap()["stopped"],
        false
    );
    assert_eq!(
        service.stop_task(&json!({}), &host).await.unwrap(),
        json!({"stopped":false})
    );
    let deleted = service
        .delete_task(
            &json!({"taskId":id,"projectKey":project,"stopRunning":true}),
            &host,
        )
        .await
        .unwrap();
    assert_eq!(deleted, json!({"deleted":false,"stoppedRunId":"run"}));
    assert!(store.get_task(id).unwrap().is_none());
    let recurring = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let id = text(&recurring, "taskId");
    store.claim(&recurring, "second", now()).unwrap();
    let failing = Host {
        fail_abort: true,
        ..Default::default()
    };
    assert!(
        service
            .delete_task(
                &json!({"taskId":id,"projectKey":project,"stopRunning":true}),
                &failing
            )
            .await
            .is_err()
    );
    assert!(store.get_task(id).unwrap().is_some());
    *host.active.lock().unwrap() =
        Some(json!({"taskId":id,"runId":"second","scheduleType":"cron"}));
    let stopped = service
        .handle("cron_stop", &json!({"taskId":id,"runId":"second"}), &host)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(stopped["deletedOneTimeTask"], false);
    assert!(store.get_task(id).unwrap().is_some());
}

#[tokio::test]
async fn project_ambiguity_sort_history_limit_and_restart_discovery() {
    let fixture = Fixture::new();
    let service = CronTasks::with_sources(
        fixture.0.clone(),
        Default::default(),
        Arc::new(now),
        Arc::new(|| "same".into()),
    )
    .unwrap();
    let host = Host::default();
    for project in ["/tmp/project-a", "/tmp/project-b"] {
        let task = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
        let store = service.store(project).unwrap();
        store.append_run(&record(&task, project)).unwrap();
    }
    assert!(
        service
            .delete_task(&json!({"taskId":"same"}), &host)
            .await
            .unwrap_err()
            .message
            .contains("ambiguous")
    );
    assert_eq!(
        service
            .list_tasks(&json!({"includeHistory":true,"limit":0}))
            .unwrap()["recentRuns"],
        json!([])
    );
    assert_eq!(
        service
            .list_tasks(&json!({"projectKey":"/tmp/project-a","includeHistory":true,"limit":0}))
            .unwrap()["recentRuns"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let fresh = fixture.service();
    assert!(fresh.stores().unwrap().is_empty());
    assert_eq!(fresh.discover().unwrap().len(), 2);
    assert_eq!(
        fresh.list_tasks(&json!({})).unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn revision_transaction_and_duplicate_claim_are_serialized_across_store_instances() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-concurrent";
    let original = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for number in 0..8 {
        let store = CronStore::new(&fixture.0, project).unwrap();
        let snapshot = original.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .claim(&snapshot, &format!("run-{number}"), now())
                .unwrap()
                .is_some()
        }));
    }
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>(),
        1
    );
    let store = service.store(project).unwrap();
    let claimed = store.get_task(text(&original, "taskId")).unwrap().unwrap();
    assert_eq!(claimed["revision"], 1);
    assert!(
        store
            .finalize(&claimed, text(&claimed, "lastRunId"), now() + 60_000, "UTC")
            .unwrap()
    );
    assert!(
        !store
            .finalize(&claimed, text(&claimed, "lastRunId"), now() + 60_000, "UTC")
            .unwrap()
    );
    let barrier = Arc::new(Barrier::new(8));
    let successes = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let store = CronStore::new(&fixture.0, project).unwrap();
        let barrier = barrier.clone();
        let successes = successes.clone();
        let id = text(&original, "taskId").to_owned();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .update_task(&id, |task| {
                    if revision_of(task) != 2 {
                        return Ok(Some(task.clone()));
                    }
                    let mut updated = task.clone();
                    bump_revision(&mut updated)?;
                    successes.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(updated))
                })
                .unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(successes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn old_snapshot_does_not_fire_or_delay_after_edit() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-stale";
    let old = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let store = service.store(project).unwrap();
    store
        .update_task(text(&old, "taskId"), |task| {
            let mut task = task.clone();
            task["message"] = json!("edited");
            task["nextRunAt"] = json!(schedule::iso(now() + 3_600_000));
            bump_revision(&mut task)?;
            Ok(Some(task))
        })
        .unwrap();
    assert!(store.claim(&old, "stale", now()).unwrap().is_none());
    assert!(!store.delay(&old, now()).unwrap());
    assert_eq!(
        store.get_task(text(&old, "taskId")).unwrap().unwrap()["message"],
        "edited"
    );
}

#[tokio::test]
async fn interrupted_runs_recover_once_without_duplicate_terminal_and_once_is_removed() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-recover";
    let recurring = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let store = service.store(project).unwrap();
    store.claim(&recurring, "interrupted", now()).unwrap();
    let once=service.create_task(&json!({"projectKey":project,"message":"one","schedule":{"type":"once","runAt":schedule::iso(now())}}),&host).await.unwrap()["task"].clone();
    store.claim(&once, "once-interrupted", now()).unwrap();
    let reopened = CronStore::new(&fixture.0, project).unwrap();
    assert_eq!(reopened.recover(now() + 1000, "UTC").unwrap().len(), 2);
    assert!(reopened.get_task(text(&once, "taskId")).unwrap().is_none());
    assert_eq!(
        reopened
            .get_task(text(&recurring, "taskId"))
            .unwrap()
            .unwrap()["status"],
        "scheduled"
    );
    let history = reopened.list_runs(None).unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|run| run["error"]["code"] == "cron_run_interrupted")
    );
    assert!(reopened.recover(now() + 2000, "UTC").unwrap().is_empty());
    assert_eq!(reopened.list_runs(None).unwrap().len(), 2);
}

#[tokio::test]
async fn legacy_revision_session_and_v2_cache_migrations_preserve_overdue() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-v2";
    let mut task = service.create_task(&input(project), &host).await.unwrap()["task"].clone();
    let store = service.store(project).unwrap();
    task.as_object_mut().unwrap().remove("revision");
    task["schedule"] = json!({"type":"cron","expression":"0 9 1 * 1","timezone":"UTC"});
    task["scheduleComputationVersion"] = json!(2);
    task["nextRunAt"] = json!("2026-07-01T09:00:00.000Z");
    task["sessionKey"] = json!("legacy:user");
    task["channelKey"] = json!("cli");
    store.put_task(&task).unwrap();
    assert_eq!(
        store.get_task(text(&task, "taskId")).unwrap().unwrap()["revision"],
        0
    );
    assert_eq!(store.migrate_sessions(now()).unwrap().len(), 1);
    store.recalculate(now(), "UTC").unwrap();
    let migrated = store.get_task(text(&task, "taskId")).unwrap().unwrap();
    assert_eq!(migrated["nextRunAt"], "2026-06-08T09:00:00.000Z");
    assert_eq!(migrated["scheduleComputationVersion"], 3);
    store.recalculate(now(), "UTC").unwrap();
    assert_eq!(
        store.get_task(text(&task, "taskId")).unwrap(),
        Some(migrated.clone())
    );
    let mut overdue = migrated;
    overdue["scheduleComputationVersion"] = json!(2);
    overdue["nextRunAt"] = json!("2026-06-01T09:00:00.000Z");
    store.put_task(&overdue).unwrap();
    store.recalculate(now(), "UTC").unwrap();
    assert_eq!(
        store.get_task(text(&task, "taskId")).unwrap().unwrap()["nextRunAt"],
        overdue["nextRunAt"]
    );
}

#[tokio::test]
async fn corrupt_store_and_history_fail_without_mutating_original() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let project = "/tmp/cron-corrupt";
    let store = service.store(project).unwrap();
    let path = store.project_dir.join("tasks.json");
    std::fs::write(&path, b"{broken").unwrap();
    assert_eq!(
        service
            .create_task(&input(project), &host)
            .await
            .unwrap_err()
            .code,
        "cron_store_corrupt"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    assert_eq!(host.prepared.lock().unwrap().len(), 1);
    assert_eq!(host.released.lock().unwrap().len(), 1);
    std::fs::remove_file(&path).unwrap();
    let history = store.project_dir.join("run-history.jsonl");
    std::fs::write(&history, b"broken\n").unwrap();
    assert_eq!(
        store.list_runs(None).unwrap_err().code,
        "cron_store_corrupt"
    );
    assert_eq!(std::fs::read(history).unwrap(), b"broken\n");
}

#[tokio::test]
async fn event_log_persists_real_payload_and_sanitizes_run_id() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let store = service.store("/tmp/cron-events").unwrap();
    store
        .append_run_event(
            "../../run:1",
            &json!({"type":"assistant_text_delta","text":"actual"}),
        )
        .unwrap();
    let raw =
        std::fs::read_to_string(store.project_dir.join("runs/..-..-run-1.events.jsonl")).unwrap();
    let event: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(event["runId"], "../../run:1");
    assert_eq!(event["event"]["text"], "actual");
}

#[test]
fn injected_clock_has_no_global_environment_side_effect() {
    let fixture = Fixture::new();
    let clock = Arc::new(AtomicI64::new(now()));
    let reader = clock.clone();
    let service = CronTasks::with_sources(
        fixture.0.clone(),
        Default::default(),
        Arc::new(move || reader.load(Ordering::SeqCst)),
        Arc::new(|| "fixed".into()),
    )
    .unwrap();
    clock.fetch_add(30_000, Ordering::SeqCst);
    assert_eq!(service.now_ms(), now() + 30_000);
}

#[tokio::test]
async fn legacy_project_slug_collision_is_rejected_without_overwriting_marker() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let first = "/tmp/pdrs-cron/a-b";
    let other = "/tmp/pdrs-cron/a/b";
    service.create_task(&input(first), &host).await.unwrap();
    let store = service.store(first).unwrap();
    assert_eq!(
        service.store(other).unwrap_err().code,
        "cron_project_collision"
    );
    assert_eq!(
        std::fs::read_to_string(store.project_dir.join(".cwd")).unwrap(),
        format!("{first}\n")
    );
    assert_eq!(store.list_tasks().unwrap().len(), 1);
}

#[test]
#[ignore = "invoked only by cross_process_claim_uses_os_lock"]
fn process_claim_child() {
    let root = std::env::var_os("PDRS_CRON_LOCK_TEST_ROOT").expect("isolated test root");
    let store = CronStore::new(Path::new(&root), "/tmp/cron-process-lock").unwrap();
    let task: Value =
        serde_json::from_str(&std::env::var("PDRS_CRON_LOCK_TEST_TASK").unwrap()).unwrap();
    let _ = store
        .claim(&task, &format!("process-{}", std::process::id()), now())
        .unwrap();
}

#[tokio::test]
async fn cross_process_claim_uses_os_lock() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let host = Host::default();
    let task = service
        .create_task(&input("/tmp/cron-process-lock"), &host)
        .await
        .unwrap()["task"]
        .clone();
    struct Children(Vec<std::process::Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut children = Children(Vec::new());
    for _ in 0..4 {
        children.0.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "cron::tasks::tests::process_claim_child",
                ])
                .env("PDRS_CRON_LOCK_TEST_ROOT", &fixture.0)
                .env("PDRS_CRON_LOCK_TEST_TASK", task.to_string())
                .spawn()
                .unwrap(),
        );
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    for child in &mut children.0 {
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Cron OS lock child did not finish within 20 seconds");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let stored = service
        .store("/tmp/cron-process-lock")
        .unwrap()
        .get_task(text(&task, "taskId"))
        .unwrap()
        .unwrap();
    assert_eq!(stored["revision"], 1);
    assert!(text(&stored, "lastRunId").starts_with("process-"));
}
