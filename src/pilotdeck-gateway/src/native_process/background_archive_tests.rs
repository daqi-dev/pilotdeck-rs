use super::*;
use crate::runtime::{
    background::{BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, StartTaskSpec, TaskStatus},
    project::SandboxMode,
    tools::sandbox::NativeSandbox,
};
use std::time::Duration;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Scratch {
    let root =
        Scratch(std::env::temp_dir().join(format!("pdrs-native-archive-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&root.0).unwrap();
    root
}
fn runtime(
    project: &Path,
    store: Arc<ArchiveHandle>,
    limits: NativeTaskLimits,
) -> BackgroundTaskRuntime {
    BackgroundTaskRuntime::new_native(
        BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(project.join(".pilotdeck/background-tasks")),
            snapshot_store: Some(store.clone()),
            ..Default::default()
        },
        Arc::new(NativeSandbox::new(
            SandboxMode::DangerFullAccess,
            std::env::temp_dir(),
        )),
        store,
        limits,
    )
}
fn spec(project: &Path, command: &str) -> StartTaskSpec {
    StartTaskSpec {
        command: command.into(),
        cwd: project.to_string_lossy().as_ref().into(),
        env: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
        session_id: "owner".into(),
        agent_id: JsValue::Undefined,
        kind: JsValue::Undefined,
    }
}
async fn wait(runtime: &BackgroundTaskRuntime, id: &JsString) -> io::Result<TaskStatus> {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.wait(id, &JsValue::Number(4000.0), None, None),
    )
    .await
    .map_err(|_| io::Error::other("owned task wait deadline"))?
    .map_err(|e| io::Error::other(e.message.to_utf8().unwrap_or_default()))?;
    result
        .map(|r| r.task.status)
        .ok_or_else(|| io::Error::other("owned task disappeared"))
}
async fn reopen(project: &Path) -> Arc<NativeArchive> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match NativeArchive::open(project) {
            Ok(archive) => return archive,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("archive did not release its owned lock: {error}"),
        }
    }
}
#[tokio::test]
async fn durable_restart_preserves_real_output_and_js_string_units() {
    let root = fixture();
    std::fs::write(root.0.join("owned.sh"), "printf DURABLE-OUTPUT\n").unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let store = archive.handle();
    let rt = runtime(&root.0, store.clone(), NativeTaskLimits::CLI);
    let mut request = spec(&root.0, "sh owned.sh #");
    let mut command = "sh owned.sh #".encode_utf16().collect::<Vec<_>>();
    command.push(0xd800);
    request.command = JsValue::String(JsString::from_utf16(command));
    request.session_id = JsValue::String(JsString::from_utf16(vec![b's' as u16, 0xdc00]));
    request.agent_id = JsValue::String(JsString::from_utf16(vec![b'a' as u16, 0xd800]));
    let expected_command = request.command.clone();
    let expected_session = request.session_id.clone();
    let expected_agent = request.agent_id.clone();
    let task = rt.start(request).await;
    let status = if let Ok(task) = &task {
        wait(&rt, &task.task_id).await
    } else {
        Err(io::Error::other("task start failed"))
    };
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    let task = task.unwrap();
    assert_eq!(status.unwrap(), TaskStatus::Completed);
    drop(rt);
    drop(store);
    drop(archive);
    let archive = reopen(&root.0).await;
    let store = archive.handle();
    let rt = runtime(&root.0, store.clone(), NativeTaskLimits::CLI);
    let restored = rt.get(&task.task_id, None).unwrap();
    let output = rt
        .get_output(
            &task.task_id,
            &JsValue::Number(0.0),
            &JsValue::Undefined,
            None,
        )
        .unwrap();
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(restored.status, TaskStatus::Completed);
    assert_eq!(restored.command, expected_command);
    assert_eq!(restored.session_id, expected_session);
    assert_eq!(restored.agent_id, expected_agent);
    assert_eq!(output.content, "DURABLE-OUTPUT");
    assert!(
        std::fs::read_to_string(root.0.join(".pilotdeck/background-tasks/state.json"))
            .unwrap()
            .contains("\\ud800")
    );
}
#[tokio::test]
async fn overlapping_generations_keep_one_archive_and_fence_stale_restored_writers() {
    let root = fixture();
    std::fs::write(
        root.0.join("long.sh"),
        "printf STARTED > started\nprintf OLD-OUTPUT\nsleep 30\nprintf LATE > never\n",
    )
    .unwrap();
    std::fs::write(root.0.join("second.sh"), "printf NEW-OUTPUT\n").unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let a_store = archive.handle();
    let a = runtime(&root.0, a_store.clone(), NativeTaskLimits::CLI);
    let old_task = a.start(spec(&root.0, "sh long.sh")).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !root.0.join("started").exists() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    let started = root.0.join("started").exists();
    let b_store = archive.handle();
    let b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime(&root.0, b_store.clone(), NativeTaskLimits::CLI)
    }));
    let b = match b {
        Ok(b) => b,
        Err(error) => {
            let _ = a.dispose().await;
            std::panic::resume_unwind(error);
        }
    };
    let restored_status = b.get(&old_task.task_id, None).map(|task| task.status);
    let new_task = b.start(spec(&root.0, "sh second.sh")).await;
    let new_status = if let Ok(task) = &new_task {
        wait(&b, &task.task_id).await
    } else {
        Err(io::Error::other("new generation start failed"))
    };
    let b_disposed = b.dispose().await;
    let old_still_running = a
        .get(&old_task.task_id, None)
        .is_some_and(|task| task.status == TaskStatus::Running);
    let a_disposed = a.dispose().await;
    assert!(a_disposed.is_ok());
    assert!(b_disposed.is_ok());
    assert!(started);
    assert_eq!(restored_status, Some(TaskStatus::Unknown));
    assert!(old_still_running);
    let new_task = new_task.unwrap();
    assert_eq!(new_status.unwrap(), TaskStatus::Completed);
    let records = lock(&archive.state).records.clone();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .find(|r| r.snapshot.task.task_id == old_task.task_id.to_utf8().unwrap())
            .unwrap()
            .snapshot
            .task
            .status,
        "cancelled"
    );
    assert_eq!(
        records
            .iter()
            .find(|r| r.snapshot.task.task_id == new_task.task_id.to_utf8().unwrap())
            .unwrap()
            .snapshot
            .task
            .status,
        "completed"
    );
    assert!(!root.0.join("never").exists());
    assert_eq!(
        NativeArchive::open(&root.0).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
}
#[tokio::test]
async fn native_limits_reject_new_metadata_and_retained_overflow_without_dropping_records() {
    let root = fixture();
    std::fs::write(root.0.join("owned.sh"), "printf OK\n").unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let store = archive.handle();
    let limits = NativeTaskLimits {
        retained_tasks: 2,
        metadata_units: 512,
        output_bytes: 512,
        read_bytes: 128,
    };
    let rt = runtime(&root.0, store.clone(), limits);
    let oversized = rt.start(spec(&root.0, &"x".repeat(513))).await;
    let first = rt.start(spec(&root.0, "sh owned.sh")).await.unwrap();
    let first_status = wait(&rt, &first.task_id).await;
    let second = rt.start(spec(&root.0, "sh owned.sh")).await.unwrap();
    let second_status = wait(&rt, &second.task_id).await;
    let rejected = rt.start(spec(&root.0, "sh owned.sh")).await;
    let old_count = rt.list(&Default::default(), None).len();
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    assert!(oversized.is_err());
    assert!(rejected.is_err());
    assert_eq!(old_count, 2);
    assert_eq!(first_status.unwrap(), TaskStatus::Completed);
    assert_eq!(second_status.unwrap(), TaskStatus::Completed);
    assert_eq!(lock(&archive.state).records.len(), 2);
}
#[tokio::test]
async fn output_budget_kills_only_the_owned_group_and_persists_an_explicit_failure() {
    let root = fixture();
    std::fs::write(
        root.0.join("emit.sh"),
        "while :; do printf '0123456789012345678901234567890123456789'; done\n",
    )
    .unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let store = archive.handle();
    let limits = NativeTaskLimits {
        retained_tasks: 2,
        metadata_units: 512,
        output_bytes: 512,
        read_bytes: 128,
    };
    let rt = runtime(&root.0, store.clone(), limits);
    let task = rt.start(spec(&root.0, "sh emit.sh")).await.unwrap();
    let status = wait(&rt, &task.task_id).await;
    let output = rt.get_output(
        &task.task_id,
        &JsValue::Number(0.0),
        &JsValue::Number(1_000_000.0),
        None,
    );
    let reason = rt.failure_reason(&task.task_id, None);
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(status.unwrap(), TaskStatus::Failed);
    let output = output.unwrap();
    assert_eq!(output.total_bytes, 512.0);
    assert!(output.content.len() <= 128);
    assert_eq!(
        reason.as_ref().unwrap(),
        &JsString::from(
            "Native background output limit reached; the owned process group was stopped."
        )
    );
    assert_eq!(lock(&archive.state).records[0].failure, reason);
    drop(rt);
    drop(store);
    drop(archive);
    let archive = reopen(&root.0).await;
    let store = archive.handle();
    let rt = runtime(&root.0, store, NativeTaskLimits::CLI);
    let restored_reason = rt.failure_reason(&task.task_id, None);
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(restored_reason, reason);
}
#[test]
fn snapshot_links_large_files_and_unbounded_years_fail_closed_without_overwriting_sources() {
    let root = fixture();
    let path = root.0.join(".pilotdeck/background-tasks");
    std::fs::create_dir_all(&path).unwrap();
    let source = root.0.join("original");
    std::fs::write(&source, "UNCHANGED").unwrap();
    std::os::unix::fs::symlink(&source, path.join("state.json")).unwrap();
    assert!(NativeArchive::open(&root.0).is_err());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "UNCHANGED");
    std::fs::remove_file(path.join("state.json")).unwrap();
    std::fs::hard_link(&source, path.join("state.json")).unwrap();
    assert!(NativeArchive::open(&root.0).is_err());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "UNCHANGED");
    std::fs::remove_file(path.join("state.json")).unwrap();
    let file = File::create(path.join("state.json")).unwrap();
    file.set_len(SNAPSHOT_BYTES as u64 + 1).unwrap();
    drop(file);
    assert!(NativeArchive::open(&root.0).is_err());
    let started = std::time::Instant::now();
    assert!(native_date("2147483647-01-01T00:00:00.000Z").is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}
