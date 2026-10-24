//! A5.2 native regressions. The complete #19 TS-only matrix is deliberately
//! exercised separately by a52_baseline: it is not a Rust correction oracle.
#[path = "common/a52.rs"]
mod fixture;
#[path = "common/scratch.rs"]
mod scratch;
use fixture::*;
use pilotdeck_gateway::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, FileRunRegistry, RunAcceptInput, RunState,
    SnapshotState, WorkspaceSnapshotConfig, WorkspaceSnapshotInput,
};
use pilotdeck_protocol::{JsValue, stringify_json};
use scratch::Scratch;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{Arc, atomic::Ordering},
};

// Keep each tagged assertion paired with a mutation in the observer control
// test below. Adding a rule also requires exhaustive label and mutation arms.
// Mapping to scripts/a5-19/preregistration-b.mjs: WireSequence/FinalReplica
// cover FRAME_SEQ for this request; RunId covers its SESSION_ISOLATION slice;
// RegistryRaw covers REGISTRY_RAW for the queried run's full record/events.
// LiveSequence, FinishReason and Registry{Events,Sequence,LastSeq,Revision,
// Result,State} are native successful-turn invariants, not claims of the full
// TS production matrix (journals, fault schedules and other sessions included).
#[derive(Clone, Copy, Debug)]
enum ObserverRule {
    LiveSequence,
    WireSequence,
    FinalReplica,
    FinishReason,
    RunId,
    RegistryEvents,
    RegistrySequence,
    RegistryLastSeq,
    RegistryRevision,
    RegistryResult,
    RegistryState,
    RegistryRaw,
}
impl ObserverRule {
    const ALL: [Self; 12] = [
        Self::LiveSequence,
        Self::WireSequence,
        Self::FinalReplica,
        Self::FinishReason,
        Self::RunId,
        Self::RegistryEvents,
        Self::RegistrySequence,
        Self::RegistryLastSeq,
        Self::RegistryRevision,
        Self::RegistryResult,
        Self::RegistryState,
        Self::RegistryRaw,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::LiveSequence => "A52_LIVE_SEQUENCE",
            Self::WireSequence => "A52_WIRE_SEQUENCE",
            Self::FinalReplica => "A52_FINAL_REPLICA",
            Self::FinishReason => "A52_FINISH_REASON",
            Self::RunId => "A52_RUN_ID",
            Self::RegistryEvents => "A52_REGISTRY_EVENTS",
            Self::RegistrySequence => "A52_REGISTRY_SEQUENCE",
            Self::RegistryLastSeq => "A52_REGISTRY_LAST_SEQ",
            Self::RegistryRevision => "A52_REGISTRY_REVISION",
            Self::RegistryResult => "A52_REGISTRY_RESULT",
            Self::RegistryState => "A52_REGISTRY_STATE",
            Self::RegistryRaw => "A52_REGISTRY_RAW",
        }
    }
}

