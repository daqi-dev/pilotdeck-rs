//! Production session-model writers tested in a subprocess: a FIFO regression
//! must not hang the main test harness or the Tokio runtime at shutdown.
use super::*;
use std::{process::{Command, Stdio}, time::{Duration, Instant}};

#[test]
fn native_model_write_fifo_subprocess() {
    for case in ["marker", "transcript", "oversize", "regular"] {
        for method in ["session_model_set", "session_model_clear"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--nocapture", "native_model_write_child"])
                .env("PDRS_MODEL_WRITE_CASE", case).env("PDRS_MODEL_WRITE_METHOD", method)
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
                .spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    let output = child.wait_with_output().unwrap();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(status.success(), "{case}/{method}: {}", String::from_utf8_lossy(&output.stderr));
                    assert!(stdout.contains("1 passed; 0 failed"), "{case}/{method}: no single child test: {stdout}");
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!("{case}/{method}: blocked child: {}", String::from_utf8_lossy(&output.stderr));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "invoked by the bounded parent test"]
async fn native_model_write_child() {
    let case = std::env::var("PDRS_MODEL_WRITE_CASE").unwrap();
    let method = std::env::var("PDRS_MODEL_WRITE_METHOD").unwrap();
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let chat = crate::session_chat_dir(&requests.options.home, &requests.options.project,
        &requests.options.project.to_string_lossy());
    std::fs::create_dir_all(&chat).unwrap();
    let transcript = chat.join("bounded-write.jsonl");
    let mut fifo = None;
    match case.as_str() {
        "marker" => {
            let dir = requests.options.home.join("projects/000-unrelated");
            std::fs::create_dir(&dir).unwrap();
            fifo = Some(dir.join(".cwd"));
        }
        "transcript" => fifo = Some(transcript.clone()),
        "oversize" => {
            let file = std::fs::File::create(&transcript).unwrap();
            file.set_len(crate::storage::TRANSCRIPT_MAX_BYTES + 1).unwrap();
        }
        "regular" => {}
        _ => panic!("unknown test case"),
    }
    if let Some(path) = &fifo {
        assert!(Command::new("/usr/bin/mkfifo").arg(path).status().unwrap().success());
    }
    let params = object([
        ("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", "bounded-write".into()),
        ("selection", object([("mode", "model".into()), ("provider", "test".into()), ("model", "test".into())])),
    ]);
    let result = tokio::time::timeout(Duration::from_secs(4),
        requests.session_config_request(method, params)).await.expect("native model write blocked");
    if case == "regular" {
        result.expect("normal model write");
        assert!(std::fs::read_to_string(&transcript).unwrap().contains("modelSelection"));
    } else {
        result.expect_err("unsafe transcript or marker accepted");
    }
    if let Some(path) = fifo { std::fs::remove_file(path).unwrap(); }
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}


/// The real storage append is gated after admission. Cancelling the waiter
/// must neither release the reservation early nor discard the admitted write.
#[test]
fn native_model_write_cancellation_subprocess() {
    let dir = std::env::temp_dir().join(format!("pdrs-model-cancel-stimulus-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("stimulus.json"),
        r#"{"writer":{"barrier":{"id":"hold","key":"session_metadata","boundMs":8000}}}"#).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--nocapture", "native_model_cancel_child"])
        .env("PILOTDECK_NATIVE_STORAGE_STIMULUS", &dir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(status.success(), "child: {}", String::from_utf8_lossy(&output.stderr));
            assert!(stdout.contains("1 passed; 0 failed"), "expected one child: {stdout}");
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
#[ignore = "invoked by bounded parent"]
async fn native_model_cancel_child() {
    let control = PathBuf::from(std::env::var("PILOTDECK_NATIVE_STORAGE_STIMULUS").unwrap());
    let (_scratch, requests) = crate::native_process::management_tests::setup();
    let key = JsString::from("cancel-model");
    let params = object([
        ("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", JsValue::String(key.clone())),
        ("selection", object([("mode", "model".into()), ("provider", "test".into()), ("model", "test".into())])),
    ]);
    let clone = requests.clone();
    let waiter = tokio::spawn(async move {
        clone.session_config_request("session_model_set".into(), params).await
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
    }).await.expect("real storage barrier was not reached");
    waiter.abort();
    let _ = waiter.await;
    assert!(requests.replacement.has_transcript_write_reservation(&key), "reservation released while append blocked");
    let clear = object([("projectKey", requests.options.project.to_string_lossy().as_ref().into()),
        ("sessionKey", JsValue::String(key.clone()))]);
    let blocked = requests.session_config_request("session_model_clear".into(), clear.clone()).await
        .expect_err("conflicting clear admitted before append");
    assert_eq!(blocked.code, "SESSION_BUSY");
    std::fs::write(control.join("release.json"), format!(r#"{{"seq":{seq}}}"#)).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while requests.replacement.has_transcript_write_reservation(&key) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("reservation not released after write");
    let chat = crate::session_chat_dir(&requests.options.home, &requests.options.project,
        &requests.options.project.to_string_lossy());
    assert!(std::fs::read_to_string(chat.join("cancel-model.jsonl")).unwrap().contains("modelSelection"));
    requests.session_config_request("session_model_clear".into(), clear).await.unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