#[tokio::test]
async fn fifo_snapshot_is_rejected_before_a_blocking_read() {
    let root = fixture();
    let path = root.0.join(".pilotdeck/background-tasks");
    std::fs::create_dir_all(&path).unwrap();
    let mut command = tokio::process::Command::new("/usr/bin/mkfifo");
    command.arg(path.join("state.json")).kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    let started = std::time::Instant::now();
    assert!(NativeArchive::open(&root.0).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn unlinked_output_is_not_reported_as_durable_success() {
    let root = fixture();
    std::fs::write(root.0.join("owned.sh"), "printf OUTPUT\n").unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let store = archive.handle();
    let rt = runtime(&root.0, store, NativeTaskLimits::CLI);
    let task = rt.start(spec(&root.0, "sh owned.sh")).await.unwrap();
    let status = wait(&rt, &task.task_id).await;
    // First flush accepted metadata before deliberately removing only our
    // own output leaf. The open fd must not be mistaken for a durable name.
    let revision = lock(&archive.state).revision;
    archive.wait(revision).await.unwrap();
    let output = root
        .0
        .join(".pilotdeck/background-tasks/output")
        .join(format!("{}.log", task.task_id.to_utf8().unwrap()));
    let removed = std::fs::remove_file(output);
    let disposed = rt.dispose().await;
    assert_eq!(status.unwrap(), TaskStatus::Completed);
    assert!(removed.is_ok());
    assert!(disposed.is_err(), "unlinked output was claimed durable");
    assert!(lock(&archive.state).error.is_some());
}
#[tokio::test]
async fn replaced_owner_lock_refuses_new_writes_without_touching_existing_records() {
    let root = fixture();
    let archive = NativeArchive::open(&root.0).unwrap();
    let store = archive.handle();
    let rt = runtime(&root.0, store, NativeTaskLimits::CLI);
    let lock_path = root.0.join(".pilotdeck/background-tasks/.lock");
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::write(&lock_path, "OTHER OWNER").unwrap();
    let result = rt.start(spec(&root.0, "printf MUST-NOT-START")).await;
    let disposed = rt.dispose().await;
    assert!(result.is_err());
    assert!(disposed.is_ok());
    assert!(lock(&archive.state).records.is_empty());
    assert_eq!(std::fs::read_to_string(lock_path).unwrap(), "OTHER OWNER");
}

#[test]
fn archive_writer_guard_reports_an_unpolled_future_drop() {
    let root = fixture();
    let archive = NativeArchive::open(&root.0).unwrap();
    let guard = WriterGuard {
        archive: archive.clone(),
        armed: true,
        _deferred_owner: None,
    };
    let future = guard.run();
    drop(future);
    let error = lock(&archive.state)
        .error
        .clone()
        .expect("unpolled writer cancellation must wake waiters");
    assert!(error.contains("cancelled or panicked"));
    assert_eq!(archive.status.borrow().error.as_ref(), Some(&error));
}
#[test]
fn archive_writer_guard_disarms_only_after_an_explicit_normal_completion() {
    let root = fixture();
    let archive = NativeArchive::open(&root.0).unwrap();
    let mut guard = WriterGuard {
        archive: archive.clone(),
        armed: true,
        _deferred_owner: None,
    };
    guard.disarm();
    drop(guard);
    assert!(lock(&archive.state).error.is_none());
}

#[tokio::test]
async fn selected_project_symlink_pins_the_target_and_keeps_real_durable_output() {
    let root = fixture();
    let project = root.0.join("real-project");
    let selected = root.0.join("selected-project");
    std::fs::create_dir(&project).unwrap();
    std::os::unix::fs::symlink(&project, &selected).unwrap();
    std::fs::write(project.join("owned.sh"), "printf LINKED-PROJECT\n").unwrap();
    let archive = NativeArchive::open(&selected).unwrap();
    let store = archive.handle();
    let rt = runtime(&selected, store, NativeTaskLimits::CLI);
    let task = rt.start(spec(&selected, "sh owned.sh")).await;
    let status = if let Ok(task) = &task {
        wait(&rt, &task.task_id).await
    } else {
        Err(io::Error::other("linked project task did not start"))
    };
    let disposed = rt.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(status.unwrap(), TaskStatus::Completed);
    let task = task.unwrap();
    assert!(
        project
            .join(".pilotdeck/background-tasks/state.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(
            project
                .join(".pilotdeck/background-tasks/output")
                .join(format!("{}.log", task.task_id.to_utf8().unwrap()))
        )
        .unwrap(),
        "LINKED-PROJECT"
    );
    assert_eq!(
        archive.path,
        project
            .canonicalize()
            .unwrap()
            .join(".pilotdeck/background-tasks")
    );
}
#[test]
fn archive_descendant_symlinks_still_refuse_without_writing_their_targets() {
    let root = fixture();
    for (index, relative) in [
        ".pilotdeck",
        ".pilotdeck/background-tasks",
        ".pilotdeck/background-tasks/output",
    ]
    .into_iter()
    .enumerate()
    {
        let project = root.0.join(format!("project-{index}"));
        let outside = root.0.join(format!("outside-{index}"));
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), "UNCHANGED").unwrap();
        let alias = project.join(relative);
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        assert!(
            NativeArchive::open(&project).is_err(),
            "accepted descendant symlink {relative}"
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel")).unwrap(),
            "UNCHANGED"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "archive wrote through {relative}"
        );
    }
}

