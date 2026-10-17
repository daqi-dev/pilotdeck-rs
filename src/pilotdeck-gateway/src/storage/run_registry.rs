//! Durable run registry, ported from TS `src/gateway/run/RunRegistry.ts` (fixed
//! baseline). Storage contract only: no host, session runtime or gateway method
//! is wired here, and the default proxy route is unchanged.
//!
//! The wire format is the TS snapshot (`schemaVersion: 1`) byte-for-byte. The
//! file lock is the TS exclusive-create `<path>.lock` protocol, so TS and Rust
//! processes sharing one registry during a handoff exclude each other.
use super::{
    ledger::{field, truthy},
    transcript::{Temporary, private_dirs, private_open, timestamp, uuid},
};
use crate::metadata::{js_trim, object};
use pilotdeck_protocol::{CodecError, JsObject, JsString, JsValue, parse_json, stringify_json};
use regex::Regex;
use std::{
    fmt,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, PoisonError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// RunRegistry.ts:43-44.
const FILE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_LOCK_STALE: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum RunRegistryError {
    /// TS `RunRegistryError` codes (RunRegistry.ts:35); see [`Self::code`].
    Conflict(String),
    NotFound(String),
    Terminal(String),
    /// TS plain `Error`/`SyntaxError`/`TypeError`: snapshot syntax or shape.
    /// JSON parser wording is Rust-specific; shape messages match TS.
    Invalid(String),
    /// RunRegistry.ts:142.
    LockTimeout(String),
    Io(io::Error),
}
impl RunRegistryError {
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Conflict(_) => Some("conflict"),
            Self::NotFound(_) => Some("not_found"),
            Self::Terminal(_) => Some("terminal"),
            _ => None,
        }
    }
}
impl fmt::Display for RunRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(m)
            | Self::NotFound(m)
            | Self::Terminal(m)
            | Self::Invalid(m)
            | Self::LockTimeout(m) => f.write_str(m),
            Self::Io(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for RunRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for RunRegistryError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<CodecError> for RunRegistryError {
    fn from(e: CodecError) -> Self {
        Self::Invalid(e.to_string())
    }
}
type Result<T> = std::result::Result<T, RunRegistryError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Accepted,
    Running,
    Completed,
    Failed,
    Aborted,
    Interrupted,
}
impl RunState {
    const ALL: [Self; 6] = [
        Self::Accepted,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Aborted,
        Self::Interrupted,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Interrupted => "interrupted",
        }
    }
    fn of(value: &JsValue) -> Option<Self> {
        let state = value.as_string()?;
        Self::ALL.into_iter().find(|s| state.equals_str(s.as_str()))
    }
}

