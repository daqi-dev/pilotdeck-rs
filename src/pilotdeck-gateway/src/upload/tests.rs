//! Contract tests against fixtures recorded from the real TS UploadStore
//! (tests/fixtures/upload/generate.mts). Golden byte tests are never ignored.
use super::*;
use crate::negative_assert;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

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
        move |key: &JsString| {
            Ok(if key.equals_str("other") {
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
/// `normalize` over UTF-16 units: the A8.1 fixture has no surrogate messages,
/// but the shared replay must not introduce a lossy boundary of its own.
fn normalize_message(message: &JsString, project: &Path) -> JsString {
    let needle = project.to_string_lossy().encode_utf16().collect::<Vec<_>>();
    let replacement = "<PROJECT>".encode_utf16().collect::<Vec<_>>();
    let haystack = message.units();
    let mut out = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].starts_with(&needle) {
            out.extend_from_slice(&replacement);
            index += needle.len();
        } else {
            out.push(haystack[index]);
            index += 1;
        }
    }
    JsString::from_utf16(out)
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
                    let message = JsValue::String(normalize_message(&e.message, &h.project));
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
        (e.code.as_str(), e.message_string().as_str()),
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

/// #2098 fix-r2 (review DS#2 / MM): the fourth path-read of the family.
/// `cleanup_expired_leases` used `fs::read` on the lease entry — a bare open
/// that blocks a FIFO swapped in for `lease.json` for a writer that never
/// comes, stalling the cleanup round. The entry read now goes through
/// `open_artifact_guarded` with the handle's own type/length terms; a
/// non-file entry is not read at all, which this half already spells
/// "expired" (the malformed-JSON default), so the FIFO lease is reclaimed
/// rather than waited on while a live future lease survives the round.
#[cfg(unix)]
#[test]
fn upload_lease_cleanup_does_not_wait_on_a_fifo_lease_entry() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let pinned = PathBuf::from(string(&lease.attachments[0], "path").unwrap());
    let root = project.join(".tmp/chat-upload-leases");
    let fifo_dir = root.join("u/fifo");
    fs::create_dir_all(&fifo_dir).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(fifo_dir.join("lease.json"))
        .status()
        .expect("#2098 run mkfifo for the FIFO lease entry fixture");
    assert!(
        made.success(),
        "#2098 create the FIFO lease entry fixture for the cleanup watchdog"
    );
    let store = h.store.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(store.cleanup_expired().map(|_| ()).map_err(|e| e.code.clone()));
    });
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(_)) => {}
        Ok(Err(code)) => panic!("#2098 the lease sweep must not fail on the FIFO entry: {code}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("#2098 a FIFO lease entry must never make the cleanup wait for a writer")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("#2098 the cleanup thread died before reporting (fixture or product panic)")
        }
    }
    assert!(
        !fifo_dir.exists(),
        "#2098 the unreadable FIFO lease is reclaimed as expired, not waited on"
    );
    assert!(
        pinned.exists(),
        "#2098 the live future lease survives the round that reclaimed the FIFO entry"
    );
    lease.release().unwrap();
}

/// #1441 三态反差（ts-probe/p1-attachment-ids.out.txt，真 TS UploadStore
/// 实测）：`attachmentIds` undefined → 全部；`[]` → 静默零个（校验与租约
/// 都成功、无硬链接）；未知 id → ATTACHMENT_NOT_FOUND "Unknown
/// attachmentId."。三态互为反差钉死：`[]` 不得退化成 unknown 的报错，
/// unknown 不得退化成 `[]` 的静默成功。
#[test]
fn upload_attachment_ids_three_state_contrast() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    // undefined → 全部
    let all = h
        .store
        .verify_attachment("upload-1", "project", None)
        .unwrap();
    assert_eq!(all.len(), 1, "undefined selects every attachment");
    assert_eq!(string(&all[0], "attachmentId").unwrap(), "file-1");
    // [] → 静默零个（wanted 为空集合，长度核对 0 == 0 通过）
    let none = h
        .store
        .verify_attachment("upload-1", "project", Some(&[]))
        .unwrap();
    assert!(none.is_empty(), "an empty selection is a silent zero");
    // 未知 → ATTACHMENT_NOT_FOUND
    let e = h
        .store
        .verify_attachment("upload-1", "project", Some(&["bogus".to_string()]))
        .err()
        .unwrap();
    assert_eq!(
        (e.code.as_str(), e.message_string().as_str()),
        ("ATTACHMENT_NOT_FOUND", "Unknown attachmentId.")
    );
    // 已知 → 精确一个（正控：过滤本身没坏）
    let one = h
        .store
        .verify_attachment("upload-1", "project", Some(&["file-1".to_string()]))
        .unwrap();
    assert_eq!(one.len(), 1);
    // `[]` 同样租得到零附件租约：无硬链接，只有 lease.json
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", Some(&[]))
        .unwrap();
    assert!(lease.attachments.is_empty());
    assert_eq!(
        names(&lease.dir),
        vec!["lease.json".to_owned()],
        "an empty lease links nothing"
    );
    lease.release().unwrap();
}

/// M1/M12（矩阵）：同一 uploadId 的两次 acquire 并发跑在两个线程上——
/// 各自拿到独立随机租约目录，内容都可读，释放互不影响。
#[test]
fn upload_concurrent_lease_acquires_are_independent() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let store = h.store.clone();
    let (left, right) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            store
                .acquire_attachment_lease("upload-1", "project", None)
                .unwrap()
        });
        let second = scope.spawn(|| {
            store
                .acquire_attachment_lease("upload-1", "project", None)
                .unwrap()
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    let left_path = PathBuf::from(string(&left.attachments[0], "path").unwrap());
    let right_path = PathBuf::from(string(&right.attachments[0], "path").unwrap());
    assert_ne!(left_path, right_path, "each acquire gets its own lease dir");
    assert_eq!(fs::read(&left_path).unwrap(), b"hello");
    assert_eq!(fs::read(&right_path).unwrap(), b"hello");
    left.release().unwrap();
    right.release().unwrap();
    assert!(!left_path.exists());
    assert!(!right_path.exists());
}

/// M5（探针 P3）：completed 拒绝 cancel（UPLOAD_ALREADY_COMPLETED）；
/// 在途（有已写分片）的 cancel 走 in-gate 转换并删掉 files 目录。
#[test]
fn upload_cancel_refuses_completed_and_removes_pending_files() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let e = h.store.cancel("upload-1").err().unwrap();
    assert_eq!(
        (e.code.as_str(), e.message_string().as_str()),
        (
            "UPLOAD_ALREADY_COMPLETED",
            "Completed uploads cannot be cancelled."
        )
    );
    // 第二个上传在途：cancel 转换状态并移除 files 目录
    let manifest = value(&crate::test_baseline::oracle(ORACLE), "manifest");
    h.store.create("project", &manifest, None).unwrap();
    h.store
        .write_part("upload-2", "file-1", &b"hello"[..])
        .unwrap();
    let files = project.join(".tmp/chat-uploads/upload-2/files");
    assert!(files.exists(), "a written part lands in the files dir");
    let cancelled = h.store.cancel("upload-2").unwrap();
    assert_eq!(status(&cancelled), "cancelled");
    assert!(!files.exists(), "cancel removes the in-gate files dir");
}
/// #2279: the expiry sweep runs between `complete`'s outside-`get` snapshot
/// and its in-gate re-read — the rendezvous crosses the clock past
/// `expiresAt`, then `cleanup_expired` removes the whole task directory. The
/// pre-fix in-gate fallback to the outside snapshot let `write`'s `make_dir`
/// resurrect the directory and report a completion whose files the sweep had
/// already removed (the #1631 shape, recreated through the sweep race).
#[test]
fn upload_complete_after_the_sweep_removed_the_record_refuses_without_resurrecting() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    let task_dir = project.join(".tmp/chat-uploads/upload-1");
    assert!(
        task_dir.join("files/file-1").exists(),
        "precondition — uploaded bytes in place"
    );
    // The interleave: the clock crosses expiresAt and the sweep removes the
    // record while `complete` is between its snapshot and the gate.
    let (store, clock) = (h.store.clone(), h.now.clone());
    h.store.set_pre_mutate_hook(Some(Arc::new(move || {
        clock.store(EPOCH + DAY + 1, Ordering::SeqCst);
        assert_eq!(
            store.cleanup_expired().unwrap(),
            1,
            "the sweep removes the record"
        );
    })));
    let result = h.store.complete("upload-1");
    h.store.set_pre_mutate_hook(None);
    match &result {
        Err(e) => assert_eq!(
            e.code, "UPLOAD_NOT_FOUND",
            "a swept upload is refused, not judged on the stale snapshot"
        ),
        Ok(record) => panic!(
            "#2279 complete reported success over a swept record: {}",
            compact(record)
        ),
    }
    assert!(
        !task_dir.join("metadata.json").exists(),
        "#2279 the swept metadata must not be resurrected"
    );
    assert!(
        !task_dir.exists(),
        "#2279 the swept task directory must stay gone"
    );
}

/// The `cancel` sibling of the #2279 interleave: same sweep window, the
/// pre-fix fallback rebuilt a cancelled record on the empty directory.
#[test]
fn upload_cancel_after_the_sweep_removed_the_record_refuses_without_resurrecting() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    let task_dir = project.join(".tmp/chat-uploads/upload-1");
    let (store, clock) = (h.store.clone(), h.now.clone());
    h.store.set_pre_mutate_hook(Some(Arc::new(move || {
        clock.store(EPOCH + DAY + 1, Ordering::SeqCst);
        assert_eq!(
            store.cleanup_expired().unwrap(),
            1,
            "the sweep removes the record"
        );
    })));
    let result = h.store.cancel("upload-1");
    h.store.set_pre_mutate_hook(None);
    match &result {
        Err(e) => assert_eq!(
            e.code, "UPLOAD_NOT_FOUND",
            "a swept upload is refused, not judged on the stale snapshot"
        ),
        Ok(record) => panic!(
            "#2279 cancel reported success over a swept record: {}",
            compact(record)
        ),
    }
    assert!(
        !task_dir.join("metadata.json").exists(),
        "#2279 the swept metadata must not be resurrected"
    );
    assert!(
        !task_dir.exists(),
        "#2279 the swept task directory must stay gone"
    );
}

