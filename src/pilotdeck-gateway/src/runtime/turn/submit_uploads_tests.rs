use super::*;
use crate::test_scratch::Scratch;
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

struct Fixture {
    _scratch: Scratch,
    home: PathBuf,
    cwd: PathBuf,
    project: PathBuf,
    store: Arc<UploadStore>,
}

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new("submit-uploads");
        let cwd = fs::canonicalize(scratch.path()).unwrap();
        let home = cwd.join("home");
        let project = cwd.join("project");
        for name in ["project", "other"] {
            let root = cwd.join(name);
            fs::create_dir_all(&root).unwrap();
            let registry = home.join("projects").join(metadata::project_id(&root));
            fs::create_dir_all(&registry).unwrap();
            fs::write(registry.join(".cwd"), root.to_str().unwrap()).unwrap();
        }
        let store = upload_store(Arc::new(Metadata {
            home: home.clone(),
            cwd: cwd.clone(),
            files: NativeFiles,
        }));
        Self {
            _scratch: scratch,
            home,
            cwd,
            project,
            store,
        }
    }

    fn upload(&self, mime: &str) -> (String, JsValue) {
        let files = JsValue::Array(vec![object([
            ("clientFileId", "proof".into()),
            ("name", "proof.txt".into()),
            ("relativePath", "proof.txt".into()),
            ("size", JsValue::Number(5.0)),
            ("mimeType", mime.into()),
        ])]);
        let created = self
            .store
            .create(self.project.to_str().unwrap(), &files, None)
            .unwrap();
        let id = text(&created, "uploadId");
        let attachment = self.store.write_part(&id, "proof", &b"hello"[..]).unwrap();
        self.store.complete(&id).unwrap();
        (id, attachment)
    }

    fn input(&self, project: &str, ids: &[&str]) -> JsValue {
        object([
            ("projectKey", project.into()),
            (
                "uploadedAttachments",
                JsValue::Array(
                    ids.iter()
                        .map(|id| {
                            object([
                                ("uploadId", (*id).into()),
                                ("attachmentIds", JsValue::Array(vec!["proof".into()])),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "attachments",
                JsValue::Array(vec![object([
                    ("type", "file".into()),
                    ("path", "ordinary.txt".into()),
                ])]),
            ),
        ])
    }

    async fn resolve(&self, input: &JsValue) -> Result<UploadedTurn, SubmitError> {
        resolve(input, &self.home, &self.cwd.to_str().unwrap().into()).await
    }

    fn assert_no_leases(&self, id: &str) {
        let parent = self.project.join(".tmp/chat-upload-leases").join(id);
        assert!(!parent.exists() || fs::read_dir(parent).unwrap().next().is_none());
    }
}

fn text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}

fn attachments(input: &JsValue) -> &[JsValue] {
    let Some(JsValue::Array(items)) = input.get("attachments") else {
        panic!("attachments array")
    };
    items
}

#[tokio::test]
async fn virtual_general_upload_can_be_reopened_and_consumed_by_a_turn() {
    let fixture = Fixture::new();
    let files = JsValue::Array(vec![object([
        ("clientFileId", "proof".into()),
        ("name", "proof.txt".into()),
        ("relativePath", "proof.txt".into()),
        ("size", JsValue::Number(5.0)),
    ])]);
    let params = object([
        ("projectKey", fixture.home.to_string_lossy().as_ref().into()),
        ("files", files),
    ]);
    let created = crate::routes::domains::request(
        Arc::new(Metadata {
            home: fixture.home.clone(),
            cwd: fixture.cwd.clone(),
            files: NativeFiles,
        }),
        "upload_create".into(),
        params,
    )
    .await
    .unwrap();
    let id = text(&created, "uploadId");
    // get scans the same General root even though list_projects omits it.
    fixture.store.get(&id).unwrap();
    fixture
        .store
        .write_part(&id, "proof", &b"hello"[..])
        .unwrap();
    fixture.store.complete(&id).unwrap();
    let turn = fixture
        .resolve(&fixture.input(fixture.home.to_str().unwrap(), &[&id]))
        .await
        .unwrap();
    assert_eq!(attachments(&turn.input).len(), 2);
    turn.release().unwrap();
    let unknown = fixture
        .store
        .create("missing", &JsValue::Array(vec![]), None)
        .unwrap_err();
    assert_eq!(unknown.code, "PROJECT_NOT_FOUND");
    assert_eq!(unknown.message, JsString::from("Unknown projectKey: missing"));
}

#[tokio::test]
async fn real_upload_maps_fields_preserves_ordinary_and_releases_hard_link() {
    let fixture = Fixture::new();
    let (id, original) = fixture.upload("text/plain");
    let source = PathBuf::from(text(&original, "path"));
    let before = fs::metadata(&source).unwrap();
    let input = fixture.input("project", &[&id]);
    let turn = fixture.resolve(&input).await.unwrap();
    let mapped = attachments(&turn.input);
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0], attachments(&input)[0]);
    assert_eq!(text(&mapped[1], "type"), "file");
    for field in ["name", "mimeType", "bytes"] {
        assert_eq!(mapped[1].get(field), original.get(field));
    }
    let meta = mapped[1].get("metadata").unwrap();
    assert_eq!(text(meta, "uploadId"), id);
    for field in ["attachmentId", "relativePath", "sha256"] {
        assert_eq!(meta.get(field), original.get(field));
    }
    let leased = PathBuf::from(text(&mapped[1], "path"));
    assert_ne!(leased, source);
    let linked = fs::metadata(&leased).unwrap();
    assert_eq!((linked.dev(), linked.ino()), (before.dev(), before.ino()));
    assert_eq!(linked.nlink(), before.nlink() + 1);
    assert_eq!(fs::read(&leased).unwrap(), b"hello");
    turn.release().unwrap();
    assert!(!leased.exists());
    assert_eq!(fs::metadata(&source).unwrap().nlink(), before.nlink());
    assert_eq!(fs::read(source).unwrap(), b"hello");
    fixture.assert_no_leases(&id);
}

#[tokio::test]
async fn image_mapping_and_drop_cleanup_use_real_lease() {
    let fixture = Fixture::new();
    let (id, _) = fixture.upload("image/png");
    let turn = fixture
        .resolve(&fixture.input("project", &[&id]))
        .await
        .unwrap();
    let mapped = &attachments(&turn.input)[1];
    assert_eq!(text(mapped, "type"), "image");
    let path = PathBuf::from(text(mapped, "path"));
    assert!(path.is_file());
    drop(turn);
    assert!(!path.exists());
    fixture.assert_no_leases(&id);
}

#[tokio::test]
async fn tampered_cross_project_and_unregistered_inputs_are_rejected() {
    let fixture = Fixture::new();
    let (id, original) = fixture.upload("text/plain");
    let cross = fixture
        .resolve(&fixture.input("other", &[&id]))
        .await
        .err()
        .unwrap();
    assert_eq!(cross.code, "PROJECT_PATH_FORBIDDEN");
    fs::create_dir(fixture.cwd.join("unregistered")).unwrap();
    let unknown = fixture
        .resolve(&fixture.input("unregistered", &[&id]))
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, "PROJECT_NOT_FOUND");
    // Same length forces the native SHA check, rather than just size validation.
    fs::write(text(&original, "path"), b"other").unwrap();
    let tampered = fixture
        .resolve(&fixture.input("project", &[&id]))
        .await
        .err()
        .unwrap();
    assert_eq!(tampered.code, "ATTACHMENT_TAMPERED");
    fixture.assert_no_leases(&id);
}

