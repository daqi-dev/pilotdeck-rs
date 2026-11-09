use super::*;
use crate::cron::{
    schedule::iso,
    tasks::{CronConfig, CronTaskHost, CronTasks},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const NOW: i64 = 1_782_000_000_000;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-cron-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn project(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Host {
    submitted: Mutex<Vec<Value>>,
    prepared: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
    closed: Mutex<Vec<String>>,
    phases: Mutex<Vec<Value>>,
    delivered: Mutex<Vec<Value>>,
    notifications: Mutex<Vec<Value>>,
    aborts: Mutex<Vec<String>>,
    entered: Notify,
    resume: Notify,
}
impl NativeHost for Host {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.prepared
                .lock()
                .unwrap()
                .push(task["sessionKey"].as_str().unwrap().into());
            Ok(())
        })
    }
    fn release_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.released
                .lock()
                .unwrap()
                .push(task["sessionKey"].as_str().unwrap().into());
            Ok(())
        })
    }
    fn turn(&self, request: NativeTurnRequest) -> HostFuture<'_, Vec<Value>> {
        Box::pin(async move {
            assert_eq!(request.permission_mode, "bypassPermissions");
            assert!(!request.can_prompt);
            assert!(request.excluded_tools.contains(&"ask_user_question"));
            self.submitted.lock().unwrap().push(json!({"session":request.session_key,"run":request.run_id,"message":request.message}));
            if request.message == "empty" {
                return Ok(vec![]);
            }
            let mut events = vec![json!({"type":"turn_started","runId":request.run_id})];
            (request.on_event)(&events[0])?;
            if matches!(
                request.message.as_str(),
                "wait" | "interaction" | "wait_synthetic" | "wait_provider" | "wait_future"
            ) {
                if request.message == "interaction" {
                    let event = json!({"type":"permission_request","runId":request.run_id,"requestId":"permission"});
                    (request.on_event)(&event)?;
                    events.push(event);
                }
                self.entered.notify_one();
                // The production host owns timeout_ms and emits turn_timeout
                // itself; this fixture must model that contract or drain waits
                // forever on a turn that never settles.
                if let Some(timeout_ms) = request.timeout_ms
                    && request.message != "wait_future"
                {
                    tokio::select! {
                        _ = self.resume.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                            self.aborts.lock().unwrap().push(request.run_id.clone());
                            let event = json!({"type":"error","runId":request.run_id,"code":"turn_timeout","message":format!("Turn exceeded the {timeout_ms}ms timeout.")});
                            (request.on_event)(&event)?;
                            events.push(event);
                            return Ok(events);
                        }
                    }
                } else {
                    self.resume.notified().await;
                }
                if request.message == "wait_future" {
                    return Err(RuntimeError::new("agent_aborted", "native future aborted"));
                }
                if request.message != "wait_synthetic" {
                    let event = if request.message == "wait_provider" {
                        json!({"type":"error","runId":request.run_id,"code":"provider_failed","message":"provider rejected"})
                    } else {
                        json!({"type":"error","runId":request.run_id,"code":"agent_aborted","message":"cancelled"})
                    };
                    (request.on_event)(&event)?;
                    events.push(event);
                }
            } else if request.message == "fail" {
                let event = json!({"type":"error","runId":request.run_id,"code":"provider_failed","message":"provider rejected"});
                (request.on_event)(&event)?;
                events.push(event);
            } else {
                let event = json!({"type":"assistant_text_delta","runId":request.run_id,"text":" actual result "});
                (request.on_event)(&event)?;
                events.push(event);
            }
            let finish = if matches!(
                request.message.as_str(),
                "wait_synthetic" | "wait_provider" | "synthetic"
            ) {
                "aborted_streaming"
            } else {
                "completed"
            };
            let terminal = json!({"type":"turn_completed","runId":request.run_id,"finishReason":finish,"usage":{}});
            (request.on_event)(&terminal)?;
            events.push(terminal);
            Ok(events)
        })
    }
    fn abort<'a>(
        &'a self,
        _session: &'a str,
        run: &'a str,
        _reason: &'a str,
    ) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.aborts.lock().unwrap().push(run.into());
            self.resume.notify_one();
            Ok(())
        })
    }
    fn close<'a>(&'a self, session: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.closed.lock().unwrap().push(session.into());
            Ok(())
        })
    }
    fn on_turn_event(&self, session: &str, channel: &str, event: &Value) {
        self.notifications
            .lock()
            .unwrap()
            .push(json!({"sessionKey":session,"channelKey":channel,"event":event}));
    }
    fn on_phase_event(&self, event: &Value) {
        self.phases.lock().unwrap().push(event.clone());
    }
    fn deliver_result<'a>(&'a self, delivery: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.delivered.lock().unwrap().push(delivery.clone());
            Ok(())
        })
    }
}
fn runtime(
    fixture: &Fixture,
    host: Arc<Host>,
    concurrency: usize,
) -> (Arc<CronRuntime>, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(NOW));
    let clock = now.clone();
    let ids = AtomicUsize::new(0);
    let tasks = CronTasks::with_sources(
        fixture.0.join("home"),
        CronConfig {
            max_concurrent_runs: concurrency,
            ..Default::default()
        },
        Arc::new(move || clock.load(Ordering::SeqCst)),
        Arc::new(move || format!("task-{}", ids.fetch_add(1, Ordering::SeqCst))),
    )
    .unwrap();
    (CronRuntime::new(tasks, host), now)
}
async fn create(runtime: &CronRuntime, project: &str, message: &str, schedule: Value) -> Value {
    runtime.tasks().create_task(&json!({"projectKey":project,"message":message,"schedule":schedule,"sessionKey":"origin","channelKey":"web"}), runtime).await.unwrap()["task"].clone()
}
fn once() -> Value {
    json!({"type":"once","runAt":iso(NOW)})
}
async fn entered(host: &Host) {
    tokio::time::timeout(Duration::from_secs(2), host.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn once_executes_streams_persists_delivers_and_releases() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    let task = create(&runtime, &f.project("p"), "success", once()).await;
    runtime.run_tick_once().await.unwrap();
    runtime.drain().await;
    let store = runtime.tasks().store(&f.project("p")).unwrap();
    assert!(store.list_tasks().unwrap().is_empty());
    let runs = store.list_runs(None).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["outcome"], "completed");
    assert_eq!(runtime.active_run_count(), 0);
    assert_eq!(host.submitted.lock().unwrap().len(), 1);
    assert_eq!(host.delivered.lock().unwrap()[0]["text"], "actual result");
    assert_eq!(
        host.delivered.lock().unwrap()[0]["originSessionKey"],
        "origin"
    );
    assert_eq!(
        host.released.lock().unwrap().as_slice(),
        &[task["sessionKey"].as_str().unwrap()]
    );
    let event_files = std::fs::read_dir(store.project_dir.join("runs"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let raw = std::fs::read_to_string(event_files[0].path()).unwrap();
    assert_eq!(raw.lines().count(), 3);
    assert!(raw.contains("actual result"));
    assert_eq!(host.notifications.lock().unwrap().len(), 3);
    assert_eq!(host.phases.lock().unwrap()[1]["phase"], "cron_completed");
    assert!(runtime.take_errors().is_empty());
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn periodic_failure_rearms_and_empty_native_turn_cannot_pass() {
    for message in ["fail", "empty"] {
        let f = Fixture::new();
        let host = Arc::new(Host::default());
        let (runtime, now) = runtime(&f, host.clone(), 1);
        let task = create(
            &runtime,
            &f.project("p"),
            message,
            json!({"type":"cron","expression":"* * * * *","timezone":"UTC"}),
        )
        .await;
        let due = crate::cron::schedule::parse_date(task["nextRunAt"].as_str().unwrap())
            .unwrap()
            .unwrap();
        now.store(due, Ordering::SeqCst);
        runtime.run_tick_once().await.unwrap();
        runtime.drain().await;
        let store = runtime.tasks().store(&f.project("p")).unwrap();
        let current = store.list_tasks().unwrap().remove(0);
        assert_eq!(current["status"], "scheduled");
        assert_eq!(current["revision"], 2);
        assert!(current["nextRunAt"].as_str().unwrap() > task["nextRunAt"].as_str().unwrap());
        assert_eq!(store.list_runs(None).unwrap()[0]["outcome"], "failed");
        assert!(host.released.lock().unwrap().is_empty());
        runtime.stop().await.unwrap();
    }
}

#[tokio::test]
async fn interaction_failure_survives_late_abort_and_consumes_terminal() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    create(&runtime, &f.project("p"), "interaction", once()).await;
    runtime.run_tick_once().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runtime.drain())
        .await
        .unwrap();
    let run = runtime
        .tasks()
        .store(&f.project("p"))
        .unwrap()
        .list_runs(None)
        .unwrap()
        .remove(0);
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["error"]["code"], "cron_interaction_required");
    assert_eq!(host.aborts.lock().unwrap().len(), 1);
    assert_eq!(
        host.notifications.lock().unwrap().last().unwrap()["event"]["type"],
        "turn_completed"
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn stop_is_run_filtered_and_real_abort_drains_once() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    let task = create(&runtime, &f.project("p"), "wait", once()).await;
    runtime.run_tick_once().await.unwrap();
    entered(&host).await;
    assert!(
        runtime
            .stop_run(
                &f.project("p"),
                Some(task["taskId"].as_str().unwrap()),
                Some("wrong-run")
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(host.aborts.lock().unwrap().is_empty());
    let stopped = runtime
        .tasks()
        .stop_task(
            &json!({"projectKey":f.project("p"),"taskId":task["taskId"]}),
            runtime.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(stopped["stopped"], true);
    runtime.drain().await;
    let store = runtime.tasks().store(&f.project("p")).unwrap();
    assert!(store.list_tasks().unwrap().is_empty());
    assert_eq!(store.list_runs(None).unwrap()[0]["outcome"], "stopped");
    assert_eq!(runtime.active_run_count(), 0);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn explicit_stop_clears_only_synthetic_abort_error() {
    for (message, expected_error) in [
        ("wait_synthetic", None),
        ("wait", Some("agent_aborted")),
        ("wait_provider", Some("provider_failed")),
        ("wait_future", Some("cron_run_failed")),
    ] {
        let f = Fixture::new();
        let host = Arc::new(Host::default());
        let (runtime, _) = runtime(&f, host.clone(), 1);
        let project = f.project("p");
        let task = create(&runtime, &project, message, once()).await;
        runtime.run_tick_once().await.unwrap();
        entered(&host).await;
        let stopped = runtime
            .tasks()
            .stop_task(
                &json!({"projectKey":project,"taskId":task["taskId"]}),
                runtime.as_ref(),
            )
            .await
            .unwrap();
        assert_eq!(stopped["stopped"], true);
        runtime.drain().await;
        let run = runtime
            .tasks()
            .store(&project)
            .unwrap()
            .list_runs(None)
            .unwrap()
            .remove(0);
        assert_eq!(run["outcome"], "stopped", "{message}");
        match expected_error {
            Some(code) => assert_eq!(run["error"]["code"], code, "{message}"),
            None => assert!(run.get("error").is_none(), "{message}: {run}"),
        }
        let delivery = host.delivered.lock().unwrap()[0].clone();
        assert_eq!(delivery.get("error"), run.get("error"), "{message}");
        let phase = host.phases.lock().unwrap().last().unwrap().clone();
        assert_eq!(phase.get("error"), run.get("error"), "{message}");
        runtime.stop().await.unwrap();
    }
}

#[tokio::test]
async fn non_explicit_aborted_stream_without_error_preserves_completion() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host, 1);
    let project = f.project("p");
    create(&runtime, &project, "synthetic", once()).await;
    runtime.run_tick_once().await.unwrap();
    runtime.drain().await;
    let run = runtime
        .tasks()
        .store(&project)
        .unwrap()
        .list_runs(None)
        .unwrap()
        .remove(0);
    assert_eq!(run["outcome"], "completed");
    assert!(run.get("error").is_none());
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn global_budget_defers_other_project_and_repeated_ticks_do_not_double_claim() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    create(&runtime, &f.project("a"), "wait", once()).await;
    let second = create(&runtime, &f.project("b"), "success", once()).await;
    runtime.run_tick_once().await.unwrap();
    entered(&host).await;
    runtime.run_tick_once().await.unwrap();
    assert_eq!(host.submitted.lock().unwrap().len(), 1);
    let deferred = runtime
        .tasks()
        .store(&f.project("b"))
        .unwrap()
        .get_task(second["taskId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(deferred["nextRunAt"], iso(NOW + 60_000));
    runtime.stop().await.unwrap();
    assert_eq!(runtime.active_run_count(), 0);
    assert!(runtime.take_errors().is_empty());
}

#[tokio::test]
async fn restart_recovers_interrupted_once_without_executing_or_duplicate_history() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    let task = create(&runtime, &f.project("p"), "success", once()).await;
    let store = runtime.tasks().store(&f.project("p")).unwrap();
    store.claim(&task, "interrupted-run", NOW).unwrap().unwrap();
    runtime.start().await.unwrap();
    assert!(store.list_tasks().unwrap().is_empty());
    assert_eq!(
        store.list_runs(None).unwrap()[0]["error"]["code"],
        "cron_run_interrupted"
    );
    assert!(host.submitted.lock().unwrap().is_empty());
    runtime.stop().await.unwrap();
    let second = CronRuntime::new(
        CronTasks::new(f.0.join("home"), CronConfig::default()).unwrap(),
        host.clone(),
    );
    second.start().await.unwrap();
    assert_eq!(store.list_runs(None).unwrap().len(), 1);
    second.stop().await.unwrap();
}

#[tokio::test]
async fn event_write_failure_is_a_real_failed_run_not_empty_success() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    create(&runtime, &f.project("p"), "success", once()).await;
    let store = runtime.tasks().store(&f.project("p")).unwrap();
    std::fs::write(store.project_dir.join("runs"), "block mkdir").unwrap();
    runtime.run_tick_once().await.unwrap();
    runtime.drain().await;
    assert_eq!(store.list_runs(None).unwrap()[0]["outcome"], "failed");
    assert!(host.notifications.lock().unwrap().is_empty());
    runtime.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn startup_timer_executes_without_manual_tick_and_shutdown_drains() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let (runtime, _) = runtime(&f, host.clone(), 1);
    let task = create(&runtime, &f.project("p"), "wait", once()).await;
    runtime.start().await.unwrap();
    runtime.start().await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    entered(&host).await;
    assert_eq!(host.submitted.lock().unwrap().len(), 1);
    assert_eq!(
        host.closed.lock().unwrap().as_slice(),
        &[task["sessionKey"].as_str().unwrap()]
    );
    assert_eq!(host.prepared.lock().unwrap().len(), 2);
    runtime.stop().await.unwrap();
    assert_eq!(runtime.active_run_count(), 0);
    assert_eq!(host.aborts.lock().unwrap().len(), 1);
    assert_eq!(
        runtime
            .tasks()
            .store(&f.project("p"))
            .unwrap()
            .list_runs(None)
            .unwrap()[0]["outcome"],
        "stopped"
    );
    assert_eq!(runtime.start().await.unwrap_err().code, "gateway_disposed");
    runtime.poke();
    tokio::time::advance(Duration::from_secs(120)).await;
    assert_eq!(host.submitted.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_aborts_real_host_and_does_not_allow_late_terminal_to_change_failure() {
    let f = Fixture::new();
    let host = Arc::new(Host::default());
    let tasks = CronTasks::with_sources(
        f.0.join("home"),
        CronConfig {
            run_timeout_minutes: 1,
            ..Default::default()
        },
        Arc::new(|| NOW),
        Arc::new(|| "timeout-task".into()),
    )
    .unwrap();
    let runtime = CronRuntime::new(tasks, host.clone());
    create(&runtime, &f.project("p"), "wait", once()).await;
    runtime.run_tick_once().await.unwrap();
    host.entered.notified().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    runtime.drain().await;
    let run = runtime
        .tasks()
        .store(&f.project("p"))
        .unwrap()
        .list_runs(None)
        .unwrap()
        .remove(0);
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["error"]["code"], "cron_run_timeout");
    assert_eq!(host.aborts.lock().unwrap().len(), 1);
    runtime.stop().await.unwrap();
}