#[tokio::test]
async fn stale_unknown_view_cannot_take_over_dead_owners_final_record_and_fresh_handle_can_restore_it()
 {
    let root = fixture();
    std::fs::write(root.0.join("gate.sh"),"printf STARTED > started\nwhile [ ! -f release ]; do sleep 0.01; done\nprintf OWNER_FINAL_OUTPUT\n").unwrap();
    let archive = NativeArchive::open(&root.0).unwrap();
    let a_store = archive.handle();
    let a = runtime(&root.0, a_store.clone(), NativeTaskLimits::CLI);
    let task = a.start(spec(&root.0, "sh gate.sh")).await.unwrap();
    let id = task.task_id.to_utf8().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !root.0.join("started").exists() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    let started = root.0.join("started").exists();
    let b_store = archive.handle();
    let b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime(&root.0, b_store.clone(), NativeTaskLimits::CLI)
    }));
    let b = match b {
        Ok(b) => b,
        Err(error) => {
            let _ = a.dispose().await;
            std::panic::resume_unwind(error)
        }
    };
    let unknown = b.get(&task.task_id, None).map(|task| task.status);
    let wait_unknown = b
        .wait(&task.task_id, &JsValue::Number(1.0), None, None)
        .await;
    let stop_unknown = b
        .stop(&task.task_id, &JsValue::Undefined, None)
        .await
        .err()
        .map(|error| error.name);
    let release = std::fs::write(root.0.join("release"), b"go");
    let finished = wait(&a, &task.task_id).await;
    let a_disposed = a.dispose().await;
    drop(a);
    drop(a_store);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let alive = lock(&archive.state)
            .records
            .iter()
            .find(|record| record.snapshot.task.task_id == id)
            .and_then(|record| record.owner.as_ref())
            .and_then(|owner| owner.token.upgrade())
            .is_some();
        if !alive || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::task::yield_now().await;
    }
    let owner_dead = lock(&archive.state)
        .records
        .iter()
        .find(|record| record.snapshot.task.task_id == id)
        .and_then(|record| record.owner.as_ref())
        .and_then(|owner| owner.token.upgrade())
        .is_none();
    let b_disposed = b.dispose().await;
    let record = lock(&archive.state)
        .records
        .iter()
        .find(|record| record.snapshot.task.task_id == id)
        .unwrap()
        .snapshot
        .clone();
    let c_store = archive.handle();
    let c = runtime(&root.0, c_store.clone(), NativeTaskLimits::CLI);
    let fresh = c.get(&task.task_id, None).map(|task| task.status);
    let c_disposed = c.dispose().await;
    // All actual children and owners are settled before the assertions.
    assert!(started);
    assert!(release.is_ok());
    assert_eq!(finished.unwrap(), TaskStatus::Completed);
    assert!(a_disposed.is_ok());
    assert!(b_disposed.is_ok());
    assert!(c_disposed.is_ok());
    assert_eq!(unknown, Some(TaskStatus::Unknown));
    let observed_unknown = wait_unknown.unwrap().unwrap();
    assert_eq!(observed_unknown.outcome, "unknown");
    assert!(!observed_unknown.timed_out);
    assert_eq!(observed_unknown.task.status, TaskStatus::Unknown);
    assert_eq!(stop_unknown, Some("BackgroundTaskStateUnknownError"));
    assert!(owner_dead);
    assert!(lock(&b_store.readonly).contains(&id));
    assert_eq!(record.task.status, "completed");
    assert!(record.output.total_bytes > 0);
    assert_eq!(fresh, Some(TaskStatus::Completed));
}

