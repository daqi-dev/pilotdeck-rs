//! Exercise request dumping through the real OpenAI and Google send paths.
//!
//! Each case runs in its own subprocess (no unsafe `set_var`/`remove_var`)
//! against a real mock transport, so `PILOTDECK_DUMP_REQUEST` is set exactly
//! for the code under test. The child's whole temp directory is an
//! exclusively acquired scratch directory of this run, so the parent can
//! prove dumping from the filesystem — it counts `pilotdeck_request_*`
//! entries there — instead of inferring it from the child's logs, and no
//! other process's temp entries are ever read or removed. The parent
//! verifies the exact dump paths the child reported and removes only the
//! scratch directory it created itself.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_gateway::runtime::model::http::{
    ModelHttpTransport, StreamOptions, StreamOutput, TransportFailure, TransportFuture,
    stream_model,
};
use pilotdeck_protocol::parse_json;
use serde_json::{Value, json};
use std::process::Command;

#[derive(Default)]
struct CaptureTransport(std::sync::Arc<std::sync::Mutex<Vec<Value>>>);

impl ModelHttpTransport for CaptureTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let bodies = self.0.clone();
        TransportFuture::io(async move {
            let bytes = request.into_body().collect().await.unwrap().to_bytes();
            bodies
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&bytes).unwrap());
            // Dumping happens before the exchange; no real model or network needed.
            Err(TransportFailure::fetch_failed(Some("ECONNREFUSED")))
        })
    }
}

/// A temp directory this run acquired exclusively (fresh UUID name, so
/// nothing pre-existing can be under it; no predictable name is probed or
/// deleted) for one child case. Only what this guard created is removed.
struct CaseRoot(std::path::PathBuf);
impl CaseRoot {
    fn acquire(tag: &str) -> Self {
        for _ in 0..8 {
            let dir =
                std::env::temp_dir().join(format!("pdrs-dump-case-{tag}-{}", uuid::Uuid::new_v4()));
            // Exclusive create (no create_dir_all): a missing parent fails
            // loudly instead of reaching into an unexpected tree.
            if std::fs::create_dir(&dir).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                return Self(dir);
            }
            // A collision is not ours: try the next UUID, never delete it.
        }
        panic!("dump case: could not exclusively create a scratch temp directory");
    }
}
impl Drop for CaseRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl std::ops::Deref for CaseRoot {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The dump paths the child reported on stdout, in order. Parsing the child's
/// own output is the only source: the parent never guesses or globs paths.
fn reported_dump_paths(stdout: &str) -> Vec<std::path::PathBuf> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("[model-debug] Request dumped to ")?;
            let path = rest.strip_suffix(" (model=dump-model)")?;
            Some(std::path::PathBuf::from(path))
        })
        .collect()
}

/// The `pilotdeck_request_*` entries the child actually left in the exclusive
/// case directory: the closed-dump check counts files, not log lines.
fn dump_files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read case dir {dir:?}: {error}"))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("pilotdeck_request_"))
        .collect();
    names.sort();
    names
}

