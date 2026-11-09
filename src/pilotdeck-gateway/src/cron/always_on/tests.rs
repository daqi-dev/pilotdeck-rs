use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Fixture {
    root: PathBuf,
    project: String,
    home: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-always-test-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        Self {
            project: project.to_string_lossy().into_owned(),
            home: root.join("home"),
            root,
        }
    }
    fn store(&self) -> storage::Store {
        storage::Store::new(&self.home, Path::new(&self.project)).unwrap()
    }
    fn config(&self) -> Value {
        json!({"projects":{self.project.clone():{"enabled":true}}})
    }
    fn seed(&self) -> storage::Store {
        let store = self.store();
        store.insert_record("plans", json!({"id":"plan-1","title":"Fix project","status":"completed","summary":"fix","rationale":"test","dedupeKey":"fix","createdAt":"2026-10-27T00:00:00.000Z","sourceRunId":"old","planFilePath":"plans/plan-1.md"})).unwrap();
        storage::write_atomic(
            &store.root.join("plans/plan-1.md"),
            b"# Fix project\n\n## Execution Steps\n1. Fix file.\n\n## Verification\n- Check file.",
        )
        .unwrap();
        std::fs::write(Path::new(&self.project).join("source.txt"), "before").unwrap();
        store
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct FakeHost {
    calls: Mutex<Vec<TurnRequest>>,
    fail_execute: bool,
    produce_plan: bool,
    fail_discovery: bool,
    fail_report: bool,
    empty_report: bool,
    closed: Mutex<Vec<String>>,
    observed: Mutex<Vec<Value>>,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
    aborted: Mutex<Vec<String>>,
    created: Mutex<Vec<(String, PathBuf)>>,
}
impl FakeHost {
    fn new() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            fail_execute: false,
            produce_plan: false,
            fail_discovery: false,
            fail_report: false,
            empty_report: false,
            closed: Mutex::new(vec![]),
            observed: Mutex::new(vec![]),
            block: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            aborted: Mutex::new(vec![]),
            created: Mutex::new(vec![]),
        }
    }
}
impl AlwaysOnHost for FakeHost {
    fn turn(&self, request: TurnRequest) -> HostFuture<'_, Vec<Value>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request.clone());
            if request.channel_key == "always-on/discovery" && self.produce_plan {
                let run = request
                    .run_id
                    .strip_suffix(".discovery")
                    .unwrap_or(&request.run_id);
                request.tools.call("always_on_discovery_plan",&json!({"title":"Discovered task","summary":"Fix file","rationale":"Observed error","dedupeKey":"task","content":valid_plan(run,&request.project_key)})).await?;
            }

            if request.channel_key == "always-on/workspace" {
                request
                    .tools
                    .call(
                        "always_on_prepare_workspace",
                        &json!({"strategy":"snapshot-copy"}),
                    )
                    .await?;
            }
            if self.block.load(Ordering::SeqCst) && request.channel_key == "always-on/execute" {
                self.entered.notify_one();
                self.release.notified().await;
            }
            let events = if request.channel_key == "always-on/discovery" && self.fail_discovery {
                vec![json!({"type":"error","code":"discovery_failed","message":"provider failed"})]
            } else if request.channel_key == "always-on/execute" && self.fail_execute {
                vec![
                    json!({"type":"error","code":"execution_failed","message":"verification command failed"}),
                ]
            } else if request.channel_key == "always-on/report" {
                if self.fail_report {
                    vec![
                        json!({"type":"error","code":"provider_failed","message":"report provider failed"}),
                    ]
                } else if self.empty_report {
                    vec![]
                } else {
                    vec![
                        json!({"type":"assistant_text_delta","text":"# Real report\n\n## Verification Results\n- command passed"}),
                    ]
                }
            } else {
                vec![
                    json!({"type":"turn_started","runId":request.run_id}),
                    json!({"type":"turn_completed"}),
                ]
            };
            for event in &events {
                (request.on_event)(event)?;
            }
            Ok(events)
        })
    }
    fn abort<'a>(
        &'a self,
        session: &'a str,
        _run: &'a str,
        _reason: &'a str,
    ) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.aborted.lock().unwrap().push(session.into());
            self.release.notify_one();
            Ok(())
        })
    }
    fn close<'a>(&'a self, session: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.closed.lock().unwrap().push(session.into());
            Ok(())
        })
    }
    fn on_turn_event(&self, _session: &str, _channel: &str, event: &Value) {
        self.observed.lock().unwrap().push(event.clone());
    }
    fn on_worktree_created(&self, run: &str, cwd: &Path) {
        assert!(cwd.exists(), "notification follows successful git add");
        self.created.lock().unwrap().push((run.into(), cwd.into()));
    }
}