// #2275: a refusal writes and emits nothing. Both refusal shapes of the
// pair — completing a settled record and cancelling a completed one — must
// answer their semantic code, leave the metadata bytes untouched, publish
// no event, and never surface an injected metadata-write fault in place of
// the verdict: pre-#2275 the gate rewrote the record even for a refusal, so
// the fault answered instead of the code and poisoned the queue behind a
// verdict that needed no disk at all.
#[test]
fn upload_refusals_write_nothing_emit_nothing_and_do_not_poison_the_gate() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let metadata = project.join(".tmp/chat-uploads/upload-1/metadata.json");
    let before = fs::read(&metadata).unwrap();
    let sub = h.store.subscribe("upload-1");
    // Still short of expiresAt, but any `updatedAt` bump would land on this
    // fresh timestamp and show up in the bytes.
    h.now.store(EPOCH + DAY - 1000, Ordering::SeqCst);
    inject(&h, "metadata-write", libc::EIO);
    assert_eq!(
        h.store.complete("upload-1").unwrap_err().code,
        "UPLOAD_INVALID_STATE",
        "#2275 the refusal answers its semantic code, not the injected write fault"
    );
    assert_eq!(
        h.store.cancel("upload-1").unwrap_err().code,
        "UPLOAD_ALREADY_COMPLETED",
        "#2275 the refusal answers its semantic code, not the injected write fault"
    );
    assert_eq!(
        h.store.faults.lock().unwrap().len(),
        1,
        "#2275 no refusal reached a metadata write, so the fault is still queued"
    );
    assert_eq!(
        fs::read(&metadata).unwrap(),
        before,
        "#2275 the metadata bytes are stable across refusals"
    );
    assert!(
        sub.receiver.try_recv().is_err(),
        "#2275 a refusal emits no event"
    );
    // The gate is not poisoned: the same id still mutates afterwards. Clear
    // the still-queued fault first, then let the sweep below expire the
    // record through the very queue the refusals used.
    h.store.faults.lock().unwrap().clear();
    h.now.store(EPOCH + DAY + 1, Ordering::SeqCst);
    assert_eq!(
        status(&h.store.get("upload-1").unwrap()),
        "expired",
        "#2275 the gate still accepts mutations after a refusal"
    );
}

// #2275: cancelling an already-cancelled upload is a no-op, not a change:
// `Ok` with the in-gate record, nothing written, emitted, or stamped.
#[test]
fn upload_cancel_of_a_cancelled_upload_is_a_silent_noop() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    h.store.cancel("upload-1").unwrap();
    let metadata = project.join(".tmp/chat-uploads/upload-1/metadata.json");
    let before = fs::read(&metadata).unwrap();
    let sub = h.store.subscribe("upload-1");
    h.now.store(EPOCH + DAY - 1000, Ordering::SeqCst);
    let again = h.store.cancel("upload-1").unwrap();
    assert_eq!(status(&again), "cancelled");
    assert_eq!(
        string(&again, "updatedAt").unwrap(),
        string(
            &parse_json(&String::from_utf8(before.clone()).unwrap()).unwrap(),
            "updatedAt"
        )
        .unwrap(),
        "#2275 the no-op leaves updatedAt alone"
    );
    assert_eq!(
        fs::read(&metadata).unwrap(),
        before,
        "#2275 the metadata bytes are stable across the no-op"
    );
    assert!(
        sub.receiver.try_recv().is_err(),
        "#2275 a no-op emits no event"
    );
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
        e.message_string(),
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
            .mutate(&h.store.get("upload-1").unwrap(), |_| true)
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
    assert_eq!(string(&record, "errorMessage"), Some(e.message_string()));
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
            (e.code.as_str(), e.message_string().as_str()),
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

/// A directory occupying `metadata.json`: both realpath checks pass, and the
/// whole-file read (UploadStore.ts:384 `readFile`, node:fs/promises) must
/// report the descriptor read stage like Node — `EISDIR: illegal operation
/// on a directory, read` with no path (node v22.22.0,
/// `_tmp/20261025-issue-656/node-probe/read-stages.out`) — not the open label
/// with the path. POSIX-only: opening a directory read-only succeeds there.
#[cfg(unix)]
#[test]
fn upload_metadata_directory_reports_the_read_stage() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = harness(&project, &scratch.dir("other"), &JsValue::Undefined);
    fs::create_dir_all(metadata_path(&project).parent().unwrap()).unwrap();
    fs::create_dir(metadata_path(&project)).unwrap();
    let error = h.store.get("upload-1").unwrap_err();
    assert_eq!(error.code, "EISDIR");
    assert_eq!(
        error.message_string(),
        "EISDIR: illegal operation on a directory, read"
    );
    assert!(!error.dialog);
}

/// #1494: `metadata.json` is swapped for an outside-project symlink inside
/// `read_at`'s canonicalize→open window. The prefix check already passed on
/// the pre-swap spelling, and the outside record carries the correct
/// uploadId and projectKey. #2098's guarded open (`O_NOFOLLOW`) refuses the
/// swapped link at the open itself — before the handle binding this test
/// used to pin — so the observable shape is Node's ELOOP open error naming
/// the canonical spelling that was asked to open. The refusal is also the
/// proof of the safety intent: no outside byte is read into the pipeline,
/// because the record would have to be read back first. RED against the
/// pre-#2098 code: the open followed the link onto the outside bytes.
#[cfg(unix)]
#[test]
fn upload_read_at_metadata_swapped_between_canonicalize_and_open_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    // The guarded open reports the canonical spelling it was asked to open;
    // sampled before the swap so it is still the real record's path.
    let expected_open_path = fs::canonicalize(&metadata).unwrap();
    let outside = project.join("outside-forged.json");
    let mut forged = parse_json(&fs::read_to_string(&metadata).unwrap()).unwrap();
    set(&mut forged, "forgedMarker", text("outside"));
    fs::write(&outside, json_line(&forged).unwrap()).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        move |_path: &Path| {
            fs::remove_file(&metadata).unwrap();
            std::os::unix::fs::symlink(&outside, &metadata).unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen, swap);
    let result = h.store.get("upload-1");
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1494 the swap hook must fire inside the canonicalize→open window"
    );
    let error = result.expect_err("#1494 the swapped-in outside record must never be read back");
    let expected_open_error = format!(
        "ELOOP: too many symbolic links encountered, open '{}'",
        expected_open_path.display()
    );
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        ("ELOOP", expected_open_error.as_str()),
        "#2098 the guarded open must refuse the swapped link at the open itself"
    );
}

/// #1494 (review concurrency#1, mirrored from #1468): the handle check
/// answers with the object's *current* name. Let the open land on an outside
/// regular file through a swapped-in hard link (so the descriptor references
/// the outside record; #2098's guarded open refuses a symlink at the open
/// itself, so the window swap-in is a hard link onto the outside file) and
/// then rename that record over `metadata.json`: the handle now names a
/// location inside the upload root, the prefix check ran on the pre-open
/// spelling and passes, and only the identity recorded before the open
/// exposes the swap. RED against the previous head, where this returned the
/// forged record.
#[cfg(unix)]
#[test]
fn upload_read_at_record_renamed_after_open_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    let outside = project.join("outside-renamed.json");
    let mut forged = parse_json(&fs::read_to_string(&metadata).unwrap()).unwrap();
    set(&mut forged, "forgedMarker", text("outside"));
    fs::write(&outside, json_line(&forged).unwrap()).unwrap();
    let opened = Arc::new(AtomicBool::new(false));
    let renamed = Arc::new(AtomicBool::new(false));
    let link = {
        let opened = opened.clone();
        let metadata = metadata.clone();
        let outside = outside.clone();
        move |_path: &Path| {
            fs::remove_file(&metadata).unwrap();
            fs::hard_link(&outside, &metadata).unwrap();
            opened.store(true, Ordering::SeqCst);
        }
    };
    let rename = {
        let renamed = renamed.clone();
        let metadata = metadata.clone();
        let outside = outside.clone();
        move |_path: &Path| {
            fs::rename(&outside, &metadata).unwrap();
            renamed.store(true, Ordering::SeqCst);
        }
    };
    let _link = swap_test_hook::set(swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen, link);
    let _rename = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, rename);
    let result = h.store.get("upload-1");
    drop(_link);
    drop(_rename);
    assert!(
        opened.load(Ordering::SeqCst),
        "#1494 the canonicalize→open hook must fire"
    );
    assert!(
        renamed.load(Ordering::SeqCst),
        "#1494 the after-open hook must fire — an unfired hook proves nothing"
    );
    let error = result.expect_err("#1494 a renamed-in outside record must never be read back");
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "PROJECT_PATH_FORBIDDEN",
            "Upload metadata handle names a different file than the one validated."
        )
    );
}

/// #1494 (review deepseek concurrency#2 / boundary#1): the "approved" identity
/// must be the object the prefix check validated, not whatever the path points
/// at afterwards. A symlink planted in the `[canonicalize, approved stat]`
/// window is refused by `symlink_metadata`; under the pre-fix `fs::metadata`
/// the approved object becomes the outside inode, the open follows the same
/// link, and the after-open rename gives that outside record an in-root name —
/// every remaining check passes and the forged record is read back.
#[cfg(unix)]
#[test]
fn upload_read_at_metadata_swapped_before_approved_stat_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    let outside = project.join("outside-preapproved.json");
    let mut forged = parse_json(&fs::read_to_string(&metadata).unwrap()).unwrap();
    set(&mut forged, "forgedMarker", text("outside"));
    fs::write(&outside, json_line(&forged).unwrap()).unwrap();
    let swapped = Arc::new(AtomicBool::new(false));
    let renamed = Arc::new(AtomicBool::new(false));
    let link = {
        let swapped = swapped.clone();
        let metadata = metadata.clone();
        let outside = outside.clone();
        move |_path: &Path| {
            fs::remove_file(&metadata).unwrap();
            std::os::unix::fs::symlink(&outside, &metadata).unwrap();
            swapped.store(true, Ordering::SeqCst);
        }
    };
    let rename = {
        let renamed = renamed.clone();
        let metadata = metadata.clone();
        let outside = outside.clone();
        move |_path: &Path| {
            fs::rename(&outside, &metadata).unwrap();
            renamed.store(true, Ordering::SeqCst);
        }
    };
    let _link = swap_test_hook::set(
        swap_test_hook::SwapWindow::BetweenCanonicalizeAndApprovedStat,
        link,
    );
    let _rename = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, rename);
    let result = h.store.get("upload-1");
    drop(_link);
    drop(_rename);
    assert!(
        swapped.load(Ordering::SeqCst),
        "#1494 the pre-approval swap hook must fire before the approved stat"
    );
    negative_assert!(
        !renamed.load(Ordering::SeqCst),
        "#1494 a link planted before the approved stat must be refused before the open"
    );
    let error =
        result.expect_err("#1494 a link planted before the approved stat must never be read back");
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "PROJECT_PATH_FORBIDDEN",
            "Upload metadata path became a symlink after validation."
        )
    );
}

