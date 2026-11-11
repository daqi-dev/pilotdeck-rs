//! Filesystem upload provider, ported against PilotDeck cc7c4e6 UploadStore.ts.
//! This module is not wired into Gateway dispatch. Records retain JS property
//! order and unknown fields so TS and Rust can continue each other's uploads.
//!
//! Concurrency scope matches TS: mutations of one uploadId are serialized only
//! within one store instance (TS `updates: Map<string, Promise>`). Two processes
//! (or two instances) mutating the same upload concurrently can lose an update,
//! exactly as in TS; the shared contract is the disk format, not a cross-process
//! lock. Inside one instance, `complete` and `cancel` judge the record
//! re-read within that per-id gate, so an interleaved complete/cancel pair
//! on the same id can no longer both succeed, and a `completed` record's
//! files survive that racing cancel (#1631) — when that in-gate re-read
//! succeeds: a record the expiry sweep removed after the outside `get()`
//! but before that re-read is refused with `UPLOAD_NOT_FOUND` instead of
//! falling back to the outside snapshot (#2279). A sweep that lands
//! between the re-read and the metadata write is outside this gate's
//! scope — the record removals are not themselves gated, so `write` can
//! still re-create a just-swept task directory (the same mutate-family
//! rework as #1503, tracked as #2670). A verdict of "no change" — an
//! already-settled no-op, or a refusal whose closure answers `false` —
//! writes and emits nothing and leaves `updatedAt` alone, so those
//! refusals cannot surface as a metadata-write failure instead of their
//! semantic code (#2275); the manifest-mismatch refusal is a different
//! shape: it settles the record with a `mark_failed` write on purpose and
//! answers `true`. The guarantee is scoped to
//! that pair: a `write_part` in flight does not judge status inside the
//! gate, so it can still flip a record back to `uploading` after a cancel —
//! the same-gate rework for `write_part`/`fail`/`remove_tree` is #1503.
//! Live TS cross-tests are `#[ignore]` (need PDRS_BASELINE); the
//! non-ignored `upload_golden_bytes_match_ts_fixtures` guards the format in CI.
mod error;
pub mod gateway;
#[cfg(test)]
mod gateway_tests;
#[cfg(test)]
mod tests;

pub use error::UploadError;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use pilotdeck_transport::system_clock::{format_epoch_iso, parse_node22_date};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, Weak, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, UploadError>;
/// Original UploadRecord / UploadedAttachment objects, including extra fields.
pub type UploadRecord = JsValue;
pub type UploadedAttachment = JsValue;
type ResolveProject = dyn Fn(&JsString) -> Result<PathBuf> + Send + Sync;
type ListProjects = dyn Fn() -> Result<Vec<PathBuf>> + Send + Sync;

pub struct UploadStoreOptions {
    pub resolve_project: Arc<ResolveProject>,
    pub list_projects: Arc<ListProjects>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub uuid: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub max_file_bytes: u64,
    pub max_task_bytes: u64,
    pub max_files: usize,
    pub max_concurrent_per_project: usize,
    pub retention_ms: i64,
    pub lease_retention_ms: i64,
}
impl UploadStoreOptions {
    pub fn new(
        resolve_project: impl Fn(&JsString) -> Result<PathBuf> + Send + Sync + 'static,
        list_projects: impl Fn() -> Result<Vec<PathBuf>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolve_project: Arc::new(resolve_project),
            list_projects: Arc::new(list_projects),
            now: Arc::new(|| match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_millis() as i64,
                Err(e) => -(e.duration().as_millis() as i64),
            }),
            uuid: None,
            max_file_bytes: 1024 * 1024 * 1024,
            max_task_bytes: 2 * 1024 * 1024 * 1024,
            max_files: 500,
            max_concurrent_per_project: 3,
            retention_ms: 86_400_000,
            lease_retention_ms: 21_600_000,
        }
    }
}

#[derive(Default)]
struct UpdateState {
    failure: Option<UploadError>,
}
type Updates = HashMap<String, Arc<Mutex<UpdateState>>>;
#[derive(Default)]
struct Listeners {
    next: u64,
    entries: HashMap<u64, (String, mpsc::Sender<UploadRecord>)>,
}

/// Locks, like TS promise queues, are scoped to this provider instance. Separate
/// processes share the disk format, not a cross-process mutation lock.
pub struct UploadStore {
    options: UploadStoreOptions,
    creates: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
    updates: Mutex<Updates>,
    listeners: Arc<Mutex<Listeners>>,
    /// #2276/#2279 test rendezvous, installed through
    /// [`Self::set_pre_mutate_hook`]: `complete` and `cancel` fire it between
    /// their outside-`get` snapshot and the `mutate` gate. No production
    /// caller sets one, and nothing else consults it.
    pre_mutate_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    faults: Mutex<Vec<(String, i32)>>,
    /// Non-record outcomes of a tolerant sweep (skips, orphan reclaims), so
    /// tests can pin what the log said without capturing stderr.
    #[cfg(test)]
    cleanup_log: Mutex<Vec<String>>,
}
impl UploadStore {
    pub fn new(options: UploadStoreOptions) -> Self {
        Self {
            options,
            creates: Mutex::new(HashMap::new()),
            updates: Mutex::new(HashMap::new()),
            listeners: Arc::new(Mutex::new(Listeners::default())),
            pre_mutate_hook: Mutex::new(None),
            #[cfg(test)]
            faults: Mutex::new(vec![]),
            #[cfg(test)]
            cleanup_log: Mutex::new(vec![]),
        }
    }

    /// UploadStore.ts:61–110: validate before idempotency; serialize by canonical project.
    ///
    /// Typed convenience entry point for callers that already hold UTF-8 keys;
    /// the gateway adapter uses [`Self::create_js`] to keep raw UTF-16 units.
    pub fn create(
        &self,
        project_key: &str,
        files: &JsValue,
        idempotency_key: Option<&str>,
    ) -> Result<UploadRecord> {
        let key = idempotency_key.map(JsValue::from);
        self.create_js(&JsString::from(project_key), files, key.as_ref())
    }

    /// Raw bridge for `upload_create`. `resolveProject` and the idempotency
    /// value keep their UTF-16 units until TS's own boundary: only the
    /// filesystem path (and `createHash.update`'s UTF-8 encoding) is lossy.
    pub fn create_js(
        &self,
        project_key: &JsString,
        files: &JsValue,
        idempotency_key: Option<&JsValue>,
    ) -> Result<UploadRecord> {
        let root = (self.options.resolve_project)(project_key)?;
        let root = fs::canonicalize(root).map_err(|_| unknown_project_key(project_key))?;
        let gate = {
            let mut gates = self.creates.lock().unwrap();
            gates.retain(|_, g| g.strong_count() > 0);
            let gate = gates.get(&root).and_then(Weak::upgrade).unwrap_or_default();
            gates.insert(root.clone(), Arc::downgrade(&gate));
            gate
        };
        let _guard = gate.lock().unwrap();
        let manifest = self.validate_manifest(files)?;
        // UploadStore.ts:75-76: `validateManifest`, then
        // `createHash("sha256").update(idempotencyKey)`. A truthy non-string
        // therefore loses to project/manifest errors and only then throws.
        let key_hash = match idempotency_key.filter(|key| truthy(key)) {
            Some(JsValue::String(key)) => {
                Some(hash(String::from_utf16_lossy(key.units()).as_bytes()))
            }
            Some(other) => {
                return Err(UploadError::dialog(
                    "ERR_INVALID_ARG_TYPE",
                    gateway::crypto_received(other),
                ));
            }
            None => None,
        };
        let existing = self.list_in_project(&root)?;
        if let Some(key) = &key_hash
            && let Some(record) = existing.iter().find(|r| {
                string(r, "idempotencyKeyHash").as_deref() == Some(key) && status(r) != "expired"
            })
        {
            return Ok(record.clone());
        }
        if existing.iter().filter(|r| active(r)).count() >= self.options.max_concurrent_per_project
        {
            return Err(UploadError::dialog(
                "UPLOAD_CONCURRENCY_LIMIT",
                "Project upload concurrency limit reached.",
            ));
        }
        let now = (self.options.now)();
        let id = self
            .options
            .uuid
            .as_ref()
            .map(|f| Ok(f()))
            .unwrap_or_else(random_id)?;
        let total = manifest.iter().fold(0.0, |sum, m| sum + number(m, "size"));
        let mut record = object([
            ("uploadId", text(&id)),
            ("projectKey", path_value(&root)),
            ("status", text("created")),
            ("manifest", JsValue::Array(manifest)),
            ("totalBytes", JsValue::Number(total)),
            ("uploadedBytes", JsValue::Number(0.0)),
            ("createdAt", text(&format_epoch_iso(now))),
            ("updatedAt", text(&format_epoch_iso(now))),
            (
                "expiresAt",
                text(&format_epoch_iso(now + self.options.retention_ms)),
            ),
        ]);
        if let Some(key) = key_hash {
            set(&mut record, "idempotencyKeyHash", text(&key));
        }
        set(&mut record, "receivedClientFileIds", JsValue::Array(vec![]));
        make_dir(&task_dir(&record).join("files"))?;
        self.write(&record)?;
        self.emit(&record);
        Ok(record)
    }

    pub fn get(&self, upload_id: &str) -> Result<UploadRecord> {
        assert_id(upload_id)?;
        for project in (self.options.list_projects)()? {
            let Ok(project) = fs::canonicalize(project) else {
                continue;
            };
            if let Some(record) = self.read_at(&project, upload_id)? {
                return self.expire_if_needed(record);
            }
        }
        Err(UploadError::dialog(
            "UPLOAD_NOT_FOUND",
            format!("Unknown uploadId: {upload_id}"),
        ))
    }

    /// Streaming bytes stay bounded; progress and final attachments use the same
    /// per-upload mutation queue as complete/cancel/fail (TS:124–187,317–331).
    pub fn write_part(
        &self,
        upload_id: &str,
        client_file_id: &str,
        stream: impl Read,
    ) -> Result<UploadedAttachment> {
        self.write_part_js(upload_id, &JsString::from(client_file_id), stream)
    }

