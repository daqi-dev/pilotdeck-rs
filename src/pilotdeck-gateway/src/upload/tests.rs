//! Contract tests against fixtures recorded from the real TS UploadStore
//! (tests/fixtures/upload/generate.mts). Golden byte tests are never ignored.
use super::*;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

const ORACLE: &str = include_str!("../../tests/fixtures/upload/oracle.json");
const SOURCE: &str = include_str!("mod.rs");
const EPOCH: i64 = 1_792_281_600_000;
const DAY: i64 = 86_400_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let dir = Path::new("/tmp").join(format!(
                "pdrs-upload-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(fs::canonicalize(dir).unwrap()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("{e}"),
            }
        }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    store: Arc<UploadStore>,
    project: PathBuf,
    now: Arc<AtomicI64>,
}

fn harness(project: &Path, other: &Path, options: &JsValue) -> Harness {
    let now = Arc::new(AtomicI64::new(EPOCH));
    let counter = Arc::new(AtomicUsize::new(0));
    let missing = options.get("missingProject").and_then(JsValue::as_bool) == Some(true);
    let (resolved, other, listed) = (project.to_owned(), other.to_owned(), project.to_owned());
    let mut opts = UploadStoreOptions::new(
        move |key| {
            Ok(if key == "other" {
                other.clone()
            } else if missing {
                resolved.join("absent")
            } else {
                resolved.clone()
            })
        },
        move || Ok(vec![listed.clone()]),
    );
    let clock = now.clone();
    opts.now = Arc::new(move || clock.load(Ordering::SeqCst));
    opts.uuid = Some(Arc::new(move || {
        format!("upload-{}", counter.fetch_add(1, Ordering::SeqCst) + 1)
    }));
    let limit = |key| options.get(key).and_then(JsValue::as_number);
    if let Some(n) = limit("maxConcurrentPerProject") {
        opts.max_concurrent_per_project = n as usize;
    }
    if let Some(n) = limit("maxFiles") {
        opts.max_files = n as usize;
    }
    if let Some(n) = limit("maxFileBytes") {
        opts.max_file_bytes = n as u64;
    }
    if let Some(n) = limit("maxTaskBytes") {
        opts.max_task_bytes = n as u64;
    }
    Harness {
        store: Arc::new(UploadStore::new(opts)),
        project: project.to_owned(),
        now,
    }
}

fn normalize(text: &str, project: &Path) -> String {
    text.replace(&*project.to_string_lossy(), "<PROJECT>")
}
fn compact(value: &JsValue) -> String {
    stringify_json(value)
        .unwrap()
        .unwrap_or_else(|| "undefined".into())
}
fn ids(value: Option<&JsValue>) -> Option<Vec<String>> {
    match value? {
        JsValue::Array(items) => Some(
            items
                .iter()
                .map(|v| v.as_string().unwrap().to_utf8().unwrap())
                .collect(),
        ),
        _ => None,
    }
}
fn metadata_path(project: &Path) -> PathBuf {
    project.join(".tmp/chat-uploads/upload-1/metadata.json")
}
fn artifact_path(project: &Path) -> PathBuf {
    project.join(".tmp/chat-uploads/upload-1/files/file-1")
}