#[test]
fn storage_corruption_never_becomes_empty_success() {
    let fixture = Fixture::new();
    let store = fixture.store();
    assert_eq!(store.index("plans").unwrap()["plans"], json!([]));
    storage::write_atomic(&store.root.join("plans/index.json"), b"{broken").unwrap();
    assert_eq!(store.index("plans").unwrap_err().code, "storage_corrupt");
    assert_eq!(
        std::fs::read_to_string(store.root.join("plans/index.json")).unwrap(),
        "{broken"
    );
}

#[test]
fn state_day_rollover_preserves_cycle_and_failure_history() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store
        .update_state(
            "2026-10-26T00:00:00.000Z",
            json!({"todayRunCount":4,"activeWorkCycleId":"cycle-1","consecutiveFailures":2}),
        )
        .unwrap();
    let state = store.state("2026-10-27T00:00:00.000Z").unwrap();
    assert_eq!(state["todayRunCount"], 0);
    assert_eq!(state["activeWorkCycleId"], "cycle-1");
    store
        .complete("2026-10-27T01:00:00.000Z", "run", "plan", "failed")
        .unwrap();
    assert_eq!(
        store.state("2026-10-27T01:00:00.000Z").unwrap()["consecutiveFailures"],
        3
    );
}

#[test]
fn report_fallback_records_missing_verification_and_real_metadata() {
    let metadata = json!({"runId":"run","planId":"plan","outcome":"executed"});
    let (report, fallbacks) =
        report::normalize("# Work\n\n# Verification Results\n- test passed", &metadata);
    assert!(report.contains("> runId: run\n"));
    assert!(report.contains("## Verification Results\n- test passed"));
    assert!(fallbacks.contains(&"h1-downgraded(Verification Results)".into()));
    let fallback = report::fallback("Work", "report_tool_not_invoked", &metadata);
    assert!(fallback.contains("(unverified)"));
    assert!(!fallback.contains("- [x]"));
}

#[tokio::test]
async fn controls_preserve_real_missing_record_errors_and_standalone_apply() {
    let fixture = Fixture::new();
    let host = Arc::new(FakeHost::new());
    let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), host.clone());
    let result = service
        .handle(
            "always_on_rerun_plan",
            &json!({"projectKey":fixture.project,"planId":"missing"}),
        )
        .await
        .unwrap();
    assert!(!result["runId"].as_str().unwrap().is_empty());
    assert_eq!(result["error"]["code"], "plan_not_found");
    let service = AlwaysOnService::new(fixture.home.clone(), json!({}), host.clone());
    let result = service
        .handle(
            "always_on_apply",
            &json!({"projectKey":fixture.project,"workCycleId":"missing","projectName":"Project"}),
        )
        .await
        .unwrap();
    assert_eq!(result["error"]["code"], "cycle_not_found");
    let result = service
        .handle(
            "always_on_rerun_plan",
            &json!({"projectKey":fixture.project,"planId":"p"}),
        )
        .await
        .unwrap();
    assert_eq!(result["error"]["code"], "not_configured");
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rerun_executes_real_phases_persists_report_and_reuses_workspace_after_restart() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    let host = Arc::new(FakeHost::new());
    let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), host.clone());
    let input = json!({"projectKey":fixture.project,"planId":"plan-1"});
    let result = service
        .handle("always_on_rerun_plan", &input)
        .await
        .unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let plan = store.record("plans", "plan-1").unwrap().unwrap();
    assert_eq!(plan["status"], "completed");
    let report =
        std::fs::read_to_string(store.root.join(plan["reportFilePath"].as_str().unwrap())).unwrap();
    assert!(report.contains("command passed"));
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    assert_ne!(calls[1].cwd, PathBuf::from(&fixture.project));
    assert_eq!(
        calls[1].permission_rules["deny"].as_array().unwrap().len(),
        4
    );
    assert!(!calls[1].can_prompt);
    let workspace = calls[1].cwd.clone();
    service.stop().await;
    let restarted = AlwaysOnService::new(fixture.home.clone(), fixture.config(), host.clone());
    let result2 = restarted
        .handle("always_on_rerun_plan", &input)
        .await
        .unwrap();
    assert!(result2.get("error").is_none(), "{result2}");
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[3].cwd, workspace);
    assert_eq!(
        store.index("cycles").unwrap()["cycles"][0]["planIds"],
        json!(["plan-1"])
    );
    assert_eq!(host.closed.lock().unwrap().len(), 5);
    assert!(!host.observed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_execution_and_missing_report_never_claim_verified_success() {
    for (failure, empty, report_failure) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let fixture = Fixture::new();
        let store = fixture.seed();
        let mut host = FakeHost::new();
        host.fail_execute = failure;
        host.empty_report = empty;
        host.fail_report = report_failure;
        let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), Arc::new(host));
        let result = service
            .handle(
                "always_on_rerun_plan",
                &json!({"projectKey":fixture.project,"planId":"plan-1"}),
            )
            .await
            .unwrap();
        let plan = store.record("plans", "plan-1").unwrap().unwrap();
        assert_eq!(
            plan["status"],
            if failure {
                "failed"
            } else {
                "completed_no_report"
            }
        );
        assert_eq!(result.get("error").is_some(), failure || report_failure);
        let report =
            std::fs::read_to_string(store.root.join(plan["reportFilePath"].as_str().unwrap()))
                .unwrap();
        assert!(report.contains("(unverified)"));
    }
}

