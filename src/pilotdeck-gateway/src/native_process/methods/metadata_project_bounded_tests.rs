//! Production project and catalog readers in bounded helper processes.
use super::*;
use std::{process::{Command, Stdio}, time::{Duration, Instant}};

#[test]
fn native_project_reader_fifo_subprocess() {
    for case in ["list", "describe", "catalog"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture", "native_project_reader_child"])
            .env("PDRS_PROJECT_READER_CASE", case)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(status.success(), "{case}: {}", String::from_utf8_lossy(&output.stderr));
                assert!(stdout.contains("1 passed; 0 failed"), "{case}: expected one child: {stdout}");
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!("{case}: child blocked: {}", String::from_utf8_lossy(&output.stderr));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "invoked by the bounded parent test"]
async fn native_project_reader_child() {
    let case = std::env::var("PDRS_PROJECT_READER_CASE").unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-project-reader-{}-{case}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let stored = home.join("projects").join(crate::metadata::project_id(&project));
    std::fs::create_dir_all(stored.join("chats")).unwrap();
    std::fs::write(stored.join(".cwd"), project.to_string_lossy().as_bytes()).unwrap();
    let path = if case == "catalog" {
        home.join("pilotdeck.yaml")
    } else {
        let unrelated = home.join("projects/000-unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        unrelated.join(".cwd")
    };
    assert!(Command::new("/usr/bin/mkfifo").arg(path).status().unwrap().success());
    let params = object([("projectKey", project.to_string_lossy().as_ref().into())]);
    let result = if case == "catalog" {
        tokio::time::timeout(Duration::from_secs(3), model_catalog(home, crate::config::Env::new(), object([]))).await
    } else {
        let method = if case == "list" { "list_projects" } else { "describe_project" };
        tokio::time::timeout(Duration::from_secs(3), project_metadata(home, crate::config::Env::new(), method.into(), params)).await
    }.expect("reader blocked");
    let error = result.expect_err("FIFO accepted");
    if case == "catalog" {
        assert_eq!(error.code, "CONFIG_READ_NOT_REGULAR");
    } else {
        assert_eq!(error.message.to_utf8().unwrap(), "Unsafe or oversized native project metadata.");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_project_reader_resource_budget_rejects_and_marks_refusal() {
    use crate::metadata::Files;
    let root = std::env::temp_dir().join(format!("pdrs-project-budget-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("sample"), b"x").unwrap();
    let reader = BoundedMetadataFiles::new();
    reader.entries.set(BoundedMetadataFiles::ENTRY_LIMIT);
    assert!(reader.entries(&root).is_err());
    assert!(reader.was_refused());
    let reader = BoundedMetadataFiles::new();
    reader.bytes.set(BoundedMetadataFiles::BYTE_LIMIT);
    assert!(reader.read(&root.join("sample")).is_err());
    assert!(reader.was_refused());
    std::fs::remove_dir_all(root).unwrap();
}