    /// Raw bridge for `upload_part`: the manifest comparison is over the exact
    /// UTF-16 units TS compares (`item.clientFileId === clientFileId`), so a
    /// lone surrogate never matches a real U+FFFD id. Conversion to UTF-8 is
    /// limited to the filesystem path; messages keep the caller's units.
    pub fn write_part_js(
        &self,
        upload_id: &str,
        client_file_id: &JsString,
        mut stream: impl Read,
    ) -> Result<UploadedAttachment> {
        let client_value = JsValue::String(client_file_id.clone());
        let mut record = self.get(upload_id)?;
        require_active(&record)?;
        let expected = array(&record, "manifest")
            .iter()
            .find(|m| value(m, "clientFileId") == client_value)
            .cloned();
        if expected.is_none() || array(&record, "receivedClientFileIds").contains(&client_value) {
            return Err(UploadError::dialog(
                "UPLOAD_MANIFEST_MISMATCH",
                part_message("Unexpected or duplicate file part: ", client_file_id, ""),
            ));
        }
        let expected = expected.unwrap();
        // Only the filesystem path crosses a UTF-8 boundary. Honest manifest
        // ids are ASCII (`safe_id`); a tampered record gets exactly Node's
        // replacement encoding here, never in a message.
        let client_path = String::from_utf16_lossy(client_file_id.units());
        record = self.mutate(&record, |r| {
            set(r, "status", text("uploading"));
            true
        })?;
        let destination = task_dir(&record).join("files").join(&client_path);
        let result = (|| {
            let mut file = open_new(&destination)?;
            let mut digest = Sha256::new();
            let mut bytes = 0_u64;
            let mut pending = 0_u64;
            let mut last_flush: Option<Instant> = None;
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                let count = match stream.read(&mut chunk) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    result => result.map_err(|e| UploadError::io(e, "read", None))?,
                };
                if count == 0 {
                    break;
                }
                bytes += count as u64;
                if bytes as f64 > number(&expected, "size") || bytes > self.options.max_file_bytes {
                    return Err(UploadError::dialog(
                        "UPLOAD_INTEGRITY_MISMATCH",
                        part_message(
                            "Uploaded bytes exceed the declared size for ",
                            client_file_id,
                            ".",
                        ),
                    ));
                }
                digest.update(&chunk[..count]);
                pending += count as u64;
                if last_flush.is_none_or(|t| t.elapsed().as_millis() >= 100) {
                    // TS schedules progress before the stream write, swallowing
                    // its immediate rejection; drain/final mutate sees failure.
                    if let Ok(updated) = self.progress(&record, pending) {
                        record = updated;
                    }
                    pending = 0;
                    last_flush = Some(Instant::now());
                }
                self.fault("part-write", &destination)?;
                file.write_all(&chunk[..count])
                    .map_err(|e| UploadError::io(e, "write", None))?;
            }
            drop(file);
            if pending > 0 {
                record = self.progress(&record, pending)?;
            }
            self.drain_updates(upload_id)?;
            let digest = format!("{:x}", digest.finalize());
            let expected_hash = string(&expected, "sha256").unwrap_or_default();
            if bytes as f64 != number(&expected, "size")
                || (!expected_hash.is_empty() && expected_hash.to_lowercase() != digest)
            {
                return Err(UploadError::dialog(
                    "UPLOAD_INTEGRITY_MISMATCH",
                    part_message("Size or SHA-256 mismatch for ", client_file_id, "."),
                ));
            }
            let mut attachment = object([
                ("attachmentId", JsValue::String(client_file_id.clone())),
                ("name", value(&expected, "name")),
                ("relativePath", value(&expected, "relativePath")),
            ]);
            if truthy(&value(&expected, "mimeType")) {
                set(&mut attachment, "mimeType", value(&expected, "mimeType"));
            }
            set(&mut attachment, "bytes", JsValue::Number(bytes as f64));
            set(&mut attachment, "sha256", text(&digest));
            set(&mut attachment, "path", path_value(&destination));
            self.mutate(&record, |r| {
                let mut received = array(r, "receivedClientFileIds").to_vec();
                received.push(JsValue::String(client_file_id.clone()));
                set(r, "receivedClientFileIds", JsValue::Array(received));
                let mut attachments = array(r, "attachments").to_vec();
                attachments.push(attachment.clone());
                set(r, "attachments", JsValue::Array(attachments));
                true
            })?;
            Ok(attachment)
        })();
        match result {
            Ok(a) => Ok(a),
            Err(e) => {
                self.fail(
                    upload_id,
                    if e.dialog {
                        &e.code
                    } else {
                        "UPLOAD_STREAM_INTERRUPTED"
                    },
                    &e.message,
                )?;
                Err(e)
            }
        }
    }

    /// #1631: the state and manifest verdicts judge the record re-read inside
    /// `mutate`'s per-id gate — the pre-gate `get()` snapshot let an
    /// interleaved cancel delete the files behind a `complete` that had just
    /// reported success. A refusal is captured and returned after the gate
    /// (an Err from the closure would poison the queue for later calls, which
    /// is TS's retained rejected Promise), and the mismatch branch writes the
    /// failed record itself because `fail` would re-enter the gate this
    /// closure already holds.
    pub fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        let mut refusal: Option<UploadError> = None;
        let updated = self.mutate(&record, |r| {
            if !active(r) {
                refusal = Some(invalid_state(r));
                return false;
            }
            if array(r, "manifest")
                .iter()
                .any(|m| !array(r, "receivedClientFileIds").contains(&value(m, "clientFileId")))
            {
                let message = JsString::from("One or more declared files were not uploaded.");
                mark_failed(r, "UPLOAD_MANIFEST_MISMATCH", &message);
                refusal = Some(UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", message));
            } else {
                set(r, "status", text("completed"));
                set(r, "uploadedBytes", value(r, "totalBytes"));
            }
            true
        })?;
        match refusal {
            Some(e) => Err(e),
            None => Ok(updated),
        }
    }

    /// Same #1631 gate-internal verdict as [`Self::complete`]: only the
    /// in-gate transition to `cancelled` is followed by `remove_tree`, so a
    /// `completed` record's files survive a racing cancel. The `remove_tree`
    /// itself still runs after the gate (the module-doc pair scope and #1503
    /// cover what a concurrently in-flight `write_part` can interleave).
    ///
    /// #1850: that delete is the fourth removal point on the `chat-uploads`
    /// tree, and unlike the sweeps it runs after the whole `mutate` rewrite —
    /// a window wide enough to swap `.tmp` (or the task directory itself) for
    /// a symlink, which `remove_tree` would follow out of the project. The
    /// status transition is already durable at that point, so a refused
    /// delete is logged and skipped, never a `cancel` error: the record is
    /// cancelled either way, and the sweep's "rather not delete" trade-off
    /// (A8-UPLOAD-CLEANUP-ROBUSTNESS-001) applies unchanged.
    pub fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        let mut refusal: Option<UploadError> = None;
        let mut cancelled = false;
        let updated = self.mutate(&record, |r| {
            if active(r) {
                set(r, "status", text("cancelled"));
                cancelled = true;
                true
            } else if status(r) == "completed" {
                refusal = Some(UploadError::dialog(
                    "UPLOAD_ALREADY_COMPLETED",
                    "Completed uploads cannot be cancelled.",
                ));
                false
            } else {
                // #2275: cancelling a cancelled (or otherwise settled) upload
                // is a no-op, not a change: nothing to write or emit.
                false
            }
        })?;
        if let Some(e) = refusal {
            return Err(e);
        }
        if cancelled {
            let project = PathBuf::from(string(&updated, "projectKey").unwrap_or_default());
            let dir = task_dir(&updated).join("files");
            let root_real = match plain_root(&project, "chat-uploads") {
                PlainRoot::Plain(root) => root,
                // The record was just read from this tree, so `Missing` means
                // the whole task directory is gone: the same quiet no-op the
                // bare `remove_tree` gave.
                PlainRoot::Missing => return Ok(updated),
                PlainRoot::Blocked => {
                    self.log_cleanup(format!(
                        "[uploads] cancel skipped {}: uploads root is not a plain directory",
                        project.join(".tmp/chat-uploads").display()
                    ));
                    return Ok(updated);
                }
            };
            // fix-r1 (minimax concurrency#2 arbitration): `inside_root` alone
            // passes a linked `<id>` whose resolution stays inside the root —
            // a deterministic alias onto another upload. The component walk
            // `canonical_home` uses for the lease/store roots refuses every
            // link component down to `files`; the residual between the walk
            // and `remove_tree` is the instruction-level TOCTOU family the
            // A8 ledger registers.
            if canonical_home(&dir, &project, &root_real).is_none() {
                self.log_cleanup(format!(
                    "[uploads] cancel skipped {}: outside the uploads root or a linked task dir",
                    dir.display()
                ));
                return Ok(updated);
            }
            remove_tree(&dir)?;
        }
        Ok(updated)
    }

    pub fn fail(&self, upload_id: &str, code: &str, message: &JsString) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        self.mutate(&record, |r| {
            mark_failed(r, code, message);
            true
        })
    }

    /// Test-only rendezvous (#2276/#2279): `complete` and `cancel` fire the
    /// installed callback between their outside-`get` snapshot and the
    /// `mutate` gate. The #1631 interleaved pair uses it to make "both
    /// callers passed the snapshot, neither entered the gate" a scheduled
    /// fact instead of a scheduler coincidence, and the #2279 interleave
    /// uses it to run the expiry sweep exactly between the snapshot and the
    /// in-gate re-read. It must be `pub` because those regressions live in
    /// the `tests/upload_concurrency_1631.rs` integration target, which
    /// compiles against the crate without `cfg(test)`. Nothing in production
    /// installs a hook.
    pub fn set_pre_mutate_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.pre_mutate_hook.lock().unwrap() = hook;
    }
    /// Fire the rendezvous with the slot lock already released, so a callback
    /// that re-enters the store (the #2279 sweep) cannot deadlock on itself.
    fn pre_mutate(&self) {
        // The guard is bound here and dropped at the semicolon: the hook runs
        // with the lock released, so a rendezvous that blocks inside the hook
        // cannot keep a second caller from ever reaching it.
        let hook = self.pre_mutate_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub fn subscribe(&self, upload_id: &str) -> UploadSubscription {
        let (sender, receiver) = mpsc::channel();
        let mut listeners = self.listeners.lock().unwrap();
        let id = listeners.next;
        listeners.next += 1;
        listeners.entries.insert(id, (upload_id.into(), sender));
        UploadSubscription {
            receiver,
            id,
            listeners: Arc::downgrade(&self.listeners),
        }
    }

    /// UploadStore.ts:229–242, except one bad record no longer costs the round:
    /// reading, judging, or removing a single record is tolerated and logged
    /// with its path, so the remaining records and the lease sweep still run
    /// (#1646; diff A8-UPLOAD-CLEANUP-ROBUSTNESS-001).
    pub fn cleanup_expired(&self) -> Result<usize> {
        let mut count = 0;
        for project in (self.options.list_projects)()? {
            let Ok(project) = fs::canonicalize(project) else {
                continue;
            };
            self.sweep_expired_records(&project, &mut count);
            self.cleanup_expired_leases(&project)?;
        }
        Ok(count)
    }

    /// The record half of [`Self::cleanup_expired`]. Unlike
    /// [`Self::list_in_project`] (whose strict errors stay for `create` and
    /// `find_idempotent`), every per-record failure here is a logged skip.
    fn sweep_expired_records(&self, project: &Path, count: &mut usize) {
        // `read_dir` and `remove_dir_all` follow intermediate symlinks, while a
        // `symlink_metadata` on `root/<id>` does not: with a symlinked root the
        // orphan branch below would delete directories outside the project.
        // `read_at`'s containment check never sees those directories (no
        // metadata.json), so gate the whole record sweep on a plain root.
        let root_real = match plain_root(project, "chat-uploads") {
            PlainRoot::Plain(root) => root,
            // No uploads yet: the ordinary empty state, as silent as the old
            // `names()` on a missing directory.
            PlainRoot::Missing => return,
            PlainRoot::Blocked => {
                self.log_cleanup(format!(
                    "[uploads] cleanup skipped {}: uploads root is not a plain directory",
                    project.join(".tmp/chat-uploads").display()
                ));
                return;
            }
        };
        // `root` is already resolved and already checked to be exactly
        // `<project>/.tmp/chat-uploads`; keep it for the whole round. Every
        // containment check below compares a *freshly* resolved entry against
        // it. Re-resolving the root per entry would follow a symlink swapped
        // in mid-sweep and make both sides of the check agree, turning the
        // guard into a no-op for the rest of the round.
        for id in names(&root_real).into_iter().filter(|id| safe_id(id)) {
            let dir = root_real.join(&id);
            let record = match self.read_at(project, &id) {
                Ok(Some(record)) => record,
                // No metadata.json: not a record this round — either crash
                // debris or a create that has not reached its metadata write
                // yet. Age decides (see `reclaim_orphan`).
                Ok(None) => {
                    self.reclaim_orphan(&root_real, &dir);
                    continue;
                }
                Err(e) => {
                    self.skip_record(&dir, &e);
                    continue;
                }
            };
            match future(&record, "expiresAt", (self.options.now)()) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    self.skip_record(&dir, &e);
                    continue;
                }
            }
            // The record branch deletes too, so it needs the same guard as the
            // orphan branch: compare the entry as resolved *now* with the
            // round-start root snapshot. Without it, a root swapped for a
            // symlink mid-round turns every remaining record into a delete
            // outside the project.
            if !inside_root(&root_real, &dir) {
                self.log_cleanup(format!(
                    "[uploads] cleanup skipped {}: outside the uploads root",
                    dir.display()
                ));
                continue;
            }
            match self
                .fault("record-rm", &dir)
                .and_then(|()| remove_tree(&dir))
            {
                Ok(()) => *count += 1,
                Err(e) => self.skip_record(&dir, &e),
            }
        }
    }

    /// A task directory without metadata.json never enters the record sweep,
    /// so TS leaks it (with any uploaded bytes) forever. Rust reclaims it once
    /// its mtime is older than the upload retention: metadata is written at
    /// create time, so a directory past a whole upload TTL without one is
    /// crash debris, and the floor also keeps a just-created directory whose
    /// metadata write has not happened yet. Not counted in `cleanup_expired`'s
    /// return, which keeps its TS meaning (expired records removed).
    fn reclaim_orphan(&self, root_real: &Path, dir: &Path) {
        // Only a real directory can be reclaimed: a symlinked entry must be
        // left alone (removing the link is a different operation and is not
        // this branch's job).
        let Ok(info) = fs::symlink_metadata(dir) else {
            return;
        };
        if !info.is_dir() {
            return;
        }
        let Ok(modified) = info.modified() else {
            return;
        };
        let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH) else {
            return;
        };
        if since_epoch.as_millis() as i64 >= (self.options.now)() - self.options.retention_ms {
            return;
        }
        // Containment last, so the window between the check and the recursive
        // delete is a few instructions instead of the rest of the round. The
        // entry is resolved now and compared with the root snapshot taken when
        // the round started: if `.tmp` or `chat-uploads` was swapped for a
        // symlink since, `dir_real` leaves the snapshot and the delete is
        // skipped instead of following the link outside the project.
        if !inside_root(root_real, dir) {
            return;
        }
        match self.fault("orphan-rm", dir).and_then(|()| remove_tree(dir)) {
            Ok(()) => self.log_cleanup(format!(
                "[uploads] cleanup reclaimed orphan: {}",
                dir.display()
            )),
            Err(e) => self.skip_record(dir, &e),
        }
    }

    fn skip_record(&self, dir: &Path, error: &UploadError) {
        self.log_cleanup(format!(
            "[uploads] cleanup skipped {}: {error}",
            dir.display()
        ));
    }

    fn log_cleanup(&self, line: String) {
        // A tolerant sweep must not reintroduce a round-wide abort through the
        // log itself: `eprintln!` panics when stderr is unwritable (closed or
        // full pipe), which would escape every `skip_record` caller (#1646
        // review). A failed log line is dropped, exactly like the skipped
        // record's own error.
        let _ = writeln!(io::stderr(), "{line}");
        #[cfg(test)]
        self.cleanup_log.lock().unwrap().push(line);
    }

    pub fn verify_attachment(
        &self,
        upload_id: &str,
        project_key: &str,
        attachment_ids: Option<&[String]>,
    ) -> Result<Vec<UploadedAttachment>> {
        Ok(self.verified(upload_id, project_key, attachment_ids)?.1)
    }

    /// Pin with process-independent hard links. Explicit release is idempotent;
    /// dropping the handle leaves a crash-orphan lease for TTL cleanup, like TS.
    pub fn acquire_attachment_lease(
        &self,
        upload_id: &str,
        project_key: &str,
        attachment_ids: Option<&[String]>,
    ) -> Result<UploadArtifactLease> {
        let (record, mut attachments, identities) =
            self.verified(upload_id, project_key, attachment_ids)?;
        // #1888: the record's `projectKey` is canonical (`verified` compared it
        // with the resolved project), so the same spelling is the containment
        // root. The root goes through the shared home guard, not a bare join.
        let project = PathBuf::from(string(&record, "projectKey").unwrap_or_default());
        let dir = prepare_lease_root(&project)?
            .join(upload_id)
            .join(random_id()?);
        // TS:262 reads the clock before mkdir.
        let expiry = object([(
            "expiresAt",
            text(&format_epoch_iso(
                (self.options.now)() + self.options.lease_retention_ms,
            )),
        )]);
        make_dir(&dir)?;
        let result: Result<()> = (|| {
            let path = dir.join("lease.json");
            // Like `write` (metadata.json): the lease file only ever appears
            // complete, so a reader never parses a half-written lease. The
            // window is narrower than "a live lease is never deleted": the
            // reader treats a *missing* lease.json as expired (TS parity), and
            // the directory exists from `make_dir` until the rename (#1646).
            // TS:404–413 writes `lease.json` in place instead.
            let temp = path.with_file_name(format!(
                "lease.json.{}.{}.tmp",
                std::process::id(),
                random_id()?
            ));
            let mut file = open_new(&temp)?;
            self.fault("lease-write", &temp)?;
            file.write_all(json_line(&expiry)?.as_bytes())
                .map_err(|e| UploadError::io(e, "write", None))?;
            file.sync_all()
                .map_err(|e| UploadError::io(e, "fsync", None))?;
            drop(file);
            self.fault("lease-rename", &temp)?;
            fs::rename(&temp, &path).map_err(|e| UploadError::io(e, "rename", Some(&temp)))?;
            for (index, attachment) in attachments.iter_mut().enumerate() {
                let source = PathBuf::from(string(attachment, "path").unwrap_or_default());
                let destination = dir.join(string(attachment, "attachmentId").unwrap_or_default());
                self.fault("lease-link", &source)?;
                #[cfg(test)]
                swap_test_hook::notify(
                    swap_test_hook::SwapWindow::BetweenVerifiedAndLeaseLink,
                    &source,
                );
                fs::hard_link(&source, &destination)
                    .map_err(|e| UploadError::io(e, "link", Some(&source)))?;
                // #1494 sibling P1: `hard_link` re-walks `source`, so a leaf
                // renamed between the integrity check in `verified` and this
                // call would be pinned as the lease artifact and later inlined
                // by the composer (it accepts any in-root regular file). The
                // link itself is the ground truth: compare the linked inode
                // with the identity recorded on the verified handle.
                if !lease_link_matches(
                    &destination,
                    identities.get(index).copied().flatten(),
                    &string(attachment, "sha256").unwrap_or_default(),
                    number(attachment, "bytes"),
                ) {
                    return Err(UploadError::dialog(
                        "ATTACHMENT_TAMPERED",
                        "Attachment artifact changed between verification and lease acquisition.",
                    ));
                }
                set(attachment, "path", path_value(&destination));
            }
            Ok(())
        })();
        if let Err(e) = result {
            remove_tree(&dir)?;
            if e.code == "ENOENT" {
                return Err(UploadError::dialog(
                    "ATTACHMENT_TAMPERED",
                    "Attachment artifact became unavailable while acquiring its Gateway lease.",
                ));
            }
            return Err(e);
        }
        Ok(UploadArtifactLease {
            attachments,
            dir,
            released: Mutex::new(None),
        })
    }

    fn verified(
        &self,
        upload_id: &str,
        project_key: &str,
        attachment_ids: Option<&[String]>,
    ) -> Result<(
        UploadRecord,
        Vec<UploadedAttachment>,
        Vec<Option<AttachmentIdentity>>,
    )> {
        let record = self.get(upload_id)?;
        let project = (self.options.resolve_project)(&JsString::from(project_key))?;
        let project = fs::canonicalize(&project)
            .map_err(|e| UploadError::io(e, "realpath", Some(&project)))?;
        if value(&record, "projectKey") != path_value(&project) {
            return Err(UploadError::dialog(
                "PROJECT_PATH_FORBIDDEN",
                "Upload belongs to another project.",
            ));
        }
        if status(&record) == "expired" {
            return Err(UploadError::dialog("ATTACHMENT_EXPIRED", "Upload expired."));
        }
        if status(&record) != "completed" {
            return Err(UploadError::dialog(
                "UPLOAD_NOT_COMPLETED",
                format!("Upload is {}.", status(&record)),
            ));
        }
        let wanted = attachment_ids.map(|ids| ids.iter().collect::<HashSet<_>>());
        let attachments: Vec<_> = array(&record, "attachments")
            .iter()
            .filter(|a| {
                wanted
                    .as_ref()
                    .is_none_or(|w| w.contains(&string(a, "attachmentId").unwrap_or_default()))
            })
            .cloned()
            .collect();
        if wanted.is_some_and(|w| attachments.len() != w.len()) {
            return Err(UploadError::dialog(
                "ATTACHMENT_NOT_FOUND",
                "Unknown attachmentId.",
            ));
        }
        // #1494 sibling P1: one open per attachment, and the integrity check
        // and the identity recorded for the later lease `hard_link` come from
        // that same handle. A path-walked `metadata` followed by a separate
        // `File::open` inside `sha256_file` left a window in which the leaf
        // could be renamed to an outside file, so the hash covered a different
        // inode than the one the lease later pinned.
        // #2098: that one open is the guarded family (`O_NOFOLLOW |
        // O_NONBLOCK`), and the handle's own type and length are the terms:
        // a leaf swapped for a FIFO must return at once instead of waiting
        // for a writer, a swapped symlink is refused at the open, and a
        // sparse file (or an approved inode that keeps growing) cannot turn
        // the digest read into an unbounded one — `hash_file_exact` reads
        // exactly the record's `bytes` and requires the entry to end there.
        let mut identities = Vec::with_capacity(attachments.len());
        for attachment in &attachments {
            let path = PathBuf::from(string(attachment, "path").unwrap_or_default());
            let mut file = match open_artifact_guarded(&path) {
                Ok(file) => file,
                Err(_) => return Err(attachment_integrity_failed(attachment)),
            };
            let info = file
                .metadata()
                .map_err(|e| UploadError::io(e, "stat", None))?;
            let expected_bytes = number(attachment, "bytes");
            // #2098 fix-r2 (review MM concurrency#1): an absolute ceiling on
            // the summary read. `bytes` is record data, not a measurement
            // this gateway made, so a forged record pairing it with a
            // matching sparse leaf turns the digest read into a multi-TiB
            // stream without this bound. The write side caps a real
            // attachment at `options.max_file_bytes` (default 1 GiB), so a
            // record beyond the ceiling cannot be one this gateway wrote;
            // it is refused as the same integrity failure as the terms
            // below — the user-perceivable difference is only that an
            // inconsistent record of that shape is refused at once instead
            // of being read to its claimed length.
            if expected_bytes > MAX_ATTACHMENT_READ_BYTES as f64 {
                return Err(attachment_integrity_failed(attachment));
            }
            // A directory is deliberately not refused here: its read fails
            // at once with Node's `EISDIR … read` descriptor-read shape
            // (#656), so it is neither the FIFO's wait nor the device's
            // unbounded stream this term exists for.
            if (!info.file_type().is_file() && !info.file_type().is_dir())
                || info.len() as f64 != expected_bytes
                || hash_file_exact(&mut file, expected_bytes as u64)?
                    != string(attachment, "sha256").unwrap_or_default()
            {
                return Err(attachment_integrity_failed(attachment));
            }
            identities.push(attachment_identity(&info));
        }
        Ok((record, attachments, identities))
    }

    fn progress(&self, record: &UploadRecord, delta: u64) -> Result<UploadRecord> {
        self.mutate(record, |r| {
            set(
                r,
                "uploadedBytes",
                JsValue::Number(
                    number(r, "totalBytes").min(number(r, "uploadedBytes") + delta as f64),
                ),
            );
            true
        })
    }

    /// Serialized per uploadId within this instance only (see module docs).
    /// The closure reports whether the record actually changed (#2275): an
    /// already-settled no-op, or a refusal whose closure answers `false`,
    /// skips the write, the emit and the `updatedAt` stamp entirely — that
    /// refusal reaches the caller without a metadata rewrite that could
    /// fail and poison the gate behind a verdict that never needed the
    /// disk in the first place. A refusal that settles the record instead
    /// (the manifest-mismatch `mark_failed`) answers `true` and does write.
    fn mutate(
        &self,
        record: &UploadRecord,
        change: impl FnOnce(&mut UploadRecord) -> bool,
    ) -> Result<UploadRecord> {
        let id = string(record, "uploadId").unwrap_or_default();
        let gate = self
            .updates
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .clone();
        let mut state = gate.lock().unwrap();
        if let Some(e) = &state.failure {
            return Err(e.clone());
        }
        let result: Result<UploadRecord> = (|| {
            let project = PathBuf::from(string(record, "projectKey").unwrap_or_default());
            // #2279: the sweep (or any remover) can take the whole task
            // directory between the caller's outside `get()` and this in-gate
            // re-read. Falling back to the outside snapshot let `write`'s
            // `make_dir` resurrect the directory and report a completion
            // whose files the sweep had already removed, so a vanished record
            // is refused instead. Like any in-gate failure the refusal stays
            // cached in the queue, exactly as TS retains the rejected
            // promise.
            let mut current = self.read_at(&project, &id)?.ok_or_else(|| {
                UploadError::dialog("UPLOAD_NOT_FOUND", format!("Unknown uploadId: {id}"))
            })?;
            if !change(&mut current) {
                return Ok(current);
            }
            set(
                &mut current,
                "updatedAt",
                text(&format_epoch_iso((self.options.now)())),
            );
            self.write(&current)?;
            self.emit(&current);
            Ok(current)
        })();
        if let Err(e) = &result {
            state.failure = Some(e.clone());
        }
        // Retain failed queues, matching TS's rejected Promise. Healthy entries
        // are removed only when there is no queued waiter holding this gate.
        let mut updates = self.updates.lock().unwrap();
        if result.is_ok() && Arc::strong_count(&gate) == 2 {
            updates.remove(&id);
        }
        result
    }
    fn drain_updates(&self, id: &str) -> Result<()> {
        let gate = self.updates.lock().unwrap().get(id).cloned();
        if let Some(gate) = gate
            && let Some(e) = &gate.lock().unwrap().failure
        {
            return Err(e.clone());
        }
        Ok(())
    }
    fn emit(&self, record: &UploadRecord) {
        let id = string(record, "uploadId").unwrap_or_default();
        self.listeners
            .lock()
            .unwrap()
            .entries
            .retain(|_, (upload, tx)| *upload != id || tx.send(record.clone()).is_ok());
    }

    fn write(&self, record: &UploadRecord) -> Result<()> {
        let path = task_dir(record).join("metadata.json");
        make_dir(path.parent().unwrap())?;
        let temp = path.with_file_name(format!(
            "metadata.json.{}.{}.tmp",
            std::process::id(),
            random_id()?
        ));
        let mut file = open_new(&temp)?;
        // Like TS: write/sync failure closes the handle, leaving the temp file.
        self.fault("metadata-write", &temp)?;
        file.write_all(json_line(record)?.as_bytes())
            .map_err(|e| UploadError::io(e, "write", None))?;
        file.sync_all()
            .map_err(|e| UploadError::io(e, "fsync", None))?;
        drop(file);
        if let Err(e) = self.rename(&temp, &path) {
            if !["EPERM", "EEXIST", "EACCES"].contains(&e.code.as_str()) {
                let _ = fs::remove_file(&temp);
                return Err(e);
            }
            let _ = fs::remove_file(&path);
            if let Err(retry) = self.rename(&temp, &path) {
                let _ = fs::remove_file(&temp);
                return Err(retry);
            }
        }
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.fault("metadata-rename", from)?;
        fs::rename(from, to).map_err(|e| UploadError::io(e, "rename", Some(from)))
    }
    fn read_at(&self, project: &Path, id: &str) -> Result<Option<UploadRecord>> {
        assert_id(id)?;
        let root = project.join(".tmp/chat-uploads");
        let path = root.join(id).join("metadata.json");
        let canonical_project = match fs::canonicalize(project) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(UploadError::io(e, "realpath", Some(project))),
        };
        let canonical_path = match fs::canonicalize(&path) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(UploadError::io(e, "realpath", Some(&path))),
        };
        let upload_root = canonical_project.join(".tmp/chat-uploads");
        if !canonical_path.starts_with(&upload_root) {
            return Err(UploadError::dialog(
                "PROJECT_PATH_FORBIDDEN",
                "Upload metadata symlink leaves the upload directory.",
            ));
        }
        // #1494: the prefix check above ran against a spelling, and
        // `File::open` re-walks the path — a `metadata.json` swapped for an
        // outside symlink between the canonicalize and the open reads
        // outside bytes while the checked spelling never changed (the same
        // race reproduced against the TS readAt, node v22.22.0,
        // `_tmp/20261106-issue-1468-1494/ts-probe/read_at_toctou_probe.mjs`).
        // The identity the validated spelling resolved to, recorded before
        // the open: the handle check below answers with the object's
        // *current* name, so a rename after the open could move an outside
        // record under the upload root. A rename cannot change which inode
        // the descriptor references (same residual as `services.rs` step 0).
        // #1494 r2 (deepseek concurrency#2 / boundary#1): the identity must
        // be the object the containment check above validated.
        // `fs::metadata` re-walks the path and follows a symlink, so a link
        // planted between the canonicalize and this stat became the
        // "approved" object, the open followed the same link, and a rename
        // then gave the outside record an in-root name — every check passed
        // and the outside record was returned. The canonical path is
        // symlink-free by construction, so a link here appeared after the
        // validation: `fs::symlink_metadata` does not follow it and the read
        // is refused. (A regular record renamed over the leaf is still
        // caught by the identity comparison below.)
        #[cfg(test)]
        swap_test_hook::notify(
            swap_test_hook::SwapWindow::BetweenCanonicalizeAndApprovedStat,
            &canonical_path,
        );
        let approved = match fs::symlink_metadata(&canonical_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(UploadError::dialog(
                    "PROJECT_PATH_FORBIDDEN",
                    "Upload metadata path became a symlink after validation.",
                ));
            }
            Ok(metadata) => metadata,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(UploadError::io(e, "realpath", Some(&canonical_path))),
        };
        // No portable cross-platform file identity outside Unix; the handle
        // containment check above is all that platform gets (the same honest
        // residual as `services.rs`'s non-Unix `attachment_identity_matches`).
        #[cfg(not(unix))]
        let _ = &approved;
        #[cfg(test)]
        swap_test_hook::notify(
            swap_test_hook::SwapWindow::BetweenCanonicalizeAndOpen,
            &canonical_path,
        );
        let bytes = match open_artifact_guarded(&canonical_path) {
            // UploadStore.ts:384 reads through `readFile` (node:fs/promises): an
            // open error names its path, a descriptor read error (EISDIR after a
            // directory swap, EIO, ...) is `read` without one (node v22.22.0,
            // `_tmp/20261025-issue-656/node-probe/read-stages.out`).
            Ok(mut file) => {
                #[cfg(test)]
                swap_test_hook::notify(swap_test_hook::SwapWindow::AfterOpen, &canonical_path);
                // #1494: bind the approval to the opened handle — the
                // platform's own resolved path for the descriptor must still
                // name a location inside the canonical upload root, and a
                // handle with no resolvable path is never read either
                // (fail closed, like the #1468 lease binding). The identity
                // term below also refuses a benign concurrent store rewrite
                // (`write` renames a fresh `metadata.json` over the old one):
                // that mismatch is indistinguishable from a post-approval
                // rename of an outside vnode into the root, and a retry would
                // re-read the now-in-root replacement and hand the attacker
                // the bytes. The availability trade-off is owned by the
                // #1494 P2 follow-up.
                let handle_inside = match crate::handle_path::opened_path(&file) {
                    Ok(handle) => handle.starts_with(&upload_root),
                    Err(_) => false,
                };
                if !handle_inside {
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata handle leaves the upload directory.",
                    ));
                }
                // #2098: the open is the guarded family, so the terms that
                // make it safe run on the one descriptor it returned. The
                // type term refuses a `metadata.json` swapped for a FIFO
                // (the `O_NONBLOCK` open returned at once) or a device, and
                // the length terms below refuse the sparse-file and
                // same-inode-append faces of the unbounded read.
                let opened = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(e) => return Err(UploadError::io(e, "stat", None)),
                };
                if !opened.file_type().is_file() && !opened.file_type().is_dir() {
                    // A directory keeps its Node read-stage shape: the read
                    // below fails at once with `EISDIR … read` (#656,
                    // `upload_metadata_directory_reports_the_read_stage`),
                    // so it is neither the FIFO's wait nor the device's
                    // unbounded stream this term exists for.
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata path became a non-regular file after validation.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
                if opened.len() > MAX_METADATA_JSON_BYTES {
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata file exceeds the supported size.",
                    ));
                }
                read_exact_file(&mut file, opened.len())?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(UploadError::io(e, "open", Some(&canonical_path))),
        };
        let record = parse_json(&String::from_utf8_lossy(&bytes)).map_err(UploadError::json)?;
        if value(&record, "uploadId") != text(id)
            || value(&record, "projectKey") != path_value(&canonical_project)
        {
            return Err(UploadError::dialog(
                "UPLOAD_INTEGRITY_MISMATCH",
                "Upload metadata identity does not match its directory.",
            ));
        }
        Ok(Some(record))
    }
    fn list_in_project(&self, project: &Path) -> Result<Vec<UploadRecord>> {
        let mut records = vec![];
        for id in names(&project.join(".tmp/chat-uploads"))
            .into_iter()
            .filter(|id| safe_id(id))
        {
            if let Some(record) = self.read_at(project, &id)? {
                records.push(record);
            }
        }
        Ok(records)
    }
    fn expire_if_needed(&self, record: UploadRecord) -> Result<UploadRecord> {
        if status(&record) == "expired" || future(&record, "expiresAt", (self.options.now)())? {
            return Ok(record);
        }
        self.mutate(&record, |r| {
            set(r, "status", text("expired"));
            set(r, "errorCode", text("ATTACHMENT_EXPIRED"));
            set(r, "errorMessage", text("Upload expired."));
            true
        })
    }
    fn cleanup_expired_leases(&self, project: &Path) -> Result<()> {
        // Same plain-root gate as the record half: `names()` and
        // `remove_tree()` follow intermediate symlinks, and "unreadable lease
        // means expired" makes deletion the default action, so a symlinked
        // `.tmp` or `.tmp/chat-upload-leases` would delete lease trees outside
        // the project. Missing is the ordinary empty state and stays silent.
        let root_real = match plain_root(project, "chat-upload-leases") {
            PlainRoot::Plain(root) => root,
            PlainRoot::Missing => return Ok(()),
            PlainRoot::Blocked => {
                self.log_cleanup(format!(
                    "[uploads] lease cleanup skipped {}: leases root is not a plain directory",
                    project.join(".tmp/chat-upload-leases").display()
                ));
                return Ok(());
            }
        };
        // Snapshot once, like the record half: the entry checks below resolve
        // against this value for the whole round.
        for id in names(&root_real).into_iter().filter(|id| safe_id(id)) {
            let leases = root_real.join(id);
            for lease in names(&leases).into_iter().filter(|id| safe_id(id)) {
                let dir = leases.join(lease);
                // #2098 fix-r2 (review DS#2 / MM): the fourth path-read of
                // the family. This was `fs::read` — a bare open that
                // follows a swapped link and blocks a FIFO swapped in for
                // `lease.json` for a writer that never comes, and reads a
                // device link without EOF (the same faces the three sibling
                // readers close with the guarded open). The entry read goes
                // through `open_artifact_guarded` and the handle's own
                // terms; anything the terms refuse is "unreadable", which
                // this half already spells "expired" (the malformed-JSON
                // default just above), so no legal entry changes meaning.
                let metadata = (|| {
                    let mut file = open_artifact_guarded(&dir.join("lease.json")).ok()?;
                    let info = file.metadata().ok()?;
                    // The type term: a FIFO or device entry is not read at
                    // all. A directory is left to its read-stage `EISDIR`,
                    // the same deliberate shape as the siblings.
                    if !info.file_type().is_file() && !info.file_type().is_dir() {
                        return None;
                    }
                    // The same cap as the record leaf this JSON mirrors.
                    if info.len() > MAX_METADATA_JSON_BYTES {
                        return None;
                    }
                    let bytes = read_exact_file(&mut file, info.len()).ok()?;
                    parse_json(&String::from_utf8_lossy(&bytes)).ok()
                })();
                if let Some(m) = metadata
                    && future(&m, "expiresAt", (self.options.now)())?
                {
                    continue;
                }
                // fix-r1 (#2423 r1 评审 deepseek concurrency#1，实测坐实):
                // containment-only `inside_root` passes a linked `<id>`
                // whose resolution stays inside the root, and the entries
                // it lists are then judged by the victim's own bytes — an
                // alias onto an **active** lease dir makes every file
                // inside it read as an expired orphan, destroying the live
                // pin set without a race (probe:
                // `_tmp/20261108-cb8-2423-arb/lease-alias-probe.out.txt`).
                // The component walk the cancel delete point uses refuses
                // every link below the root; the instruction gap between
                // the walk and `remove_tree` is the same registered
                // TOCTOU residual family.
                if canonical_home(&dir, &root_real, &root_real).is_none() {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root or a linked lease dir",
                        dir.display()
                    ));
                    continue;
                }
                self.fault("lease-rm", &dir)?;
                remove_tree(&dir)?;
            }
            if names(&leases).is_empty() && inside_root(&root_real, &leases) {
                remove_tree(&leases)?;
            }
        }
        Ok(())
    }
    fn validate_manifest(&self, files: &JsValue) -> Result<Vec<JsValue>> {
        let JsValue::Array(files) = files else {
            return Err(self.manifest_count_error());
        };
        if files.is_empty() || files.len() > self.options.max_files {
            return Err(self.manifest_count_error());
        }
        let mut ids = HashSet::new();
        let mut total = 0.0;
        files
            .iter()
            .map(|file| {
                let id = string(file, "clientFileId").unwrap_or_default();
                if !safe_id(&id) || !ids.insert(id.clone()) {
                    return Err(UploadError::dialog(
                        "UPLOAD_MANIFEST_INVALID",
                        "clientFileId must be unique and filesystem-safe.",
                    ));
                }
                let name = value(file, "name");
                let name = name.as_string().map(trim_js);
                let relative = file.get("relativePath").and_then(JsValue::as_string);
                if name.as_ref().is_none_or(|s| s.is_empty())
                    || relative.is_none_or(|r| !safe_relative(r))
                {
                    return Err(UploadError::dialog(
                        "UPLOAD_MANIFEST_INVALID",
                        "name and a safe relativePath are required.",
                    ));
                }
                let size = file
                    .get("size")
                    .and_then(JsValue::as_number)
                    .unwrap_or(f64::NAN);
                if !size.is_finite()
                    || size.fract() != 0.0
                    || !(0.0..=9_007_199_254_740_991.0).contains(&size)
                    || size > self.options.max_file_bytes as f64
                {
                    return Err(UploadError::dialog(
                        "UPLOAD_FILE_TOO_LARGE",
                        format!("Invalid file size for {id}."),
                    ));
                }
                let sha = value(file, "sha256");
                if !sha.is_undefined()
                    && string(file, "sha256")
                        .is_none_or(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err(UploadError::dialog(
                        "UPLOAD_MANIFEST_INVALID",
                        "sha256 must contain 64 hexadecimal characters.",
                    ));
                }
                total += size;
                if total > self.options.max_task_bytes as f64 {
                    return Err(UploadError::dialog(
                        "UPLOAD_TASK_TOO_LARGE",
                        "Upload task exceeds its byte limit.",
                    ));
                }
                let mut file = file.clone();
                set(&mut file, "name", JsValue::String(name.unwrap()));
                Ok(file)
            })
            .collect()
    }
    fn manifest_count_error(&self) -> UploadError {
        UploadError::dialog(
            "UPLOAD_MANIFEST_INVALID",
            format!("files must contain 1..{} entries.", self.options.max_files),
        )
    }

    // Per-instance syscall seam, present only in unit tests. Tests also exercise
    // actual filesystem conflicts and an interrupting Read implementation.
    fn fault(&self, operation: &str, path: &Path) -> Result<()> {
        #[cfg(test)]
        {
            let mut faults = self.faults.lock().unwrap();
            if faults.first().is_some_and(|(op, _)| op == operation) {
                let (_, errno) = faults.remove(0);
                return Err(UploadError::io(
                    io::Error::from_raw_os_error(errno),
                    operation.strip_prefix("metadata-").unwrap_or(operation),
                    Some(path),
                ));
            }
        }
        let _ = (operation, path);
        Ok(())
    }
}