fn run_case(protocol: &str, enabled: Option<&str>) {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = CaseRoot::acquire("run");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "dump_child", "--ignored", "--nocapture"])
        .env("PDRS_DUMP_PROTOCOL", protocol)
        .env("PDRS_DUMP_MARKER", &marker)
        .env("PDRS_DUMP_MODE", "ok")
        // The child's temp dir is this case's exclusive scratch directory.
        .env("TMPDIR", &*root)
        .env_remove("PILOTDECK_DUMP_REQUEST");
    if let Some(value) = enabled {
        command.env("PILOTDECK_DUMP_REQUEST", value);
    }
    let output = command.output().expect("run isolated dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{protocol}/{enabled:?}:\n{stdout}\n{stderr}"
    );
    let sent: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("SENT_BODY="))
            .expect("child must exercise the transport"),
    )
    .unwrap();
    assert_eq!(sent["dumpMarker"], marker);

    // Closed-dump check: the filesystem of the child's own temp directory,
    // independent of anything the child printed.
    let files = dump_files_in(&root);
    let dumps = reported_dump_paths(&stdout);
    if enabled == Some("1") {
        assert_eq!(
            files.len(),
            1,
            "{protocol}/{enabled:?}: expected exactly one dump file in {:?}: {files:?}",
            &*root
        );
        assert_eq!(
            dumps.len(),
            1,
            "{protocol}/{enabled:?}: expected exactly one dump: {stdout}"
        );
        let path = &dumps[0];
        assert_eq!(
            path.parent(),
            Some(&*root),
            "{protocol}/{enabled:?}: dump escaped the case directory: {path:?}"
        );
        assert_eq!(path.file_name().unwrap().to_string_lossy(), files[0]);
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read child-reported dump {path:?}: {error}"));
        assert_eq!(serde_json::from_str::<Value>(&contents).unwrap(), sent);
        assert_eq!(contents, serde_json::to_string_pretty(&sent).unwrap());
    } else {
        assert!(
            dumps.is_empty(),
            "{protocol}/{enabled:?} wrote a dump: {dumps:?}"
        );
        assert!(
            files.is_empty(),
            "{protocol}/{enabled:?} left dump files: {files:?}"
        );
        assert!(!stdout.contains("[model-debug]"), "{stdout}");
    }
    assert!(!stderr.contains("[model-debug]"), "{stderr}");
}

#[test]
fn dump_request_creates_file() {
    for protocol in ["openai", "google"] {
        run_case(protocol, Some("1"));
    }
}

/// TS gates strictly on `=== "1"` (streamModel.ts:195, 394); anything else —
/// including "0" and "true" — must leave the dump path cold.
#[test]
fn dump_request_disabled_by_default() {
    for protocol in ["openai", "google"] {
        for value in [None, Some("0"), Some("true")] {
            run_case(protocol, value);
        }
    }
}