/// #1494 chain segment 2: a forged record whose attachments describe its own
/// outside bytes (path/bytes/sha256 all self-consistent, so `verified`'s
/// integrity check passes) must not reach `acquire_attachment_lease`, which
/// would hard-link those outside bytes into the gateway lease directory.
/// Segment 1 — `read_at` returning the forged record — is
/// `upload_read_at_metadata_swapped_between_canonicalize_and_open_is_rejected`
/// above; the composer's lease handling of such bytes is covered by the
/// existing services tests (same crate, same lease root). #2098's guarded
/// open refuses the swapped link at the open itself, so the acquisition
/// fails with Node's ELOOP open error before any lease is granted — the
/// refusal is the proof the outside bytes never reach the lease. RED against
/// the pre-fix code: the lease is granted over the outside bytes.
#[cfg(unix)]
#[test]
fn upload_read_at_forged_record_cannot_reach_the_lease() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let metadata = metadata_path(&project);
    // The guarded open reports the canonical spelling it was asked to open;
    // sampled before the swap so it is still the real record's path.
    let expected_open_path = fs::canonicalize(&metadata).unwrap();
    let secret = project.join("outside-secret.bin");
    fs::write(&secret, b"outside-secret-bytes").unwrap();
    let mut forged = parse_json(&fs::read_to_string(&metadata).unwrap()).unwrap();
    let mut attachments = array(&forged, "attachments").to_vec();
    let mut attachment = attachments.remove(0);
    set(&mut attachment, "path", path_value(&secret));
    set(
        &mut attachment,
        "bytes",
        JsValue::Number(b"outside-secret-bytes".len() as f64),
    );
    set(
        &mut attachment,
        "sha256",
        text(&hash(b"outside-secret-bytes")),
    );
    attachments.insert(0, attachment);
    set(&mut forged, "attachments", JsValue::Array(attachments));
    set(&mut forged, "forgedMarker", text("outside"));
    let outside = project.join("outside-forged.json");
    fs::write(&outside, json_line(&forged).unwrap()).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        move |_path: &Path| {
            fs::remove_file(&metadata).unwrap();
            std::os::unix::fs::symlink(&outside, &metadata).unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen, swap);
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None);
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1494 the swap hook must fire inside the canonicalize→open window"
    );
    let error = lease
        .err()
        .expect("#1494 a forged record must never reach the attachment lease");
    let expected_open_error = format!(
        "ELOOP: too many symbolic links encountered, open '{}'",
        expected_open_path.display()
    );
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        ("ELOOP", expected_open_error.as_str()),
        "#2098 the guarded open must refuse the swapped link before the lease"
    );
}

/// #1494 (review sibling P1): `verified` checks the attachment through a
/// handle, but the lease `hard_link` re-walks the source path. A leaf renamed
/// in that window gets pinned as the lease artifact; the composer accepts any
/// in-root regular file, so outside bytes would be inlined. The linked inode
/// must be the one the integrity check approved.
#[cfg(unix)]
#[test]
fn upload_lease_link_of_a_swapped_attachment_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let verified = h
        .store
        .verify_attachment("upload-1", "project", None)
        .unwrap();
    let source = PathBuf::from(string(&verified[0], "path").unwrap());
    let outside = project.join("outside-pinned.bin");
    fs::write(&outside, b"outside-pinned-bytes").unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        let source = source.clone();
        let outside = outside.clone();
        move |path: &Path| {
            assert_eq!(path, source.as_path());
            fs::rename(&outside, &source).unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(
        swap_test_hook::SwapWindow::BetweenVerifiedAndLeaseLink,
        swap,
    );
    let result = h
        .store
        .acquire_attachment_lease("upload-1", "project", None);
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1494 the attachment swap hook must fire between verification and the lease link"
    );
    let error = result
        .err()
        .expect("#1494 a swapped attachment must never be pinned into the lease");
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "ATTACHMENT_TAMPERED",
            "Attachment artifact changed between verification and lease acquisition."
        )
    );
}

/// #1494 (review deepseek boundary#1): the swap window can also put a
/// *symlink* at the source spelling. `std::fs::hard_link` does not follow a
/// symlink source on any shipping Unix (`linkat(..., 0)` on macOS as well as
/// Linux, measured 2026-11-06: `symlink_metadata(dest)` reports a symlink
/// after `hard_link(link, dest)`), so the lease entry would be the link
/// itself; `fs::metadata` followed it and compared the target's identity,
/// which is the swap's alibi. The entry is therefore refused outright: a link
/// is never a lease entry.
#[cfg(unix)]
#[test]
fn upload_lease_link_never_pins_a_symlink_source() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let verified = h
        .store
        .verify_attachment("upload-1", "project", None)
        .unwrap();
    let source = PathBuf::from(string(&verified[0], "path").unwrap());
    let moved = project.join("moved-approved.bin");
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        let source = source.clone();
        let moved = moved.clone();
        move |path: &Path| {
            assert_eq!(path, source.as_path());
            fs::rename(&source, &moved).unwrap();
            std::os::unix::fs::symlink(&moved, &source).unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(
        swap_test_hook::SwapWindow::BetweenVerifiedAndLeaseLink,
        swap,
    );
    let result = h
        .store
        .acquire_attachment_lease("upload-1", "project", None);
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1494 the attachment swap hook must fire between verification and the lease link"
    );
    match result {
        Err(error) => assert_eq!(
            (error.code.as_str(), error.message_string().as_str()),
            (
                "ATTACHMENT_TAMPERED",
                "Attachment artifact changed between verification and lease acquisition."
            ),
            "#1494 the lease entry must be a regular file, never a link"
        ),
        Ok(lease) => panic!(
            "#1494 the lease entry must be a regular file, never a link (leased {})",
            string(&lease.attachments[0], "path").unwrap()
        ),
    }
}

/// #1889 (review concurrency#1, measured 2026-11-07
/// `_tmp/20261107-issue-1887/lease-open-probe/`): the identity term and the
/// content term must read one descriptor, not one path twice. Once the entry
/// has been linked, swapping it for a symlink to a byte-identical copy would
/// satisfy a follow-the-path content term while the approved inode is gone,
/// and swapping it for a FIFO would make a plain open wait for a writer that
/// never comes. Both are refused at the descriptor.
#[cfg(unix)]
#[test]
fn upload_lease_link_of_a_swapped_entry_is_not_read_through_the_path() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let approved_bytes = b"approved-bytes-0123456789";
    let orig = project.join("approved.bin");
    fs::write(&orig, approved_bytes).unwrap();
    let mut approved = File::open(&orig).unwrap();
    let sha = hash_file_exact(&mut approved, approved_bytes.len() as u64).unwrap();
    let dest = project.join("entry");
    fs::hard_link(&orig, &dest).unwrap();
    let want = attachment_identity(&fs::symlink_metadata(&dest).unwrap()).unwrap();

    // 1. Symlink to a byte-identical copy: the bytes match, the object does not.
    let copy = project.join("copy.bin");
    fs::write(&copy, approved_bytes).unwrap();
    fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink(&copy, &dest).unwrap();
    assert!(
        !lease_link_matches(&dest, Some(want), &sha, approved_bytes.len() as f64),
        "#1889 a symlink swapped into the lease entry must never be followed"
    );

    // 2. FIFO: the open must return instead of waiting for a writer.
    fs::remove_file(&dest).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(&dest)
        .status()
        .expect("#1889 probe could not run mkfifo");
    assert!(
        made.success(),
        "#1889 probe could not create the FIFO entry"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let path = dest.clone();
    std::thread::spawn(move || {
        let _ = tx.send(lease_link_matches(
            &path,
            Some(want),
            &sha,
            approved_bytes.len() as f64,
        ));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(matched) => assert!(
            !matched,
            "#1889 a FIFO swapped into the lease entry must never be accepted"
        ),
        Err(_) => panic!("#1889 a FIFO entry must never make the lease acquire wait"),
    }
}

/// #1889 (review concurrency#1 / boundary#1): the content term reads exactly
/// the record's `bytes` and requires the entry to end there. An append to the
/// approved inode leaves both identity terms equal and the leading bytes
/// intact, so only the length term can refuse it — and without that bound the
/// read would run to whatever EOF the writer allows, up to never.
#[cfg(unix)]
#[test]
fn upload_lease_link_of_a_grown_entry_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let approved_bytes = b"approved-bytes-0123456789";
    let orig = project.join("approved.bin");
    fs::write(&orig, approved_bytes).unwrap();
    let mut approved = File::open(&orig).unwrap();
    let sha = hash_file_exact(&mut approved, approved_bytes.len() as u64).unwrap();
    let dest = project.join("entry");
    fs::hard_link(&orig, &dest).unwrap();
    let want = attachment_identity(&fs::symlink_metadata(&dest).unwrap()).unwrap();
    let mut grown = fs::OpenOptions::new().append(true).open(&orig).unwrap();
    grown.write_all(b"appended-after-verification").unwrap();
    drop(grown);
    assert!(
        !lease_link_matches(&dest, Some(want), &sha, approved_bytes.len() as f64),
        "#1889 an entry that grew after verification must never be pinned into the lease"
    );
}

/// #2098 (S1, FIFO T0): the attachment leaf is a FIFO before `verify` runs.
/// The guarded open returns at once (`O_NONBLOCK`) and the handle's own type
/// term refuses it, so the verification answer — not the calling thread — is
/// what the watchdog measures. The TS baseline shape (coordinator probe,
/// node v22.22.0, 2026-11-11) blocks `sha256File` past 3 s on the same swap;
/// refusing at once is the registered hardening (A7.2-ATTACH-HANDLE-BINDING-003).
#[cfg(unix)]
#[test]
fn upload_verify_attachment_of_a_fifo_leaf_is_rejected_within_the_watchdog() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let leaf = artifact_path(&project);
    fs::remove_file(&leaf).expect("#2098 remove the stored leaf for the FIFO swap");
    let made = std::process::Command::new("mkfifo")
        .arg(&leaf)
        .status()
        .expect("#2098 run mkfifo for the FIFO leaf fixture");
    assert!(
        made.success(),
        "#2098 create the FIFO leaf fixture for the verification watchdog"
    );
    let store = h.store.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            store
                .verify_attachment("upload-1", "project", None)
                .map(|_| ())
                .map_err(|e| (e.code.clone(), e.message_string())),
        );
    });
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Err((code, message))) => {
            assert_eq!(
                code.as_str(),
                "ATTACHMENT_TAMPERED",
                "#2098 a FIFO leaf must fail the attachment integrity check"
            );
            assert!(
                message.starts_with("Attachment integrity check failed"),
                "#2098 the FIFO leaf must be refused by the integrity term: {message}"
            );
        }
        Ok(Ok(())) => panic!("#2098 a FIFO leaf must never pass attachment verification"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("#2098 a FIFO leaf must never make verification wait for a writer")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("#2098 the verification thread died before reporting (fixture or product panic)")
        }
    }
}

/// #2098 (S1, symlink T1): the leaf swapped for a symlink to a byte-identical
/// copy. The copy satisfies both the length and the digest, so only the
/// no-follow half of the guarded open can refuse it — following the link
/// would hand `verified` (and the lease it feeds) the copy's bytes as if they
/// were the stored artifact's.
#[cfg(unix)]
#[test]
fn upload_verify_attachment_never_follows_a_symlink_leaf() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let leaf = artifact_path(&project);
    let bytes = fs::read(&leaf).expect("#2098 read the approved leaf bytes");
    let copy = project.join("copy-of-the-leaf.bin");
    fs::write(&copy, &bytes).expect("#2098 write the byte-identical leaf copy");
    fs::remove_file(&leaf).expect("#2098 remove the stored leaf for the symlink swap");
    std::os::unix::fs::symlink(&copy, &leaf)
        .expect("#2098 swap the leaf for a symlink to the copy");
    let error = h
        .store
        .verify_attachment("upload-1", "project", None)
        .expect_err("#2098 a symlink leaf must never pass attachment verification");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#2098 a symlink swapped for the leaf must be refused at the open"
    );
}