#[tokio::test]
async fn later_acquisition_or_validation_failure_releases_previous_leases() {
    let fixture = Fixture::new();
    let (id, original) = fixture.upload("text/plain");
    let source = text(&original, "path");
    let links = fs::metadata(&source).unwrap().nlink();
    let error = fixture
        .resolve(&fixture.input("project", &[&id, "missing-upload"]))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "UPLOAD_NOT_FOUND");
    fixture.assert_no_leases(&id);
    assert_eq!(fs::metadata(&source).unwrap().nlink(), links);
    let mut input = fixture.input("project", &[&id]);
    if let JsValue::Object(fields) = &mut input {
        fields.insert(
            "uploadedAttachments".into(),
            JsValue::Array(vec![
                object([("uploadId", id.as_str().into())]),
                object([("uploadId", JsValue::Number(1.0))]),
            ]),
        );
    }
    assert_eq!(
        fixture.resolve(&input).await.err().unwrap().code,
        "INVALID_UPLOADED_ATTACHMENT"
    );
    fixture.assert_no_leases(&id);
    assert_eq!(fs::metadata(source).unwrap().nlink(), links);
}

#[tokio::test]
async fn empty_uploads_do_not_touch_store_or_require_project_or_valid_cwd() {
    let scratch = Scratch::new("submit-empty-uploads");
    let home = scratch.path().join("must-not-be-created");
    let input = object([
        ("uploadedAttachments", JsValue::Array(vec![])),
        ("attachments", JsValue::Array(vec!["ordinary".into()])),
    ]);
    let cwd = JsString::from_utf16(vec![0xd800]);
    let turn = resolve(&input, &home, &cwd).await.unwrap();
    assert_eq!(turn.input, input);
    turn.release().unwrap();
    assert!(!home.exists());
}

#[tokio::test]
async fn release_attempts_every_lease_in_reverse_order_and_reports_all_errors() {
    let fixture = Fixture::new();
    let (first, _) = fixture.upload("text/plain");
    let (middle, _) = fixture.upload("text/plain");
    let (last, _) = fixture.upload("text/plain");
    let turn = fixture
        .resolve(&fixture.input("project", &[&first, &middle, &last]))
        .await
        .unwrap();
    let paths: Vec<PathBuf> = attachments(&turn.input)[1..]
        .iter()
        .map(|a| PathBuf::from(text(a, "path")))
        .collect();
    let mut restore = Vec::new();
    // An ancestor replaced by a file deterministically makes rm fail (ENOTDIR),
    // including when tests run as root; permissions alone cannot guarantee it.
    for index in [0, 2] {
        let parent = paths[index]
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let backup = parent.with_extension("saved");
        fs::rename(&parent, &backup).unwrap();
        fs::write(&parent, b"blocked").unwrap();
        restore.push((parent, backup));
    }
    let error = turn.release().unwrap_err();
    for (parent, backup) in restore {
        fs::remove_file(&parent).unwrap();
        fs::rename(backup, parent).unwrap();
    }
    assert_eq!(error.code, "UPLOAD_LEASE_RELEASE_FAILED");
    assert!(error.message.find(&last).unwrap() < error.message.find(&first).unwrap());
    assert!(
        !paths[1].exists(),
        "middle release still attempted after last failed"
    );
    fixture.assert_no_leases(&middle);
}