/// TS `ui/server/routes/uploads.js:73-76`: `15 * 60 * 1000`.
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Lower bound [`spawn_cleanup`] clamps its period to. Exported inside the crate
/// so callers and tests can check a period against it instead of hard-coding the
/// value; the clamp still rewrites anything below it.
pub(crate) const MIN_CLEANUP_INTERVAL: Duration = Duration::from_secs(1);

/// TS `ui/server/routes/uploads.js:73-76`: detached `setInterval` that calls
/// [`UploadStore::cleanup_expired`] and only logs failures. `Weak` follows
/// `config::store::watch_loop`: the tick after the last store handle is
/// dropped ends the loop, so nothing has to cancel it on shutdown. Dropping
/// the returned handle does not wait, but a sweep already running inside
/// `spawn_blocking` is not cancelled either: dropping the runtime still waits
/// for that one in-flight sweep (TS's `unref` timer likewise leaves an
/// already-started `rm` running).
pub fn spawn_cleanup(store: &Arc<UploadStore>, interval: Duration) -> tokio::task::JoinHandle<()> {
    // `tokio::time::interval` panics on a zero period, and a panic raised
    // inside this detached task is invisible: the caller drops the handle and
    // cleanup stops silently. Clamp instead so any sub-floor period yields a
    // live loop; the floor also keeps a mistyped separator from turning into a
    // full-project directory scan per millisecond.
    let interval = interval.max(MIN_CLEANUP_INTERVAL);
    let store = Arc::downgrade(store);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // A `setInterval` callback first runs after one interval, not at t=0.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(store) = store.upgrade() else { return };
            let failure = match tokio::task::spawn_blocking(move || store.cleanup_expired()).await {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(error) => Some(error.to_string()),
            };
            if let Some(error) = failure {
                // Same reason as `log_cleanup`: `eprintln!` panics when
                // stderr is unwritable, and that panic would end this
                // detached loop — the one failure the schedule must survive.
                let _ = writeln!(io::stderr(), "[uploads] cleanup failed: {error}");
            }
        }
    })
}