fn run(h: &Harness, op: &JsValue) -> Result<JsValue> {
    let id = string(op, "uploadId").unwrap_or_else(|| "upload-1".into());
    let store = &h.store;
    let project = &h.project;
    Ok(match string(op, "op").unwrap().as_str() {
        "create" => store.create("project", &value(op, "files"), string(op, "key").as_deref())?,
        "get" => store.get(&id)?,
        "part" => store.write_part(
            &id,
            &string(op, "id").unwrap(),
            string(op, "bytes").unwrap().as_bytes(),
        )?,
        "complete" => store.complete(&id)?,
        "cancel" => store.cancel(&id)?,
        "verify" => JsValue::Array(store.verify_attachment(
            &id,
            "project",
            ids(op.get("ids")).as_deref(),
        )?),
        "verify-other" => JsValue::Array(store.verify_attachment(&id, "other", None)?),
        "advance" => {
            h.now.fetch_add(number(op, "ms") as i64, Ordering::SeqCst);
            JsValue::Null
        }
        "cleanup" => JsValue::Number(store.cleanup_expired()? as f64),
        "tamper-file" => {
            fs::write(artifact_path(project), "jello").unwrap();
            JsValue::Null
        }
        "remove-file" => {
            fs::remove_file(artifact_path(project)).unwrap();
            JsValue::Null
        }
        "tamper-identity" => {
            let path = metadata_path(project);
            let mut record = parse_json(&fs::read_to_string(&path).unwrap()).unwrap();
            set(&mut record, "uploadId", text("other"));
            fs::write(&path, json_line(&record).unwrap()).unwrap();
            JsValue::Null
        }
        "escape-metadata" => {
            let path = metadata_path(project);
            let outside = project.join("outside.json");
            fs::copy(&path, &outside).unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            JsValue::Null
        }
        other => panic!("unhandled {other}"),
    })
}

/// Replays every recorded TS scenario; returns the dialog codes Rust produced.
fn replay_oracle() -> HashSet<String> {
    assert!(
        !ORACLE.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    let oracle = crate::test_baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let mut codes = HashSet::new();
    let mut operations = 0;
    for scenario in array(&oracle, "scenarios") {
        let name = string(scenario, "name").unwrap();
        let h = harness(&scratch.dir(&name), &scratch.0, &value(scenario, "options"));
        let ops = array(scenario, "operations");
        let results = array(scenario, "results");
        assert_eq!(ops.len(), results.len(), "{name}");
        for (index, (op, expected)) in ops.iter().zip(results).enumerate() {
            let actual = match run(&h, op) {
                Ok(v) => {
                    codes.extend(string(&v, "errorCode"));
                    object([(
                        "ok",
                        parse_json(&normalize(&compact(&v), &h.project)).unwrap(),
                    )])
                }
                Err(e) => {
                    if e.dialog {
                        codes.insert(e.code.clone());
                    }
                    let code = if e.code.is_empty() {
                        JsValue::Null
                    } else {
                        text(&e.code)
                    };
                    let message = text(&normalize(&e.message, &h.project));
                    object([("error", object([("code", code), ("message", message)]))])
                }
            };
            assert_eq!(
                compact(&actual),
                compact(expected),
                "{name}#{index} {}",
                compact(op)
            );
            operations += 1;
        }
    }
    assert_eq!(
        array(&oracle, "scenarios").len(),
        41,
        "recorded scenario coverage"
    );
    assert!(
        operations >= 100,
        "recorded operation coverage: {operations}"
    );
    codes
}

#[test]
fn upload_oracle_replay_matches_ts_results() {
    replay_oracle();
}

/// (code, TS line) for every ledger row anchored in UploadStore.ts.
fn ledger_rows() -> Vec<(String, usize)> {
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/gateway-coverage/ledger.json"),
        )
        .expect("read gateway coverage ledger"),
    )
    .unwrap();
    let mut rows = vec![];
    fn walk(v: &serde_json::Value, rows: &mut Vec<(String, usize)>) {
        match v {
            serde_json::Value::Object(o) => {
                if let (Some(code), Some(refs)) =
                    (o.get("code").and_then(|c| c.as_str()), o.get("refs"))
                {
                    for r in refs
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r.as_str())
                    {
                        if let Some(line) = r.strip_prefix("B/src/gateway/dialog/UploadStore.ts:") {
                            rows.push((code.to_owned(), line.parse().unwrap()));
                        }
                    }
                }
                o.values().for_each(|v| walk(v, rows));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, rows)),
            _ => {}
        }
    }
    walk(&ledger, &mut rows);
    rows
}

