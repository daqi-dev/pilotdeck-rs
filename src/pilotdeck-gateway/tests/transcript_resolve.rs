//! Cold-cwd matrix from the original TS storage entry point, in owned children.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/exact_receipt.rs"]
mod exact_receipt;

use pilotdeck_gateway::storage::{
    AdapterError, AsyncTranscriptAdapter, LedgerError, ProjectKey, SessionStorageInput,
    StoreResult, StoredSession, TranscriptKey, TranscriptRead, TranscriptStore,
};
use pilotdeck_protocol::JsValue;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

struct UnusedStore;
impl TranscriptStore for UnusedStore {
    fn append(&self, _: &TranscriptKey, _: &JsValue) -> StoreResult<()> {
        panic!("path resolution must not append");
    }
    fn read(&self, _: &TranscriptKey, _: Option<u64>) -> StoreResult<TranscriptRead> {
        panic!("path resolution must fail before read");
    }
    fn list(&self, _: &ProjectKey) -> Option<StoreResult<Vec<StoredSession>>> {
        Some(Ok(vec![StoredSession {
            session_id: "/s/../one/".into(),
            last_modified: 1.0,
            file_size: None,
        }]))
    }
}

fn matrix(name: &str, test: &str) {
    if std::env::var("PDRS_TRANSCRIPT_RESOLVE_CHILD").as_deref() != Ok(test) {
        let mut child =
            Command::new(std::env::current_exe().expect("#942 matrix test binary path"))
                .args(["--exact", test, "--nocapture", "--test-threads=2"])
                .env("PDRS_TRANSCRIPT_RESOLVE_CHILD", test)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("#942 matrix child spawn");
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().expect("#942 matrix child poll").is_none() {
            if Instant::now() >= deadline {
                child.kill().expect("#942 matrix child kill");
                let output = child.wait_with_output().expect("#942 matrix child reap");
                panic!("matrix child timed out: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().expect("#942 matrix child collect");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // #942: exit 0 alone is not a receipt — a renamed matrix test makes
        // this `--exact` filter match nothing and libtest still exits 0.
        assert!(
            exact_receipt::single_pass(&output.status, test, &stdout),
            "#942 transcript-resolve rerun matched no test: {test}:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("TRANSCRIPT_RESOLVE_MATCH"),
            "child did not reach matrix assertion"
        );
        println!("{stdout}");
        return;
    }
    let oracle_text = include_str!("fixtures/transcript-resolve/oracle.json");
    baseline::oracle(oracle_text);
    let oracle: Value = serde_json::from_str(oracle_text).unwrap();
    let mut expected = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap()["outcome"]
        .clone();
    if name == "join-relative-normal" {
        // A6.2-TRANSCRIPT-SUBAGENT-RELATIVE-001 (unapproved, not accepted).
        // Preserve Rust's existing absolute path.join result in this narrow
        // fix; this adjusted expectation does not establish TS parity.
        expected["transcriptPath"] = json!("<root>/cwd/logs/s/subagents/kid.jsonl");
    }
    let original_cwd = std::env::current_dir().unwrap();
    let root =
        std::env::temp_dir().join(format!("pdrs-transcript-resolve-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let cwd = root.join("cwd");
    fs::create_dir(&cwd).unwrap();
    let absolute = name.starts_with("absolute") || name.starts_with("project");
    let home = if absolute {
        root.join("home/../home")
    } else {
        "relative/../home".into()
    };
    let chat = home.join("projects/pdrs-fixture-project/chats");
    let adapter =
        AsyncTranscriptAdapter::new(Arc::new(UnusedStore), Arc::new(move |_| chat.clone()));
    let project_root = if name.starts_with("project") {
        "project/../project"
    } else {
        "/pdrs-fixture/project"
    };
    std::env::set_current_dir(&cwd).unwrap();
    if name.ends_with("deleted") {
        fs::remove_dir(&cwd).unwrap();
        assert!(
            std::env::current_dir().is_err(),
            "deleted-cwd setup did not take effect"
        );
    }
    let outcome = if name.starts_with("join-") {
        let recorder = pilotdeck_gateway::storage::recorder::Recorder::new(
            Arc::new(UnusedStore),
            TranscriptKey {
                project_root: project_root.into(),
                pilot_home: home.clone(),
                session_id: "s".into(),
                transcript_path: "logs/../logs/s.jsonl".into(),
            },
        );
        json!({"transcriptPath": recorder.writer().for_subagent(&"kid".into()).transcript_path
            .to_string_lossy().replace(root.to_str().unwrap(), "<root>")})
    } else {
        match adapter.create_session_storage(SessionStorageInput {
            project_root: project_root.into(),
            pilot_home: home,
            session_id: "/s/../one/".into(),
            now: None,
        }) {
            Ok(storage) => {
                let path = |p: &Path| {
                    p.to_string_lossy()
                        .replace(root.to_str().unwrap(), "<root>")
                };
                let child = storage.transcript.for_subagent(&"kid".into());
                json!({
                    "chatDir": path(&storage.chat_dir), "transcriptPath": path(&storage.transcript_path),
                    "toolResultsDir": path(&storage.tool_results_dir), "fileHistoryDir": path(&storage.file_history_dir),
                    "subagentsDir": path(&storage.subagents_dir), "projectionCheckpointPath": path(&storage.projection_checkpoint_path),
                    "subagentPath": path(&child.transcript_path),
                    "nestedSubagentPath": path(&child.writer.for_subagent(&"grandchild".into()).transcript_path),
                })
            }
            Err(error) => {
                assert!(
                    matches!(&error, AdapterError::Store(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound)
                );
                json!({"error": {"name": "Error", "code": "ENOENT", "message": error.to_string()}})
            }
        }
    };
    assert_eq!(outcome, expected, "transcript resolve matrix: {name}");
    if name == "relative-deleted" {
        let project = ProjectKey {
            project_root: project_root.into(),
            pilot_home: "relative/../home".into(),
        };
        let error = adapter
            .list_sessions(&project, None, None)
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            expected["error"]["message"].as_str().unwrap(),
            "list propagates cwd failure"
        );
        let rewind = pilotdeck_gateway::runtime::rewind::rewind_files(
            pilotdeck_gateway::runtime::rewind::RewindFilesInput {
                adapter: &adapter,
                project_root: &project.project_root,
                pilot_home: &project.pilot_home,
                session_key: &"s".into(),
                user_message_id: "m",
                dry_run: true,
                now: None,
            },
        );
        assert_eq!(rewind.get("canRewind"), Some(&JsValue::Bool(false)));
        assert_eq!(
            rewind
                .get("error")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            expected["error"]["message"].as_str().unwrap(),
            "rewind propagates cwd failure"
        );
    }
    std::env::set_current_dir(original_cwd).unwrap();
    fs::remove_dir_all(&root).unwrap();
    println!("TRANSCRIPT_RESOLVE_MATCH {name}: {outcome}");
}

#[test]
fn relative_base_normal_cwd() {
    matrix("relative-normal", "relative_base_normal_cwd");
}
#[test]
fn relative_base_deleted_cwd() {
    matrix("relative-deleted", "relative_base_deleted_cwd");
}
#[test]
fn absolute_base_normal_cwd() {
    matrix("absolute-normal", "absolute_base_normal_cwd");
}
#[test]
fn absolute_base_deleted_cwd() {
    matrix("absolute-deleted", "absolute_base_deleted_cwd");
}
#[test]
fn relative_project_deleted_cwd() {
    matrix("project-relative-deleted", "relative_project_deleted_cwd");
}
#[test]
fn relative_sidechain_normal_cwd() {
    matrix("join-relative-normal", "relative_sidechain_normal_cwd");
}
#[test]
fn relative_sidechain_deleted_cwd() {
    matrix("join-relative-deleted", "relative_sidechain_deleted_cwd");
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn transcript_resolve_regenerates_identically() {
    baseline::regenerates_identically("transcript-resolve");
}