/// #2098 (S1, sparse): the leaf extended in place into a sparse file far
/// larger than the record's `bytes`. The unbounded-read face of the bare
/// digest — TS read 7.5 GB in 3 s without EOF on the same shape — is refused
/// by the length term taken on the handle.
#[cfg(unix)]
#[test]
fn upload_verify_attachment_of_an_oversized_sparse_leaf_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let leaf = artifact_path(&project);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&leaf)
        .expect("#2098 open the stored leaf for the sparse extension");
    file.set_len(1024 * 1024 * 1024)
        .expect("#2098 extend the leaf into a sparse file past the record bytes");
    drop(file);
    let error = h
        .store
        .verify_attachment("upload-1", "project", None)
        .expect_err("#2098 a sparse leaf larger than the record bytes must never verify");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#2098 the length term must refuse the sparse leaf"
    );
}

/// #2098 (S1, same-inode append): the leaf grows after the record was
/// written, and the record is forged to describe the grown content — the
/// record's `bytes` stays at the approved length while its `sha256` is the
/// digest of the grown file. Reading to EOF and skipping the length term
/// would accept exactly this record; the length terms (`fstat` length and the
/// must-end-there read) refuse it.
#[cfg(unix)]
#[test]
fn upload_verify_attachment_of_a_grown_leaf_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let leaf = artifact_path(&project);
    let approved = fs::read(&leaf).expect("#2098 read the approved leaf bytes");
    let mut grown = fs::OpenOptions::new()
        .append(true)
        .open(&leaf)
        .expect("#2098 open the leaf for the append");
    grown
        .write_all(b"-appended-after-verification")
        .expect("#2098 append to the approved inode");
    drop(grown);
    let metadata = metadata_path(&project);
    let mut record = parse_json(
        &fs::read_to_string(&metadata).expect("#2098 read the record for the forged digest"),
    )
    .expect("#2098 parse the record for the forged digest");
    let mut attachments = array(&record, "attachments").to_vec();
    assert!(
        !attachments.is_empty(),
        "#2098 the completed record carries its attachment"
    );
    let mut attachment = attachments.remove(0);
    set(
        &mut attachment,
        "sha256",
        text(&hash(
            &fs::read(&leaf).expect("#2098 read the grown leaf bytes"),
        )),
    );
    attachments.insert(0, attachment);
    set(&mut record, "attachments", JsValue::Array(attachments));
    fs::write(
        &metadata,
        json_line(&record).expect("#2098 serialize the forged record"),
    )
    .expect("#2098 write the forged record");
    assert!(
        number(&array(&record, "attachments")[0], "bytes") == approved.len() as f64,
        "#2098 the forged record must keep the approved `bytes` length"
    );
    let error = h
        .store
        .verify_attachment("upload-1", "project", None)
        .expect_err("#2098 a grown leaf must never pass attachment verification");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#2098 the record's length terms must refuse the same-inode append"
    );
}

/// #2098 fix-r2 (review MM concurrency#1): the absolute ceiling on the
/// summary read. The record's `bytes` and `sha256` are forged together with
/// a sparse leaf of exactly that length — type, length, and digest all
/// agree, so without the ceiling this record verifies and the digest read
/// streams the claimed `bytes` off the disk. The write side caps a real
/// attachment at 1 GiB (`options.max_file_bytes`), so a larger `bytes`
/// cannot describe a record this gateway wrote; it is refused as the same
/// integrity failure as the other forged faces.
#[test]
fn upload_verify_attachment_bytes_over_the_absolute_cap_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let leaf = artifact_path(&project);
    let approved = fs::read(&leaf).expect("#2098 read the approved leaf bytes");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&leaf)
        .expect("#2098 open the stored leaf for the sparse forged length");
    file.set_len(MAX_ATTACHMENT_READ_BYTES + 1)
        .expect("#2098 extend the leaf to the forged over-cap length");
    drop(file);
    let metadata = metadata_path(&project);
    let mut record = parse_json(
        &fs::read_to_string(&metadata).expect("#2098 read the record for the forged bytes"),
    )
    .expect("#2098 parse the record for the forged bytes");
    let mut attachments = array(&record, "attachments").to_vec();
    assert!(
        !attachments.is_empty(),
        "#2098 the completed record carries its attachment"
    );
    let mut attachment = attachments.remove(0);
    // The digest of exactly MAX_ATTACHMENT_READ_BYTES + 1 bytes as the
    // extended leaf holds them — the approved bytes, then sparse zeros.
    // With the ceiling dropped this is what `hash_file_exact` computes off
    // the leaf, so the record is internally consistent and only the cap
    // refuses it. Streamed, not allocated: the forged length is 1 GiB + 1.
    let mut digest = Sha256::new();
    digest.update(&approved);
    let zeros = [0_u8; 64 * 1024];
    let mut left = MAX_ATTACHMENT_READ_BYTES + 1 - approved.len() as u64;
    while left > 0 {
        let take = zeros.len().min(left as usize);
        digest.update(&zeros[..take]);
        left -= take as u64;
    }
    set(
        &mut attachment,
        "sha256",
        text(&format!("{:x}", digest.finalize())),
    );
    set(
        &mut attachment,
        "bytes",
        JsValue::Number((MAX_ATTACHMENT_READ_BYTES + 1) as f64),
    );
    attachments.insert(0, attachment);
    set(&mut record, "attachments", JsValue::Array(attachments));
    fs::write(
        &metadata,
        json_line(&record).expect("#2098 serialize the forged record"),
    )
    .expect("#2098 write the forged record");
    let error = h
        .store
        .verify_attachment("upload-1", "project", None)
        .expect_err("#2098 a forged bytes past the absolute cap must never verify");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#2098 the absolute ceiling must refuse the over-cap summary read"
    );
}

/// #2098 (S2, FIFO T1): `metadata.json` swapped for a FIFO inside
/// `read_at`'s canonicalize→open window. The approved stat only refuses a
/// symlink, so the FIFO passes it; the guarded open returns at once and the
/// handle's type term answers inside the watchdog. The TS baseline shape
/// blocks `readFile` past 3 s on the same swap (coordinator probe,
/// node v22.22.0, 2026-11-11).
#[cfg(unix)]
#[test]
fn upload_read_at_metadata_swapped_for_a_fifo_is_rejected_within_the_watchdog() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    let fired = Arc::new(AtomicBool::new(false));
    let store = h.store.clone();
    let (tx, rx) = mpsc::channel();
    let fired_in_thread = Arc::clone(&fired);
    std::thread::spawn(move || {
        let swap = {
            let fired = fired_in_thread;
            let metadata = metadata.clone();
            move |_path: &Path| {
                fs::remove_file(&metadata).expect("#2098 remove the record leaf for the FIFO swap");
                let made = std::process::Command::new("mkfifo")
                    .arg(&metadata)
                    .status()
                    .expect("#2098 run mkfifo for the FIFO record fixture");
                assert!(
                    made.success(),
                    "#2098 create the FIFO record fixture for the read watchdog"
                );
                fired.store(true, Ordering::SeqCst);
            }
        };
        let _guard =
            swap_test_hook::set(swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen, swap);
        let _ = tx.send(
            store
                .get("upload-1")
                .map(|_| ())
                .map_err(|e| (e.code.clone(), e.message_string())),
        );
    });
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Err((code, message))) => {
            assert!(
                fired.load(Ordering::SeqCst),
                "#2098 the FIFO swap hook must fire inside the canonicalize→open window"
            );
            assert_eq!(
                code.as_str(),
                "PROJECT_PATH_FORBIDDEN",
                "#2098 a FIFO swapped for the record must be refused as non-regular"
            );
            assert_eq!(
                message.as_str(),
                "Upload metadata path became a non-regular file after validation.",
                "#2098 the FIFO record must be refused by the type term"
            );
        }
        Ok(Ok(())) => panic!("#2098 a FIFO record must never be read back"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("#2098 a FIFO record must never make the read wait for a writer")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("#2098 the read thread died before reporting (fixture or product panic)")
        }
    }
}

/// #2098 (S2, symlink T1): `metadata.json` swapped for a symlink to
/// `/dev/zero` inside the same window — the unbounded-read face (TS read
/// 2.7 GB in 3 s without EOF). `O_NOFOLLOW` refuses the link at the open
/// itself; following it would stream the device through the record read.
#[cfg(unix)]
#[test]
fn upload_read_at_metadata_swapped_for_a_dev_zero_symlink_is_rejected_at_open() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    let swap = {
        let metadata = metadata.clone();
        move |_path: &Path| {
            fs::remove_file(&metadata)
                .expect("#2098 remove the record leaf for the /dev/zero swap");
            std::os::unix::fs::symlink("/dev/zero", &metadata)
                .expect("#2098 point the record leaf at /dev/zero");
        }
    };
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen, swap);
    let result = h.store.get("upload-1");
    drop(_guard);
    let error =
        result.expect_err("#2098 a /dev/zero symlink must never be read through as a record");
    assert_eq!(
        error.code.as_str(),
        "ELOOP",
        "#2098 the guarded open must refuse the swapped link at the open itself"
    );
}

/// #2098 (S2, sparse): the record leaf extended in place past the metadata
/// size cap — same inode, same approved identity, only the length term can
/// refuse it before the read pulls the sparse bytes in.
#[test]
fn upload_read_at_metadata_over_the_size_cap_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let metadata = metadata_path(&project);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&metadata)
        .expect("#2098 open the record leaf for the sparse extension");
    file.set_len(MAX_METADATA_JSON_BYTES + 1)
        .expect("#2098 extend the record leaf past the size cap");
    drop(file);
    let error = h
        .store
        .get("upload-1")
        .expect_err("#2098 an oversized record leaf must be refused");
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "PROJECT_PATH_FORBIDDEN",
            "Upload metadata file exceeds the supported size."
        ),
        "#2098 a sparse record leaf past the cap must be refused before the read"
    );
}

/// #2098 fix-r2 (review DS#1, option b): one record leaf past the read cap
/// must not poison the project listing. The write side can reach ~100 MiB of
/// record JSON in one frame (`name`/`relativePath` carry no length term)
/// while the read cap is 64 MiB, so the strict `?` in `list_in_project`
/// turned one such record into every later `create` of that project failing
/// with the read-side refusal. The listing now skips an unreadable record
/// with the sweep's own logged-skip 口径; the readable records — and a new
/// `create` — keep their exact semantics.
#[test]
fn upload_create_survives_a_sibling_record_over_the_read_cap() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    created(&h);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(metadata_path(&project))
        .expect("#2098 open the sibling record leaf for the sparse extension");
    file.set_len(MAX_METADATA_JSON_BYTES + 1)
        .expect("#2098 extend the sibling record leaf past the read cap");
    drop(file);
    let manifest = value(&crate::test_baseline::oracle(ORACLE), "manifest");
    let record = h
        .store
        .create("project", &manifest, None)
        .expect("#2098 an oversized sibling record must not poison the project listing");
    assert_eq!(
        status(&h.store.get(&string(&record, "uploadId").unwrap()).unwrap()),
        "created",
        "#2098 the new record round-trips through the listing that skipped its sibling"
    );
}