/// Every ledger code must map to an explicit Rust branch that the TS-recorded
/// replay (or a fault-injection test) actually produces.
#[test]
fn upload_ledger_codes_are_exact_and_exercised() {
    let rows = ledger_rows();
    let codes: HashSet<_> = rows.iter().map(|(c, _)| c.clone()).collect();
    let expected: HashSet<String> = [
        "ATTACHMENT_EXPIRED",
        "ATTACHMENT_NOT_FOUND",
        "ATTACHMENT_TAMPERED",
        "PROJECT_NOT_FOUND",
        "PROJECT_PATH_FORBIDDEN",
        "UPLOAD_CONCURRENCY_LIMIT",
        "UPLOAD_FILE_TOO_LARGE",
        "UPLOAD_INTEGRITY_MISMATCH",
        "UPLOAD_MANIFEST_INVALID",
        "UPLOAD_NOT_COMPLETED",
        "UPLOAD_NOT_FOUND",
        "UPLOAD_TASK_TOO_LARGE",
        "UPLOAD_INVALID_STATE",
        "UPLOAD_MANIFEST_MISMATCH",
        "UPLOAD_ALREADY_COMPLETED",
        "UPLOAD_STREAM_INTERRUPTED",
    ]
    .map(String::from)
    .into();
    assert_eq!(codes, expected, "ledger UploadStore code set changed");
    let mut observed = replay_oracle();
    observed.insert(lease_link_enoent_code());
    observed.insert(part_write_failure_code());
    let source: String = SOURCE.split_whitespace().collect();
    for code in &codes {
        // Thrown codes are dialog errors; STREAM_INTERRUPTED is the recorded errorCode (TS:184).
        assert!(
            source.contains(&format!("UploadError::dialog(\"{code}\""))
                || (code == "UPLOAD_STREAM_INTERRUPTED"
                    && source.contains("\"UPLOAD_STREAM_INTERRUPTED\"")),
            "no Rust branch for {code}"
        );
        assert!(observed.contains(code), "{code} never produced by replay");
    }
    // TS:375 is unreachable after assertUploadId; its code is covered via TS:382.
    println!(
        "PDRS_UPLOAD_LEDGER rows={} codes={}",
        rows.len(),
        codes.len()
    );
}

fn golden_store(project: &Path) -> Harness {
    harness(project, project, &JsValue::Object(JsObject::new()))
}

#[test]
fn upload_golden_bytes_match_ts_fixtures() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let project = scratch.dir("golden");
    let h = golden_store(&project);
    let fixture = |name: &str| {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/upload")
                .join(name),
        )
        .unwrap()
    };
    let snapshot = |name: &str| {
        let bytes = fs::read_to_string(metadata_path(&project)).unwrap();
        assert_eq!(
            normalize(&bytes, &project),
            fixture(&format!("{name}.json")),
            "{name}"
        );
    };
    h.store
        .create("project", &value(&oracle, "manifest"), Some("once"))
        .unwrap();
    snapshot("created");
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    snapshot("uploading");
    h.store.complete("upload-1").unwrap();
    snapshot("completed");
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let lease_file =
        Path::new(&string(&lease.attachments[0], "path").unwrap()).with_file_name("lease.json");
    assert_eq!(
        fs::read_to_string(lease_file).unwrap(),
        fixture("lease.json")
    );
    lease.release().unwrap();
    h.now.fetch_add(DAY, Ordering::SeqCst);
    h.store.get("upload-1").unwrap();
    snapshot("expired");
}