fn value(v: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(v).unwrap().unwrap()).unwrap()
}
fn events(path: &std::path::Path, session: &str, id: &str) -> Vec<Value> {
    FileRunRegistry::open(path)
        .unwrap()
        .events(&run(session, id), None, None)
        .unwrap()
        .iter()
        .map(value)
        .collect()
}
fn assert_completed(path: &std::path::Path, session: &str, id: &str, frames: &[Value]) {
    // Check raw identity/state before API reloads: a changed runId would make
    // events/get miss the record and mask the intended raw-snapshot rejection.
    let disk: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let raw_record = disk["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["sessionKey"] == session && r["runId"] == id)
        .expect(ObserverRule::RegistryRaw.label());
    assert_eq!(
        raw_record["state"],
        "completed",
        "{}",
        ObserverRule::RegistryState.label()
    );
    let live: Vec<_> = frames
        .iter()
        .filter(|v| v["type"] == "event" && v["final"] == false && v["event"].is_object())
        .map(|v| v["event"].clone())
        .collect();
    let durable = events(path, session, id);
    assert_eq!(
        live.iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["turn_started", "input_accepted", "turn_completed"],
        "{}",
        ObserverRule::LiveSequence.label()
    );
    // Validate live fields before comparing them with final/durable replicas.
    assert_eq!(
        live.last().unwrap()["finishReason"],
        "completed",
        "{}",
        ObserverRule::FinishReason.label()
    );
    assert!(
        live.iter().all(|v| v["runId"] == id),
        "{}",
        ObserverRule::RunId.label()
    );
    let wire: Vec<_> = frames.iter().filter(|v| v["type"] == "event").collect();
    for (seq, frame) in wire.iter().enumerate() {
        assert_eq!(frame["seq"], seq, "{}", ObserverRule::WireSequence.label());
    }
    assert_eq!(
        wire.iter().filter(|v| v["final"] == true).count(),
        1,
        "{}",
        ObserverRule::FinalReplica.label()
    );
    assert_eq!(
        wire.last().unwrap()["final"],
        true,
        "{}",
        ObserverRule::FinalReplica.label()
    );
    assert_eq!(
        wire.last().unwrap()["event"],
        *live.last().unwrap(),
        "{}",
        ObserverRule::FinalReplica.label()
    );
    assert_eq!(
        live,
        durable
            .iter()
            .map(|v| v["event"].clone())
            .collect::<Vec<_>>(),
        "{}",
        ObserverRule::RegistryEvents.label()
    );
    for (index, entry) in durable.iter().enumerate() {
        assert_eq!(
            entry["seq"],
            index + 1,
            "{}",
            ObserverRule::RegistrySequence.label()
        );
    }
    assert_eq!(live.last().unwrap()["type"], "turn_completed");
    let fresh = FileRunRegistry::open(path).unwrap();
    let record = fresh.get(&run(session, id)).unwrap().unwrap();
    assert_eq!(
        record.state(),
        Some(RunState::Completed),
        "{}",
        ObserverRule::RegistryState.label()
    );
    assert_eq!(
        record.last_seq(),
        Some(durable.len() as f64),
        "{}",
        ObserverRule::RegistryLastSeq.label()
    );
    assert_eq!(
        record.revision(),
        Some(durable.len() as f64 + 1.0),
        "{}",
        ObserverRule::RegistryRevision.label()
    );
    assert_eq!(
        record.result().map(value),
        live.last().cloned(),
        "{}",
        ObserverRule::RegistryResult.label()
    );
    // TS REGISTRY_RAW alignment for this native run: compare the entire stored
    // record (including result) and event list with the independent byte read.
    assert_eq!(
        value(record.as_json()),
        *raw_record,
        "{}",
        ObserverRule::RegistryRaw.label()
    );
    assert_eq!(
        json!(durable),
        disk["events"][format!("\0{session}\0{id}")],
        "{}",
        ObserverRule::RegistryRaw.label()
    );
}

#[tokio::test]
async fn two_connections_compete_for_one_session_without_second_execution() {
    let scratch = Scratch::new("a52-two-clients");
    let path = scratch.path().join("runs.json");
    let session = Arc::new(Session::default());
    let gateway = gateway(&path, HashMap::from([("s".into(), session.clone())]));
    let (a, a_text) = connect(&gateway).await;
    let (b, b_text) = connect(&gateway).await;
    let active = tokio::spawn(async move {
        a.receive(request("a", "s", "r1")).await.unwrap();
        a
    });
    bounded(session.entered.wait()).await;
    bounded(b.receive(request("b", "s", "r2"))).await.unwrap();
    let rejected = b_text.frames();
    assert!(
        rejected
            .iter()
            .any(|v| v["event"]["code"] == "session_busy"),
        "{rejected:?}"
    );
    assert_eq!(session.submits.load(Ordering::SeqCst), 1);
    assert!(!active.is_finished());
    assert_eq!(session.aborts.load(Ordering::SeqCst), 0);
    session.release.open();
    let a = bounded(active).await.unwrap();
    assert_completed(&path, "s", "r1", &a_text.frames());
    assert_eq!(session.aborts.load(Ordering::SeqCst), 0);
    assert!(
        !events(&path, "s", "r2")
            .iter()
            .any(|v| v["event"]["type"] == "turn_started")
    );
    bounded(a.closed()).await.unwrap();
    bounded(b.closed()).await.unwrap();
}

#[tokio::test]
async fn independent_sessions_progress_while_the_other_is_blocked() {
    let scratch = Scratch::new("a52-multi-session");
    let path = scratch.path().join("runs.json");
    let first = Arc::new(Session::default());
    let second = Arc::new(Session::default());
    let gateway = gateway(
        &path,
        HashMap::from([("s1".into(), first.clone()), ("s2".into(), second.clone())]),
    );
    let (a, a_text) = connect(&gateway).await;
    let (b, b_text) = connect(&gateway).await;
    let a_task = tokio::spawn(async move {
        a.receive(request("a", "s1", "r1")).await.unwrap();
        a
    });
    let b_task = tokio::spawn(async move {
        b.receive(request("b", "s2", "r2")).await.unwrap();
        b
    });
    bounded(async {
        tokio::join!(first.entered.wait(), second.entered.wait());
    })
    .await;
    first.release.open();
    let a = bounded(a_task).await.unwrap();
    assert_completed(&path, "s1", "r1", &a_text.frames());
    assert!(!b_task.is_finished());
    assert_eq!(
        FileRunRegistry::open(&path)
            .unwrap()
            .get(&run("s2", "r2"))
            .unwrap()
            .unwrap()
            .state(),
        Some(RunState::Running)
    );
    assert!(
        !events(&path, "s2", "r2")
            .iter()
            .any(|v| v["event"]["type"] == "turn_completed")
    );
    second.release.open();
    let b = bounded(b_task).await.unwrap();
    assert_completed(&path, "s2", "r2", &b_text.frames());
    assert_eq!(first.aborts.load(Ordering::SeqCst), 0);
    assert_eq!(second.aborts.load(Ordering::SeqCst), 0);
    bounded(a.closed()).await.unwrap();
    bounded(b.closed()).await.unwrap();
}

#[tokio::test]
async fn old_close_after_new_connection_admission_preserves_observed_abort_semantics() {
    // Baseline production reconnect-first ordering is NOT a promise that old
    // close cannot abort. TS run-production-b.py measures that stale close can
    // still abort the current run. Lock this behavior without changing A5.1.
    let scratch = Scratch::new("a52-reconnect");
    let path = scratch.path().join("runs.json");
    let session = Arc::new(Session::default());
    let gateway = gateway(&path, HashMap::from([("s".into(), session.clone())]));
    let (old, old_text) = connect(&gateway).await;
    let (new, new_text) = connect(&gateway).await;
    let old_request = old.clone();
    let active = tokio::spawn(async move {
        old_request
            .receive(request("old", "s", "r1"))
            .await
            .unwrap();
    });
    bounded(session.entered.wait()).await;
    let previous = old_text.frames()[0]["interactionBinding"].clone();
    bounded(
        new.receive(
            json!({"type":"request","id":"reconnect","method":"reconnect_interaction",
        "params":{"sessionKey":"s","previousBinding":previous}})
            .to_string(),
        ),
    )
    .await
    .unwrap();
    let reconnect = new_text
        .frames()
        .into_iter()
        .find(|v| v["id"] == "reconnect")
        .unwrap();
    assert_eq!(
        reconnect["result"]["outcome"], "reconnected",
        "{reconnect:?}"
    );
    assert_eq!(
        reconnect["result"]["binding"],
        new_text.frames()[0]["interactionBinding"]
    );
    bounded(new.receive(request("new", "s", "r2")))
        .await
        .unwrap();
    assert!(
        new_text
            .frames()
            .iter()
            .any(|v| v["event"]["code"] == "session_busy")
    );
    assert!(!active.is_finished());
    assert_eq!(session.aborts.load(Ordering::SeqCst), 0);
    bounded(old.closed()).await.unwrap();
    bounded(active).await.unwrap();
    assert_eq!(session.aborts.load(Ordering::SeqCst), 1);
    assert_eq!(session.submits.load(Ordering::SeqCst), 1);
    assert_completed(&path, "s", "r1", &old_text.frames());
    bounded(new.closed()).await.unwrap();
}

#[tokio::test]
async fn native_observers_reject_frame_event_sequence_and_result_corruption() {
    let scratch = Scratch::new("a52-observer-controls");
    let path = scratch.path().join("runs.json");
    let session = Arc::new(Session::default());
    let gateway = gateway(&path, HashMap::from([("s".into(), session.clone())]));
    let (client, text) = connect(&gateway).await;
    session.release.open();
    bounded(client.receive(request("a", "s", "r")))
        .await
        .unwrap();
    let frames = text.frames();
    assert_completed(&path, "s", "r", &frames);
    bounded(client.closed()).await.unwrap();
    drop(client);
    drop(gateway);
    let original = fs::read(&path).unwrap();
    for rule in ObserverRule::ALL {
        let mut damaged_frames = frames.clone();
        let mut disk: Value = serde_json::from_slice(&original).unwrap();
        match rule {
            ObserverRule::LiveSequence => {
                damaged_frames.retain(|frame| frame["event"]["type"] != "input_accepted")
            }
            ObserverRule::WireSequence => {
                damaged_frames
                    .iter_mut()
                    .find(|frame| frame["type"] == "event")
                    .unwrap()["seq"] = json!(99);
            }
            ObserverRule::FinalReplica => {
                damaged_frames.retain(|frame| frame["type"] != "event" || frame["final"] != true);
            }
            ObserverRule::FinishReason => {
                damaged_frames
                    .iter_mut()
                    .find(|frame| frame["event"]["type"] == "turn_completed")
                    .unwrap()["event"]["finishReason"] = json!("error");
            }
            ObserverRule::RunId => {
                damaged_frames
                    .iter_mut()
                    .find(|frame| frame["event"]["type"] == "turn_started")
                    .unwrap()["event"]["runId"] = json!("wrong");
            }
            ObserverRule::RegistryEvents => {
                disk["events"]["\0s\0r"].as_array_mut().unwrap().remove(1);
            }
            ObserverRule::RegistrySequence => disk["events"]["\0s\0r"][0]["seq"] = json!(99),
            ObserverRule::RegistryLastSeq => disk["records"][0]["lastSeq"] = json!(99),
            ObserverRule::RegistryRevision => disk["records"][0]["revision"] = json!(99),
            ObserverRule::RegistryResult => {
                disk["records"][0]["result"] = json!({"type":"error","code":"stale"})
            }
            ObserverRule::RegistryState => disk["records"][0]["state"] = json!("accepted"),
            ObserverRule::RegistryRaw => disk["records"][0]["runId"] = json!("tampered"),
        }
        fs::write(&path, serde_json::to_vec(&disk).unwrap()).unwrap();
        let rejected =
            std::panic::catch_unwind(|| assert_completed(&path, "s", "r", &damaged_frames))
                .expect_err("corrupted observer must be rejected");
        let message = rejected
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| rejected.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            message.contains(rule.label()),
            "{rule:?}: unexpected failure instead of oracle rejection: {message}"
        );
    }
    fs::write(&path, &original).unwrap();
    assert_completed(&path, "s", "r", &frames);
}

