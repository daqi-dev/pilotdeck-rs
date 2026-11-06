//! Filesystem upload provider, ported against PilotDeck cc7c4e6 UploadStore.ts.
//! This module is not wired into Gateway dispatch. Records retain JS property
//! order and unknown fields so TS and Rust can continue each other's uploads.
//!
//! Concurrency scope matches TS: mutations of one uploadId are serialized only
//! within one store instance (TS `updates: Map<string, Promise>`). Two processes
//! (or two instances) mutating the same upload concurrently can lose an update,
//! exactly as in TS; the shared contract is the disk format, not a cross-process
//! lock. Live TS cross-tests are `#[ignore]` (need PDRS_BASELINE); the
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
    path::{Path, PathBuf},
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
        record = self.mutate(&record, |r| set(r, "status", text("uploading")))?;
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

    pub fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        require_active(&record)?;
        if array(&record, "manifest")
            .iter()
            .any(|m| !array(&record, "receivedClientFileIds").contains(&value(m, "clientFileId")))
        {
            let message = JsString::from("One or more declared files were not uploaded.");
            self.fail(upload_id, "UPLOAD_MANIFEST_MISMATCH", &message)?;
            return Err(UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", message));
        }
        self.mutate(&record, |r| {
            set(r, "status", text("completed"));
            set(r, "uploadedBytes", value(r, "totalBytes"));
        })
    }

    pub fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        if status(&record) == "completed" {
            return Err(UploadError::dialog(
                "UPLOAD_ALREADY_COMPLETED",
                "Completed uploads cannot be cancelled.",
            ));
        }
        if !active(&record) {
            return Ok(record);
        }
        let updated = self.mutate(&record, |r| set(r, "status", text("cancelled")))?;
        remove_tree(&task_dir(&updated).join("files"))?;
        Ok(updated)
    }

    pub fn fail(&self, upload_id: &str, code: &str, message: &JsString) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        self.mutate(&record, |r| {
            set(r, "status", text("failed"));
            set(r, "errorCode", text(code));
            set(r, "errorMessage", JsValue::String(message.clone()));
        })
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
        let root = match plain_root(project, "chat-uploads") {
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
        // Resolve the root once and keep this value for the whole round: every
        // containment check below compares a *freshly* resolved entry against
        // it. Re-canonicalizing the root per entry would resolve through a
        // symlink swapped in mid-sweep and make both sides of the check agree,
        // turning the guard into a no-op for the rest of the round.
        let Ok(root_real) = fs::canonicalize(&root) else {
            return;
        };
        // Anchor the snapshot inside the project: `.tmp` or `chat-uploads`
        // could already be a symlink that `plain_root` saw as a real directory
        // only because it was swapped between the two stats above.
        if !root_real.starts_with(project) {
            return;
        }
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
        match remove_tree(dir) {
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
        let dir = Path::new(&string(&record, "projectKey").unwrap_or_default())
            .join(".tmp/chat-upload-leases")
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
                if !lease_link_matches(&destination, identities.get(index).copied().flatten()) {
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
        let mut identities = Vec::with_capacity(attachments.len());
        for attachment in &attachments {
            let path = PathBuf::from(string(attachment, "path").unwrap_or_default());
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(_) => return Err(attachment_integrity_failed(attachment)),
            };
            let info = file
                .metadata()
                .map_err(|e| UploadError::io(e, "stat", None))?;
            if info.len() as f64 != number(attachment, "bytes")
                || hash_file(&mut file)? != string(attachment, "sha256").unwrap_or_default()
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
            )
        })
    }

    /// Serialized per uploadId within this instance only (see module docs).
    fn mutate(
        &self,
        record: &UploadRecord,
        change: impl FnOnce(&mut UploadRecord),
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
            let mut current = self
                .read_at(&project, &id)?
                .unwrap_or_else(|| record.clone());
            change(&mut current);
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
        let bytes = match fs::File::open(&canonical_path) {
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
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let opened = match file.metadata() {
                        Ok(metadata) => metadata,
                        Err(e) => return Err(UploadError::io(e, "stat", None)),
                    };
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
                let mut bytes = Vec::new();
                match file.read_to_end(&mut bytes) {
                    Ok(_) => bytes,
                    Err(e) => return Err(UploadError::io(e, "read", None)),
                }
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
        })
    }
    fn cleanup_expired_leases(&self, project: &Path) -> Result<()> {
        // Same plain-root gate as the record half: `names()` and
        // `remove_tree()` follow intermediate symlinks, and "unreadable lease
        // means expired" makes deletion the default action, so a symlinked
        // `.tmp` or `.tmp/chat-upload-leases` would delete lease trees outside
        // the project. Missing is the ordinary empty state and stays silent.
        let root = match plain_root(project, "chat-upload-leases") {
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
        let Ok(root_real) = fs::canonicalize(&root) else {
            return Ok(());
        };
        if !root_real.starts_with(project) {
            return Ok(());
        }
        for id in names(&root_real).into_iter().filter(|id| safe_id(id)) {
            let leases = root_real.join(id);
            for lease in names(&leases).into_iter().filter(|id| safe_id(id)) {
                let dir = leases.join(lease);
                let metadata = fs::read(dir.join("lease.json"))
                    .ok()
                    .and_then(|b| parse_json(&String::from_utf8_lossy(&b)).ok());
                if let Some(m) = metadata
                    && future(&m, "expiresAt", (self.options.now)())?
                {
                    continue;
                }
                if !inside_root(&root_real, &dir) {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root",
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
            .get_or_insert_with(|| remove_tree(&self.dir))
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
fn require_active(record: &UploadRecord) -> Result<()> {
    if active(record) {
        Ok(())
    } else {
        Err(UploadError::dialog(
            "UPLOAD_INVALID_STATE",
            format!("Upload is {}.", status(record)),
        ))
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

/// The `(dev, inode)` pair recorded on the verified handle. Non-Unix has no
/// cross-platform file identity, so the lease-link comparison is skipped
/// there (`lease_link_matches` returns true); Unix is the shipped target.
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
/// The check must look at what actually got linked, so it uses
/// `symlink_metadata`: `std::fs::hard_link` does not follow a symlink source on
/// any shipping Unix — it goes through `linkat(..., 0)` on macOS as well as on
/// Linux — so a source swapped for a link puts the *link* into the lease.
/// `metadata` would follow it and compare the target's identity — exactly the
/// alibi the swap needs (review deepseek boundary#1). Measured on macOS
/// 2026-11-06: after `fs::hard_link(link, dest)`, `fs::symlink_metadata(dest)`
/// reports a symlink; Python's `os.link` is the one that follows.
#[cfg(unix)]
fn lease_link_matches(destination: &Path, expected: Option<AttachmentIdentity>) -> bool {
    match (fs::symlink_metadata(destination), expected) {
        (Ok(linked), Some(want)) => {
            linked.file_type().is_file() && attachment_identity(&linked) == Some(want)
        }
        _ => false,
    }
}

#[cfg(not(unix))]
fn lease_link_matches(_destination: &Path, _expected: Option<AttachmentIdentity>) -> bool {
    true
}

fn hash_file(file: &mut File) -> Result<String> {
    let mut digest = Sha256::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let n = file
            .read(&mut chunk)
            .map_err(|e| UploadError::io(e, "read", None))?;
        if n == 0 {
            break;
        }
        digest.update(&chunk[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
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
/// to catch. A missing or unresolvable entry counts as "not inside", so every
/// caller treats it as "do not delete".
fn inside_root(root_real: &Path, dir: &Path) -> bool {
    let Ok(dir_real) = fs::canonicalize(dir) else {
        return false;
    };
    dir_real != root_real && dir_real.starts_with(root_real)
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
    /// Both `<project>/.tmp` and `<project>/.tmp/<leaf>` are plain directories.
    Plain(PathBuf),
    /// There is no such tree yet (never uploaded / no live lease): nothing to
    /// clean, and nothing worth logging every interval.
    Missing,
    /// Something is there, but at least one component is a symlink or not a
    /// directory: `read_dir`/`remove_dir_all` would follow it out of the
    /// project, so the sweep must not touch it.
    Blocked,
}

/// Gate a cleanup half on `<project>/.tmp/<leaf>` being a plain directory.
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
    PlainRoot::Plain(root)
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
mod swap_test_hook {
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
