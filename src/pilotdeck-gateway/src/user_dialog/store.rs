use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pilotdeck_protocol::parse_json;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct StoreOptions {
    pub directory: PathBuf,
    pub runtime: DialogRuntime,
    pub lock_timeout: Duration,
    pub lock_retry: Duration,
    pub stale_lock: Duration,
}
impl StoreOptions {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            runtime: DialogRuntime::default(),
            lock_timeout: Duration::from_secs(5),
            lock_retry: Duration::from_millis(20),
            stale_lock: Duration::from_secs(30),
        }
    }
}

/// Synchronous filesystem boundary. Call from a blocking worker when integrating
/// an async host. Each mutation rereads under the TS-compatible per-session lock.
pub struct FileGatewayUserDialogStore {
    options: StoreOptions,
    #[cfg(test)]
    pub(super) fault: Option<Fault>,
}
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum Fault {
    Write,
    Rename,
}

impl FileGatewayUserDialogStore {
    pub fn new(mut options: StoreOptions) -> Result<Self> {
        if options
            .directory
            .as_os_str()
            .to_string_lossy()
            .trim()
            .is_empty()
        {
            return Err(DialogError::Invalid(
                "FileGatewayUserDialogStore directory is required.".into(),
            ));
        }
        if !options.directory.is_absolute() {
            options.directory = std::env::current_dir()?.join(options.directory);
        }
        for (name, duration) in [
            ("lockTimeoutMs", options.lock_timeout),
            ("lockRetryMs", options.lock_retry),
            ("staleLockMs", options.stale_lock),
        ] {
            positive_integer(duration.as_secs_f64() * 1000.0, name)?;
        }
        Ok(Self {
            options,
            #[cfg(test)]
            fault: None,
        })
    }

    pub fn record_path(&self, key: &StoreKey) -> Result<PathBuf> {
        if key.project_root.is_empty() || key.pilot_home.is_empty() || key.session_id.is_empty() {
            return Err(DialogError::Invalid(
                "GatewayUserDialogStoreKey requires projectRoot, pilotHome, and sessionId.".into(),
            ));
        }
        let encoded = URL_SAFE_NO_PAD.encode(json(&JsValue::Array(vec![
            JsValue::String(key.project_root.clone()),
            JsValue::String(key.pilot_home.clone()),
            JsValue::String(key.session_id.clone()),
        ]))?);
        let mut path = self.options.directory.clone();
        for segment in encoded.as_bytes().chunks(120) {
            // base64url is ASCII, so a byte boundary is a character boundary.
            path.push(std::str::from_utf8(segment).expect("base64url ASCII"));
        }
        Ok(path.join("dialogs.json"))
    }

    pub fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> Result<()> {
        self.with_record(key, |path, record| {
            let id = string(&dialog.request, "requestId")?;
            let mut next = dialog.to_value();
            if let Some(index) = find(record, id) {
                for field in ["lease", "owner", "answer"] {
                    if let Some(value) = record[index].get(field) {
                        set(&mut next, field, value.clone());
                    }
                }
                record[index] = next;
            } else {
                record.push(next);
            }
            self.write(path, record)
        })
    }

    pub fn list(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        self.read(&self.record_path(key)?)?
            .iter()
            .map(public_dialog)
            .collect()
    }

    pub fn list_live(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        let mut record = self.read(&self.record_path(key)?)?;
        let mut dialogs = Vec::new();
        for dialog in &mut record {
            if self.active(dialog, "owner")?.is_some() {
                dialogs.push(public_dialog(dialog)?);
            }
        }
        // TS listLive expiry pruning is only in the read copy, never persisted.
        Ok(dialogs)
    }