#[tokio::test]
async fn abort_is_scoped_and_stop_drains_admitted_control() {
    let fixture = Fixture::new();
    fixture.seed();
    let host = Arc::new(FakeHost::new());
    host.block.store(true, Ordering::SeqCst);
    let service = Arc::new(AlwaysOnService::new(
        fixture.home.clone(),
        fixture.config(),
        host.clone(),
    ));
    let worker = {
        let service = service.clone();
        let project = fixture.project.clone();
        tokio::spawn(async move {
            service
                .handle(
                    "always_on_rerun_plan",
                    &json!({"projectKey":project,"planId":"plan-1"}),
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), host.entered.notified())
        .await
        .unwrap();
    let session = host
        .calls
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .session_key
        .clone();
    let foreign = service
        .handle(
            "always_on_abort",
            &json!({"projectKey":fixture.project,"sessionKey":"user/main"}),
        )
        .await
        .unwrap();
    assert_eq!(foreign["error"]["code"], "session_not_active");
    let stopped = service
        .handle(
            "always_on_abort",
            &json!({"projectKey":fixture.project,"sessionKey":session}),
        )
        .await
        .unwrap();
    assert_eq!(stopped["aborted"], true);
    assert_eq!(host.aborted.lock().unwrap().as_slice(), &[session]);
    let stopping = {
        let service = service.clone();
        tokio::spawn(async move { service.stop().await })
    };
    worker.await.unwrap().unwrap();
    stopping.await.unwrap();
    let result = service
        .handle(
            "always_on_rerun_plan",
            &json!({"projectKey":fixture.project,"planId":"plan-1"}),
        )
        .await
        .unwrap();
    assert_eq!(result["error"]["code"], "not_ready");
}

#[tokio::test]
async fn workspace_escape_and_duplicate_prepare_are_rejected() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    std::fs::create_dir_all(&store.snapshots).unwrap();
    assert!(
        workspace::validate(
            &store,
            &json!({"strategy":"snapshot-copy","cwd":fixture.project})
        )
        .is_err()
    );
    let tools = RunTools::new(
        store,
        "run",
        "workspace",
        json!({"title":"Test"}),
        Value::Null,
        now(),
    );
    tools
        .call(
            "always_on_prepare_workspace",
            &json!({"strategy":"snapshot-copy"}),
        )
        .await
        .unwrap();
    assert_eq!(
        tools
            .call(
                "always_on_prepare_workspace",
                &json!({"strategy":"snapshot-copy"})
            )
            .await
            .unwrap_err()
            .code,
        "tool_execution_failed"
    );
    assert!(
        tools
            .call("always_on_report", &json!({"content":"# Unowned"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn standalone_apply_uses_project_cwd_and_does_not_mark_cycle_applied() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    let ws = workspace::prepare(&store, "apply-workspace", "Test", "snapshot-copy")
        .await
        .unwrap();
    std::fs::write(
        Path::new(ws["cwd"].as_str().unwrap()).join("source.txt"),
        "after",
    )
    .unwrap();
    store.insert_record("cycles", json!({"id":"cycle-1","projectKey":fixture.project,"status":"active","workspace":ws,"planIds":["plan-1"]})).unwrap();
    let host = Arc::new(FakeHost::new());
    let service = AlwaysOnService::new(fixture.home.clone(), json!({}), host.clone());
    let result = service
        .handle(
            "always_on_apply",
            &json!({"projectKey":fixture.project,"workCycleId":"cycle-1","projectName":"Project"}),
        )
        .await
        .unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cwd, PathBuf::from(&fixture.project));
    assert!(calls[0].message.contains("after"));
    assert_eq!(calls[0].tools.tool_names().len(), 4);
    assert_eq!(
        store.record("cycles", "cycle-1").unwrap().unwrap()["status"],
        "active"
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&fixture.project).join("source.txt")).unwrap(),
        "before"
    );
}

#[test]
fn selected_project_marker_storage_is_reused() {
    let fixture = Fixture::new();
    let project_dir = fixture.home.join("projects").join("custom-marker-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join(".cwd"), &fixture.project).unwrap();
    let store = fixture.store();
    assert_eq!(store.root.file_name().unwrap(), "custom-marker-project");
}

#[tokio::test]
async fn report_tool_writes_a_structured_report_and_rejects_other_phases() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    let tools = RunTools::new(
        store,
        "report-run",
        "report",
        json!({"id":"plan-1"}),
        json!({"strategy":"snapshot-copy","cwd":"/workspace"}),
        "2026-10-27T00:00:00.000Z".into(),
    );
    let result = tools
        .call(
            "always_on_report",
            &json!({"content":"# Report\n\n## Verification Results\n- actual output"}),
        )
        .await
        .unwrap();
    assert_eq!(result["data"]["ok"], true);
    let content = std::fs::read_to_string(tools.report_path().unwrap()).unwrap();
    assert!(content.contains("> planId: plan-1"));
    assert!(content.contains("actual output"));
    assert!(
        tools
            .call("always_on_prepare_workspace", &json!({"strategy":"auto"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn execution_phase_rejects_report_without_report_store() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    let tools = RunTools::new(
        store,
        "execution-run",
        "execute",
        json!({"id":"plan-1"}),
        json!({"strategy":"snapshot-copy","cwd":"/workspace"}),
        "2026-10-27T00:00:00.000Z".into(),
    );
    assert_eq!(tools.tool_names().len(), 4);
    let err = tools
        .call(
            "always_on_report",
            &json!({"content":"# Report\n\n## Verification Results\n- execution output"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "tool_execution_failed");
    assert!(err.message.contains("reportStore"));

    let discovery = RunTools::new(
        fixture.store(),
        "discovery-run",
        "discovery",
        Value::Null,
        Value::Null,
        now(),
    );
    let history = discovery
        .schemas()
        .into_iter()
        .find(|schema| schema["name"] == "always_on_read_chat_history")
        .unwrap();
    assert_eq!(history["readOnly"], true);
    assert_eq!(history["concurrencySafe"], true);
}

#[tokio::test]
async fn stop_waits_for_an_admitted_turn_and_rejects_later_controls() {
    let fixture = Fixture::new();
    fixture.seed();
    let host = Arc::new(FakeHost::new());
    host.block.store(true, Ordering::SeqCst);
    let service = Arc::new(AlwaysOnService::new(
        fixture.home.clone(),
        fixture.config(),
        host.clone(),
    ));
    let worker = {
        let service = service.clone();
        let project = fixture.project.clone();
        tokio::spawn(async move {
            service
                .handle(
                    "always_on_rerun_plan",
                    &json!({"projectKey":project,"planId":"plan-1"}),
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), host.entered.notified())
        .await
        .unwrap();
    let stopping = {
        let service = service.clone();
        tokio::spawn(async move { service.stop().await })
    };
    while !service.active.lock().unwrap().stopping {
        tokio::task::yield_now().await;
    }
    assert!(!stopping.is_finished());
    let result = service
        .handle(
            "always_on_rerun_plan",
            &json!({"projectKey":fixture.project,"planId":"plan-1"}),
        )
        .await
        .unwrap();
    assert_eq!(result["error"]["code"], "not_ready");
    host.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), stopping)
        .await
        .unwrap()
        .unwrap();
}

fn test_git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}
fn init_repo(fixture: &Fixture, commit: bool) {
    let root = Path::new(&fixture.project);
    test_git(root, &["init", "-b", "main"]);
    test_git(root, &["config", "user.name", "Always-On Test"]);
    test_git(
        root,
        &["config", "user.email", "always-test@example.invalid"],
    );
    if commit {
        test_git(root, &["add", "-A"]);
        test_git(root, &["commit", "-m", "initial test fixture"]);
    }
}

#[tokio::test]
async fn git_workspace_checkpoints_dirty_files_and_generates_real_diff() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    init_repo(&fixture, true);
    let project = Path::new(&fixture.project);
    std::fs::write(project.join("source.txt"), "checkpointed change").unwrap();
    std::fs::write(project.join("new.txt"), "new checkpointed file").unwrap();
    let workspace = workspace::prepare(&store, "git-run", "Test checkpoint", "auto")
        .await
        .unwrap();
    assert_eq!(workspace["strategy"], "git-worktree");
    assert_eq!(test_git(project, &["status", "--porcelain"]), "");
    assert_eq!(test_git(project, &["rev-list", "--count", "HEAD"]), "2");
    let cwd = Path::new(workspace["cwd"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(cwd.join("source.txt")).unwrap(),
        "checkpointed change"
    );
    assert_eq!(
        test_git(cwd, &["branch", "--show-current"]),
        "always-on/git-run"
    );
    std::fs::write(cwd.join("source.txt"), "executed in isolation").unwrap();
    std::fs::write(cwd.join("added.txt"), "added during execution").unwrap();
    std::fs::remove_file(cwd.join("new.txt")).unwrap();
    let diff = workspace::diff(&store, &workspace).await.unwrap();
    assert_eq!(diff["fileCount"], 3);
    assert_eq!(diff["truncated"], false);
    assert!(
        diff["diff"]
            .as_str()
            .unwrap()
            .contains("executed in isolation")
    );
    assert!(project.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("source.txt")).unwrap(),
        "checkpointed change"
    );
}

#[tokio::test]
async fn worktree_notifications_follow_success_and_do_not_repeat_on_reuse_or_failure() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    init_repo(&fixture, true);
    let host = Arc::new(FakeHost::new());
    let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), host.clone());
    let plan = json!({"title":"Prepare"});
    let first = workspace::prepare_notified(
        &store,
        "notified-run",
        "Prepare",
        "git-worktree",
        Some(host.as_ref()),
    )
    .await
    .unwrap();
    assert_eq!(host.created.lock().unwrap().len(), 1);
    store.update_state(&now(), json!({"currentWorkspace":{"runId":"notified-run","strategy":first["strategy"],"cwd":first["cwd"],"metadata":first["metadata"]}})).unwrap();
    let cycle = service.ensure_cycle(&store, "reuse", &plan).await.unwrap();
    assert_eq!(cycle["workspace"]["cwd"], first["cwd"]);
    service
        .ensure_cycle(&store, "reuse-again", &plan)
        .await
        .unwrap();
    assert_eq!(host.created.lock().unwrap().len(), 1);
    assert!(
        workspace::prepare_notified(
            &store,
            "notified-run",
            "duplicate",
            "git-worktree",
            Some(host.as_ref())
        )
        .await
        .is_err()
    );
    assert_eq!(host.created.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn git_checkpoint_failure_preserves_user_changes_and_reports_error() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let store = fixture.seed();
    init_repo(&fixture, true);
    let project = Path::new(&fixture.project);
    std::fs::write(project.join("source.txt"), "must survive failed hook").unwrap();
    let hook = project.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = workspace::prepare(&store, "failed-git-run", "Test failure", "git-worktree")
        .await
        .unwrap_err();
    assert_eq!(error.code, "workspace_prepare_failed");
    assert_eq!(test_git(project, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        std::fs::read_to_string(project.join("source.txt")).unwrap(),
        "must survive failed hook"
    );
    assert!(!store.worktrees.join("failed-git-run").exists());
}

#[tokio::test]
async fn uncommitted_repository_falls_back_to_snapshot_and_ignores_only_root_exclusions() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    init_repo(&fixture, false);
    let project = Path::new(&fixture.project);
    std::fs::create_dir_all(project.join("node_modules")).unwrap();
    std::fs::write(project.join("node_modules/ignored"), "ignored").unwrap();
    std::fs::create_dir_all(project.join("nested/dist")).unwrap();
    std::fs::write(project.join("nested/dist/keep"), "retained").unwrap();
    let ws = workspace::prepare(&store, "no-head", "Test", "auto")
        .await
        .unwrap();
    assert_eq!(ws["strategy"], "snapshot-copy");
    let cwd = Path::new(ws["cwd"].as_str().unwrap());
    assert!(!cwd.join(".git").exists());
    assert!(!cwd.join("node_modules").exists());
    assert_eq!(
        std::fs::read_to_string(cwd.join("nested/dist/keep")).unwrap(),
        "retained"
    );
    assert!(
        workspace::prepare(&store, "no-head", "Test", "snapshot-copy")
            .await
            .is_err()
    );
    assert!(
        workspace::prepare(&store, "require-head", "Test", "git-worktree")
            .await
            .is_err()
    );
    assert!(
        workspace::prepare(&store, "unknown", "Test", "invalid")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn automatic_discovery_without_plan_enters_dormancy_and_respects_gate_order() {
    let fixture = Fixture::new();
    let host = Arc::new(FakeHost::new());
    let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), host.clone());
    service.set_lease(json!({"projectKey":fixture.project,"channelKey":"web","writerId":"writer","sessionKey":"user","agentBusy":true})).unwrap();
    let blocked = service.run_tick_once(&fixture.project).await.unwrap();
    assert_eq!(blocked["reason"], "agent_busy");
    service.mark_idle(&fixture.project, "web", "writer");
    assert_eq!(
        service.run_tick_once(&fixture.project).await.unwrap()["reason"],
        "recent_user_msg"
    );
    service.remove_writer("writer");
    service.set_project_busy(&fixture.project, true);
    assert_eq!(
        service.run_tick_once(&fixture.project).await.unwrap()["reason"],
        "agent_busy"
    );
    service.set_project_busy(&fixture.project, false);
    let fired = service.run_tick_once(&fixture.project).await.unwrap();
    assert_eq!(fired["outcome"], "fired");
    let store = fixture.store();
    let state = store.state(&now()).unwrap();
    assert_eq!(state["lastFireOutcome"], "no_plan");
    assert_eq!(state["todayRunCount"], 1.0);
    assert!(state["dormant"].is_object());
    assert!(!store.root.join("locks/discovery.lock").exists());
    assert_eq!(
        service.run_tick_once(&fixture.project).await.unwrap()["reason"],
        "dormant_no_signal"
    );
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

fn valid_plan(run: &str, project: &str) -> String {
    format!(
        "# Concrete task\n\n> Always-On Discovery Plan\n> id: plan_{run}\n> sourceRunId: {run}\n> createdAt: 2026-10-27T00:00:00.000Z\n> projectRoot: {project}\n> dedupeKey: concrete\n\n## Summary\nFix a file.\n\n## Rationale\nThe file has an error.\n\n## Context Signals\n- Actual error\n\n## Proposed Change\nFix the file.\n\n## Execution Steps\n1. Update file.\n\n## Verification\n- Check contents.\n"
    )
}

#[tokio::test]
async fn plan_tool_rejects_invalid_contract_and_enforces_one_successful_plan() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let tools = RunTools::new(
        store.clone(),
        "discovery",
        "discovery",
        Value::Null,
        Value::Null,
        now(),
    );
    let mut input = json!({"title":"Title","summary":"Summary","rationale":"Reason","dedupeKey":"key","content":"# Broken"});
    assert_eq!(
        tools
            .call("always_on_discovery_plan", &input)
            .await
            .unwrap_err()
            .code,
        "tool_execution_failed"
    );
    input["content"] = json!(valid_plan("discovery", &fixture.project));
    let result = tools
        .call("always_on_discovery_plan", &input)
        .await
        .unwrap();
    assert_eq!(result["data"]["planId"], "plan_discovery");
    assert!(
        store
            .plan_body("plan_discovery")
            .unwrap()
            .unwrap()
            .contains("## Verification")
    );
    assert!(
        tools
            .call("always_on_discovery_plan", &input)
            .await
            .unwrap_err()
            .message
            .contains("plan_quota_exhausted")
    );
    let content = valid_plan("r", &fixture.project);
    for invalid in [
        content.replace("## Summary", "## Unknown"),
        content.replace("1. Update file.", "- unordered"),
        content.replace("Fix the file.", "TODO"),
        content.replace("> dedupeKey: concrete", "> unknown: concrete"),
    ] {
        assert!(super::plan::parse(&invalid).is_err());
    }
}

#[test]
fn chinese_and_english_prompts_preserve_baseline_language_and_contract_sections() {
    let ws = json!({"cwd":"/isolated","strategy":"snapshot-copy","metadata":{}});
    let diff = json!({"diff":"","fileCount":0,"truncated":false});
    assert!(super::prompts::workspace(true, "/project", "Plan").starts_with("你正在为 Always-On"));
    assert!(super::prompts::execute(true, &ws, "body").contains("请勿修改用户的项目根目录"));
    assert!(super::prompts::report(true, &ws, "body").contains("Plan Reference, Steps Performed"));
    assert!(
        super::prompts::apply(true, "/project", "Project", "cycle", "Plan", &ws, &diff)
            .contains("无需应用任何变更")
    );
    let discovery = super::prompts::discovery(
        true,
        "/project",
        "run",
        "timestamp",
        None,
        &json!({"sessions":[]}),
        &json!({"plans":[]}),
    );
    assert!(discovery.starts_with("你正在为项目执行自主"));
    assert!(discovery.contains("> id: plan_run"));
    assert!(discovery.contains("未找到近期用户对话"));
    assert!(super::prompts::execute(false, &ws, "body").starts_with("You are executing"));
}

#[tokio::test]
async fn discovery_executes_plan_through_all_phases_and_records_its_original_run() {
    let fixture = Fixture::new();
    let mut host = FakeHost::new();
    host.produce_plan = true;
    let host = Arc::new(host);
    let mut config = fixture.config();
    config["language"] = json!("zh-CN");
    let service = AlwaysOnService::new(fixture.home.clone(), config, host.clone());
    assert_eq!(
        service.run_tick_once(&fixture.project).await.unwrap()["outcome"],
        "fired"
    );
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .map(|c| c.channel_key.as_str())
            .collect::<Vec<_>>(),
        [
            "always-on/discovery",
            "always-on/workspace",
            "always-on/execute",
            "always-on/report"
        ]
    );
    assert!(calls[0].message.starts_with("你正在为项目执行自主"));
    assert!(calls[1].message.starts_with("你正在为 Always-On"));
    let run = calls[0].run_id.strip_suffix(".discovery").unwrap();
    let store = fixture.store();
    let state = store.state(&now()).unwrap();
    assert_eq!(state["lastRunId"], run);
    assert_eq!(state["lastFireOutcome"], "executed");
    let index = store.index("plans").unwrap();
    assert_eq!(index["plans"][0]["status"], "completed");
    assert_eq!(state["todayRunCount"], 1.0);
    let phases = std::fs::read_to_string(store.root.join("events.jsonl")).unwrap();
    assert!(phases.contains("plan_produced"));
    assert!(phases.contains("report_produced"));
    assert!(
        !store
            .root
            .join("runs")
            .join(format!("{run}.discovery.events.jsonl"))
            .exists()
    );
}

#[tokio::test]
async fn discovery_provider_error_marks_failed_and_releases_lock_without_creating_plan() {
    let fixture = Fixture::new();
    let mut host = FakeHost::new();
    host.fail_discovery = true;
    let service = AlwaysOnService::new(fixture.home.clone(), fixture.config(), Arc::new(host));
    assert_eq!(
        service.run_tick_once(&fixture.project).await.unwrap()["outcome"],
        "fired"
    );
    let store = fixture.store();
    let state = store.state(&now()).unwrap();
    assert_eq!(state["lastFireOutcome"], "failed");
    assert_eq!(state["consecutiveFailures"], 1);
    assert!(state.get("dormant").is_none());
    assert!(
        store.index("plans").unwrap()["plans"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!store.root.join("locks/discovery.lock").exists());
}

#[tokio::test]
async fn snapshot_preparation_failure_does_not_modify_source_files() {
    let fixture = Fixture::new();
    let store = fixture.seed();
    std::fs::create_dir_all(store.snapshots.parent().unwrap()).unwrap();
    std::fs::write(&store.snapshots, "blocking file").unwrap();
    let error = workspace::prepare(&store, "failure", "Test failure", "snapshot-copy")
        .await
        .unwrap_err();
    assert_eq!(error.code, "workspace_prepare_failed");
    assert_eq!(
        std::fs::read_to_string(Path::new(&fixture.project).join("source.txt")).unwrap(),
        "before"
    );
    assert_eq!(
        std::fs::read_to_string(&store.snapshots).unwrap(),
        "blocking file"
    );
}