/// `{ sessionKey, runId, projectKey? }`. An absent and an empty project key
/// address the same run, as TS `projectKey ?? ""` does (RunRegistry.ts:70).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRef {
    pub project_key: Option<JsString>,
    pub session_key: JsString,
    pub run_id: JsString,
}
impl RunRef {
    fn key(&self) -> JsString {
        let project = self
            .project_key
            .clone()
            .map(JsValue::String)
            .unwrap_or(JsValue::Undefined);
        run_key(
            &project,
            &JsValue::String(self.session_key.clone()),
            &JsValue::String(self.run_id.clone()),
        )
    }
    fn run_id(&self) -> String {
        String::from_utf16_lossy(self.run_id.units())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAcceptInput {
    pub run: RunRef,
    pub request_material: JsString,
}

/// A stored record exactly as persisted, including fields this port does not
/// interpret (TS keeps them on reload/mutation, too).
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord(JsValue);
impl RunRecord {
    pub fn as_json(&self) -> &JsValue {
        &self.0
    }
    pub fn state(&self) -> Option<RunState> {
        RunState::of(field(&self.0, "state"))
    }
    pub fn revision(&self) -> Option<f64> {
        field(&self.0, "revision").as_number()
    }
    pub fn last_seq(&self) -> Option<f64> {
        field(&self.0, "lastSeq").as_number()
    }
    pub fn owner_pid(&self) -> Option<f64> {
        field(&self.0, "ownerPid").as_number()
    }
    pub fn result(&self) -> Option<&JsValue> {
        self.0.get("result")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunAccepted {
    pub record: RunRecord,
    /// True means the run was already durable; callers replay, never execute.
    pub duplicate: bool,
}

#[derive(Default)]
struct Snapshot {
    /// Keyed like the TS Map: first insertion keeps its position on replace.
    records: JsObject,
    events: JsObject,
}

/// TS `FileRunRegistry`. Every mutation takes the in-process chain, then the
/// cross-process file lock, reloads the snapshot and atomically replaces it;
/// reads reload without the file lock. Nothing is cached between calls, so a
/// failed write never publishes memory that disk does not hold.
///
/// Opening fences dead-owner accepted/running runs as interrupted, as TS
/// readiness does (RunRegistry.ts:63-66). Like TS, rename is the visibility
/// point: the temporary file is synced, the parent directory is not.
pub struct FileRunRegistry {
    path: PathBuf,
    lock_path: PathBuf,
    chain: Mutex<()>,
    lock_timeout: Duration,
    #[cfg(test)]
    pub(super) fail_persist: std::sync::atomic::AtomicBool,
}
impl FileRunRegistry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path.as_ref(), FILE_LOCK_TIMEOUT)
    }
    #[cfg(test)]
    pub(super) fn open_for_test(path: impl AsRef<Path>, lock_timeout: Duration) -> Result<Self> {
        Self::open_with(path.as_ref(), lock_timeout)
    }
    fn open_with(path: &Path, lock_timeout: Duration) -> Result<Self> {
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let registry = Self {
            path: path.to_path_buf(),
            lock_path: lock_path.into(),
            chain: Mutex::new(()),
            lock_timeout,
            #[cfg(test)]
            fail_persist: Default::default(),
        };
        registry.fence_orphans()?;
        Ok(registry)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// RunRegistry.ts:211-234.
    pub fn accept(&self, input: &RunAcceptInput) -> Result<RunAccepted> {
        self.mutate(|snapshot| {
            let key = input.run.key();
            if let Some(existing) = snapshot.records.get_key(&key).filter(|r| truthy(r)) {
                if field(existing, "requestMaterial").as_string() != Some(&input.request_material) {
                    return Err(RunRegistryError::Conflict(format!(
                        "Run {} already exists with different request material.",
                        input.run.run_id()
                    )));
                }
                // Duplicate: no write, so the caller replays rather than executes.
                return Ok(((RunRecord(existing.clone()), true), false));
            }
            let now = now()?;
            // InProcessGateway.ts:906-911 key order, then RunRegistry.ts:221-229.
            let mut record = JsObject::new();
            record.insert(
                "sessionKey".into(),
                JsValue::String(input.run.session_key.clone()),
            );
            if let Some(project) = &input.run.project_key {
                record.insert("projectKey".into(), JsValue::String(project.clone()));
            }
            record.insert("runId".into(), JsValue::String(input.run.run_id.clone()));
            record.insert(
                "requestMaterial".into(),
                JsValue::String(input.request_material.clone()),
            );
            record.insert(
                "ownerPid".into(),
                JsValue::Number(f64::from(std::process::id())),
            );
            record.insert("state".into(), RunState::Accepted.as_str().into());
            record.insert("revision".into(), JsValue::Number(1.0));
            record.insert("lastSeq".into(), JsValue::Number(0.0));
            record.insert("acceptedAt".into(), now.clone());
            record.insert("updatedAt".into(), now);
            let record = JsValue::Object(record);
            snapshot.records.insert(key, record.clone());
            Ok(((RunRecord(record), false), true))
        })
        .map(|(record, duplicate)| RunAccepted { record, duplicate })
    }

    /// RunRegistry.ts:236-264. `failed` is not terminal: a later event may
    /// still complete the run (preflight error followed by turn_completed).
    pub fn append(&self, run: &RunRef, event: &JsValue) -> Result<()> {
        self.mutate(|snapshot| {
            let key = run.key();
            let record = snapshot
                .records
                .get_key(&key)
                .filter(|r| truthy(r))
                .ok_or_else(|| {
                    RunRegistryError::NotFound(format!("Run {} is not registered.", run.run_id()))
                })?;
            if let Some(state) = field(record, "state").as_string()
                && ["completed", "aborted", "interrupted"]
                    .iter()
                    .any(|s| state.equals_str(s))
            {
                return Err(RunRegistryError::Terminal(format!(
                    "Run {} is already terminal ({}).",
                    run.run_id(),
                    String::from_utf16_lossy(state.units())
                )));
            }
            // TS strict-mode TypeErrors: property writes on a primitive record,
            // `.push` on a non-array list, and `.type` of a null/undefined event.
            let JsValue::Object(record) = record else {
                return Err(invalid("Run registry record is not an object."));
            };
            let mut record = record.clone();
            let mut events = event_list(snapshot.events.get_key(&key))?.to_vec();
            if matches!(event, JsValue::Null | JsValue::Undefined) {
                return Err(invalid("Run registry event must be an object."));
            }
            let seq = js_plus_one(record_field(&record, "lastSeq"));
            events.push(object([("seq", seq.clone()), ("event", event.clone())]));
            snapshot.events.insert(key.clone(), JsValue::Array(events));
            record.insert("lastSeq".into(), seq);
            let revision = js_plus_one(record_field(&record, "revision"));
            record.insert("revision".into(), revision);
            record.insert("updatedAt".into(), now()?);
            let kind = field(event, "type");
            if is(kind, "turn_completed") {
                let aborted = is(field(event, "finishReason"), "aborted_streaming");
                let state = if aborted {
                    RunState::Aborted
                } else {
                    RunState::Completed
                };
                record.insert("state".into(), state.as_str().into());
                record.insert("result".into(), event.clone());
            } else if is(kind, "error") {
                let unknown = is(field(event, "code"), "result_unknown");
                let state = if unknown {
                    RunState::Interrupted
                } else {
                    RunState::Failed
                };
                record.insert("state".into(), state.as_str().into());
                record.insert("result".into(), event.clone());
            } else if is(record_field(&record, "state"), "accepted") {
                record.insert("state".into(), RunState::Running.as_str().into());
            }
            snapshot.records.insert(key, JsValue::Object(record));
            Ok(((), true))
        })
    }

    /// RunRegistry.ts:266-272.
    pub fn get(&self, run: &RunRef) -> Result<Option<RunRecord>> {
        let _chain = self.chain.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = read_snapshot(&self.path)?;
        Ok(snapshot
            .records
            .get_key(&run.key())
            .filter(|r| truthy(r))
            .cloned()
            .map(RunRecord))
    }

    /// RunRegistry.ts:274-283: stored `{ seq, event }` entries with
    /// `seq > after_seq` (default 0), then `slice(0, limit)` (default 500).
    /// Cursor/limit validation belongs to the gateway method, not storage.
    pub fn events(
        &self,
        run: &RunRef,
        after_seq: Option<f64>,
        limit: Option<f64>,
    ) -> Result<Vec<JsValue>> {
        let _chain = self.chain.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = read_snapshot(&self.path)?;
        let key = run.key();
        if snapshot.records.get_key(&key).is_none() {
            return Ok(vec![]);
        }
        let after_seq = after_seq.unwrap_or(0.0);
        let mut selected = Vec::new();
        for entry in event_list(snapshot.events.get_key(&key))? {
            if matches!(entry, JsValue::Null) {
                return Err(invalid("Run registry event entry is null."));
            }
            if js_to_number(field(entry, "seq")) > after_seq {
                selected.push(entry.clone());
            }
        }
        selected.truncate(slice_end(selected.len(), limit.unwrap_or(500.0)));
        Ok(selected)
    }

    /// RunRegistry.ts:285-309. Opening already does this once.
    pub fn mark_orphans_interrupted(&self) -> Result<()> {
        self.fence_orphans()
    }

    /// RunRegistry.ts:158-182 and 285-309: only an existing snapshot is fenced
    /// (a fresh home is not created), and ENOENT/EINVAL are observational.
    fn fence_orphans(&self) -> Result<()> {
        match fs::metadata(&self.path) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let result = self.mutate(|snapshot| {
            let mut changed = false;
            for (key, record) in snapshot.records.entries().to_vec() {
                let JsValue::Object(record) = &record else {
                    continue;
                };
                let mut record = record.clone();
                if !matches!(
                    RunState::of(record_field(&record, "state")),
                    Some(RunState::Accepted | RunState::Running)
                ) || owner_alive(record_field(&record, "ownerPid"))
                {
                    continue;
                }
                record.insert("state".into(), RunState::Interrupted.as_str().into());
                let revision = js_plus_one(record_field(&record, "revision"));
                record.insert("revision".into(), revision);
                record.insert("updatedAt".into(), now()?);
                snapshot.records.insert(key, JsValue::Object(record));
                changed = true;
            }
            Ok(((), changed))
        });
        match result {
            Err(RunRegistryError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(())
            }
            other => other,
        }
    }

    /// RunRegistry.ts:196-209; the closure reports whether to persist.
    fn mutate<T>(&self, operation: impl FnOnce(&mut Snapshot) -> Result<(T, bool)>) -> Result<T> {
        let _chain = self.chain.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = self.acquire_file_lock()?;
        let mut snapshot = read_snapshot(&self.path)?;
        let (value, changed) = operation(&mut snapshot)?;
        if changed {
            self.persist(&snapshot)?;
        }
        Ok(value)
    }

    /// RunRegistry.ts:110-146.
    pub(super) fn acquire_file_lock(&self) -> Result<FileLock> {
        create_parent(&self.path)?;
        let deadline = Instant::now() + self.lock_timeout;
        loop {
            match private_open()
                .write(true)
                .create_new(true)
                .open(&self.lock_path)
            {
                Ok(file) => return FileLock::write(self.lock_path.clone(), file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            match fs::metadata(&self.lock_path) {
                Ok(meta) => {
                    let stale = meta
                        .modified()
                        .ok()
                        .and_then(|m| SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age > FILE_LOCK_STALE);
                    if stale {
                        let contents = fs::read(&self.lock_path)
                            .map(|b| String::from_utf8_lossy(&b).into_owned())
                            .unwrap_or_default();
                        if !lock_owner_alive(&contents) {
                            let _ = fs::remove_file(&self.lock_path);
                            continue;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if Instant::now() >= deadline {
                return Err(RunRegistryError::LockTimeout(format!(
                    "Timed out acquiring run registry lock: {}",
                    self.lock_path.display()
                )));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// RunRegistry.ts:184-194: compact JSON, sibling temporary, rename.
    fn persist(&self, snapshot: &Snapshot) -> Result<()> {
        let records = snapshot
            .records
            .entries()
            .iter()
            .map(|(_, r)| r.clone())
            .collect();
        let value = object([
            ("schemaVersion", JsValue::Number(1.0)),
            ("records", JsValue::Array(records)),
            ("events", JsValue::Object(snapshot.events.clone())),
        ]);
        let text = stringify_json(&value)?.ok_or_else(|| invalid("Snapshot is not JSON."))?;
        create_parent(&self.path)?;
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| invalid(e.to_string()))?
            .as_millis();
        let mut temporary = self.path.as_os_str().to_os_string();
        temporary.push(format!(
            ".{}.{millis}.{}.tmp",
            std::process::id(),
            uuid()?.replace('-', "")
        ));
        let temporary = Temporary(temporary.into());
        let mut file = private_open()
            .write(true)
            .create_new(true)
            .open(&temporary.0)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        #[cfg(test)]
        if self
            .fail_persist
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(io::Error::other("injected persist failure before rename").into());
        }
        fs::rename(&temporary.0, &self.path)?;
        Ok(())
    }
}

/// RunRegistry.ts:148-156: the lock file is removed on release, error or not.
pub(super) struct FileLock(PathBuf);
impl FileLock {
    fn write(path: PathBuf, mut file: File) -> Result<Self> {
        let lock = Self(path);
        let body = object([
            ("pid", JsValue::Number(f64::from(std::process::id()))),
            ("acquiredAt", now()?),
        ]);
        let body = stringify_json(&body)?.ok_or_else(|| invalid("Lock is not JSON."))?;
        file.write_all(body.as_bytes())?;
        Ok(lock)
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// RunRegistry.ts:93-108.
fn read_snapshot(path: &Path) -> Result<Snapshot> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::default()),
        Err(e) => return Err(e.into()),
    };
    // Node readFile(path, "utf8") replaces invalid bytes.
    let value = parse_json(&String::from_utf8_lossy(&bytes))?;
    if matches!(value, JsValue::Null) {
        return Err(invalid(
            "Cannot read properties of null (reading 'schemaVersion')",
        ));
    }
    let JsValue::Array(records) = field(&value, "records") else {
        return Err(invalid("invalid run registry snapshot"));
    };
    if field(&value, "schemaVersion") != &JsValue::Number(1.0) || !truthy(field(&value, "events")) {
        return Err(invalid("invalid run registry snapshot"));
    }
    let mut snapshot = Snapshot::default();
    for record in records {
        if matches!(record, JsValue::Null) {
            return Err(invalid(
                "Cannot read properties of null (reading 'projectKey')",
            ));
        }
        let key = run_key(
            field(record, "projectKey"),
            field(record, "sessionKey"),
            field(record, "runId"),
        );
        snapshot.records.insert(key, record.clone());
    }
    // Object.entries(snapshot.events). Load order is irrelevant to the bytes:
    // persist re-serializes with JS key order, as Object.fromEntries does.
    match field(&value, "events") {
        JsValue::Object(events) => {
            for (key, list) in events.entries() {
                snapshot.events.insert(key.clone(), list.clone());
            }
        }
        JsValue::Array(items) => {
            for (index, list) in items.iter().enumerate() {
                snapshot
                    .events
                    .insert(index.to_string().into(), list.clone());
            }
        }
        JsValue::String(text) => {
            for (index, unit) in text.units().iter().enumerate() {
                snapshot.events.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    Ok(snapshot)
}

/// `${projectKey ?? ""}\0${sessionKey}\0${runId}` (RunRegistry.ts:70).
fn run_key(project: &JsValue, session: &JsValue, run: &JsValue) -> JsString {
    let mut units = match project {
        JsValue::Undefined | JsValue::Null => vec![],
        value => template_units(value),
    };
    units.push(0);
    units.extend(template_units(session));
    units.push(0);
    units.extend(template_units(run));
    JsString::from_utf16(units)
}
/// JS template-literal ToString of a JSON value.
fn template_units(value: &JsValue) -> Vec<u16> {
    match value {
        JsValue::String(s) => s.units().to_vec(),
        JsValue::Array(items) => {
            let mut units = vec![];
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    units.push(u16::from(b','));
                }
                if !matches!(item, JsValue::Undefined | JsValue::Null) {
                    units.extend(template_units(item));
                }
            }
            units
        }
        other => {
            let text = match other {
                JsValue::Undefined => "undefined".to_owned(),
                JsValue::Null => "null".to_owned(),
                JsValue::Bool(b) => b.to_string(),
                JsValue::Number(n) => number_string(*n),
                _ => "[object Object]".to_owned(),
            };
            text.encode_utf16().collect()
        }
    }
}
fn number_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        (if n > 0.0 { "Infinity" } else { "-Infinity" }).into()
    } else {
        // JSON.stringify uses Number::toString for finite values.
        stringify_json(&JsValue::Number(n))
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}
/// JS `value + 1` for a JSON value (`revision += 1`, `lastSeq + 1`).
fn js_plus_one(value: &JsValue) -> JsValue {
    match value {
        JsValue::Number(n) => JsValue::Number(n + 1.0),
        JsValue::Undefined => JsValue::Number(f64::NAN),
        JsValue::Null => JsValue::Number(1.0),
        JsValue::Bool(b) => JsValue::Number(f64::from(u8::from(*b)) + 1.0),
        other => {
            let mut units = template_units(other);
            units.push(u16::from(b'1'));
            JsValue::String(JsString::from_utf16(units))
        }
    }
}
/// JS ToNumber, for `entry.seq > afterSeq` with a numeric right side.
fn js_to_number(value: &JsValue) -> f64 {
    static DECIMAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$").unwrap()
    });
    match value {
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        JsValue::Number(n) => *n,
        other => {
            let text = String::from_utf16_lossy(&template_units(other));
            let text = js_trim(&text);
            if text.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0o", 8),
                ("0O", 8),
                ("0b", 2),
                ("0B", 2),
            ] {
                if let Some(digits) = text.strip_prefix(prefix) {
                    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                        return f64::NAN;
                    }
                    return digits.chars().fold(0.0, |acc, c| {
                        acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap())
                    });
                }
            }
            match text {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ if DECIMAL.is_match(text) => text.parse().unwrap_or(f64::NAN),
                _ => f64::NAN,
            }
        }
    }
}
/// `Array.prototype.slice(0, limit)` end index.
fn slice_end(len: usize, limit: f64) -> usize {
    let relative = if limit.is_nan() { 0.0 } else { limit.trunc() };
    if relative < 0.0 {
        (len as f64 + relative).max(0.0) as usize
    } else {
        relative.min(len as f64) as usize
    }
}
/// `eventMap.get(key) ?? []`; anything else lacks `.push`/`.filter` in TS.
fn event_list(value: Option<&JsValue>) -> Result<&[JsValue]> {
    match value {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(&[]),
        Some(JsValue::Array(items)) => Ok(items),
        Some(_) => Err(invalid("Run registry event list is not an array.")),
    }
}

/// RunRegistry.ts:73-81.
fn owner_alive(owner: &JsValue) -> bool {
    owner.as_number().is_some_and(process_alive)
}
/// RunRegistry.ts:123-133: `"pid"\s*:\s*(\d+)` then `parseInt`.
fn lock_owner_alive(contents: &str) -> bool {
    static PID: LazyLock<Regex> = LazyLock::new(|| {
        let ws = r"[\t\n\x0B\x0C\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*";
        Regex::new(&format!(r#""pid"{ws}:{ws}([0-9]+)"#)).unwrap()
    });
    PID.captures(contents)
        .and_then(|c| c[1].parse::<f64>().ok())
        .is_some_and(process_alive)
}
/// `Number.isInteger(pid) && pid > 0`, then `process.kill(pid, 0)`: only ESRCH
/// means dead. Node rejects `pid != (pid | 0)` with ERR_INVALID_ARG_TYPE, which
/// is not ESRCH, so a pid above i32::MAX counts as alive.
fn process_alive(pid: f64) -> bool {
    if !pid.is_finite() || pid.fract() != 0.0 || pid <= 0.0 {
        return false;
    }
    if pid > f64::from(i32::MAX) {
        return true;
    }
    rustix::process::Pid::from_raw(pid as i32).is_some_and(|pid| {
        !matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        )
    })
}

/// Strict `value === expected` against a string literal.
fn is(value: &JsValue, expected: &str) -> bool {
    value.as_string().is_some_and(|v| v.equals_str(expected))
}
fn record_field<'a>(record: &'a JsObject, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    record.get(key).unwrap_or(&UNDEFINED)
}
fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => private_dirs(parent),
        None => Ok(()),
    }
}
fn now() -> Result<JsValue> {
    timestamp()
        .map(|t| JsValue::from(t.as_str()))
        .map_err(|e| invalid(e.to_string()))
}
fn invalid(message: impl Into<String>) -> RunRegistryError {
    RunRegistryError::Invalid(message.into())
}
