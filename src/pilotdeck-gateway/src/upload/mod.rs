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
#[cfg(test)]
mod tests;

pub use error::UploadError;
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use pilotdeck_transport::system_clock::{format_epoch_iso, parse_node22_date};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak, mpsc},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, UploadError>;
/// Original UploadRecord / UploadedAttachment objects, including extra fields.
pub type UploadRecord = JsValue;
pub type UploadedAttachment = JsValue;
type ResolveProject = dyn Fn(&str) -> Result<PathBuf> + Send + Sync;
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
        resolve_project: impl Fn(&str) -> Result<PathBuf> + Send + Sync + 'static,
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
        }
    }

    /// UploadStore.ts:61–110: validate before idempotency; serialize by canonical project.
    pub fn create(
        &self,
        project_key: &str,
        files: &JsValue,
        idempotency_key: Option<&str>,
    ) -> Result<UploadRecord> {
        let root = (self.options.resolve_project)(project_key)?;
        let root = fs::canonicalize(root).map_err(|_| {
            UploadError::dialog(
                "PROJECT_NOT_FOUND",
                format!("Unknown projectKey: {project_key}"),
            )
        })?;
        let gate = {
            let mut gates = self.creates.lock().unwrap();
            gates.retain(|_, g| g.strong_count() > 0);
            let gate = gates.get(&root).and_then(Weak::upgrade).unwrap_or_default();
            gates.insert(root.clone(), Arc::downgrade(&gate));
            gate
        };
        let _guard = gate.lock().unwrap();
        let manifest = self.validate_manifest(files)?;
        let key_hash = idempotency_key
            .filter(|k| !k.is_empty())
            .map(|k| hash(k.as_bytes()));
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
        mut stream: impl Read,
    ) -> Result<UploadedAttachment> {
        let mut record = self.get(upload_id)?;
        require_active(&record)?;
        let expected = array(&record, "manifest")
            .iter()
            .find(|m| string(m, "clientFileId").as_deref() == Some(client_file_id))
            .cloned();
        if expected.is_none()
            || array(&record, "receivedClientFileIds").contains(&text(client_file_id))
        {
            return Err(UploadError::dialog(
                "UPLOAD_MANIFEST_MISMATCH",
                format!("Unexpected or duplicate file part: {client_file_id}"),
            ));
        }
        let expected = expected.unwrap();
        record = self.mutate(&record, |r| set(r, "status", text("uploading")))?;
        let destination = task_dir(&record).join("files").join(client_file_id);
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
                        format!("Uploaded bytes exceed the declared size for {client_file_id}."),
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
                    format!("Size or SHA-256 mismatch for {client_file_id}."),
                ));
            }
            let mut attachment = object([
                ("attachmentId", text(client_file_id)),
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
                received.push(text(client_file_id));
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
            let message = "One or more declared files were not uploaded.";
            self.fail(upload_id, "UPLOAD_MANIFEST_MISMATCH", message)?;
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

    pub fn fail(&self, upload_id: &str, code: &str, message: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        self.mutate(&record, |r| {
            set(r, "status", text("failed"));
            set(r, "errorCode", text(code));
            set(r, "errorMessage", text(message));
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

    pub fn cleanup_expired(&self) -> Result<usize> {
        let mut count = 0;
        for project in (self.options.list_projects)()? {
            let Ok(project) = fs::canonicalize(project) else {
                continue;
            };
            for record in self.list_in_project(&project)? {
                if future(&record, "expiresAt", (self.options.now)())? {
                    continue;
                }
                remove_tree(&task_dir(&record))?;
                count += 1;
            }
            self.cleanup_expired_leases(&project)?;
        }
        Ok(count)
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
        let (record, mut attachments) = self.verified(upload_id, project_key, attachment_ids)?;
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
            let mut file = open_new(&path)?;
            self.fault("lease-write", &path)?;
            file.write_all(json_line(&expiry)?.as_bytes())
                .map_err(|e| UploadError::io(e, "write", None))?;
            file.sync_all()
                .map_err(|e| UploadError::io(e, "fsync", None))?;
            for attachment in &mut attachments {
                let source = PathBuf::from(string(attachment, "path").unwrap_or_default());
                let destination = dir.join(string(attachment, "attachmentId").unwrap_or_default());
                self.fault("lease-link", &source)?;
                fs::hard_link(&source, &destination)
                    .map_err(|e| UploadError::io(e, "link", Some(&source)))?;
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
    ) -> Result<(UploadRecord, Vec<UploadedAttachment>)> {
        let record = self.get(upload_id)?;
        let project = (self.options.resolve_project)(project_key)?;
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
        for attachment in &attachments {
            let path = PathBuf::from(string(attachment, "path").unwrap_or_default());
            let info = fs::metadata(&path).ok();
            if info.is_none_or(|i| i.len() as f64 != number(attachment, "bytes"))
                || sha256_file(&path)? != string(attachment, "sha256").unwrap_or_default()
            {
                return Err(UploadError::dialog(
                    "ATTACHMENT_TAMPERED",
                    format!(
                        "Attachment integrity check failed: {}",
                        string(attachment, "attachmentId").unwrap_or_default()
                    ),
                ));
            }
        }
        Ok((record, attachments))
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
        if !canonical_path.starts_with(canonical_project.join(".tmp/chat-uploads")) {
            return Err(UploadError::dialog(
                "PROJECT_PATH_FORBIDDEN",
                "Upload metadata symlink leaves the upload directory.",
            ));
        }
        let bytes = match fs::read(&canonical_path) {
            Ok(b) => b,
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
        let root = project.join(".tmp/chat-upload-leases");
        for id in names(&root).into_iter().filter(|id| safe_id(id)) {
            let leases = root.join(id);
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
                remove_tree(&dir)?;
            }
            if names(&leases).is_empty() {
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
fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| UploadError::io(e, "open", Some(path)))?;
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
    fn space(c: u16) -> bool {
        matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
    }
    let units = s.units();
    let start = units.iter().position(|&c| !space(c)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|&c| !space(c))
        .map_or(start, |i| i + 1);
    pilotdeck_protocol::JsString::from_utf16(units[start..end].to_vec())
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
