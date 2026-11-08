//! Real bounded native session requests; parent owns the hang deadline.
use super::*;
use std::{process::{Command, Stdio}, time::{Duration, Instant}};

#[test]
fn native_session_io_fifo_subprocess() {
    for (case, method) in [
        ("marker", "rename_session"), ("marker", "tag_session"),
        ("marker", "record_agent_status_message"), ("marker", "delete_session"),
        ("marker", "rewind_files"),
        ("transcript", "rename_session"), ("transcript", "tag_session"),
        ("transcript", "record_agent_status_message"), ("transcript", "rewind_files"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture", "native_session_io_child"])
            .env("PDRS_SESSION_IO_CASE", case).env("PDRS_SESSION_IO_METHOD", method)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(status.success(), "{case}/{method}: {}", String::from_utf8_lossy(&output.stderr));
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                    "{case}/{method}: missing child receipt");
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!("{case}/{method}: blocked: {}", String::from_utf8_lossy(&output.stderr));
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
        &requests.options.home, &requests.options.project, &requests.options.project.to_string_lossy()
    );
    std::fs::create_dir_all(&chat).unwrap();
    let path = if case == "marker" {
        let dir = requests.options.home.join("projects/000-unrelated");
        std::fs::create_dir(&dir).unwrap();
        dir.join(".cwd")
    } else {
        chat.join("bounded-session.jsonl")
    };
    assert!(Command::new("/usr/bin/mkfifo").arg(&path).status().unwrap().success());
    let params = object([
        ("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", "bounded-session".into()), ("turnId", "turn".into()),
        ("userMessageId", "user".into()), ("value", "fixture".into()),
    ]);
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        if method == "rewind_files" {
            requests.tools_request(method.clone(), params).await
        } else {
            requests.session_dir_request(method.clone(), params).await
        }
    }).await.expect("session request blocked");
    if case == "marker" {
        assert!(result.is_err(), "{method}: unsafe marker accepted");
    } else if method == "rewind_files" {
        assert_eq!(result.unwrap().get("canRewind"), Some(&JsValue::Bool(false)));
    } else if method == "record_agent_status_message" {
        assert_eq!(result.unwrap().get("recorded"), Some(&JsValue::Bool(false)));
    } else {
        assert!(result.is_err(), "{method}: unsafe transcript accepted");
    }
    std::fs::remove_file(path).unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}

#[test]
fn native_session_rename_cancel_keeps_write_fence_subprocess() {
    let dir = std::env::temp_dir().join(format!("pdrs-session-cancel-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("stimulus.json"),
        r#"{"writer":{"barrier":{"id":"hold","key":"session_metadata","boundMs":8000}}}"#).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--nocapture", "native_session_rename_cancel_child"])
        .env("PILOTDECK_NATIVE_STORAGE_STIMULUS", &dir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(status.success(), "{}", String::from_utf8_lossy(&output.stderr));
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
    let control = std::path::PathBuf::from(std::env::var("PILOTDECK_NATIVE_STORAGE_STIMULUS").unwrap());
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let key = JsString::from("cancel-rename");
    let params = object([
        ("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", JsValue::String(key.clone())), ("value", "first-retained".into()),
    ]);
    let clone = requests.clone();
    let caller = tokio::spawn(async move { clone.session_dir_request("rename_session".into(), params).await });
    let seq = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(files) = std::fs::read_dir(control.join("witness")) {
                for file in files.flatten() {
                    if let Ok(bytes) = std::fs::read(file.path())
                        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && value["phase"] == "block"
                    { return value["seq"].as_u64().unwrap(); }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("actual append barrier was not reached");
    caller.abort();
    let _ = caller.await;
    let chat = crate::session_chat_dir(&requests.options.home, &requests.options.project,
        &requests.options.project.to_string_lossy());
    let path = chat.join("cancel-rename.jsonl");
    let fence_key = crate::storage::canonical_transcript_path(&path).unwrap();
    let mut fence = tokio::spawn(async move { transcript_locks::acquire_key(fence_key).await });
    assert!(tokio::time::timeout(Duration::from_millis(200), &mut fence).await.is_err(),
        "aborted caller released the fence while actual append remained blocked");
    std::fs::write(control.join("release.json"), format!(r#"{{"seq":{seq}}}"#)).unwrap();
    let guard = tokio::time::timeout(Duration::from_secs(5), fence).await.unwrap().unwrap();
    drop(guard);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("first-retained"), "accepted write lost after cancellation");
    let second = object([
        ("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", JsValue::String(key)), ("value", "second-after-release".into()),
    ]);
    requests.session_dir_request("rename_session".into(), second).await.unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("first-retained") && text.contains("second-after-release"));
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
