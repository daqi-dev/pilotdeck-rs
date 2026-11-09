//! The helper process bounds failures even if a future regression blocks a FIFO.
use super::*;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
#[cfg(unix)]
fn native_model_fifo_subprocess_is_bounded() {
    for case in ["marker", "transcript", "config"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture", "native_model_fifo_child"])
            .env("PDRS_MODEL_FIFO_CASE", case)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    status.success(),
                    "{case}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    stdout.contains("1 passed; 0 failed"),
                    "{case}: child did not execute exactly one test: {stdout}"
                );
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{case}: child blocked: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "invoked by the bounded parent test"]
#[cfg(unix)]
async fn native_model_fifo_child() {
    let case = std::env::var("PDRS_MODEL_FIFO_CASE").unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-model-fifo-{}-{case}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let storage = home
        .join("projects")
        .join(crate::metadata::project_id(&project));
    let chats = storage.join("chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(storage.join(".cwd"), project.to_string_lossy().as_bytes()).unwrap();
    if case == "config" {
        let config = format!(
            "schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: fake\n      models: {{mock: {{}}}}\n# {}\n",
            "x".repeat(70_000)
        );
        std::fs::write(home.join("pilotdeck.yaml"), config).unwrap();
    } else {
        let path = if case == "marker" {
            let unrelated = home.join("projects").join("000-unrelated");
            std::fs::create_dir(&unrelated).unwrap();
            unrelated.join(".cwd")
        } else {
            chats.join("s.jsonl")
        };
        assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
    }
    let params = object([
        ("projectKey", project.to_string_lossy().as_ref().into()),
        ("sessionKey", "s".into()),
    ]);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        session_model_get(home, crate::config::Env::new(), params),
    )
    .await
    .expect("model read blocked");
    if case == "config" {
        let result = result.expect("large valid configuration must load");
        assert!(result.get("effective").is_some());
    } else {
        let error = result.expect_err("unsafe FIFO accepted");
        assert_eq!(error.code, "gateway_request_failed");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            "Unsafe project marker or session transcript."
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