#[test]
fn registry_failed_write_does_not_publish_uncommitted_snapshot_and_can_retry() {
    let scratch = Scratch::new("a52-registry-failure");
    let parent = scratch.path().join("store");
    let saved = scratch.path().join("saved");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("runs.json");
    let registry = FileRunRegistry::open(&path).unwrap();
    let input = RunAcceptInput {
        run: run("s", "r"),
        request_material: "request".into(),
    };
    registry.accept(&input).unwrap();
    let before = fs::read(&path).unwrap();
    fs::rename(&parent, &saved).unwrap();
    fs::write(&parent, b"not a directory").unwrap();
    let event = js(json!({"type":"error","runId":"r","code":"turn_timeout"}));
    let error = registry.append(&input.run, &event).unwrap_err();
    assert!(
        matches!(error, pilotdeck_gateway::storage::RunRegistryError::Io(_)),
        "{error:?}"
    );
    assert_eq!(fs::read(saved.join("runs.json")).unwrap(), before);
    fs::remove_file(&parent).unwrap();
    fs::rename(&saved, &parent).unwrap();
    let fresh = FileRunRegistry::open(&path).unwrap();
    let record = fresh.get(&input.run).unwrap().unwrap();
    assert_eq!(record.state(), Some(RunState::Accepted));
    assert_eq!(record.last_seq(), Some(0.0));
    assert!(record.result().is_none());
    assert!(fresh.events(&input.run, None, None).unwrap().is_empty());
    fresh.append(&input.run, &event).unwrap();
    assert_eq!(
        fresh.get(&input.run).unwrap().unwrap().result(),
        Some(&event)
    );
}