pub struct UploadSubscription {
    pub receiver: mpsc::Receiver<UploadRecord>,
    id: u64,
    listeners: Weak<Mutex<Listeners>>,
}
impl Drop for UploadSubscription {
    fn drop(&mut self) {
        if let Some(listeners) = self.listeners.upgrade() {
            listeners.lock().unwrap().entries.remove(&self.id);
        }
    }
}
pub struct UploadArtifactLease {
    pub attachments: Vec<UploadedAttachment>,
    dir: PathBuf,
    released: Mutex<Option<Result<()>>>,
}
impl UploadArtifactLease {
    pub fn release(&self) -> Result<()> {
        self.released
            .lock()
            .unwrap()
            .get_or_insert_with(|| {
                // The same guard as the lease sweep, applied late: `dir` was
                // built from the record's canonical project at acquire time,
                // and the lease can live for many turns. If `.tmp` or
                // `chat-upload-leases` was swapped for a symlink since, the
                // recursive delete would follow it out of the project. Require
                // the root to still resolve to exactly the plain path, then the
                // entry to still be strictly inside it.
                let Some(root) = self.dir.parent().and_then(Path::parent) else {
                    return Ok(());
                };
                // "Gone" stays a quiet no-op (the lease is already clean, and
                // `release` is idempotent); any other failure keeps the
                // original `remove_tree` behaviour of surfacing the error
                // instead of caching a success that never happened.
                let root_real = match fs::canonicalize(root) {
                    Ok(real) => real,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(UploadError::io(e, "realpath", Some(root))),
                };
                if root_real != root {
                    return Ok(());
                }
                match resolved_inside(&root_real, &self.dir) {
                    Ok(Some(_)) => remove_tree(&self.dir),
                    Ok(None) => Ok(()),
                    Err(e) => Err(UploadError::io(e, "realpath", Some(&self.dir))),
                }
            })
            .clone()
    }
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn set(object: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(o) = object {
        o.insert(key.into(), value);
    }
}
fn value(object: &JsValue, key: &str) -> JsValue {
    object.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn text(s: &str) -> JsValue {
    JsValue::from(s)
}
/// `Unknown projectKey: ${projectKey}` (UploadStore.ts:63) with the caller's
/// raw UTF-16 units. `display`/lossy decoding would turn a lone surrogate into
/// U+FFFD, changing the message even though the key stayed unavailable.
pub(crate) fn unknown_project_key(project_key: &JsString) -> UploadError {
    let mut message = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(project_key.units());
    UploadError::dialog("PROJECT_NOT_FOUND", JsString::from_utf16(message))
}
/// `Unexpected or duplicate file part: ${clientFileId}` and the other part
/// messages, keeping every UTF-16 unit (P1-1 #865).
fn part_message(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    let mut message = prefix.encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(value.units());
    message.extend(suffix.encode_utf16());
    JsString::from_utf16(message)
}
fn string(object: &JsValue, key: &str) -> Option<String> {
    object.get(key)?.as_string()?.to_utf8().ok()
}
fn number(object: &JsValue, key: &str) -> f64 {
    object
        .get(key)
        .and_then(JsValue::as_number)
        .unwrap_or(f64::NAN)
}
fn array<'a>(object: &'a JsValue, key: &str) -> &'a [JsValue] {
    match object.get(key) {
        Some(JsValue::Array(a)) => a,
        _ => &[],
    }
}
fn status(record: &UploadRecord) -> String {
    string(record, "status").unwrap_or_default()
}
fn active(record: &UploadRecord) -> bool {
    matches!(status(record).as_str(), "created" | "uploading")
}
/// The `UPLOAD_INVALID_STATE` refusal for a record that left the active set
/// — one message formula shared by the pre-gate check (`require_active`) and
/// the in-gate refusals (#1631).
fn invalid_state(record: &UploadRecord) -> UploadError {
    UploadError::dialog(
        "UPLOAD_INVALID_STATE",
        format!("Upload is {}.", status(record)),
    )
}
/// The three-field failed-record write shared by `fail` and `complete`'s
/// in-gate mismatch branch (which cannot re-enter the gate through `fail`).
fn mark_failed(record: &mut UploadRecord, code: &str, message: &JsString) {
    set(record, "status", text("failed"));
    set(record, "errorCode", text(code));
    set(record, "errorMessage", JsValue::String(message.clone()));
}
fn require_active(record: &UploadRecord) -> Result<()> {
    if active(record) {
        Ok(())
    } else {
        Err(invalid_state(record))
    }
}
fn path_value(path: &Path) -> JsValue {
    text(&path.to_string_lossy())
}
fn task_dir(record: &UploadRecord) -> PathBuf {
    Path::new(&string(record, "projectKey").unwrap_or_default())
        .join(".tmp/chat-uploads")
        .join(string(record, "uploadId").unwrap_or_default())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn json_line(value: &JsValue) -> Result<String> {
    Ok(format!(
        "{}\n",
        stringify_json(value)
            .map_err(UploadError::json)?
            .unwrap_or_default()
    ))
}
fn attachment_integrity_failed(attachment: &UploadedAttachment) -> UploadError {
    UploadError::dialog(
        "ATTACHMENT_TAMPERED",
        format!(
            "Attachment integrity check failed: {}",
            string(attachment, "attachmentId").unwrap_or_default()
        ),
    )
}

/// #1468/#1887/#1888: the one guard both legitimate homes share — the lease
/// root and the upload store — and, since #1888, the writer's lease root too.
/// A root that is itself a symlink, even one pointing back into the project,
/// is never a home: the lexical spelling stays accepted while every path under
/// the link target becomes resolvable, which is the exact shape `read_at`'s
/// lexical prefix check rejects.
///
/// #1887: the same holds for every *ancestor* between the project root and the
/// root. A `.tmp` swapped for a link to another subtree of the same project
/// re-resolves the canonical spelling to a path that is still inside
/// `project_canon`, so the containment term below accepts it and `src/...`
/// becomes a home for both roots; the walk below refuses that layout, which is
/// the one `read_at` already refuses lexically, so the read side and the
/// attachment-composition side now answer the same way.
///
/// `relative` is spelled off `root`: the project's own spelling may go through
/// symlinks (#1618 — `projectKey` keeps Node's logical identity), and those
/// are legitimate, so only the components below the project root are walked.
/// `project_canon` is the canonical project root the resolved spellings are
/// held against.
///
/// Residual (review deepseek boundary#2, not introduced by this change): the
/// link check and the canonicalization are two syscalls, so **any** walked
/// component — the leaf or an ancestor such as `.tmp` — can be exchanged
/// between them. Swapping one for a link that resolves *outside* the project
/// is still refused, by the containment term below (deterministic proof:
/// `store_root_swapped_outside_after_the_walk_is_not_a_home`); swapping
/// an ancestor for a link that resolves *inside* the project is admitted
/// (#1887 review concurrency#3). Both are strictly narrower than the
/// pre-guard behaviour (which accepted every such root unconditionally);
/// closing them would need `O_NOFOLLOW` plus a handle-derived path, as
/// `handle_path` does.
///
/// The guard lives in one function so every root is exercised by the same
/// unit tests and mutation rows (`upload_store_root_outside_the_project_is_not_a_home`,
/// `lease_root_link_is_not_a_home`, `lease_root_under_a_linked_tmp_ancestor_is_not_a_home`,
/// `store_root_swapped_outside_after_the_walk_is_not_a_home`).
pub(crate) fn canonical_home(root: &Path, project: &Path, project_canon: &Path) -> Option<PathBuf> {
    // The home must be spelled under the project root the caller named; a
    // spelling that escapes it is not a home whatever it resolves to.
    let relative = root.strip_prefix(project).ok()?;
    let mut walked = project.to_path_buf();
    for component in relative.components() {
        // `..` or an absolute re-spelling would walk outside `project`; the
        // callers pass project-relative homes only.
        if !matches!(component, Component::Normal(_)) {
            return None;
        }
        walked.push(component);
        // Leaf and ancestor are the same answer: neither may be a link.
        match fs::symlink_metadata(&walked) {
            Ok(metadata) if metadata.file_type().is_symlink() => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    #[cfg(test)]
    swap_test_hook::notify(
        swap_test_hook::SwapWindow::BetweenWalkAndCanonicalize,
        &walked,
    );
    match fs::canonicalize(&walked) {
        Ok(resolved) if resolved.starts_with(project_canon) => Some(resolved),
        _ => None,
    }
}

/// #1888: the lease root the writer creates under, through the same guard the
/// read side uses. The writer used to join `record.projectKey` directly and
/// `mkdir` whatever was there, so a `.tmp/chat-upload-leases` (or an ancestor
/// `.tmp`) swapped for a link took the write into the link target while the
/// read side refused the same root — the lease was written under a spelling
/// the composer then answered not-found for.
///
/// Only the lease directory itself can be missing here: `verified` just read
/// the record from `<project>/.tmp/chat-uploads`. A missing `.tmp` therefore
/// means the root is not the one the record came from, and the guard refuses
/// it rather than creating a tree outside it.
fn prepare_lease_root(project: &Path) -> Result<PathBuf> {
    let home = project.join(".tmp").join("chat-upload-leases");
    if let Some(canonical) = canonical_home(&home, project, project) {
        return Ok(canonical);
    }
    // Only now is a directory created. `create_dir` never follows a link at
    // the leaf (EEXIST), so a leaf swapped in after the check above is refused
    // by the second pass. An **ancestor** swapped in that gap is not covered by
    // this call: path resolution follows it, so the directory is created under
    // the link target and only the second pass refuses the root — the stray
    // empty directory here is harmless on its own.
    //
    // What is **not** harmless, and is the residual (#1887 review
    // boundary#1 / concurrency#1): the value returned above is a plain path,
    // so it is re-resolved by every later syscall. An ancestor swapped after
    // *this* function's last check but before the caller's `make_dir`
    // (`acquire_attachment_lease`) and `fs::hard_link` re-resolves the same
    // spelling outside the project, and a whole lease — `lease.json` plus the
    // attachment's hard link — is written there. No later term refuses it: the
    // linked inode really is the approved attachment and its digest really
    // matches, and the TTL sweep only walks the in-project `.tmp`. Closing it
    // needs the same dirfd/`O_NOFOLLOW` chain creation the read side uses.
    if canonical_home(&project.join(".tmp"), project, project).is_none() {
        return Err(lease_root_unavailable());
    }
    match fs::create_dir(&home) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(UploadError::io(e, "mkdir", Some(&home))),
    }
    canonical_home(&home, project, project).ok_or_else(lease_root_unavailable)
}

fn lease_root_unavailable() -> UploadError {
    UploadError::dialog(
        "PROJECT_PATH_FORBIDDEN",
        "Attachment lease root is unavailable.",
    )
}

/// The `(dev, inode)` pair recorded on the verified handle. Non-Unix has no
/// cross-platform file identity, so `lease_link_matches` keeps only its
/// content term there (#1889); Unix is the shipped target.
#[cfg(unix)]
type AttachmentIdentity = (u64, u64);
#[cfg(not(unix))]
type AttachmentIdentity = ();

#[cfg(unix)]
fn attachment_identity(metadata: &fs::Metadata) -> Option<AttachmentIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn attachment_identity(_metadata: &fs::Metadata) -> Option<AttachmentIdentity> {
    None
}

/// #1494: the lease hard link must pin the inode the integrity check
/// approved, not whatever the source path names after that check.
///
/// The check must look at what actually got linked: `std::fs::hard_link` does
/// not follow a symlink source on any shipping Unix — it goes through
/// `linkat(..., 0)` on macOS as well as on Linux — so a source swapped for a
/// link puts the *link* into the lease. Measured on macOS 2026-11-06: after
/// `fs::hard_link(link, dest)`, `fs::symlink_metadata(dest)` reports a
/// symlink; Python's `os.link` is the one that follows.
///
/// #1889 (review concurrency#1): the identity term and the content term must
/// read *one* object. A path stat followed by a reopen of the same spelling
/// leaves a window between them, and the reopen resolves whatever is there by
/// then. Measured 2026-11-07
/// (`_tmp/20261107-issue-1887/lease-open-probe/`): a FIFO swapped into the
/// entry made a plain `File::open` never return, and a symlink to `/dev/zero`
/// read past 64 MiB without reaching EOF. Both terms therefore run on the one
/// descriptor `open_lease_entry` returns.
#[cfg(unix)]
fn lease_link_matches(
    destination: &Path,
    expected: Option<AttachmentIdentity>,
    expected_sha256: &str,
    expected_bytes: f64,
) -> bool {
    let (Some(mut file), Some(want)) = (open_artifact_guarded(destination).ok(), expected) else {
        return false;
    };
    let Ok(listed) = file.metadata() else {
        return false;
    };
    listed.file_type().is_file()
        && attachment_identity(&listed) == Some(want)
        && lease_link_content_matches(&mut file, expected_sha256, expected_bytes)
}

/// The guarded open every artifact read through a path goes through (#1889
/// for the lease entry, #2098 for its three siblings): no following a link,
/// no waiting for a writer. A symlink swapped into the spelling is refused
/// at open (`O_NOFOLLOW`), a FIFO swapped into it returns at once instead
/// of blocking the caller for a writer that never comes (`O_NONBLOCK`), and
/// the type check the callers run on the returned descriptor refuses both.
/// `O_NONBLOCK` is a no-op on the regular file this is meant to be.
/// Residual: `open` resolves the path once; the callers' identity and
/// content terms do not resolve it again.
#[cfg(unix)]
pub(crate) fn open_artifact_guarded(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path)
}

/// Non-Unix keeps the bare open (the same honest residual as the lease
/// side's `cfg(not(unix))` branch): no `O_NOFOLLOW`/`O_NONBLOCK` flags exist
/// there, so a swapped symlink is followed and a swapped FIFO still blocks
/// the caller. The descriptor type/length terms the callers run apply on
/// every platform; only the open-family flags are Unix-only.
#[cfg(not(unix))]
pub(crate) fn open_artifact_guarded(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(not(unix))]
fn lease_link_matches(
    destination: &Path,
    _expected: Option<AttachmentIdentity>,
    expected_sha256: &str,
    expected_bytes: f64,
) -> bool {
    // The identity term has no cross-platform answer (see above), and the
    // `O_NOFOLLOW` open is Unix-only; the content term applies here too
    // rather than being skipped with it.
    match File::open(destination) {
        Ok(mut file) => lease_link_content_matches(&mut file, expected_sha256, expected_bytes),
        Err(_) => false,
    }
}

/// #1889: `(dev, inode)` is not an identity on every filesystem. Measured
/// 2026-11-07 (`_tmp/20261107-issue-1887/inode-probe/`): ext4 (cb8) hands the
/// just-freed inode number straight back to a file recreated at the same path
/// (20000/20000 rounds — deterministic), so a delete-and-recreate in the
/// `[verified, hard link]` window compares equal and pins the attacker's
/// bytes; APFS never reused one (0/20000). A ctid-style `ctime` term cannot
/// close it either: `link(2)` itself bumps the inode ctime (5/5 on both
/// filesystems), so it differs even for the untouched file.
///
/// The auxiliary identity is therefore the content digest `verified` already
/// bound to the opened handle: it compared the handle's bytes with the
/// record's `sha256`, and this re-reads the entry the link actually created
/// through the same descriptor the identity term fstat'ed, and compares the
/// same digest. An inode-reusing replacement — and, for free, an in-place
/// rewrite that keeps the inode — no longer matches. Cost is one extra
/// sequential read of the attachment per lease, bounded by the length
/// `verified` approved (see `hash_file_exact`).
fn lease_link_content_matches(file: &mut File, expected_sha256: &str, expected_bytes: f64) -> bool {
    // No digest or no usable length is a refusal, not a pass: a missing
    // record field must never turn the length term into "read to EOF".
    // #2098 fix-r2 adds the same absolute ceiling `verified` enforces, so
    // the two readers of one record can never be talked into different
    // lengths by the record's own `bytes`.
    if expected_sha256.is_empty()
        || !expected_bytes.is_finite()
        || expected_bytes < 0.0
        || expected_bytes > MAX_ATTACHMENT_READ_BYTES as f64
    {
        return false;
    }
    hash_file_exact(file, expected_bytes as u64).is_ok_and(|digest| digest == expected_sha256)
}

/// `verified()` bound the digest to a handle whose length equalled the
/// record's `bytes`. Re-reading to EOF here would let an entry that grows
/// after the `fstat` (same inode, so both identity terms still pass) turn the
/// acquire into an unbounded read: a sparse multi-TiB file is enough, and a
/// writer appending at least as fast as the read never reaches EOF at all.
/// Reading exactly `bytes` and requiring the entry to end there bounds the
/// work by the approved size.
fn hash_file_exact(file: &mut File, limit: u64) -> Result<String> {
    let mut digest = Sha256::new();
    let mut chunk = [0; 64 * 1024];
    let mut left = limit;
    while left > 0 {
        let want = chunk.len().min(left as usize);
        let n = file
            .read(&mut chunk[..want])
            .map_err(|e| UploadError::io(e, "read", None))?;
        if n == 0 {
            return Err(UploadError::io(
                io::Error::from(io::ErrorKind::UnexpectedEof),
                "read",
                None,
            ));
        }
        digest.update(&chunk[..n]);
        left -= n as u64;
    }
    let mut extra = [0_u8; 1];
    let trailing = file
        .read(&mut extra)
        .map_err(|e| UploadError::io(e, "read", None))?;
    if trailing != 0 {
        return Err(UploadError::io(
            io::Error::from(io::ErrorKind::InvalidData),
            "read",
            None,
        ));
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// #2098: the ceiling on a `metadata.json` read. The file is written by this
/// gateway alone and a real record (attachments list included) is far below
/// this bound; the cap exists so a leaf swapped for a sparse file cannot
/// turn the read into an unbounded one, not as a validation of real records.
const MAX_METADATA_JSON_BYTES: u64 = 64 * 1024 * 1024;

/// #2098 fix-r2 (review MM concurrency#1): the absolute ceiling on an
/// attachment summary read — the record's `bytes` paired with the digest
/// read it drives. The value is chosen to match the write side's own bound
/// (`UploadOptions::max_file_bytes`, default 1 GiB), so no record this
/// gateway wrote is ever refused; the ceiling exists so a forged record
/// paired with a matching sparse leaf cannot turn the digest read into an
/// unbounded one.
const MAX_ATTACHMENT_READ_BYTES: u64 = 1024 * 1024 * 1024;

/// [`hash_file_exact`]'s byte form, for the record leaf: read exactly `len`
/// bytes and require the file to end there. A `read_to_end` here would let
/// the same faces through that the lease entry's digest read refuses — a
/// sparse file grown past its stat, or a same-inode writer appending at
/// least as fast as the read — so the read is bounded by the length the
/// type-checked descriptor reported. `len == 0` reads nothing and only
/// requires the trailing EOF, which is what an empty record is.
fn read_exact_file(file: &mut File, len: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0; 64 * 1024];
    let mut left = len;
    while left > 0 {
        let want = chunk.len().min(left as usize);
        let n = file
            .read(&mut chunk[..want])
            .map_err(|e| UploadError::io(e, "read", None))?;
        if n == 0 {
            return Err(UploadError::io(
                io::Error::from(io::ErrorKind::UnexpectedEof),
                "read",
                None,
            ));
        }
        out.extend_from_slice(&chunk[..n]);
        left -= n as u64;
    }
    let mut extra = [0_u8; 1];
    match file.read(&mut extra) {
        Ok(0) => Ok(out),
        Ok(_) => Err(UploadError::io(
            io::Error::from(io::ErrorKind::InvalidData),
            "read",
            None,
        )),
        Err(e) => Err(UploadError::io(e, "read", None)),
    }
}

fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| UploadError::io(e, "read", None))?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
fn make_dir(path: &Path) -> Result<()> {
    crate::node_io::mkdir_recursive(path)
        .map_err(|error| UploadError::io(error.source, "mkdir", Some(&error.path)))
}
fn open_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| UploadError::io(e, "open", Some(path)))
}
/// Whether `dir` currently resolves to a real directory strictly inside
/// `root_real`, the root **snapshot taken when the round started**.
///
/// The entry is resolved freshly, the root is not: re-resolving the root
/// would follow a `.tmp` swapped for a symlink mid-round and make both sides
/// of the comparison agree again, which is exactly the case this guard exists
/// to catch. A missing entry counts as "not inside", so every caller treats it
/// as "do not delete"; a resolution failure that is *not* "missing" is
/// reported so `release` can keep propagating it instead of caching a fake
/// success.
fn inside_root(root_real: &Path, dir: &Path) -> bool {
    resolved_inside(root_real, dir).ok().flatten().is_some()
}