/// #1887: the read side of the shared "home" question. A `.tmp` swapped for a
/// link to another subtree of the same project keeps the canonical spelling
/// inside the project, so a canonical-containment check alone would accept the
/// record; the lexical prefix check refuses it. The attachment-composition
/// side (`native_process/services_tests.rs::lease_root_under_a_linked_tmp_ancestor_is_not_a_home`)
/// is the pair of this test — the two assertion strings are deliberately
/// different, so a negative control proves which side lost its guard.
#[cfg(unix)]
#[test]
fn upload_read_at_rejects_a_linked_tmp_ancestor() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let record = fs::read(project.join(".tmp/chat-uploads/upload-1/metadata.json")).unwrap();
    // The store's own bytes move under the link target, so the canonical
    // spelling of the record now resolves to `<project>/src/...`.
    let moved = project.join("src/chat-uploads/upload-1");
    fs::create_dir_all(&moved).unwrap();
    fs::write(moved.join("metadata.json"), &record).unwrap();
    fs::remove_dir_all(project.join(".tmp")).unwrap();
    std::os::unix::fs::symlink(project.join("src"), project.join(".tmp"))
        .expect("#1887 swap .tmp for an in-project link");
    let error = h.store.get("upload-1").expect_err(
        "#1887 a .tmp ancestor that is a link into the project must not be read through",
    );
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "PROJECT_PATH_FORBIDDEN",
            "Upload metadata symlink leaves the upload directory."
        ),
        "#1887 the read side must keep its lexical prefix check"
    );
}

/// #1888: the writer's half of the shared home guard. The lease root used to
/// be joined straight off `projectKey` and created through whatever was there,
/// so a linked root took the write while the read side refused it; the lease
/// would be written and then answer not-found.
#[cfg(unix)]
#[test]
fn upload_lease_root_link_is_refused() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let elsewhere = project.join("elsewhere-leases");
    fs::create_dir_all(&elsewhere).unwrap();
    let root = project.join(".tmp/chat-upload-leases");
    let _ = fs::remove_dir_all(&root);
    std::os::unix::fs::symlink(&elsewhere, &root).expect("#1888 swap the lease root for a link");
    let error = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .expect("#1888 the writer must not create through a linked lease root");
    assert_eq!(
        (error.code.as_str(), error.message_string().as_str()),
        (
            "PROJECT_PATH_FORBIDDEN",
            "Attachment lease root is unavailable."
        ),
        "#1888 the writer must not create through a linked lease root"
    );
    assert!(
        !elsewhere.join("upload-1").exists(),
        "#1888 the writer must not create the lease under the link target"
    );
}

/// #1889: `(dev, inode)` alone cannot bind the lease. Measured 2026-11-07
/// (`_tmp/20261107-issue-1887/inode-probe/`): ext4 hands a recreated file the
/// just-freed inode number deterministically (20000/20000), while `link(2)`
/// bumps the inode ctime on every filesystem (5/5), so a ctid term cannot help
/// either. The auxiliary identity is the content digest `verified` already
/// checked against the opened handle; rewriting the file in place keeps the
/// inode *and* the size, so only the digest term can refuse this.
#[cfg(unix)]
#[test]
fn upload_lease_link_of_a_rewritten_attachment_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let verified = h
        .store
        .verify_attachment("upload-1", "project", None)
        .unwrap();
    let source = PathBuf::from(string(&verified[0], "path").unwrap());
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        let source = source.clone();
        move |path: &Path| {
            assert_eq!(path, source.as_path());
            // Same inode, same length, different bytes.
            fs::write(&source, b"HELLO").unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(
        swap_test_hook::SwapWindow::BetweenVerifiedAndLeaseLink,
        swap,
    );
    let result = h
        .store
        .acquire_attachment_lease("upload-1", "project", None);
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1889 the rewrite hook must fire between verification and the lease link"
    );
    let error = result
        .err()
        .expect("#1889 a rewritten artifact must never be pinned into the lease");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#1889 a rewritten artifact must be diagnosed as tampering"
    );
}

/// #1889, the reported scenario: delete the approved file and recreate it at
/// the same path inside the window. On ext4 the recreated file gets the same
/// inode number, so the identity term passes and only the digest term refuses
/// it; on APFS the inode itself differs. Either way nothing is pinned.
#[cfg(unix)]
#[test]
fn upload_lease_link_of_a_recreated_attachment_is_rejected() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let verified = h
        .store
        .verify_attachment("upload-1", "project", None)
        .unwrap();
    let source = PathBuf::from(string(&verified[0], "path").unwrap());
    let fired = Arc::new(AtomicBool::new(false));
    let swap = {
        let fired = fired.clone();
        let source = source.clone();
        move |path: &Path| {
            assert_eq!(path, source.as_path());
            fs::remove_file(&source).unwrap();
            fs::write(&source, b"HELLO").unwrap();
            fired.store(true, Ordering::SeqCst);
        }
    };
    let _guard = swap_test_hook::set(
        swap_test_hook::SwapWindow::BetweenVerifiedAndLeaseLink,
        swap,
    );
    let result = h
        .store
        .acquire_attachment_lease("upload-1", "project", None);
    drop(_guard);
    assert!(
        fired.load(Ordering::SeqCst),
        "#1889 the recreate hook must fire between verification and the lease link"
    );
    let error = result
        .err()
        .expect("#1889 a recreated artifact must never be pinned into the lease");
    assert_eq!(
        error.code.as_str(),
        "ATTACHMENT_TAMPERED",
        "#1889 a recreated artifact must be diagnosed as tampering"
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

#[test]
fn upload_mkdir_failure_matches_real_ts_caller() {
    let raw = include_str!("../../tests/fixtures/mkdir-boundary/oracle.json");
    crate::test_baseline::oracle(raw);
    let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
    let row = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "upload")
        .unwrap();
    let scratch = Scratch::new();
    let project = scratch.dir("upload");
    fs::write(project.join(".tmp"), b"block").unwrap();
    let h = harness(&project, &project, &JsValue::Undefined);
    let manifest = parse_json(r#"[{"clientFileId":"f","name":"a.txt","relativePath":"a.txt","size":1,"mimeType":"text/plain"}]"#).unwrap();
    let error = h.store.create("p", &manifest, None).unwrap_err();
    assert_eq!(error.code, row["observed"]["code"]);
    assert_eq!(
        error
            .message_string()
            .replace(&scratch.0.to_string_lossy().into_owned(), "<dir>"),
        row["observed"]["message"]
    );
}

// #1439: the native assembly is the only production caller of
// `cleanup_expired`. These pin the schedule the TS baseline owns
// (`ui/server/routes/uploads.js:73-76`): the literal 15 min interval, the
// `Weak` exit that stands in for `unref()`, and "a failed sweep does not end
// the schedule".
fn seed_cleanup_record(uploads: &Path, project_key: &Path, id: &str, expires_at: &str) {
    let dir = uploads.join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut record = JsValue::Object(JsObject::new());
    set(&mut record, "uploadId", text(id));
    set(&mut record, "projectKey", path_value(project_key));
    set(&mut record, "status", text("created"));
    set(&mut record, "expiresAt", text(expires_at));
    let line = stringify_json(&record).unwrap().unwrap();
    fs::write(dir.join("metadata.json"), format!("{line}\n")).unwrap();
}

/// Pin a directory's mtime so the orphan-retention check is judged against the
/// store's fake clock, not the real one. A read-only open is enough: the test
/// process owns the scratch tree, and `O_WRONLY` cannot open a directory.
fn set_mtime(path: &Path, millis: u64) {
    let dir = fs::File::open(path).unwrap();
    dir.set_modified(UNIX_EPOCH + Duration::from_millis(millis))
        .unwrap();
}

// #1646: one bad record must no longer cost the round, the good records, or
// the lease sweep that follows it.
#[test]
fn upload_cleanup_bad_record_does_not_abort_the_sweep() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    // "upload-bad" sorts first: the corrupt record is met before the good one.
    seed_cleanup_record(&uploads, &project, "upload-bad", "1970-01-01T00:00:00.000Z");
    seed_cleanup_record(
        &uploads,
        &project,
        "upload-good",
        "1970-01-01T00:00:00.000Z",
    );
    fs::write(uploads.join("upload-bad/metadata.json"), "{").unwrap();
    let lease = project.join(".tmp/chat-upload-leases/upload-bad/l1");
    fs::create_dir_all(&lease).unwrap();
    fs::write(
        lease.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();
    let cleaned = h
        .store
        .cleanup_expired()
        .expect("bad record aborts the sweep");
    assert_eq!(cleaned, 1, "bad record aborts the sweep");
    assert!(!uploads.join("upload-good").exists());
    assert!(
        uploads.join("upload-bad").exists(),
        "a corrupt record is left in place, not guessed at"
    );
    assert!(
        !lease.exists(),
        "the lease sweep still runs after the bad record"
    );
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    let bad_path = uploads.join("upload-bad").to_string_lossy().into_owned();
    assert!(
        log.contains(&bad_path),
        "the skip log names the bad record's path: {log}"
    );
}

// #1646: a task directory without metadata.json never enters the record sweep;
// Rust reclaims it once it is older than the upload retention.
#[test]
fn upload_cleanup_reclaims_expired_orphan_directories() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    let orphan = uploads.join("upload-orphan");
    fs::create_dir_all(orphan.join("files")).unwrap();
    fs::write(orphan.join("files/x"), "debris").unwrap();
    set_mtime(&orphan, (EPOCH - DAY - 1) as u64);
    let empty = uploads.join("upload-empty");
    fs::create_dir_all(&empty).unwrap();
    set_mtime(&empty, (EPOCH - 2 * DAY) as u64);
    let cleaned = h.store.cleanup_expired().unwrap();
    assert_eq!(
        cleaned, 0,
        "orphan reclamation keeps the return value's TS meaning (records removed)"
    );
    assert!(
        !orphan.exists(),
        "an orphan past the upload retention is reclaimed, files and all"
    );
    assert!(!empty.exists(), "an old empty orphan is reclaimed");
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(log.contains("reclaimed orphan"), "{log}");
}

// #1646 review r5: the orphan branch deletes too, so its failure path needs
// the same teeth as the record branch's `record-rm`. Without an injection
// point nothing exercises the "unremovable orphan" arm at all, and a
// regression that swallows the error (or ends the round on it) stays green.
#[test]
fn upload_cleanup_orphan_removal_failure_does_not_stop_the_sweep() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    // The one-shot fault lands on the first entry `names()` yields, which is
    // byte-sorted: `upload-a-orphan` before `upload-b-orphan`.
    let unremovable = uploads.join("upload-a-orphan");
    fs::create_dir_all(unremovable.join("files")).unwrap();
    fs::write(unremovable.join("files/x"), "debris").unwrap();
    set_mtime(&unremovable, (EPOCH - DAY - 1) as u64);
    let next = uploads.join("upload-b-orphan");
    fs::create_dir_all(next.join("files")).unwrap();
    set_mtime(&next, (EPOCH - DAY - 1) as u64);
    seed_cleanup_record(
        &uploads,
        &project,
        "upload-expired",
        "1970-01-01T00:00:00.000Z",
    );
    let lease = project.join(".tmp/chat-upload-leases/upload-expired/l1");
    fs::create_dir_all(&lease).unwrap();
    fs::write(
        lease.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();
    inject(&h, "orphan-rm", libc::EACCES);
    let cleaned = h.store.cleanup_expired().unwrap();
    assert_eq!(
        cleaned, 1,
        "the record half still runs after a failed orphan"
    );
    assert!(
        unremovable.exists(),
        "an unremovable orphan stays for the next sweep"
    );
    assert!(
        !next.exists(),
        "the sweep continues past an unremovable orphan"
    );
    assert!(!lease.exists(), "the lease half still runs");
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("upload-a-orphan"),
        "a failed orphan removal is logged with its path: {log}"
    );
}