/// Rust reads TS-written bytes and continues the upload (TS → Rust direction).
#[test]
fn upload_reads_ts_written_metadata_and_continues() {
    crate::test_baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let task = project.join(".tmp/chat-uploads/upload-1");
    fs::create_dir_all(task.join("files")).unwrap();
    let ts = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upload/created.json"),
    )
    .unwrap()
    .replace("<PROJECT>", &project.to_string_lossy());
    fs::write(task.join("metadata.json"), ts).unwrap();
    let h = golden_store(&project);
    assert_eq!(status(&h.store.get("upload-1").unwrap()), "created");
    // Idempotency hash written by TS is honoured.
    let again = h
        .store
        .create(
            "project",
            &parse_json(r#"[{"clientFileId":"x","name":"x","relativePath":"x","size":0}]"#)
                .unwrap(),
            Some("once"),
        )
        .unwrap();
    assert_eq!(string(&again, "uploadId").as_deref(), Some("upload-1"));
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    let done = h.store.complete("upload-1").unwrap();
    assert_eq!(
        compact(&value(&array(&done, "manifest")[0], "extra")),
        r#"{"z":1,"a":2}"#
    );
    assert_eq!(
        h.store
            .verify_attachment("upload-1", "project", None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn upload_lease_pins_release_is_idempotent_and_orphans_expire() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let lease_root = project.join(".tmp/chat-upload-leases/upload-1");
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let pinned = PathBuf::from(string(&lease.attachments[0], "path").unwrap());
    assert!(pinned.starts_with(&lease_root));
    assert_eq!(fs::read(&pinned).unwrap(), b"hello");
    let orphan = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let orphan_dir = PathBuf::from(string(&orphan.attachments[0], "path").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    drop(orphan); // crash-orphan: no release
    lease.release().unwrap();
    lease.release().unwrap();
    assert!(!pinned.exists());
    assert!(orphan_dir.exists());
    // Not yet expired (+6h − 1 ms): nothing is reclaimed.
    h.now.store(EPOCH + 21_600_000 - 1, Ordering::SeqCst);
    assert_eq!(h.store.cleanup_expired().unwrap(), 0);
    assert!(orphan_dir.exists());
    // At expiresAt (not strictly in the future) the orphan and its empty parent go.
    h.now.store(EPOCH + 21_600_000, Ordering::SeqCst);
    assert_eq!(h.store.cleanup_expired().unwrap(), 0);
    assert!(!lease_root.exists(), "expired orphan lease is reclaimed");
    // Upload expiry: get marks expired, pins refuse, cleanup removes the task.
    h.now.store(EPOCH + DAY, Ordering::SeqCst);
    let e = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .unwrap();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("ATTACHMENT_EXPIRED", "Upload expired.")
    );
    assert_eq!(status(&h.store.get("upload-1").unwrap()), "expired");
    assert_eq!(h.store.cleanup_expired().unwrap(), 1);
    assert!(!task_dir_of(&project).exists());
}

#[test]
fn upload_live_lease_survives_upload_cleanup_until_lease_expiry() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let mut h = golden_store(&project);
    completed(&h);
    h.now.store(EPOCH + DAY - 1000, Ordering::SeqCst);
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let pinned = PathBuf::from(string(&lease.attachments[0], "path").unwrap());
    h.now.store(EPOCH + DAY, Ordering::SeqCst);
    assert_eq!(h.store.cleanup_expired().unwrap(), 1);
    assert_eq!(
        fs::read(&pinned).unwrap(),
        b"hello",
        "hard link outlives the upload"
    );
    lease.release().unwrap();
    assert!(!pinned.exists());
    // Unsafe lease names are ignored, not traversed; malformed lease.json expires at once.
    let root = project.join(".tmp/chat-upload-leases");
    fs::create_dir_all(root.join("u/bad")).unwrap();
    fs::write(root.join("u/bad/lease.json"), "{").unwrap();
    fs::create_dir_all(root.join("u/..x")).unwrap();
    h = golden_store(&project);
    h.store.cleanup_expired().unwrap();
    assert!(!root.join("u/bad").exists());
    assert!(root.join("u/..x").exists());
}

fn task_dir_of(project: &Path) -> PathBuf {
    project.join(".tmp/chat-uploads/upload-1")
}
fn completed(h: &Harness) {
    let manifest = parse_json(&compact(&value(
        &crate::test_baseline::oracle(ORACLE),
        "manifest",
    )))
    .unwrap();
    h.store.create("project", &manifest, None).unwrap();
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    h.store.complete("upload-1").unwrap();
}
fn created(h: &Harness) {
    let manifest = value(&crate::test_baseline::oracle(ORACLE), "manifest");
    h.store.create("project", &manifest, None).unwrap();
}
fn inject(h: &Harness, operation: &str, errno: i32) {
    h.store
        .faults
        .lock()
        .unwrap()
        .push((operation.into(), errno));
}
fn leftovers(dir: &Path, suffix: &str) -> usize {
    names(dir).iter().filter(|n| n.ends_with(suffix)).count()
}

fn lease_link_enoent_code() -> String {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    inject(&h, "lease-link", libc::ENOENT);
    let e = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .unwrap();
    assert_eq!(
        e.message,
        "Attachment artifact became unavailable while acquiring its Gateway lease."
    );
    // TS:272 removes the half-built lease directory.
    assert!(names(&project.join(".tmp/chat-upload-leases/upload-1")).is_empty());
    e.code
}

#[test]
fn upload_lease_failures_remove_partial_lease() {
    assert_eq!(lease_link_enoent_code(), "ATTACHMENT_TAMPERED");
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    // Non-ENOENT errors propagate unchanged (TS:279).
    inject(&h, "lease-write", libc::ENOSPC);
    let e = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .unwrap();
    assert_eq!(e.code, "ENOSPC");
    assert!(!e.dialog);
    assert!(names(&project.join(".tmp/chat-upload-leases/upload-1")).is_empty());
    // A real vanished artifact also maps to TAMPERED, at verification (TS:311).
    fs::remove_file(artifact_path(&project)).unwrap();
    let e = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .unwrap();
    assert_eq!(e.code, "ATTACHMENT_TAMPERED");
}

#[test]
fn upload_metadata_write_failure_leaves_temp_and_old_record() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let before = fs::read(metadata_path(&project)).unwrap();
    inject(&h, "metadata-write", libc::ENOSPC);
    let e = h.store.cancel("upload-1").err().unwrap();
    assert_eq!((e.code.as_str(), e.dialog), ("ENOSPC", false));
    assert_eq!(
        fs::read(metadata_path(&project)).unwrap(),
        before,
        "atomic: old record intact"
    );
    // TS closes the handle and propagates; the temp file is not removed (TS:352).
    assert_eq!(leftovers(&task_dir_of(&project), ".tmp"), 1);
    // TS keeps the rejected mutation promise, so this instance stays poisoned.
    assert_eq!(h.store.cancel("upload-1").err().unwrap().code, "ENOSPC");
    // A fresh provider (new process) reads the intact record.
    assert_eq!(
        status(&golden_store(&project).store.get("upload-1").unwrap()),
        "created"
    );
}