/// A failed dump write surfaces like TS (`streamModel.ts:196-203,395-401`,
/// probe evidence in `_tmp/20261025-issue-621-fix-r1/ts-probe/`): the child
/// runs with a temp directory that does not exist, so the exclusive create
/// fails with ENOENT.
///
/// Google: the SDK send never happens — zero transport calls — and the
/// attempt yields exactly one non-retryable `error` event with code ENOENT
/// and the `writeFileSync`-shaped raw fields. OpenAI: this port's
/// pre-existing (already in main) swallow-and-continue behavior — the send
/// proceeds and only stderr mentions the failure; registered in
/// A6.4-MODEL-DUMP-SECURE-001.
#[test]
fn dump_request_write_failure_semantics() {
    for protocol in ["google", "openai"] {
        let marker = format!(
            "dump-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = CaseRoot::acquire("write-fail");
        let missing_tmp = root.join("nonexistent-tmpdir");
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "dump_child", "--ignored", "--nocapture"])
            .env("PDRS_DUMP_PROTOCOL", protocol)
            .env("PDRS_DUMP_MARKER", &marker)
            .env("PDRS_DUMP_MODE", "write-fail")
            .env("PILOTDECK_DUMP_REQUEST", "1")
            .env("TMPDIR", &missing_tmp)
            .output()
            .expect("run isolated dump write-failure case");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{protocol} write-fail:\n{stdout}\n{stderr}"
        );
        assert!(
            dump_files_in(&root).is_empty(),
            "{protocol} write-fail must not write any dump file"
        );
        match protocol {
            "google" => {
                assert!(
                    !stdout.contains("SENT_BODY="),
                    "Google send must not run after a failed dump: {stdout}"
                );
                let code = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("ERROR_CODE="))
                    .unwrap_or_else(|| panic!("no error event reported: {stdout}"));
                assert_eq!(code, "ENOENT");
                let retryable = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("ERROR_RETRYABLE="))
                    .expect("error event must carry retryable");
                assert_eq!(retryable, "false");
                let message = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("ERROR_MESSAGE="))
                    .expect("error event must carry a message");
                assert!(
                    message.starts_with("ENOENT: no such file or directory, open '"),
                    "Node writeFileSync message shape: {message}"
                );
                assert!(
                    message.contains("pilotdeck_request_"),
                    "message names the dump path: {message}"
                );
                assert!(
                    !stderr.contains("[model-debug]"),
                    "Google path must surface, not swallow, the failure: {stderr}"
                );
            }
            "openai" => {
                assert!(
                    stdout.contains("SENT_BODY="),
                    "OpenAI pre-existing behavior sends on: {stdout}"
                );
                assert!(
                    stderr.contains("[model-debug] Failed to write request dump"),
                    "failure is still visible on stderr: {stderr}"
                );
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
#[ignore = "subprocess helper; run through the parent tests"]
async fn dump_child() {
    let protocol = std::env::var("PDRS_DUMP_PROTOCOL").unwrap();
    let marker = std::env::var("PDRS_DUMP_MARKER").unwrap();
    let mode = std::env::var("PDRS_DUMP_MODE").unwrap_or_else(|_| "ok".into());
    let request = parse_json(
        &json!({
            "provider": "dump-provider", "model": "dump-model",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "dump test"}]}]
        })
        .to_string(),
    )
    .unwrap();
    let config = parse_json(
        &json!({"providers": {"dump-provider": {
            "id": "dump-provider", "protocol": protocol,
            "url": "http://dump.invalid/v1", "apiKey": "test-only",
            "retry": {"streamMaxRetries": 0},
            "extraBody": {"dumpMarker": marker},
            "models": {"dump-model": {
                "id": "dump-model", "capabilities": {"supportsStreaming": true},
                "multimodal": {"input": ["text"]}
            }}
        }}})
        .to_string(),
    )
    .unwrap();
    let transport = CaptureTransport::default();
    let mut error_event: Option<pilotdeck_protocol::JsValue> = None;
    let result = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut |output| {
            if let StreamOutput::Event(event) = output
                && event
                    .get("type")
                    .and_then(|value| value.as_string())
                    .is_some_and(|kind| kind.equals_str("error"))
            {
                error_event = Some(event.clone());
            }
        },
    )
    .await;
    let bodies = transport.0.lock().unwrap();
    if mode == "write-fail" {
        // With a dead temp directory: Google never sends and yields one
        // error event; OpenAI (pre-existing port behavior) still sends.
        if protocol == "google" {
            assert!(result.is_ok(), "error event path completes: {result:?}");
            assert!(
                bodies.is_empty(),
                "Google SDK send must not run after a failed dump: {bodies:?}"
            );
            let error = error_event
                .as_ref()
                .and_then(|event| event.get("error"))
                .expect("Google yields an error event");
            let text = |field: &str| {
                error
                    .get(field)
                    .and_then(|value| value.as_string())
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default()
            };
            println!("ERROR_CODE={}", text("code"));
            println!(
                "ERROR_RETRYABLE={}",
                if error
                    .get("retryable")
                    .and_then(pilotdeck_protocol::JsValue::as_bool)
                    == Some(true)
                {
                    "true"
                } else {
                    "false"
                }
            );
            println!("ERROR_MESSAGE={}", text("message"));
        } else {
            assert_eq!(
                bodies.len(),
                1,
                "OpenAI pre-existing behavior still sends: {result:?}"
            );
            println!("SENT_BODY={}", bodies[0]);
        }
        return;
    }
    assert!(
        result.is_err() || error_event.is_some(),
        "transport failure must propagate as an error or a Google error event"
    );
    assert_eq!(
        bodies.len(),
        1,
        "must reach the provider send path once: {result:?}"
    );
    assert_eq!(bodies[0]["dumpMarker"], marker);
    println!("SENT_BODY={}", bodies[0]);
}