// #1646 pairing case: a fresh orphan — a create whose metadata write has not
// happened yet — must survive the sweep.
#[test]
fn upload_cleanup_keeps_fresh_orphan_directories() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    let fresh = uploads.join("upload-fresh");
    fs::create_dir_all(fresh.join("files")).unwrap();
    set_mtime(&fresh, EPOCH as u64);
    let young = uploads.join("upload-young");
    fs::create_dir_all(&young).unwrap();
    set_mtime(&young, (EPOCH - DAY + 1) as u64);
    h.store.cleanup_expired().unwrap();
    assert!(
        fresh.join("files").exists(),
        "a just-created orphan must survive its metadata write window"
    );
    assert!(
        young.exists(),
        "an orphan younger than the retention must survive"
    );
}

// #1646: the sweep must never act through a symlinked uploads root —
// `remove_dir_all` follows intermediate links, so a crafted project tree could
// otherwise delete directories outside the project (the orphan branch has no
// metadata.json for `read_at`'s containment check to validate).
#[test]
fn upload_cleanup_never_deletes_through_a_symlinked_root() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside");
    let victim = outside.join("victim");
    fs::create_dir_all(victim.join("files")).unwrap();
    fs::write(victim.join("files/x"), "keep").unwrap();
    set_mtime(&victim, (EPOCH - 2 * DAY) as u64);
    // (a) `.tmp/chat-uploads` itself points at the outside tree.
    let project = scratch.dir("p-root-link");
    let h = golden_store(&project);
    fs::create_dir_all(project.join(".tmp")).unwrap();
    std::os::unix::fs::symlink(&outside, project.join(".tmp/chat-uploads")).unwrap();
    h.store.cleanup_expired().unwrap();
    assert!(
        victim.join("files/x").exists(),
        "a symlinked uploads root must not be swept"
    );
    // (b) `.tmp` is the link, so the real root lives outside the project.
    let nest = scratch.dir("outside-nested");
    let nested_victim = nest.join("chat-uploads/victim");
    fs::create_dir_all(nested_victim.join("files")).unwrap();
    fs::write(nested_victim.join("files/x"), "keep").unwrap();
    set_mtime(&nested_victim, (EPOCH - 2 * DAY) as u64);
    let project = scratch.dir("p-tmp-link");
    let h = golden_store(&project);
    std::os::unix::fs::symlink(&nest, project.join(".tmp")).unwrap();
    h.store.cleanup_expired().unwrap();
    assert!(
        nested_victim.join("files/x").exists(),
        "a symlinked .tmp must not be swept"
    );
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("uploads root is not a plain directory"),
        "the skip is logged: {log}"
    );
}

// #1646 review r4: the gate is an equality test on the resolved path, not a
// "is it inside the project" test. Two different arms refuse a root that
// resolves elsewhere, and both are pinned here:
//
// * a symlinked `.tmp/chat-uploads` is caught by the leading directory stats
//   (a symlink is not a directory to `symlink_metadata`);
// * a root that *stats* as a directory but resolves to a different path — the
//   aliased project below — can only be caught by the equality comparison.
#[test]
fn plain_root_rejects_a_root_that_resolves_elsewhere() {
    let scratch = Scratch::new();
    let project = scratch.dir("p-plain-root");
    fs::create_dir_all(project.join(".tmp")).unwrap();
    let root = project.join(".tmp/chat-uploads");

    // Inside the project but not the uploads root.
    let sibling = project.join("important");
    fs::create_dir_all(sibling.join("victim")).unwrap();
    std::os::unix::fs::symlink(&sibling, &root).unwrap();
    assert!(
        matches!(plain_root(&project, "chat-uploads"), PlainRoot::Blocked),
        "a root linked to another directory inside the project must be blocked"
    );

    // Outside the project.
    let outside = scratch.dir("outside-plain");
    fs::remove_file(&root).unwrap();
    std::os::unix::fs::symlink(&outside, &root).unwrap();
    assert!(
        matches!(plain_root(&project, "chat-uploads"), PlainRoot::Blocked),
        "a root linked outside the project must be blocked"
    );

    // A real directory is accepted, and the resolved path is what the round
    // snapshots.
    fs::remove_file(&root).unwrap();
    fs::create_dir(&root).unwrap();
    match plain_root(&project, "chat-uploads") {
        PlainRoot::Plain(resolved) => {
            assert_eq!(resolved, fs::canonicalize(&root).unwrap());
            assert_eq!(resolved, root);
        }
        _ => panic!("a plain uploads root is accepted"),
    }

    // Both stats say "a directory is here", yet the resolve lands elsewhere:
    // the project itself is reached through a symlink. Only the equality arm
    // can refuse this, so this cell is what makes
    // `plain-root-accepts-any-resolve` (the pre-r4 "resolve and accept" shape)
    // a killed mutation instead of a green one.
    let real = scratch.dir("p-plain-alias-real");
    fs::create_dir_all(real.join(".tmp/chat-uploads")).unwrap();
    let aliased = real.parent().unwrap().join("p-plain-alias-link");
    std::os::unix::fs::symlink(&real, &aliased).unwrap();
    assert!(
        matches!(plain_root(&aliased, "chat-uploads"), PlainRoot::Blocked),
        "a root whose resolve lands on a different literal path must be blocked"
    );
}

// #1646 review r2: the plain-root gate only covers the moment the round
// starts. If `.tmp` is swapped for a symlink *while* the round runs, every
// later delete must still be measured against the root the round snapshotted —
// re-resolving the root per entry would resolve through the new link and make
// both sides of the containment check agree (codex P0 / GLM concurrency P1).
//
// The swap happens inside the `now` callback: after the round's plain check and
// its root snapshot, and before the recursive delete. That is the whole-round
// window a crafted tree gets, with no timing luck involved. `fire_at` picks
// which clock read does the swapping — the sweep paths pass 0 (the first read
// is already past their snapshot point), the cancel paths pass 1 so the swap
// lands after `mutate`'s in-gate re-read (#2279 refuses a record the swap has
// already removed) and before the metadata write and the guarded delete.
fn store_swapping_tmp(
    project: &Path,
    outside: &Path,
    clock: Arc<AtomicI64>,
    swaps: Arc<AtomicUsize>,
    fire_at: usize,
) -> Arc<UploadStore> {
    let (resolved, listed) = (project.to_owned(), project.to_owned());
    let (root, target) = (project.to_owned(), outside.to_owned());
    let (tick, swap) = (clock, swaps);
    let mut opts = UploadStoreOptions::new(
        move |_: &JsString| Ok(resolved.clone()),
        move || Ok(vec![listed.clone()]),
    );
    opts.now = Arc::new(move || {
        if swap.fetch_add(1, Ordering::SeqCst) == fire_at {
            fs::rename(root.join(".tmp"), root.join(".tmp.real")).unwrap();
            std::os::unix::fs::symlink(&target, root.join(".tmp")).unwrap();
        }
        tick.load(Ordering::SeqCst)
    });
    Arc::new(UploadStore::new(opts))
}

#[test]
fn upload_cleanup_does_not_follow_a_root_swapped_mid_round() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap");
    let victim = outside.join("chat-uploads/upload-orphan");
    fs::create_dir_all(victim.join("files")).unwrap();
    fs::write(victim.join("files/x"), "keep").unwrap();
    set_mtime(&victim, (EPOCH - 2 * DAY) as u64);

    let project = scratch.dir("p-swap");
    let orphan = project.join(".tmp/chat-uploads/upload-orphan");
    fs::create_dir_all(orphan.join("files")).unwrap();
    fs::write(orphan.join("files/x"), "own").unwrap();
    set_mtime(&orphan, (EPOCH - 2 * DAY) as u64);

    let swaps = Arc::new(AtomicUsize::new(0));
    let store = store_swapping_tmp(
        &project,
        &outside,
        Arc::new(AtomicI64::new(EPOCH)),
        swaps.clone(),
        0,
    );
    store.cleanup_expired().unwrap();

    assert_eq!(
        swaps.load(Ordering::SeqCst),
        1,
        "the sweep reached the clock, so the swap happened mid-round"
    );
    assert!(
        victim.join("files/x").exists(),
        "a root swapped mid-round must not be followed"
    );
}

// #1646 review r2: the same swap, but the delete that must be refused is the
// expired-**record** one — it follows `read_at`, whose containment check ran
// before the swap and does not cover the delete.
#[test]
fn upload_cleanup_record_removal_does_not_follow_a_root_swapped_mid_round() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap-record");
    let victim = outside.join("chat-uploads/upload-a/files/x");
    fs::create_dir_all(victim.parent().unwrap()).unwrap();
    fs::write(&victim, "keep").unwrap();

    let project = scratch.dir("p-swap-record");
    let h = golden_store(&project);
    seed_cleanup_record(
        &project.join(".tmp/chat-uploads"),
        &project,
        "upload-a",
        "1970-01-01T00:00:00.000Z",
    );
    drop(h);

    let swaps = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicI64::new(EPOCH));
    let store = store_swapping_tmp(&project, &outside, clock.clone(), swaps.clone(), 0);
    store.cleanup_expired().unwrap();

    assert_eq!(
        swaps.load(Ordering::SeqCst),
        1,
        "the swap happened mid-round"
    );
    assert!(
        victim.exists(),
        "a record delete must not follow a root swapped mid-round"
    );
}

// #1646 review r2 (GLM sibling / concurrency / boundary, P0): the lease half
// follows a symlinked **entry**, not just a symlinked root. "No lease.json or
// unparsable means expired" makes the recursive delete the default action for
// whatever such an entry points at.
#[test]
fn upload_lease_cleanup_never_deletes_through_an_entry_symlink() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-entry");
    let leaked = outside.join("lease-a");
    fs::create_dir_all(leaked.join("files")).unwrap();
    fs::write(leaked.join("files/x"), "keep").unwrap();

    let project = scratch.dir("p-entry-link");
    let h = golden_store(&project);
    let root = project.join(".tmp/chat-upload-leases");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("upload-a")).unwrap();
    h.store.cleanup_expired().unwrap();

    assert!(
        leaked.join("files/x").exists(),
        "a symlinked lease entry must not be swept"
    );
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("outside the leases root"),
        "the skip is logged: {log}"
    );
}