#[test]
fn snapshot_store_failure_is_observable_and_not_cached_as_success() {
    let scratch = Scratch::new("a52-snapshot-failure");
    let workspace = scratch.path().join("workspace");
    let store = scratch.path().join("snapshots");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("evidence.txt"), "original").unwrap();
    fs::write(&store, "blocks snapshot directory").unwrap();
    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(WorkspaceSnapshotConfig {
        root: store.to_str().unwrap().into(),
        snapshot_root: store.to_str().unwrap().into(),
    });
    let input = WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "r".into(),
        run_id: "r".into(),
        workspace_dir: workspace.to_str().unwrap().into(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    assert!(recorder.capture_pre_user(&input).is_err());
    assert_eq!(
        fs::read_to_string(&store).unwrap(),
        "blocks snapshot directory"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("evidence.txt")).unwrap(),
        "original"
    );
    fs::remove_file(&store).unwrap();
    fs::create_dir(&store).unwrap();
    let result = recorder.capture_pre_user(&input).unwrap();
    assert_eq!(result.state, SnapshotState::Committed);
    assert!(result.error.is_none());
    let manifest = result.manifest_path.unwrap();
    assert!(std::path::Path::new(&manifest).exists());
    assert!(
        std::path::Path::new(&manifest)
            .parent()
            .unwrap()
            .join("_COMMITTED")
            .exists()
    );
}

