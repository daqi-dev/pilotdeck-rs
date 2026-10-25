use super::*;
use crate::runtime::project::persistence::PersistenceResources;
use crate::storage::{SnapshotState, WorkspaceSnapshotInput, recorder::RecorderError};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::fs;

fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/project-persistence/oracle.json"
    ))
    .unwrap()
}
fn registry(scratch: &Scratch, configured: bool) -> ProjectRuntimeRegistry {
    let mut env = write_project_config(scratch, CONFIG_YAML_NO_MEMORY);
    env.insert(
        "PILOTDECK_SNAPSHOT_ROOT".into(),
        scratch.0.join("snapshots").display().to_string(),
    );
    env.insert(
        "PILOTDECK_STORAGE_ROOT".into(),
        if configured {
            scratch.0.join("storage").display().to_string()
        } else {
            "  ".into()
        },
    );
    ProjectRuntimeRegistry::new(
        scratch.0.join("project"),
        Arc::new(ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: scratch.0.join("pilot-home"),
            cwd: scratch.0.clone(),
            env,
            homedir: Some(scratch.0.join("home")),
            builtin_skills_root: None,
            now: Arc::new(|| UNIX_EPOCH),
            config_version: None,
            runtime_profile_overrides: RuntimeProfileOverrides::default(),
            families: silent_families().with_context_memory(),
        })),
    )
}
fn resource(lease: &RuntimeLease) -> Arc<dyn FamilyResource> {
    let resource = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context()
        .resource(Resource::Persistence)
        .expect("real persistence slot");
    assert_eq!(Resource::Persistence.as_str(), "persistence");
    resource
}
fn native(resource: &Arc<dyn FamilyResource>) -> &PersistenceResources {
    resource.as_any().downcast_ref().unwrap()
}
fn bytes(entries: &[JsValue]) -> Vec<u8> {
    entries
        .iter()
        .map(|e| stringify_json(e).unwrap().unwrap() + "\n")
        .collect::<String>()
        .into_bytes()
}
fn input(scratch: &Scratch) -> WorkspaceSnapshotInput {
    WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "r".into(),
        workspace_dir: scratch.0.join("workspace").display().to_string(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    }
}