// #1646 review r2: the lease half must also refuse a delete after `.tmp` is
// swapped mid-round (the clock is read right before the delete).
#[test]
fn upload_lease_cleanup_does_not_follow_a_root_swapped_mid_round() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap-lease");
    let leaked = outside.join("chat-upload-leases/upload-a/lease-a");
    fs::create_dir_all(&leaked).unwrap();
    fs::write(
        leaked.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();

    let project = scratch.dir("p-swap-lease");
    let own = project.join(".tmp/chat-upload-leases/upload-a/lease-a");
    fs::create_dir_all(&own).unwrap();
    fs::write(
        own.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();

    let swaps = Arc::new(AtomicUsize::new(0));
    let store = store_swapping_tmp(
        &project,
        &outside,
        Arc::new(AtomicI64::new(EPOCH)),
        swaps.clone(),
        0,
    );
    store.cleanup_expired().unwrap();

    assert_eq!(
        swaps.load(Ordering::SeqCst),
        1,
        "the swap happened mid-round"
    );
    assert!(
        leaked.join("lease.json").exists(),
        "a lease delete must not follow a root swapped mid-round"
    );
}

// #1646 review r2: the lease half needs the same gate as the record half —
// "unreadable lease means expired" makes deletion the default, so a symlinked
// leases root would delete lease trees outside the project.
#[test]
fn upload_lease_cleanup_never_deletes_through_a_symlinked_root() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-leases");
    let leaked = outside.join("upload-a/lease-a");
    fs::create_dir_all(&leaked).unwrap();
    fs::write(
        leaked.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();

    let project = scratch.dir("p-lease-link");
    let h = golden_store(&project);
    fs::create_dir_all(project.join(".tmp")).unwrap();
    std::os::unix::fs::symlink(&outside, project.join(".tmp/chat-upload-leases")).unwrap();
    h.store.cleanup_expired().unwrap();

    assert!(
        leaked.join("lease.json").exists(),
        "a symlinked leases root must not be swept"
    );
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("leases root is not a plain directory"),
        "the skip is logged: {log}"
    );
}

// #1646 review r3: the third delete point on the leases tree is `release`, and
// it fires long after acquire — the swap window is the whole lease lifetime,
// not a few instructions, so no timing luck is needed.
#[test]
fn upload_lease_release_does_not_follow_a_swapped_leases_root() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let outside = scratch.dir("outside-release");
    let h = golden_store(&project);
    completed(&h);
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let root = project.join(".tmp/chat-upload-leases");
    let dir = PathBuf::from(string(&lease.attachments[0], "path").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    assert!(dir.starts_with(&root));
    // Move the whole lease tree out of the project and leave the root behind
    // as a symlink to it: `release` would now delete outside the project.
    let leaked = outside.join("chat-upload-leases");
    fs::rename(&root, &leaked).unwrap();
    std::os::unix::fs::symlink(&leaked, &root).unwrap();
    lease.release().unwrap();
    assert!(
        leaked.join(dir.strip_prefix(&root).unwrap()).exists(),
        "a lease release must not follow a swapped leases root"
    );
}

// #1646: a failed record removal is a logged skip too — the rest of the sweep
// (and the lease sweep after it) still runs.
#[test]
fn upload_cleanup_record_removal_failure_does_not_stop_the_sweep() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    seed_cleanup_record(&uploads, &project, "upload-a", "1970-01-01T00:00:00.000Z");
    seed_cleanup_record(&uploads, &project, "upload-b", "1970-01-01T00:00:00.000Z");
    inject(&h, "record-rm", libc::EACCES);
    let cleaned = h.store.cleanup_expired().unwrap();
    assert_eq!(
        cleaned, 1,
        "a failed record removal does not stop the sweep"
    );
    assert!(
        uploads.join("upload-a").exists(),
        "the unremovable record stays for the next sweep"
    );
    assert!(
        !uploads.join("upload-b").exists(),
        "the sweep continues past a failed removal"
    );
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(log.contains("upload-a"), "the skip log names it: {log}");
}

// #1646: lease.json appears only through a temp + rename, so a reader sees the
// complete lease or nothing at all — never a 0-byte/half file it would treat
// as expired. The rename fault is the deterministic stand-in for observing
// the write window mid-flight.
#[test]
fn upload_lease_atomic_rename_failure_keeps_valid_leases() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    completed(&h);
    let live = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let live_dir = PathBuf::from(string(&live.attachments[0], "path").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    inject(&h, "lease-rename", libc::EIO);
    let e = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .err()
        .expect("lease.json must appear only through an atomic rename");
    assert_eq!(e.code, "EIO");
    let lease_root = project.join(".tmp/chat-upload-leases/upload-1");
    assert_eq!(
        names(&lease_root),
        vec![live_dir.file_name().unwrap().to_string_lossy().into_owned()],
        "the failed acquire leaves no partial lease behind"
    );
    assert_eq!(leftovers(&lease_root, ".tmp"), 0, "no lease temp survives");
    // A valid lease survives a sweep that watched its neighbour fail.
    h.store.cleanup_expired().unwrap();
    assert!(
        live_dir.join("lease.json").exists(),
        "a valid lease is not deleted by the sweep"
    );
    live.release().unwrap();
}

#[tokio::test(start_paused = true)]
async fn cleanup_schedule_pins_the_ts_interval_and_ends_with_the_store() {
    // The TS literal is `15 * 60 * 1000`; any other value silently diverges.
    assert_eq!(CLEANUP_INTERVAL, Duration::from_secs(15 * 60));
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    seed_cleanup_record(
        &uploads,
        &project,
        "upload-gone",
        "1970-01-01T00:00:00.000Z",
    );
    let handle = spawn_cleanup(&h.store, Duration::from_secs(1));
    // Last strong handle gone before the first tick: like `unref()`, a pending
    // schedule must neither keep the store alive nor touch the disk.
    drop(h);
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    // `await` (not `is_finished`) so a task that died by panic is a failure
    // rather than a clean-looking exit. The timeout bounds the other failure
    // mode: a regression that keeps the store alive would tick forever here,
    // and a hung suite never reports anything.
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("the detached loop exits once the store is dropped")
        .expect("the detached loop exits cleanly once the store is dropped");
    assert!(
        uploads.join("upload-gone").exists(),
        "a dropped store is never swept"
    );
}