#[test]
fn upload_rename_failures_retry_or_clean_temp() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    // EPERM/EEXIST/EACCES: remove target and retry once (TS:357–363).
    for errno in [libc::EPERM, libc::EEXIST, libc::EACCES] {
        inject(&h, "metadata-rename", errno);
        h.store
            .mutate(&h.store.get("upload-1").unwrap(), |_| {})
            .unwrap();
        assert_eq!(leftovers(&task_dir_of(&project), ".tmp"), 0);
    }
    // Retry failure removes the temp and loses the old record, like TS rm(path).
    inject(&h, "metadata-rename", libc::EPERM);
    inject(&h, "metadata-rename", libc::EIO);
    let e = h.store.cancel("upload-1").err().unwrap();
    assert_eq!(e.code, "EIO");
    assert_eq!(leftovers(&task_dir_of(&project), ".tmp"), 0);
    assert!(!metadata_path(&project).exists());
    // Other errors remove the temp but keep the old record.
    let project = scratch.dir("q");
    let h = golden_store(&project);
    created(&h);
    let before = fs::read(metadata_path(&project)).unwrap();
    inject(&h, "metadata-rename", libc::EXDEV);
    assert_eq!(h.store.cancel("upload-1").err().unwrap().code, "EXDEV");
    assert_eq!(leftovers(&task_dir_of(&project), ".tmp"), 0);
    assert_eq!(fs::read(metadata_path(&project)).unwrap(), before);
}

