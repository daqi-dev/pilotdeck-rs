//! Real bounded native session requests; parent owns the hang deadline.
use super::*;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn native_session_io_fifo_subprocess() {
    for (case, method) in [
        ("lock-fifo", "rename_session"),
        ("lock-symlink", "rename_session"),
        ("lock-fifo", "tag_session"),
        ("lock-symlink", "tag_session"),
        ("lock-fifo", "record_agent_status_message"),
        ("lock-symlink", "record_agent_status_message"),
        ("marker", "rename_session"),
        ("marker", "tag_session"),
        ("marker", "record_agent_status_message"),
        ("marker", "delete_session"),
        ("marker", "rewind_files"),
        ("marker", "list_sessions"),
        ("marker", "manager_sessions"),
        ("transcript", "rename_session"),
        ("transcript", "tag_session"),
        ("transcript", "record_agent_status_message"),
        ("transcript", "rewind_files"),
        ("transcript", "list_sessions"),
        ("transcript", "manager_sessions"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture", "native_session_io_child"])
            .env("PDRS_SESSION_IO_CASE", case)
            .env("PDRS_SESSION_IO_METHOD", method)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    status.success(),
                    "{case}/{method}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                    "{case}/{method}: missing child receipt"
                );
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{case}/{method}: blocked: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "run by bounded parent"]
async fn native_session_io_child() {
    let case = std::env::var("PDRS_SESSION_IO_CASE").unwrap();
    let method = std::env::var("PDRS_SESSION_IO_METHOD").unwrap();
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let chat = crate::session_chat_dir(
        &requests.options.home,
        &requests.options.project,
        &requests.options.project.to_string_lossy(),
    );
    std::fs::create_dir_all(&chat).unwrap();
    let path = if case == "marker" {
        let dir = requests.options.home.join("projects/000-unrelated");
        std::fs::create_dir(&dir).unwrap();
        dir.join(".cwd")
    } else if case.starts_with("lock-") {
        chat.join("bounded-session.jsonl.lock")
    } else {
        chat.join("bounded-session.jsonl")
    };
    let target = requests.options.home.join("outside-lock-target");
    if case == "lock-symlink" {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &path).unwrap();
        #[cfg(not(unix))]
        panic!("native bounded lock requires Unix");
    } else {
        assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
    }
    let params = object([
        (
            "projectKey",
            requests.options.project.to_string_lossy().as_ref().into(),
        ),
        ("sessionKey", "bounded-session".into()),
        ("turnId", "turn".into()),
        ("userMessageId", "user".into()),
        ("value", "fixture".into()),
    ]);
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        if method == "rewind_files" {
            requests.tools_request(method.clone(), params).await
        } else if method == "manager_sessions" {
            requests
                .management_request(&method, &params)
                .await
                .expect("management method")
        } else {
            requests.session_dir_request(method.clone(), params).await
        }
    })
    .await
    .expect("session request blocked");
    if case == "marker" {
        if method == "manager_sessions" {
            assert_eq!(
                result
                    .expect_err(
                        "native manager preserves unsafe marker refusal: unsafe marker accepted"
                    )
                    .message,
                JsString::from("Unsafe project marker."),
                "native manager preserves unsafe marker refusal"
            );
        } else {
            assert_eq!(
                result
                    .expect_err("session route unsafe marker accepted")
                    .message,
                JsString::from("Unsafe project marker."),
                "native session route preserves unsafe marker refusal"
            );
        }
    } else if method == "rewind_files" {
        assert_eq!(
            result.unwrap().get("canRewind"),
            Some(&JsValue::Bool(false))
        );
    } else if method == "record_agent_status_message" {
        // #1873: a refused cold append is a storage failure, never a successful
        // recorded:false response or the retryable writer-contention answer.
        let error = result.expect_err("unsafe cold status append must fail");
        assert_eq!(error.code, "gateway_request_failed");
        if case == "lock-fifo" {
            assert_eq!(
                error.message,
                JsString::from("Native session ledger lock is not a regular file.")
            );
        } else if case == "lock-symlink" {
            assert!(
                error
                    .message
                    .to_utf8()
                    .unwrap()
                    .contains("Could not open session ledger lock"),
                "cold status must preserve the lock-open diagnostic: {error:?}"
            );
        } else {
            let message = error.message.to_utf8().unwrap();
            assert!(
                message.contains("transcript") && message.contains("not a regular file"),
                "cold status must preserve the transcript refusal: {error:?}"
            );
        }
    } else {
        assert!(result.is_err(), "{method}: unsafe transcript accepted");
    }
    assert!(!target.exists(), "cold lease followed the lock symlink");
    std::fs::remove_file(path).unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}