#[test]
fn cold_restart_fences_accepted_and_running_without_inventing_unknown_result() {
    let scratch = Scratch::new("a52-cold-restart");
    for initial in ["accepted", "running", "unknown"] {
        let path = scratch.path().join(format!("{initial}.json"));
        let stdout = scratch.path().join(format!("{initial}.stdout"));
        let stderr = scratch.path().join(format!("{initial}.stderr"));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cold_restart_child", "--nocapture"])
            .env("PDRS_A52_CHILD_PATH", &path)
            .env("PDRS_A52_CHILD_STATE", initial)
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        // Poll only for process termination; the clock never selects a passing
        // interleaving. Expiry kills/reaps this exact child and fails the test.
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                child.wait().unwrap();
                panic!(
                    "cold child {pid} did not exit: {}",
                    fs::read_to_string(&stderr).unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "{}\n{}",
            fs::read_to_string(&stdout).unwrap(),
            fs::read_to_string(&stderr).unwrap()
        );
        let before: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let old = &before["records"][0];
        assert_eq!(old["ownerPid"], pid);
        assert_ne!(pid, std::process::id());
        assert_eq!(
            old["state"],
            if initial == "unknown" {
                "interrupted"
            } else {
                initial
            }
        );
        let fresh = FileRunRegistry::open(&path).unwrap();
        let record = fresh.get(&run("s", "r")).unwrap().unwrap();
        assert_eq!(record.state(), Some(RunState::Interrupted));
        assert_eq!(record.last_seq(), old["lastSeq"].as_f64());
        assert_eq!(
            record.revision(),
            Some(old["revision"].as_f64().unwrap() + if initial == "unknown" { 0.0 } else { 1.0 })
        );
        assert_eq!(record.result().map(value), old.get("result").cloned());
        let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(before["events"], after["events"]);
        assert_eq!(
            events(&path, "s", "r"),
            before["events"]["\0s\0r"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        );
        assert!(matches!(
            fresh.append(&run("s", "r"), &js(json!({"type":"error","code":"late"}))),
            Err(pilotdeck_gateway::storage::RunRegistryError::Terminal(_))
        ));
    }
}

#[test]
fn cold_restart_child() {
    let Some(path) = std::env::var_os("PDRS_A52_CHILD_PATH") else {
        return;
    };
    let registry = FileRunRegistry::open(path).unwrap();
    let input = RunAcceptInput {
        run: run("s", "r"),
        request_material: "request".into(),
    };
    registry.accept(&input).unwrap();
    match std::env::var("PDRS_A52_CHILD_STATE").unwrap().as_str() {
        "accepted" => {}
        "running" => registry
            .append(&input.run, &js(json!({"type":"turn_started","runId":"r"})))
            .unwrap(),
        "unknown" => registry
            .append(
                &input.run,
                &js(json!({"type":"error","runId":"r","code":"result_unknown"})),
            )
            .unwrap(),
        other => panic!("unknown initial state {other}"),
    }
}