fn deferred_runtime(project: &Path, store: Arc<DeferredHandle>) -> BackgroundTaskRuntime {
    BackgroundTaskRuntime::new_native(
        BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(project.join(".pilotdeck/background-tasks")),
            snapshot_store: Some(store.clone()),
            ..Default::default()
        },
        Arc::new(NativeSandbox::new(
            SandboxMode::DangerFullAccess,
            std::env::temp_dir(),
        )),
        store,
        NativeTaskLimits::CLI,
    )
}

#[tokio::test]
async fn absent_archive_load_and_dispose_are_read_only() {
    let root = fixture();
    let shared = DeferredArchive::new(&root.0).unwrap();
    shared.prepare_existing().unwrap();
    let store = shared.handle();
    let rt = deferred_runtime(&root.0, store.clone());
    assert!(store.load().is_empty());
    assert!(lock(&shared.state).archive.is_none());
    assert!(
        !root.0.join(".pilotdeck").exists(),
        "read-only background construction must not create archive paths"
    );
    store.flush().await.unwrap();
    rt.dispose().await.unwrap();
    assert!(
        !root.0.join(".pilotdeck").exists(),
        "read-only background disposal must not create archive paths"
    );
}

#[tokio::test]
async fn deferred_first_write_queues_synchronously_and_retains_weak_owner_until_writer_finishes() {
    let root = fixture();
    let shared = DeferredArchive::new(&root.0).unwrap();
    shared.prepare_existing().unwrap();
    let weak = Arc::downgrade(&shared);
    let store = shared.handle();
    let id = uuid::Uuid::new_v4().to_string();
    let task = BackgroundBashTask {
        task_id: id.as_str().into(),
        agent_id: JsValue::Undefined,
        session_id: "owner".into(),
        kind: "bash".into(),
        command: "fixture metadata only".into(),
        cwd: root.0.to_string_lossy().as_ref().into(),
        pid: None,
        status: TaskStatus::Pending,
        exit_code: None,
        interrupted: false,
        started_at: 0.0,
        ended_at: None,
        output_bytes: 0,
    };
    let pending = store.write_task(
        task,
        PersistedOutput {
            total_bytes: 0,
            persisted: false,
        },
        0,
        None,
    );
    // No await or poll occurred: open/flock and metadata queue must already
    // be complete, just as for the original ArchiveHandle.
    let archive = lock(&shared.state).archive.clone().unwrap();
    assert_eq!(
        lock(&archive.state).records.len(),
        1,
        "first deferred write must synchronously admit metadata"
    );
    assert!(root.0.join(".pilotdeck/background-tasks/.lock").is_file());
    drop(pending);
    drop(archive);
    drop(store);
    drop(shared);
    assert!(
        weak.upgrade().is_some(),
        "pending writer must retain the same Weak cache owner"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while weak.upgrade().is_some() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert!(
        weak.upgrade().is_none(),
        "completed writer must release its temporary lazy owner retain"
    );
    let text =
        std::fs::read_to_string(root.0.join(".pilotdeck/background-tasks/state.json")).unwrap();
    assert!(
        text.contains(&id),
        "unpolled caller future must not discard the queued native snapshot"
    );
}

#[tokio::test]
async fn deferred_first_real_task_is_durable_and_a_second_generation_reuses_the_archive() {
    let root = fixture();
    let shared = DeferredArchive::new(&root.0).unwrap();
    shared.prepare_existing().unwrap();
    let first_store = shared.handle();
    let first = deferred_runtime(&root.0, first_store.clone());
    let one = first
        .start(spec(&root.0, "printf FIRST-DURABLE"))
        .await
        .unwrap();
    let first_status = wait(&first, &one.task_id).await;
    shared.prepare_existing().unwrap();
    let second_store = shared.handle();
    let second = deferred_runtime(&root.0, second_store.clone());
    let restored = second.get(&one.task_id, None).map(|task| task.status);
    let same = Arc::ptr_eq(
        &first_store.existing().unwrap().archive,
        &second_store.existing().unwrap().archive,
    );
    let two = second.start(spec(&root.0, "printf SECOND-DURABLE")).await;
    let second_status = if let Ok(task) = &two {
        wait(&second, &task.task_id).await
    } else {
        Err(io::Error::other("second generation start failed"))
    };
    let second_closed = second.dispose().await;
    let first_closed = first.dispose().await;
    assert!(second_closed.is_ok());
    assert!(first_closed.is_ok());
    assert_eq!(first_status.unwrap(), TaskStatus::Completed);
    assert_eq!(restored, Some(TaskStatus::Completed));
    assert!(
        same,
        "overlapping generations must use the same concrete native archive"
    );
    assert_eq!(second_status.unwrap(), TaskStatus::Completed);
    let two = two.unwrap();
    let state =
        std::fs::read_to_string(root.0.join(".pilotdeck/background-tasks/state.json")).unwrap();
    assert!(
        state.contains(&one.task_id.to_utf8().unwrap())
            && state.contains(&two.task_id.to_utf8().unwrap())
    );
    let output = root
        .0
        .join(".pilotdeck/background-tasks/output")
        .join(format!("{}.log", one.task_id.to_utf8().unwrap()));
    assert_eq!(std::fs::read(output).unwrap(), b"FIRST-DURABLE");
}

#[tokio::test]
async fn deferred_archive_refuses_existing_and_late_foreign_lock_before_spawning() {
    let root = fixture();
    let shared = DeferredArchive::new(&root.0).unwrap();
    shared.prepare_existing().unwrap();
    let rt = deferred_runtime(&root.0, shared.handle());
    // Another process/owner creates the archive after our empty load. Its
    // lock must still be checked by the first real metadata write.
    let foreign = NativeArchive::open(&root.0).unwrap();
    let separate = DeferredArchive::new(&root.0).unwrap();
    let initial_error = separate.prepare_existing().unwrap_err();
    let started = rt
        .start(spec(&root.0, "printf SHOULD-NOT-RUN > forbidden-child.txt"))
        .await;
    let closed = rt.dispose().await;
    assert!(closed.is_ok());
    assert_eq!(initial_error.kind(), io::ErrorKind::WouldBlock);
    let error = started.unwrap_err();
    assert!(
        error
            .message
            .to_utf8()
            .unwrap()
            .contains("Another process owns this project's background archive.")
    );
    assert!(
        !root.0.join("forbidden-child.txt").exists(),
        "foreign archive lock refusal must precede native child spawn"
    );
    assert!(lock(&foreign.state).records.is_empty());
}