#[tokio::test]
async fn persistence_real_recorder_survives_runtime_restart_and_isolates_sessions() {
    let scratch = Scratch::new();
    let path = scratch.0.join("events.jsonl");
    let first = registry(&scratch, false);
    let lease = first.acquire(None).unwrap();
    let handle = resource(&lease);
    let session = native(&handle)
        .create_session("s".into(), Some(path.clone()))
        .unwrap();
    assert!(session.is_persistent());
    assert_eq!(session.restored.diagnostics[0].code, "transcript_missing");
    session
        .recorder
        .start_turn(&"s".into(), &"t1".into())
        .unwrap();
    session
        .recorder
        .record_model_request(
            &"s".into(),
            &"t1".into(),
            parse_json(r#"{"messages":[],"label":"中文\n\u0000"}"#).unwrap(),
        )
        .unwrap();
    let original = fs::read(&path).unwrap();
    assert_eq!(original, bytes(&session.read().unwrap().entries));
    let other = native(&handle)
        .create_session("other".into(), Some(scratch.0.join("other.jsonl")))
        .unwrap();
    other
        .recorder
        .start_turn(&"other".into(), &"t1".into())
        .unwrap();
    assert_eq!(other.recorder.snapshot_state().0, 1);
    assert_eq!(session.recorder.snapshot_state().0, 3);
    drop((session, other, handle, lease));
    bounded(first.dispose()).await.unwrap();
    drop(first);
    let second = registry(&scratch, false);
    let lease = second.acquire(None).unwrap();
    let handle = resource(&lease);
    let restored = native(&handle)
        .create_session("s".into(), Some(path.clone()))
        .unwrap();
    assert_eq!(bytes(&restored.restored.entries), original);
    assert_eq!(fs::read(&path).unwrap(), original);
    let parent = restored.recorder.snapshot_state().1.unwrap();
    assert!(matches!(
        restored.recorder.start_turn(&"s".into(), &"t1".into()),
        Err(RecorderError::Domain {
            code: "duplicate_turn_start",
            ..
        })
    ));
    restored
        .recorder
        .start_turn(&"s".into(), &"t2".into())
        .unwrap();
    let entries = restored.read().unwrap().entries;
    assert_eq!(entries.len(), 4);
    assert_eq!(
        entries[3].get("parentEntryId").unwrap().as_string(),
        Some(&parent)
    );
    assert!(fs::read(&path).unwrap().starts_with(&original));
    drop((restored, handle, lease));
    bounded(second.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_ts_bytes_and_torn_tail_are_preserved_on_reopen() {
    let scratch = Scratch::new();
    let oracle = oracle();
    let full = oracle["events"]["fullBytes"].as_str().unwrap();
    let expected = oracle["events"]["repairedBytes"].as_str().unwrap();
    let path = scratch.0.join("gold.jsonl");
    let first = registry(&scratch, false);
    let lease = first.acquire(None).unwrap();
    let handle = resource(&lease);
    let session = native(&handle)
        .create_session("s".into(), Some(path.clone()))
        .unwrap();
    // Replay actual TS envelopes through the shared production writer.
    for line in full.lines() {
        session
            .recorder
            .writer()
            .record_entry(&parse_json(line).unwrap())
            .unwrap();
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), full);
    drop((session, handle, lease));
    bounded(first.dispose()).await.unwrap();
    drop(first);
    fs::write(&path, format!("{full}{{\"type\":")).unwrap();
    let second = registry(&scratch, false);
    let lease = second.acquire(None).unwrap();
    let handle = resource(&lease);
    let session = native(&handle)
        .create_session("s".into(), Some(path.clone()))
        .unwrap();
    assert_eq!(session.restored.entries.len(), 2);
    assert_eq!(
        session
            .restored
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        [oracle["events"]["partialCodes"][0].as_str().unwrap()]
    );
    assert_eq!(session.restored.diagnostics[0].line, Some(3));
    session
        .recorder
        .writer()
        .record_entry(&parse_json(expected.lines().last().unwrap()).unwrap())
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    let read = session.read().unwrap();
    assert_eq!(
        read.entries.len(),
        oracle["events"]["repairedCount"].as_u64().unwrap() as usize
    );
    assert_eq!(read.diagnostics.len(), 1);
    drop((session, handle, lease));
    bounded(second.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_unconfigured_is_real_memory_recording_without_disk_or_snapshot() {
    let scratch = Scratch::new();
    let registry = registry(&scratch, false);
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    assert!(native(&handle).snapshots.is_none());
    let first = native(&handle).create_session("s".into(), None).unwrap();
    assert!(!first.is_persistent());
    assert!(first.restored.diagnostics.is_empty());
    first.recorder.start_turn(&"s".into(), &"t".into()).unwrap();
    assert_eq!(
        first.read().unwrap().entries.len(),
        oracle()["events"]["memoryCount"].as_u64().unwrap() as usize
    );
    let second = native(&handle).create_session("s".into(), None).unwrap();
    assert!(second.read().unwrap().entries.is_empty());
    assert!(!scratch.0.join("snapshots").exists());
    assert!(!scratch.0.join("pilot-home").exists());
    drop((first, second, handle, lease));
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_append_failure_does_not_commit_and_same_instance_retries() {
    let scratch = Scratch::new();
    let registry = registry(&scratch, false);
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    let path = scratch.0.join("parent/events.jsonl");
    let session = native(&handle)
        .create_session("s".into(), Some(path.clone()))
        .unwrap();
    fs::write(path.parent().unwrap(), "blocker").unwrap();
    let error = session
        .recorder
        .start_turn(&"s".into(), &"failed".into())
        .unwrap_err();
    eprintln!("native append error: {error:?}; display={error}");
    assert!(matches!(
        error,
        RecorderError::Store(crate::storage::LedgerError::Io(_))
    ));
    assert_eq!(session.recorder.snapshot_state(), (0, None));
    assert_eq!(oracle()["events"]["stateAfterFailure"]["sequence"], 0);
    fs::remove_file(path.parent().unwrap()).unwrap();
    session
        .recorder
        .start_turn(&"s".into(), &"ok".into())
        .unwrap();
    let before = fs::read(&path).unwrap();
    // Actual mid-session IO failure after a successful write, with recoverable evidence.
    fs::rename(path.parent().unwrap(), scratch.0.join("saved")).unwrap();
    fs::write(path.parent().unwrap(), "blocker").unwrap();
    assert!(matches!(
        session.recorder.start_turn(&"s".into(), &"retry".into()),
        Err(RecorderError::Store(_))
    ));
    assert_eq!(
        session.recorder.snapshot_state().0,
        oracle()["events"]["midSequence"].as_u64().unwrap()
    );
    assert_eq!(
        fs::read(scratch.0.join("saved/events.jsonl")).unwrap(),
        before
    );
    fs::remove_file(path.parent().unwrap()).unwrap();
    fs::rename(scratch.0.join("saved"), path.parent().unwrap()).unwrap();
    session
        .recorder
        .start_turn(&"s".into(), &"retry".into())
        .unwrap();
    assert_eq!(
        session.recorder.snapshot_state().0,
        oracle()["events"]["retrySequence"].as_u64().unwrap()
    );
    assert_eq!(session.read().unwrap().entries.len(), 2);
    drop((session, handle, lease));
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_restore_rejects_io_and_invalid_chain_instead_of_resetting() {
    let scratch = Scratch::new();
    let registry = registry(&scratch, false);
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    assert!(matches!(
        native(&handle).create_session("s".into(), Some(scratch.0.clone())),
        Err(RecorderError::Store(_))
    ));
    let first = oracle()["events"]["firstBytes"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = scratch.0.join("invalid.jsonl");
    fs::write(&path, format!("{first}{first}")).unwrap();
    assert!(matches!(
        native(&handle).create_session("s".into(), Some(path.clone())),
        Err(RecorderError::Envelope { .. })
    ));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{first}{first}")
    );
    drop((handle, lease));
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_snapshot_restores_real_bytes_after_runtime_restart() {
    let scratch = Scratch::new();
    let first = registry(&scratch, true);
    fs::create_dir(scratch.0.join("workspace")).unwrap();
    let payload = [0, 255, 10, 228, 184, 173];
    fs::write(scratch.0.join("workspace/payload.bin"), payload).unwrap();
    let lease = first.acquire(None).unwrap();
    let handle = resource(&lease);
    let snapshots = native(&handle).snapshots.as_ref().unwrap();
    let result = snapshots
        .recorder
        .capture_pre_user(&input(&scratch))
        .unwrap();
    assert_eq!(result.state, SnapshotState::Committed);
    assert_eq!(
        result.snapshot_id,
        oracle()["snapshots"]["snapshotId"].as_str().unwrap()
    );
    let manifest = fs::read(result.manifest_path.as_ref().unwrap()).unwrap();
    assert_eq!(snapshots.provider.list().unwrap().len(), 1);
    drop((handle, lease));
    bounded(first.dispose()).await.unwrap();
    drop(first);
    fs::remove_file(scratch.0.join("workspace/payload.bin")).unwrap();
    let second = registry(&scratch, true);
    let lease = second.acquire(None).unwrap();
    let handle = resource(&lease);
    let snapshots = native(&handle).snapshots.as_ref().unwrap();
    let repeated = snapshots
        .recorder
        .capture_pre_user(&input(&scratch))
        .unwrap();
    assert_eq!(repeated, result);
    assert_eq!(fs::read(repeated.manifest_path.unwrap()).unwrap(), manifest);
    snapshots
        .provider
        .restore(
            &result.snapshot_id,
            scratch.0.join("restored").to_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        fs::read(scratch.0.join("restored/payload.bin")).unwrap(),
        payload
    );
    drop((handle, lease));
    bounded(second.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_snapshot_failed_marker_and_hard_error_are_not_success() {
    let scratch = Scratch::new();
    let registry = registry(&scratch, true);
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    let snapshots = native(&handle).snapshots.as_ref().unwrap();
    let mut input = input(&scratch);
    input.workspace_stable = Some(false);
    let result = snapshots.recorder.capture_pre_user(&input).unwrap();
    assert_eq!(result.state, SnapshotState::Failed);
    assert!(result.manifest_path.is_none());
    let marker: Value =
        serde_json::from_slice(&fs::read(result.failure_marker_path.as_ref().unwrap()).unwrap())
            .unwrap();
    assert_eq!(marker["error"], oracle()["snapshots"]["failureError"]);
    assert!(snapshots.provider.list().unwrap().is_empty());
    assert!(
        snapshots
            .provider
            .restore(
                &result.snapshot_id,
                scratch.0.join("restored").to_str().unwrap()
            )
            .is_err()
    );
    // Missing workspace becomes a failed snapshot, not an empty committed one.
    input.turn_id = "missing".into();
    input.workspace_stable = None;
    assert_eq!(
        snapshots.recorder.capture_pre_user(&input).unwrap().state,
        SnapshotState::Failed
    );
    fs::rename(
        scratch.0.join("snapshots"),
        scratch.0.join("saved-snapshots"),
    )
    .unwrap();
    fs::write(scratch.0.join("snapshots"), "blocker").unwrap();
    input.turn_id = "hard".into();
    assert!(snapshots.recorder.capture_pre_user(&input).is_err());
    drop((handle, lease));
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_oversized_log_never_publishes_a_reset_writer() {
    let scratch = Scratch::new();
    let registry = registry(&scratch, false);
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    let path = scratch.0.join("large.jsonl");
    let file = fs::File::create(&path).unwrap();
    let size = 50 * 1024 * 1024 + 1;
    file.set_len(size).unwrap();
    let Err(error) = native(&handle).create_session("s".into(), Some(path.clone())) else {
        panic!("size-limit diagnostics must not publish a writable empty chain");
    };
    assert!(matches!(error, RecorderError::State(_)));
    assert!(error.to_string().contains("larger than 52428800 bytes"));
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
    // Sibling writable restore must reject the same size-limited projection.
    let chat_dir = scratch.0.clone();
    let adapter = crate::storage::AsyncTranscriptAdapter::new(
        Arc::new(crate::storage::JsonlTranscriptStore::new()),
        Arc::new(move |_| chat_dir.clone()),
    );
    let storage = adapter.create_session_storage(crate::storage::SessionStorageInput {
        project_root: scratch.0.join("project"),
        pilot_home: scratch.0.join("home"),
        session_id: "large".into(),
        now: None,
    });
    assert_eq!(storage.transcript_path, path);
    assert!(storage.restore().is_err());
    assert_eq!(storage.transcript.snapshot_state().sequence, 0);
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
    assert_eq!(oracle()["events"]["oversizedSequence"], 1);
    // Positive control: an actual empty log does open and record.
    let empty = native(&handle)
        .create_session("s".into(), Some(scratch.0.join("empty.jsonl")))
        .unwrap();
    empty.recorder.start_turn(&"s".into(), &"t".into()).unwrap();
    assert_eq!(empty.read().unwrap().entries.len(), 1);
    drop((empty, handle, lease));
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn persistence_missing_snapshot_environment_is_explicitly_absent() {
    let scratch = Scratch::new();
    let registry = registry_for_yaml(
        &scratch,
        CONFIG_YAML_NO_MEMORY,
        silent_families().with_context_memory(),
    );
    let lease = registry.acquire(None).unwrap();
    let handle = resource(&lease);
    assert!(native(&handle).snapshots.is_none());
    assert!(
        !native(&handle)
            .create_session("s".into(), None)
            .unwrap()
            .is_persistent()
    );
    drop((handle, lease));
    bounded(registry.dispose()).await.unwrap();
}