#[test]
fn native_session_rename_cancel_keeps_write_fence_subprocess() {
    let dir = std::env::temp_dir().join(format!("pdrs-session-cancel-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("stimulus.json"),
        r#"{"writer":{"barrier":{"id":"hold","key":"session_metadata","boundMs":8000}}}"#,
    )
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--nocapture",
            "native_session_rename_cancel_child",
        ])
        .env("PILOTDECK_NATIVE_STORAGE_STIMULUS", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("child blocked: {}", String::from_utf8_lossy(&output.stderr));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "run by bounded parent"]
async fn native_session_rename_cancel_child() {
    let control =
        std::path::PathBuf::from(std::env::var("PILOTDECK_NATIVE_STORAGE_STIMULUS").unwrap());
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let key = JsString::from("cancel-rename");
    let params = object([
        (
            "projectKey",
            requests.options.project.to_string_lossy().as_ref().into(),
        ),
        ("sessionKey", JsValue::String(key.clone())),
        ("value", "first-retained".into()),
    ]);
    let clone = requests.clone();
    let caller = tokio::spawn(async move {
        clone
            .session_dir_request("rename_session".into(), params)
            .await
    });
    let seq = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(files) = std::fs::read_dir(control.join("witness")) {
                for file in files.flatten() {
                    if let Ok(bytes) = std::fs::read(file.path())
                        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && value["phase"] == "block"
                    {
                        return value["seq"].as_u64().unwrap();
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual append barrier was not reached");
    caller.abort();
    let _ = caller.await;
    let chat = crate::session_chat_dir(
        &requests.options.home,
        &requests.options.project,
        &requests.options.project.to_string_lossy(),
    );
    let path = chat.join("cancel-rename.jsonl");
    let fence_key = crate::storage::canonical_transcript_path(&path).unwrap();
    let mut fence = tokio::spawn(async move { transcript_locks::acquire_key(fence_key).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut fence)
            .await
            .is_err(),
        "aborted caller released the fence while actual append remained blocked"
    );
    std::fs::write(control.join("release.json"), format!(r#"{{"seq":{seq}}}"#)).unwrap();
    let guard = tokio::time::timeout(Duration::from_secs(5), fence)
        .await
        .unwrap()
        .unwrap();
    drop(guard);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("first-retained"),
        "accepted write lost after cancellation"
    );
    let second = object([
        (
            "projectKey",
            requests.options.project.to_string_lossy().as_ref().into(),
        ),
        ("sessionKey", JsValue::String(key)),
        ("value", "second-after-release".into()),
    ]);
    requests
        .session_dir_request("rename_session".into(), second)
        .await
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("first-retained") && text.contains("second-after-release"));
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_catalog_real_seeded_session_matches_legacy_projection() {
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let home = requests.options.home.clone();
    let project = requests.options.project.clone();
    let chat = bounded_chat_dir(&home, &project).unwrap();
    let old = crate::runtime::manager::file_catalog::list_sessions(&chat, &project, false).unwrap();
    let new = crate::runtime::manager::file_catalog::list_sessions_bounded(&chat, &project, false)
        .unwrap();
    assert!(
        !old.is_empty(),
        "fixture must exercise a real catalog record"
    );
    assert_eq!(old, new);
    let list = requests
        .session_dir_request("list_sessions".into(), object([]))
        .await
        .unwrap();
    assert!(matches!(list.get("sessions"), Some(JsValue::Array(items)) if !items.is_empty()));
    let manager = requests
        .management_request("manager_sessions", &object([]))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(manager.get("items"), Some(JsValue::Array(items)) if !items.is_empty()));
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_catalog_rejects_aggregate_stream_budget_without_hanging() {
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let project = &requests.options.project;
    let chat = bounded_chat_dir(&requests.options.home, project).unwrap();
    let huge = std::fs::File::create(chat.join("huge.jsonl")).unwrap();
    huge.set_len(129 * 1024 * 1024).unwrap();
    drop(huge);
    let error = crate::runtime::manager::file_catalog::list_sessions_bounded(&chat, project, false)
        .expect_err("stream budget must reject oversized fallback");
    assert!(error.equals_str("Native session catalog byte limit exceeded."));
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}

#[test]
fn session_marker_limits_fail_closed_before_repeated_io() {
    use crate::metadata::Files;
    let root = std::env::temp_dir().join(format!("pdrs-marker-limit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("item"), b"x").unwrap();
    let files = BoundedSessionMarkers::new();
    files.entries.set(16_384);
    assert!(files.entries(&root).is_err());
    assert!(files.was_refused());
    assert!(files.read(&root.join("item")).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

/// A relative marker must resolve to one identity for native factory and cold writers.
/// Each case runs in a child because cwd is process-global and one case deletes it.
#[test]
fn relative_marker_writer_identity_disagreement_subprocess() {
    for case in ["different-cwd", "deleted-cwd"] {
        let root = std::env::temp_dir().join(format!(
            "pdrs-fence-identity-{}-{case}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        let legacy = home.join("projects/000-fixture");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(project.join("project")).unwrap();
        let marker: &[u8] = if case == "different-cwd" {
            b"project"
        } else {
            b"../project"
        };
        std::fs::write(legacy.join(".cwd"), marker).unwrap();
        let cwd = if case == "different-cwd" {
            root.clone()
        } else {
            let cwd = root.join("vanishing-cwd");
            std::fs::create_dir(&cwd).unwrap();
            cwd
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--nocapture",
                "relative_marker_writer_identity_child",
            ])
            .current_dir(&cwd)
            .env("PDRS_IDENTITY_ROOT", &root)
            .env("PDRS_IDENTITY_CASE", case)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    status.success(),
                    "{case}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{case}: blocked: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore = "run by bounded parent"]
fn relative_marker_writer_identity_child() {
    let root = std::path::PathBuf::from(std::env::var_os("PDRS_IDENTITY_ROOT").unwrap());
    let case = std::env::var("PDRS_IDENTITY_CASE").unwrap();
    let home = root.join("home");
    let project = root.join("project");
    let options = crate::native_process::Options {
        home: home.clone(),
        project: project.clone(),
        listen: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        token_file: root.join("unused-token"),
        sidecar: root.join("unused-sidecar"),
        sidecar_args: Vec::new(),
        drain_budget: Duration::from_secs(1),
        storage_root: None,
    };
    // Exercise real startup registration first, while cwd is still valid.
    crate::native_process::assembly::register_project(&options).unwrap();
    if case == "deleted-cwd" {
        std::fs::remove_dir(root.join("vanishing-cwd")).unwrap();
        assert!(std::env::current_dir().is_err());
    }
    let request = bounded_chat_dir(&home, &project).unwrap();
    // This is the same native bounded resolver used by the RecordingPlan;
    // host-probe exercises the plan and recorder with a real model turn.
    let assembly = bounded_chat_dir_at(&home, &project, std::path::Path::new("")).unwrap();
    let legacy = home.join("projects/000-fixture/chats");
    if case == "different-cwd" {
        assert_eq!(request, legacy);
    } else {
        assert_ne!(request, legacy);
    }
    let request_file = request.join("same-session.jsonl");
    let assembly_file = assembly.join("same-session.jsonl");
    let request_key = crate::storage::canonical_transcript_path(&request_file).unwrap();
    let assembly_key = crate::storage::canonical_transcript_path(&assembly_file).unwrap();
    assert_eq!(
        request_key, assembly_key,
        "native factory and cold writer must share a fence"
    );
}