#[tokio::test]
async fn cleanup_failure_keeps_the_schedule_alive() {
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    // #1646 made a corrupt record a logged skip instead of a round failure,
    // so the failing stage is now the lease sweep: an injected lease rm error
    // aborts the round after the records were already removed. The property
    // under test is unchanged: a failed sweep does not end the schedule.
    seed_cleanup_record(
        &uploads,
        &project,
        "upload-expired",
        "1970-01-01T00:00:00.000Z",
    );
    let lease = project.join(".tmp/chat-upload-leases/upload-expired/l1");
    fs::create_dir_all(&lease).unwrap();
    fs::write(
        lease.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();
    h.now.store(EPOCH, Ordering::SeqCst);
    inject(&h, "lease-rm", libc::EACCES);
    // The assertions below only mean something once one tick has run, so the
    // period must be one the clamp leaves alone — checked, not assumed, so a
    // raised floor turns this red instead of silently making it vacuous.
    let interval = Duration::from_secs(1);
    assert!(
        interval >= MIN_CLEANUP_INTERVAL,
        "this test relies on spawn_cleanup honouring its period verbatim"
    );
    let handle = spawn_cleanup(&h.store, interval);
    // Wait for the injected fault to be consumed instead of sleeping a fixed
    // 1.3 s: the sleep could pass before any tick (assertion vacuous) or
    // overshoot into the next tick, which succeeds and removes the lease
    // (flaky red). Consuming the one-shot fault proves the failing round ran.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !h.store.faults.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the cleanup schedule runs its first tick");
    assert!(
        lease.exists(),
        "the failed round leaves the lease sweep's work undone"
    );
    // The second tick must run. Probing "the next tick happened" *before*
    // "the task is still alive" keeps the `cleanup-schedule-stop-on-failure`
    // mutation deterministic: a loop that returns after the failure leaves the
    // lease in place, so this times out instead of racing `is_finished`.
    tokio::time::timeout(Duration::from_secs(10), async {
        while lease.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the next tick sweeps again after the failed one");
    assert!(
        !handle.is_finished(),
        "a failed sweep does not end the schedule"
    );
    handle.abort();
}

#[tokio::test]
async fn cleanup_clamps_a_zero_interval_instead_of_panicking() {
    // The floor's value is a decision, not an implementation detail: 1 ms turns
    // a mistyped separator into a thousand full-project scans per second, and
    // minutes hide the mistake. Pin it here, and pin that the failure test's
    // period clears it.
    assert_eq!(MIN_CLEANUP_INTERVAL, Duration::from_secs(1));
    let scratch = Scratch::new();
    let project = scratch.dir("p");
    let h = golden_store(&project);
    let uploads = project.join(".tmp/chat-uploads");
    seed_cleanup_record(
        &uploads,
        &project,
        "upload-gone",
        "1970-01-01T00:00:00.000Z",
    );
    // `tokio::time::interval(Duration::ZERO)` panics, and inside this detached
    // task that panic is invisible: the record would never be swept. The clamp
    // must keep the loop alive at its one-second floor.
    let handle = spawn_cleanup(&h.store, Duration::ZERO);
    tokio::time::timeout(Duration::from_secs(5), async {
        while uploads.join("upload-gone").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a zero interval is clamped, not fatal");
    assert!(!handle.is_finished(), "the clamped schedule keeps running");
    handle.abort();
}

// #1850: `store_swapping_tmp` for any single directory: the first `now`
// callback renames `dir` to a sibling and replaces it with a symlink to
// `outside`. Used with the record's task directory — the plain root above it
// stays real, so only the `canonical_home` component walk can refuse the
// cancel delete (fix-r1; containment alone passes an in-root alias) — and
// with the lease `<id>` directory, whose emptied-parent removal is guarded
// the same way.
fn store_swapping_dir(
    project: &Path,
    dir: PathBuf,
    outside: &Path,
    clock: Arc<AtomicI64>,
    swaps: Arc<AtomicUsize>,
    fire_at: usize,
) -> Arc<UploadStore> {
    let (resolved, listed) = (project.to_owned(), project.to_owned());
    let (target, dir) = (outside.to_owned(), dir);
    let (tick, swap) = (clock, swaps);
    let mut opts = UploadStoreOptions::new(
        move |_: &JsString| Ok(resolved.clone()),
        move || Ok(vec![listed.clone()]),
    );
    opts.now = Arc::new(move || {
        if swap.fetch_add(1, Ordering::SeqCst) == fire_at {
            fs::rename(&dir, dir.with_extension("real")).unwrap();
            std::os::unix::fs::symlink(&target, &dir).unwrap();
        }
        tick.load(Ordering::SeqCst)
    });
    Arc::new(UploadStore::new(opts))
}

// #1850: the cancel delete is the fourth removal point on the `chat-uploads`
// tree, and its window is everything between `get`'s record read and the
// delete: the expiry clock read, then the whole `mutate` rewrite. The device
// fires at the second clock read — after `mutate`'s in-gate re-read (which
// #2279 turns into an `UPLOAD_NOT_FOUND` refusal once the swap has removed
// the record, so an earlier fire would never reach the delete) and before
// the metadata write and the delete below, both of which run against a `.tmp`
// that is already a symlink. The metadata write follows the same link (a
// pre-existing residual, out of scope here); what this test pins is that the
// *removal* does not.
#[test]
fn upload_cancel_does_not_follow_a_root_swapped_mid_mutate() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap-cancel");
    let victim = outside.join("chat-uploads/upload-1/files/x");
    fs::create_dir_all(victim.parent().unwrap()).unwrap();
    fs::write(&victim, "keep").unwrap();

    let project = scratch.dir("p-swap-cancel");
    let h = golden_store(&project);
    created(&h);
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    drop(h);

    let swaps = Arc::new(AtomicUsize::new(0));
    let store = store_swapping_tmp(
        &project,
        &outside,
        Arc::new(AtomicI64::new(EPOCH)),
        swaps.clone(),
        1,
    );
    let updated = store.cancel("upload-1").unwrap();

    assert_eq!(status(&updated), "cancelled");
    assert_eq!(
        swaps.load(Ordering::SeqCst),
        2,
        "the swap fired before the delete (expiry check, then updatedAt)"
    );
    assert!(
        victim.exists(),
        "a cancel delete must not follow a root swapped mid-mutate"
    );
    let log = store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("[uploads] cancel skipped"),
        "the refused delete is logged: {log}"
    );
}

// #1850: the same delete, but the component swapped for an outside symlink
// is the record's task directory — the root above it is still plain, so the
// plain-root gate alone would let `remove_tree` follow the link. Only the
// per-entry `canonical_home` component walk refuses it (fix-r1;
// `cancel-alias-walk-downgraded` is the containment-only shape it replaced).
#[test]
fn upload_cancel_does_not_follow_a_task_dir_swapped_mid_mutate() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap-task");
    let victim = outside.join("files/x");
    fs::create_dir_all(outside.join("files")).unwrap();
    fs::write(&victim, "keep").unwrap();

    let project = scratch.dir("p-swap-task");
    let h = golden_store(&project);
    created(&h);
    h.store
        .write_part("upload-1", "file-1", &b"hello"[..])
        .unwrap();
    drop(h);

    let swaps = Arc::new(AtomicUsize::new(0));
    let store = store_swapping_dir(
        &project,
        project.join(".tmp/chat-uploads/upload-1"),
        &outside,
        Arc::new(AtomicI64::new(EPOCH)),
        swaps.clone(),
        1,
    );
    let updated = store.cancel("upload-1").unwrap();

    assert_eq!(status(&updated), "cancelled");
    assert_eq!(
        swaps.load(Ordering::SeqCst),
        2,
        "the swap fired before the delete (expiry check, then updatedAt)"
    );
    assert!(
        victim.exists(),
        "a cancel delete must not follow a task dir swapped mid-mutate"
    );
    let log = store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("outside the uploads root"),
        "the refused delete is logged: {log}"
    );
}

// #1850: the emptied `<id>` directory at the end of a lease round. The entry
// sweep skipped the (now linked) `<id>` entirely, so `names` reads through the
// link and finds the outside target empty — without the `inside_root` term
// the parent removal would unlink the symlinked `<id>` itself. The record
// half never runs (no uploads root), so the first `now` callback is the lease
// expiry check, exactly where the swap lands.
#[test]
fn upload_lease_cleanup_does_not_remove_an_id_dir_swapped_mid_round() {
    let scratch = Scratch::new();
    let outside = scratch.dir("outside-swap-lease-id");
    let project = scratch.dir("p-swap-lease-id");
    let own = project.join(".tmp/chat-upload-leases/upload-1/lease-a");
    fs::create_dir_all(&own).unwrap();
    fs::write(
        own.join("lease.json"),
        "{\"expiresAt\":\"1970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();

    let swaps = Arc::new(AtomicUsize::new(0));
    let store = store_swapping_dir(
        &project,
        project.join(".tmp/chat-upload-leases/upload-1"),
        &outside,
        Arc::new(AtomicI64::new(EPOCH)),
        swaps.clone(),
        0,
    );
    store.cleanup_expired().unwrap();

    assert_eq!(swaps.load(Ordering::SeqCst), 1, "the swap happened");
    let swapped = project.join(".tmp/chat-upload-leases/upload-1");
    assert!(
        matches!(fs::symlink_metadata(&swapped), Ok(m) if m.file_type().is_symlink()),
        "an id directory swapped mid-round must not be removed"
    );
    assert!(
        names(&outside).is_empty(),
        "the outside target must stay untouched"
    );
    let log = store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("outside the leases root"),
        "the skipped entry is logged: {log}"
    );
}

// #1850: the non-`NotFound` `realpath` branch of `release` — an ELOOP root has
// no permission-based injection, so a self-referential symlink is the one
// deterministic shape. The failure must surface (not be swallowed into the
// idempotent `Ok`), and it must stay cached: healing the root and releasing
// again has to answer with the same error and leave the lease tree in place.
#[test]
fn upload_lease_release_surfaces_and_caches_a_realpath_failure() {
    let scratch = Scratch::new();
    let project = scratch.dir("p-realpath");
    let h = golden_store(&project);
    completed(&h);
    let lease = h
        .store
        .acquire_attachment_lease("upload-1", "project", None)
        .unwrap();
    let dir = PathBuf::from(string(&lease.attachments[0], "path").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let root = dir.parent().unwrap().parent().unwrap().to_owned();
    let real = root.with_extension("real");
    fs::rename(&root, &real).unwrap();
    std::os::unix::fs::symlink(&root, &root).unwrap();

    let first = lease.release();
    assert!(
        first.is_err(),
        "a release realpath failure must surface, not be swallowed"
    );
    assert_eq!(first.unwrap_err().code, "ELOOP");

    // Heal the root: the cached failure must still be the answer, and the
    // lease tree must still be there (the release did not run again).
    fs::remove_file(&root).unwrap();
    fs::rename(&real, &root).unwrap();
    let second = lease.release();
    assert!(
        second.is_err(),
        "a cached release failure must stay cached after the root heals"
    );
    assert_eq!(second.unwrap_err().code, "ELOOP");
    assert!(
        dir.join("lease.json").exists(),
        "a cached release failure must not re-run the release"
    );
}

// fix-r1 (minimax concurrency#2 arbitration): the benign preplanted alias —
// `<root>/upload-1` is a symlink to `<root>/upload-2` with no metadata
// forgery. `read_at`'s identity check refuses the record it reads through the
// link (its `uploadId` is the victim's), so the delete is never reached.
#[test]
fn upload_cancel_on_a_preplanted_alias_is_refused_at_the_identity_check() {
    let scratch = Scratch::new();
    let project = scratch.dir("p-alias-identity");
    let h = golden_store(&project);
    created(&h);
    created(&h);
    h.store
        .write_part("upload-2", "file-1", &b"hello"[..])
        .unwrap();
    drop(h);
    let root = project.join(".tmp/chat-uploads");
    let victim = root.join("upload-2/files/file-1");
    assert!(victim.exists());
    fs::remove_dir_all(root.join("upload-1")).unwrap();
    std::os::unix::fs::symlink(root.join("upload-2"), root.join("upload-1")).unwrap();
    let store = golden_store(&project);
    let e = store.store.cancel("upload-1").err().unwrap();
    assert_eq!(
        (e.code.as_str(), e.message_string().as_str()),
        (
            "UPLOAD_INTEGRITY_MISMATCH",
            "Upload metadata identity does not match its directory."
        )
    );
    assert!(
        victim.exists(),
        "a preplanted alias must not delete the victim's files"
    );
}

// fix-r1 (minimax concurrency#2 arbitration): the adversarial alias — the
// victim's metadata is forged to carry this upload's id, which an attacker
// with write access inside the uploads root can produce. The identity check
// passes, and the resolved path stays inside the root, so the containment
// comparison alone waves the delete through and removes the other upload's
// files with no race at all. The component walk refuses the linked `<id>`.
// The forged record is corrupted either way by the mutate rewrite (the write
// follows the link too — the registered write-path residual); what this pins
// is that the *removal* does not happen.
#[test]
fn upload_cancel_does_not_delete_through_an_aliased_task_dir() {
    let scratch = Scratch::new();
    let project = scratch.dir("p-alias-delete");
    let h = golden_store(&project);
    created(&h);
    created(&h);
    h.store
        .write_part("upload-2", "file-1", &b"hello"[..])
        .unwrap();
    drop(h);
    let root = project.join(".tmp/chat-uploads");
    let meta2 = root.join("upload-2/metadata.json");
    let mut record = parse_json(&fs::read_to_string(&meta2).unwrap()).unwrap();
    set(&mut record, "uploadId", text("upload-1"));
    fs::write(&meta2, compact(&record)).unwrap();
    let victim = root.join("upload-2/files/file-1");
    assert!(victim.exists());
    fs::remove_dir_all(root.join("upload-1")).unwrap();
    std::os::unix::fs::symlink(root.join("upload-2"), root.join("upload-1")).unwrap();
    let store = golden_store(&project);
    let updated = store.store.cancel("upload-1").unwrap();
    assert_eq!(status(&updated), "cancelled");
    assert!(
        victim.exists(),
        "a cancel delete must not remove another upload's files through an aliased task dir"
    );
    let log = store.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("[uploads] cancel skipped"),
        "the refused delete is logged: {log}"
    );
}

// #2423 fix-r1 (deepseek r1 concurrency#1，仲裁实测坐实，比评审所述更深一层):
// containment-only 比对放行指向根内 `<id>` 的链接 `<id>` 别名，而别名扫到的
// 条目按 victim 自身字节判过期——别名指向**在用**租约目录时，其 lease.json 与
// pin 硬链接全被当成孤儿租约逐个删光（探针 `lease-alias-probe.out.txt` 的
// arb-lease-C 三行全 false）。条目删除点与 cancel 同用 `canonical_home` 组件
// 走查后，链接 `<id>` 整体拒扫并记日志。
#[test]
fn upload_lease_cleanup_does_not_sweep_through_an_aliased_id_dir() {
    let scratch = Scratch::new();
    let project = scratch.dir("p-lease-alias-active");
    let h = golden_store(&project);
    let root = project.join(".tmp/chat-upload-leases");
    let victim = root.join("upload-2/lease-a");
    fs::create_dir_all(&victim).unwrap();
    fs::write(
        victim.join("lease.json"),
        "{\"expiresAt\":\"2970-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();
    fs::write(victim.join("pin-1"), "pin").unwrap();
    std::os::unix::fs::symlink(&victim, root.join("upload-1")).unwrap();
    h.store.cleanup_expired().unwrap();

    assert!(
        victim.join("lease.json").exists(),
        "an aliased id dir must not put an active lease's files on the sweep"
    );
    assert!(
        victim.join("pin-1").exists(),
        "an aliased id dir must not delete an active lease's pin set"
    );
    assert!(victim.exists(), "the active lease dir must survive");
    let log = h.store.cleanup_log.lock().unwrap().join("\n");
    assert!(
        log.contains("linked lease dir"),
        "the skip is logged: {log}"
    );
}