/// [`inside_root`], but telling "gone / outside" (`Ok(None)`) apart from a real
/// I/O failure.
fn resolved_inside(root_real: &Path, dir: &Path) -> io::Result<Option<PathBuf>> {
    match fs::canonicalize(dir) {
        Ok(real) if real != root_real && real.starts_with(root_real) => Ok(Some(real)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn remove_tree(path: &Path) -> Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UploadError::io(e, "rm", Some(path))),
    }
}
/// Whether `<project>/.tmp/<leaf>` may be swept at all.
enum PlainRoot {
    /// `<project>/.tmp/<leaf>` resolves to exactly itself: no component on the
    /// path is a symlink, not even one pointing elsewhere inside the project.
    /// Carries the resolved path, which is also the round's snapshot.
    Plain(PathBuf),
    /// There is no such tree yet (never uploaded / no live lease): nothing to
    /// clean, and nothing worth logging every interval.
    Missing,
    /// Something is there, but at least one component is a symlink or not a
    /// directory: `read_dir`/`remove_dir_all` would follow it out of the
    /// project, so the sweep must not touch it.
    Blocked,
}

/// Gate a cleanup half on `<project>/.tmp/<leaf>` being a plain directory, and
/// return its resolved path — the snapshot the round measures every entry
/// against.
fn plain_root(project: &Path, leaf: &str) -> PlainRoot {
    let dot_tmp = project.join(".tmp");
    let root = dot_tmp.join(leaf);
    for dir in [&dot_tmp, &root] {
        match fs::symlink_metadata(dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return PlainRoot::Blocked,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return PlainRoot::Missing,
            Err(_) => return PlainRoot::Blocked,
        }
    }
    // Both stats can pass and the path can still be swapped before this
    // resolve, so require the plain path back. Equality — not "inside the
    // project" — is the point: a `.tmp` symlinked to another directory
    // *within* the project would still be inside, and the sweep would then
    // treat that directory's children as uploads (or leases) to delete.
    // Both stats already said "a directory is here", so anything other than a
    // clean resolve is a race, not the ordinary empty state: report it as
    // `Blocked` (logged) rather than `Missing` (silent), or a root that
    // vanished exactly here would be indistinguishable from one that never
    // existed.
    match fs::canonicalize(&root) {
        Ok(real) if real == root => PlainRoot::Plain(real),
        _ => PlainRoot::Blocked,
    }
}

