//! Production project and catalog readers in bounded helper processes.
use super::*;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn native_project_reader_fifo_subprocess() {
    for case in ["list", "describe", "catalog", "files_list", "file_read", "archive_manifest", "archive_entries", "archive_artifact"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture", "native_project_reader_child"])
            .env("PDRS_PROJECT_READER_CASE", case)
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
                    "{case}: expected one child: {stdout}"
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
async fn native_project_reader_child() {
    let case = std::env::var("PDRS_PROJECT_READER_CASE").unwrap();
    let root =
        std::env::temp_dir().join(format!("pdrs-project-reader-{}-{case}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let stored = home
        .join("projects")
        .join(crate::metadata::project_id(&project));
    std::fs::create_dir_all(stored.join("chats")).unwrap();
    std::fs::write(stored.join(".cwd"), project.to_string_lossy().as_bytes()).unwrap();
    let path = if case == "catalog" {
        home.join("pilotdeck.yaml")
    } else {
        let unrelated = home.join("projects/000-unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        unrelated.join(".cwd")
    };
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
    let params = object([("projectKey", project.to_string_lossy().as_ref().into())]);
    let result = if case == "catalog" {
        tokio::time::timeout(
            Duration::from_secs(3),
            model_catalog(home, crate::config::Env::new(), object([])),
        )
        .await
    } else {
        let method = match case.as_str() {
            "list" => "list_projects",
            "describe" => "describe_project",
            "files_list" => "project_files_list",
            "file_read" => "project_file_read",
            "archive_manifest" => "native_archive_manifest",
            "archive_entries" => "native_archive_entries",
            "archive_artifact" => "native_archive_artifact",
            _ => unreachable!(),
        };
        let params = if case.starts_with("archive_") {
            object([
                ("projectKey", project.to_string_lossy().as_ref().into()),
                ("sessionKey", "fixture".into()),
                ("artifactName", "fixture.txt".into()),
            ])
        } else if case == "file_read" {
            object([
                ("projectKey", project.to_string_lossy().as_ref().into()),
                ("path", "fixture".into()),
            ])
        } else {
            params
        };
        tokio::time::timeout(
            Duration::from_secs(3),
            project_metadata(home, crate::config::Env::new(), method.into(), params),
        )
        .await
    }
    .expect("reader blocked");
    let error = result.expect_err("FIFO accepted");
    if case == "catalog" {
        assert_eq!(error.code, "CONFIG_READ_NOT_REGULAR");
    } else if !case.starts_with("archive_") {
        assert_eq!(
            error.message.to_utf8().unwrap(),
            "Unsafe or oversized native project metadata."
        );
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

#[tokio::test(flavor = "current_thread")]
async fn large_summary_and_many_projects_remain_readable() {
    use crate::metadata::Files;
    use std::io::{Seek, SeekFrom, Write};
    let root = std::env::temp_dir().join(format!("pdrs-project-large-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    let project = root.join("p000");
    std::fs::create_dir_all(&home).unwrap();
    for n in 0..128 {
        let project = root.join(format!("p{n:03}"));
        std::fs::create_dir_all(&project).unwrap();
        let stored = home
            .join("projects")
            .join(crate::metadata::project_id(&project));
        std::fs::create_dir_all(stored.join("chats")).unwrap();
        std::fs::write(stored.join(".cwd"), project.to_string_lossy().as_bytes()).unwrap();
    }
    let chat = home
        .join("projects")
        .join(crate::metadata::project_id(&project))
        .join("chats");
    // A sparse regular file supplies a real 17 MiB transcript without a large fixture.
    // Title in the middle forces the existing fallback scanner; another case
    // keeps it in the head to exercise the quick path.
    for (name, position) in [("head", 0u64), ("middle", 8 * 1024 * 1024u64)] {
        let path = chat.join(format!("{name}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.set_len(17 * 1024 * 1024).unwrap();
        file.seek(SeekFrom::Start(position)).unwrap();
        file.write_all(b"\n{\"type\":\"session_metadata\",\"metadata\":{\"title\":\"fixture title\"}}\n")
            .unwrap();
        drop(file);
        let expected = NativeFiles.session_summary(&path, 17 * 1024 * 1024);
        let files = BoundedMetadataFiles::new();
        assert_eq!(
            files.session_summary(&path, 17 * 1024 * 1024),
            expected,
            "{name}"
        );
        assert_eq!(expected, Ok(true), "{name}");
        assert!(!files.was_refused(), "{name}");
        let result = project_metadata(
            home.clone(),
            crate::config::Env::new(),
            "list_projects".into(),
            object([]),
        )
        .await
        .expect("large legal transcript must not reject list_projects");
        let Some(JsValue::Array(projects)) = result.get("projects") else {
            panic!("missing projects")
        };
        assert_eq!(projects.len(), 128);
        std::fs::remove_file(path).unwrap();
    }
    let result = project_metadata(
        home.clone(),
        crate::config::Env::new(),
        "list_projects".into(),
        object([]),
    )
    .await
    .expect("128 real project directories remain readable");
    let Some(JsValue::Array(projects)) = result.get("projects") else {
        panic!("missing projects")
    };
    assert_eq!(projects.len(), 128);
    std::fs::write(project.join("fixture.txt"), b"hello").unwrap();
    for method in ["project_files_list", "project_file_read"] {
        let params = object([
            ("projectKey", project.to_string_lossy().as_ref().into()),
            ("path", "fixture.txt".into()),
        ]);
        let result = project_metadata(
            home.clone(),
            crate::config::Env::new(),
            method.into(),
            params,
        )
        .await
        .expect("registered project file RPC works");
        assert!(!result.is_undefined(), "{method}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn archive_markers_and_artifacts_use_distinct_bounded_reads() {
    use crate::metadata::Files;
    let root = std::env::temp_dir().join(format!("pdrs-archive-bounded-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    let project = root.join("project");
    let stored = home.join("projects").join(crate::metadata::project_id(&project));
    std::fs::create_dir_all(stored.join("chats")).unwrap();
    let artifacts = project.join(".pilotdeck/tool-results/fixture");
    std::fs::create_dir_all(&artifacts).unwrap();
    let marker = stored.join(".cwd");
    std::fs::write(&marker, project.to_string_lossy().as_bytes()).unwrap();
    // Artifact bytes above the marker budget retain the legacy byte count and
    // truncation contract; a marker and artifact must never share that limit.
    std::fs::write(artifacts.join("fixture.txt"), vec![b'x'; 100_000]).unwrap();
    let params = object([
        ("projectKey", project.to_string_lossy().as_ref().into()),
        ("sessionKey", "fixture".into()),
        ("artifactName", "fixture.txt".into()),
        ("maxBytes", JsValue::Number(20.0)),
    ]);
    let result = project_metadata(home.clone(), crate::config::Env::new(), "native_archive_artifact".into(), params.clone()).await.unwrap();
    assert_eq!(result.get("bytes"), Some(&JsValue::Number(100_000.0)));
    assert_eq!(result.get("truncated"), Some(&JsValue::Bool(true)));
    // Real sparse storage, not a mocked length, reaches the descriptor bound.
    std::fs::File::create(artifacts.join("fixture.txt")).unwrap()
        .set_len(crate::storage::TRANSCRIPT_MAX_BYTES + 1).unwrap();
    assert!(project_metadata(home.clone(), crate::config::Env::new(), "native_archive_artifact".into(), params).await.is_err());
    std::fs::write(&marker, vec![b'x'; 65_537]).unwrap();
    let reader = BoundedMetadataFiles::new();
    assert!(reader.archive_marker_read(&home.canonicalize().unwrap(), &marker.canonicalize().unwrap()).is_err());
    assert!(reader.was_refused());
    std::fs::remove_dir_all(root).unwrap();
}