#[test]
fn upload_part_write_failure_marks_failed_with_stream_interrupted() {
    assert_eq!(part_write_failure_code(), "UPLOAD_STREAM_INTERRUPTED");
}
fn part_write_failure_code() -> String {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    inject(&h, "part-write", libc::ENOSPC);
    let e = h
        .store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .err()
        .unwrap();
    assert_eq!(e.code, "ENOSPC");
    let record = h.store.get("upload-1").unwrap();
    assert_eq!(status(&record), "failed");
    assert_eq!(string(&record, "errorMessage"), Some(e.message));
    string(&record, "errorCode").unwrap()
}

#[test]
fn upload_part_destination_conflict_and_interrupted_reads() {
    // Real filesystem conflict: the wx open fails on a pre-existing artifact.
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    fs::write(artifact_path(&project), "x").unwrap();
    let e = h
        .store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .err()
        .unwrap();
    assert_eq!(e.code, "EEXIST");
    assert_eq!(
        fs::read(artifact_path(&project)).unwrap(),
        b"x",
        "wx never clobbers"
    );
    assert_eq!(
        string(&h.store.get("upload-1").unwrap(), "errorCode").as_deref(),
        Some("UPLOAD_STREAM_INTERRUPTED")
    );
    // EINTR is retried; a hard read error interrupts the stream.
    struct Flaky(u8);
    impl Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0 += 1;
            match self.0 {
                1 => Err(io::ErrorKind::Interrupted.into()),
                2 => {
                    buf[..2].copy_from_slice(b"he");
                    Ok(2)
                }
                _ => Err(io::Error::from_raw_os_error(libc::ECONNRESET)),
            }
        }
    }
    let project = scratch.dir("q");
    let h = golden_store(&project);
    created(&h);
    assert!(h.store.write_part("upload-1", "file-1", Flaky(0)).is_err());
    let record = h.store.get("upload-1").unwrap();
    assert_eq!(
        string(&record, "errorCode").as_deref(),
        Some("UPLOAD_STREAM_INTERRUPTED")
    );
    assert_eq!(
        number(&record, "uploadedBytes"),
        2.0,
        "progress before the failure is kept"
    );
}

#[test]
fn upload_concurrent_creates_respect_limit_and_idempotency() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let manifest = value(&crate::test_baseline::oracle(ORACLE), "manifest");
    let run = |key: Option<&'static str>| {
        let store = h.store.clone();
        let manifest = manifest.clone();
        std::thread::spawn(move || {
            store
                .create("project", &manifest, key)
                .map(|r| string(&r, "uploadId").unwrap())
        })
    };
    // Eight racing creates: the per-project gate admits exactly three.
    let results: Vec<_> = (0..8)
        .map(|_| run(None))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.code == "UPLOAD_CONCURRENCY_LIMIT")
    );
    // Racing idempotent creates on a fresh project yield one record.
    let project = scratch.dir("q");
    let h = golden_store(&project);
    let run = |key| {
        let store = h.store.clone();
        let manifest = manifest.clone();
        std::thread::spawn(move || {
            store
                .create("project", &manifest, Some(key))
                .map(|r| string(&r, "uploadId").unwrap())
        })
    };
    let ids: HashSet<_> = (0..8)
        .map(|_| run("k"))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap().unwrap())
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(names(&project.join(".tmp/chat-uploads")).len(), 1);
}

#[test]
fn upload_concurrent_mutations_serialize_per_upload() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let record = h.store.get("upload-1").unwrap();
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let (store, record) = (h.store.clone(), record.clone());
            std::thread::spawn(move || store.progress(&record, 1).unwrap())
        })
        .collect();
    threads.into_iter().for_each(|t| {
        t.join().unwrap();
    });
    // Each read-modify-write sees the prior one; capped at totalBytes (5).
    assert_eq!(
        number(&h.store.get("upload-1").unwrap(), "uploadedBytes"),
        5.0
    );
    assert!(
        h.store.updates.lock().unwrap().is_empty(),
        "healthy queues are released"
    );
    assert_eq!(leftovers(&task_dir_of(&project), ".tmp"), 0);
}