    pub fn remove(&self, key: &StoreKey, request_id: &JsString) -> Result<()> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(());
            };
            record.remove(index);
            if record.is_empty() {
                remove_file(path)
            } else {
                self.write(path, record)
            }
        })
    }

    pub fn clear(&self, key: &StoreKey) -> Result<()> {
        let path = self.record_path(key)?;
        self.with_lock(&path, || remove_file(&path))
    }

    pub fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        renew_lease_id: Option<&JsString>,
    ) -> Result<LeaseClaim> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(LeaseClaim::NotPending);
            };
            let existing = self.active(&mut record[index], "lease")?;
            if let Some(lease) = &existing
                && Some(string(lease, "leaseId")?) != renew_lease_id
            {
                return Ok(LeaseClaim::Busy {
                    expires_at: string(lease, "expiresAt")?.clone(),
                });
            }
            // Match TS validation order: missing/busy returns before validating TTL.
            positive_integer(ttl_ms, "ttlMs")?;
            let lease = Lease {
                lease_id: match existing {
                    Some(value) => string(&value, "leaseId")?.clone(),
                    None => (self.options.runtime.uuid)().into(),
                },
                expires_at: expiry((self.options.runtime.now)(), ttl_ms)?,
            };
            set(
                &mut record[index],
                "lease",
                object([
                    ("leaseId", JsValue::String(lease.lease_id.clone())),
                    ("expiresAt", JsValue::String(lease.expires_at.clone())),
                ]),
            );
            self.write(path, record)?;
            Ok(LeaseClaim::Claimed(lease))
        })
    }

    pub fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> Result<bool> {
        self.release_field(key, request_id, lease_id, "lease", "leaseId")
    }

    pub fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> Result<bool> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(false);
            };
            if let Some(active) = self.active(&mut record[index], "lease")?
                && Some(string(&active, "leaseId")?) != lease_id
            {
                return Ok(false);
            }
            let answer = StoredAnswer {
                request_id: request_id.clone(),
                result: result.clone(),
                submitted_at: format_epoch_iso((self.options.runtime.now)()).into(),
            };
            set(&mut record[index], "answer", answer.to_value());
            self.write(path, record)?;
            Ok(true)
        })
    }

    pub fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> Result<Option<StoredAnswer>> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(None);
            };
            let Some(value) = record[index].get("answer") else {
                return Ok(None);
            };
            let answer = StoredAnswer::from_value(value)?;
            delete(&mut record[index], "answer");
            self.write(path, record)?;
            Ok(Some(answer))
        })
    }

    pub fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<OwnerClaim> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(OwnerClaim::NotPending);
            };
            if let Some(owner) = self.active(&mut record[index], "owner")?
                && string(&owner, "ownerId")? != owner_id
            {
                return Ok(OwnerClaim::Busy {
                    expires_at: string(&owner, "expiresAt")?.clone(),
                });
            }
            positive_integer(ttl_ms, "ttlMs")?;
            let expires_at = expiry((self.options.runtime.now)(), ttl_ms)?;
            set(
                &mut record[index],
                "owner",
                object([
                    ("ownerId", JsValue::String(owner_id.clone())),
                    ("expiresAt", JsValue::String(expires_at.clone())),
                ]),
            );
            self.write(path, record)?;
            Ok(OwnerClaim::Owned {
                owner_id: owner_id.clone(),
                expires_at,
            })
        })
    }

    pub fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<bool> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(false);
            };
            let owner = self.active(&mut record[index], "owner")?;
            if owner
                .as_ref()
                .map(|owner| string(owner, "ownerId"))
                .transpose()?
                != Some(owner_id)
            {
                if owner.is_none() {
                    self.write(path, record)?;
                }
                return Ok(false);
            }
            positive_integer(ttl_ms, "ttlMs")?;
            let mut owner = owner.expect("matching owner");
            set(
                &mut owner,
                "expiresAt",
                JsValue::String(expiry((self.options.runtime.now)(), ttl_ms)?),
            );
            set(&mut record[index], "owner", owner);
            self.write(path, record)?;
            Ok(true)
        })
    }

    pub fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> Result<bool> {
        self.release_field(key, request_id, owner_id, "owner", "ownerId")
    }

    fn release_field(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        id: &JsString,
        field: &str,
        id_field: &str,
    ) -> Result<bool> {
        self.with_record(key, |path, record| {
            let Some(index) = find(record, request_id) else {
                return Ok(false);
            };
            let active = self.active(&mut record[index], field)?;
            if active
                .as_ref()
                .map(|value| string(value, id_field))
                .transpose()?
                != Some(id)
            {
                if active.is_none() {
                    self.write(path, record)?;
                }
                return Ok(false);
            }
            delete(&mut record[index], field);
            self.write(path, record)?;
            Ok(true)
        })
    }

    fn active(&self, dialog: &mut JsValue, field: &str) -> Result<Option<JsValue>> {
        let Some(value) = dialog.get(field) else {
            return Ok(None);
        };
        if parse_expiry(string(value, "expiresAt")?)? > (self.options.runtime.now)() as f64 {
            return Ok(Some(value.clone()));
        }
        delete(dialog, field);
        Ok(None)
    }

    fn with_record<T>(
        &self,
        key: &StoreKey,
        operation: impl FnOnce(&Path, &mut Vec<JsValue>) -> Result<T>,
    ) -> Result<T> {
        let path = self.record_path(key)?;
        self.with_lock(&path, || operation(&path, &mut self.read(&path)?))
    }

    fn with_lock<T>(&self, path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let lock = path.with_file_name("dialogs.json.lock");
        self.acquire_lock(&lock)?;
        let result = operation();
        // Cleanup errors take precedence, matching TS finally.
        remove_file(&lock)?;
        result
    }

    fn acquire_lock(&self, path: &Path) -> Result<()> {
        mkdir(path.parent().expect("session directory"))?;
        let started = Instant::now();
        loop {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(path) {
                Ok(mut handle) => {
                    writeln!(handle, "{}", (self.options.runtime.uuid)())?;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            match fs::metadata(path) {
                Ok(metadata) => {
                    if metadata
                        .modified()?
                        .elapsed()
                        .is_ok_and(|age| age >= self.options.stale_lock)
                    {
                        remove_file(path)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if started.elapsed() >= self.options.lock_timeout {
                return Err(DialogError::Invalid(format!(
                    "Timed out waiting for Gateway user-dialog store lock {}.",
                    path.display()
                )));
            }
            thread::sleep(self.options.lock_retry);
        }
    }

    fn read(&self, path: &Path) -> Result<Vec<JsValue>> {
        let result = (|| {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error.into()),
            };
            let value = parse_json(&String::from_utf8_lossy(&bytes))
                .map_err(|error| DialogError::Invalid(error.to_string()))?;
            if value.get("version").and_then(JsValue::as_number) != Some(1.0) {
                return Err(invalid_record());
            }
            let Some(JsValue::Array(dialogs)) = value.get("dialogs") else {
                return Err(invalid_record());
            };
            let mut ids = HashSet::new();
            dialogs
                .iter()
                .map(|dialog| {
                    let public = public_dialog(dialog)?;
                    let id = string(&public.request, "requestId")?;
                    if !ids.insert(id.clone()) {
                        return Err(invalid_record());
                    }
                    let mut clean = public.to_value();
                    for (field, id_field) in [("lease", "leaseId"), ("owner", "ownerId")] {
                        if let Some(value) = dialog.get(field) {
                            let id = string(value, id_field)?;
                            let expires_at = string(value, "expiresAt")?;
                            parse_expiry(expires_at)?;
                            set(
                                &mut clean,
                                field,
                                object([
                                    (id_field, JsValue::String(id.clone())),
                                    ("expiresAt", JsValue::String(expires_at.clone())),
                                ]),
                            );
                        }
                    }
                    if let Some(value) = dialog.get("answer") {
                        let answer = StoredAnswer::from_value(value)?;
                        if &answer.request_id != id {
                            return Err(invalid_record());
                        }
                        set(&mut clean, "answer", answer.to_value());
                    }
                    Ok(clean)
                })
                .collect()
        })();
        result.map_err(|error: DialogError| {
            DialogError::Invalid(format!(
                "Unable to read Gateway user-dialog store {}: {error}",
                path.display()
            ))
        })
    }

    fn write(&self, path: &Path, dialogs: &[JsValue]) -> Result<()> {
        let directory = path.parent().expect("session directory");
        mkdir(directory)?;
        let temporary = directory.join(format!(
            ".dialogs.json.{}.tmp",
            (self.options.runtime.uuid)()
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            #[cfg(test)]
            if matches!(self.fault, Some(Fault::Write)) {
                file.write_all(b"{\"version\":")?;
                return Err(io::Error::other("injected write failure").into());
            }
            file.write_all(
                json(&object([
                    ("version", JsValue::Number(1.0)),
                    ("dialogs", JsValue::Array(dialogs.to_vec())),
                ]))?
                .as_bytes(),
            )?;
            drop(file);
            #[cfg(test)]
            if matches!(self.fault, Some(Fault::Rename)) {
                return Err(io::Error::other("injected rename failure").into());
            }
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        remove_file(&temporary)?;
        result
    }
}

fn find(record: &[JsValue], id: &JsString) -> Option<usize> {
    record.iter().position(|dialog| {
        dialog
            .get("request")
            .and_then(|request| request.get("requestId"))
            .and_then(JsValue::as_string)
            == Some(id)
    })
}
fn public_dialog(value: &JsValue) -> Result<StoredDialog> {
    let request = value.get("request").ok_or_else(invalid_record)?;
    if !string(request, "type")?.equals_str("user_dialog_request") {
        return Err(invalid_record());
    }
    string(request, "requestId")?;
    string(request, "dialogKind")?;
    Ok(StoredDialog {
        request: request.clone(),
        created_at: string(value, "createdAt")?.clone(),
    })
}
fn mkdir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    Ok(())
}
fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