fn names(path: &Path) -> Vec<String> {
    // Node readdir is byte-sorted; read failures are deliberately swallowed.
    let mut names: Vec<_> = fs::read_dir(path)
        .ok()
        .and_then(|r| r.collect::<io::Result<Vec<_>>>().ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}
fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn assert_id(s: &str) -> Result<()> {
    if safe_id(s) {
        Ok(())
    } else {
        Err(UploadError::dialog("UPLOAD_NOT_FOUND", "Invalid uploadId."))
    }
}
/// UploadStore.ts:456–459 over UTF-16 units, so lone surrogates behave as in TS.
fn safe_relative(s: &pilotdeck_protocol::JsString) -> bool {
    let units = s.units();
    !units.is_empty()
        && units[0] != u16::from(b'/')
        && !units.contains(&u16::from(b'\\'))
        && units
            .split(|&c| c == u16::from(b'/'))
            .all(|p| !matches!(p, [] | [0x2e] | [0x2e, 0x2e]))
}
fn trim_js(s: &pilotdeck_protocol::JsString) -> pilotdeck_protocol::JsString {
    pilotdeck_protocol::JsString::from_utf16(
        crate::runtime::memory::js_trim_units(s.units()).to_vec(),
    )
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn future(record: &JsValue, field: &str, now: i64) -> Result<bool> {
    let Some(date) = record.get(field).and_then(JsValue::as_string) else {
        return Ok(false);
    };
    Ok(parse_node22_date(date)
        .map_err(|e| UploadError::json(format!("{e:?}")))?
        .is_some_and(|ms| ms > now as f64))
}

/// #1494 window-injection seam (tests only): `read_at` notifies
/// [`swap_test_hook::notify`] between its canonicalize containment check and
/// `File::open`; a test installs a callback that swaps `metadata.json` for an
/// outside symlink exactly inside that window, and must assert the hook
/// actually fired — an unfired hook proves nothing. Per-thread and
/// RAII-removed like the swap seam in native_process/services.rs; production
/// builds compile none of this.
#[cfg(test)]
pub(crate) mod swap_test_hook {
    use std::path::Path;

    /// The #1494 windows: after `read_at`'s canonicalize prefix check but
    /// before the approved identity is sampled (r2 — the identity must not
    /// be taken by following a link planted here), after that sample and
    /// before the `File::open` that re-walks the path, and (third) right
    /// after the open succeeded, before the handle is validated — the window
    /// a rename uses to give an outside vnode a name inside the upload root.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum SwapWindow {
        BetweenCanonicalizeAndApprovedStat,
        BetweenCanonicalizeAndOpen,
        AfterOpen,
        /// #1494 sibling P1: after `verified` hashed the attachment through a
        /// handle but before the lease `hard_link` re-walks its path.
        BetweenVerifiedAndLeaseLink,
        /// #1887 boundary P1: after `canonical_home`'s per-component walk has
        /// passed but before the canonicalization. A root swapped for an
        /// outside link inside that gap is refused by the containment term,
        /// not by the walk, so this window is what keeps that term provable —
        /// without it the term has no deterministic kill.
        BetweenWalkAndCanonicalize,
    }

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static HOOKS: std::cell::RefCell<Vec<(SwapWindow, Hook)>> =
            std::cell::RefCell::default();
    }

    /// Removes exactly its own window's hook on drop.
    pub struct Guard {
        window: SwapWindow,
    }

    pub fn set(window: SwapWindow, hook: impl FnMut(&Path) + 'static) -> Guard {
        HOOKS.with(|hooks| {
            assert!(
                hooks.borrow().iter().all(|(set, _)| *set != window),
                "one swap hook per window"
            );
            hooks.borrow_mut().push((window, Box::new(hook)));
        });
        Guard { window }
    }

    pub fn notify(window: SwapWindow, path: &Path) {
        HOOKS.with(|hooks| {
            for (hook_window, hook) in hooks.borrow_mut().iter_mut() {
                if *hook_window == window {
                    hook(path);
                }
            }
        });
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOKS.with(|hooks| {
                hooks
                    .borrow_mut()
                    .retain(|(window, _)| *window != self.window)
            });
        }
    }
}