#[test]
fn upload_paths_outside_upload_root_are_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let long = "x".repeat(129);
    for id in ["../evil", "a/b", "..", "", long.as_str(), "a..b", "é"] {
        let e = h.store.get(id).err().unwrap();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            ("UPLOAD_NOT_FOUND", "Invalid uploadId."),
            "{id:?}"
        );
    }
    // Whole upload directory symlinked elsewhere.
    created(&h);
    let elsewhere = scratch.dir("elsewhere");
    fs::rename(task_dir_of(&project), elsewhere.join("upload-1")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("upload-1"), task_dir_of(&project)).unwrap();
    assert_eq!(
        h.store.get("upload-1").err().unwrap().code,
        "PROJECT_PATH_FORBIDDEN"
    );
    // Lease ids and unsafe directory names never become paths.
    let bad =
        parse_json(r#"[{"clientFileId":"../x","name":"n","relativePath":"r","size":0}]"#).unwrap();
    assert_eq!(
        h.store.create("project", &bad, None).err().unwrap().code,
        "UPLOAD_MANIFEST_INVALID"
    );
    // Lone surrogate relativePath is still a safe relative path in TS (UTF-16).
    let surrogate = JsValue::Array(vec![object([
        ("clientFileId", text("s")),
        ("name", text("n")),
        (
            "relativePath",
            JsValue::String(pilotdeck_protocol::JsString::from_utf16(vec![0xd800])),
        ),
        ("size", JsValue::Number(0.0)),
    ])]);
    let fresh = scratch.dir("s");
    assert!(
        golden_store(&fresh)
            .store
            .create("project", &surrogate, None)
            .is_ok()
    );
}

#[test]
fn upload_subscription_receives_updates_until_dropped() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let sub = h.store.subscribe("upload-1");
    created(&h);
    assert_eq!(status(&sub.receiver.try_recv().unwrap()), "created");
    drop(sub);
    assert!(h.store.listeners.lock().unwrap().entries.is_empty());
}

fn baseline() -> PathBuf {
    PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE to the PilotDeck checkout"),
    )
}
fn node(args: &[&std::ffi::OsStr]) -> String {
    let base = baseline();
    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upload/generate.mts");
    let out = std::process::Command::new("node")
        .arg("--import")
        .arg(base.join("node_modules/tsx/dist/loader.mjs"))
        .arg(generator)
        .args(args)
        .env("PDRS_BASELINE", &base)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Regenerates every fixture from the real TS store and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn upload_fixtures_regenerate_identically_from_ts() {
    let scratch = Scratch::new();
    let out = scratch.dir("out");
    println!("{}", node(&[out.as_os_str()]));
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upload");
    for name in [
        "oracle.json",
        "created.json",
        "uploading.json",
        "completed.json",
        "lease.json",
        "expired.json",
    ] {
        assert_eq!(
            fs::read(out.join(name)).unwrap(),
            fs::read(fixtures.join(name)).unwrap(),
            "{name}"
        );
    }
    // Ledger anchors point at the real TS throw (code on that line or the next).
    let source = fs::read_to_string(baseline().join("src/gateway/dialog/UploadStore.ts")).unwrap();
    let lines: Vec<_> = source.lines().collect();
    for (code, n) in ledger_rows() {
        assert!(
            lines[n - 1].contains(&*code) || lines[n].contains(&*code),
            "TS line {n} lacks {code}"
        );
    }
}

/// Rust → TS direction: the real TS store continues a Rust-written upload and lease.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn upload_ts_reads_rust_written_upload_and_lease() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let manifest = value(&crate::test_baseline::oracle(ORACLE), "manifest");
    h.store.create("project", &manifest, Some("once")).unwrap();
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    h.store.complete("upload-1").unwrap();
    // Unreleased handle = crash-orphan lease left for TS to read and expire.
    drop(
        h.store
            .acquire_attachment_lease("upload-1", "project", None)
            .unwrap(),
    );
    h.store.create("project", &manifest, None).unwrap();
    h.store
        .write_part("upload-2", "file-1", &b"hello"[..])
        .unwrap();
    println!(
        "{}",
        node(&[project.as_os_str(), std::ffi::OsStr::new("--verify-rust")])
    );
}
