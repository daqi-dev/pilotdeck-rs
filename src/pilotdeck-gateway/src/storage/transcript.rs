use super::{LedgerError, Result, SessionOperationLedger, TranscriptSink, invalid, ledger::field};
use crate::{
    metadata::js_trim,
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 50 * 1024 * 1024;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: &'static str,
    pub line: Option<usize>,
    /// JSON parser wording is Rust-specific; code/severity/line match TS.
    pub message: String,
}
#[derive(Debug)]
pub struct TranscriptRead {
    pub entries: Vec<JsValue>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriterState {
    /// TS transcript sequence is the commit revision. No separate disk field.
    pub sequence: u64,
    pub last_entry_id: Option<JsString>,
}

pub fn read_transcript(path: impl AsRef<Path>) -> Result<TranscriptRead> {
    Ok(load(path.as_ref(), MAX_BYTES)?.1)
}
/// TS `readTranscript(path, { maxBytes })`; `None` is the 50 MiB default.
pub fn read_transcript_limited(
    path: impl AsRef<Path>,
    max_bytes: Option<u64>,
) -> Result<TranscriptRead> {
    Ok(load(path.as_ref(), max_bytes.unwrap_or(MAX_BYTES))?.1)
}
fn load(path: &Path, max_bytes: u64) -> Result<(Vec<u8>, TranscriptRead)> {
    let bytes = match fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => {
            return Ok((
                vec![],
                TranscriptRead {
                    entries: vec![],
                    diagnostics: vec![Diagnostic {
                        code: "transcript_too_large",
                        severity: "error",
                        line: None,
                        message: format!(
                            "Transcript {} is larger than {max_bytes} bytes.",
                            path.display()
                        ),
                    }],
                },
            ));
        }
        Ok(_) => fs::read(path),
        Err(error) => Err(error),
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((
                vec![],
                TranscriptRead {
                    entries: vec![],
                    diagnostics: vec![Diagnostic {
                        code: "transcript_missing",
                        severity: "warning",
                        line: None,
                        message: format!("Transcript {} does not exist.", path.display()),
                    }],
                },
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let mut read = TranscriptRead {
        entries: vec![],
        diagnostics: vec![],
    };
    // Match Node readFile(path, "utf8"): replacement characters are observable
    // in parsed values, while original bytes stay intact for the next append.
    for (index, line) in String::from_utf8_lossy(&bytes).split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if js_trim(line).is_empty() {
            continue;
        }
        match parse_json(line) {
            Ok(entry)
                if entry.as_object().is_some()
                    && ["type", "sessionId", "turnId", "createdAt"]
                        .iter()
                        .all(|k| field(&entry, k).as_string().is_some())
                    && field(&entry, "sequence").as_number().is_some() =>
            {
                read.entries.push(entry)
            }
            Ok(_) => read.diagnostics.push(Diagnostic {
                code: "transcript_entry_invalid",
                severity: "error",
                line: Some(index + 1),
                message: "Transcript entry has an invalid shape.".into(),
            }),
            Err(error) => read.diagnostics.push(Diagnostic {
                code: "transcript_line_invalid",
                severity: "error",
                line: Some(index + 1),
                message: error.to_string(),
            }),
        }
    }
    // Reuse the gateway's pinned Node/ICU collation for equal sequence values.
    let mut seen = HashSet::new();
    let mut ties = HashSet::new();
    for entry in &read.entries {
        let n = field(entry, "sequence").as_number().unwrap();
        let bits = if n == 0.0 { 0 } else { n.to_bits() };
        if !seen.insert(bits) {
            ties.insert(bits);
        }
    }
    let collator = if ties.is_empty() {
        None
    } else {
        for entry in &read.entries {
            let n = field(entry, "sequence").as_number().unwrap();
            if ties.contains(&if n == 0.0 { 0 } else { n.to_bits() })
                && has_unpaired_surrogate(field(entry, "createdAt").as_string().unwrap())
            {
                return Err(invalid(
                    "Transcript timestamp collation requires the Node host for unpaired surrogates.",
                ));
            }
        }
        Some(
            node_collator(&node_locale(|key| std::env::var(key).ok()))
                .map_err(|(_, message)| invalid(message))?,
        )
    };
    read.entries.sort_by(|a, b| {
        let a_seq = field(a, "sequence").as_number().unwrap();
        let b_seq = field(b, "sequence").as_number().unwrap();
        if a_seq != b_seq {
            (a_seq - b_seq).partial_cmp(&0.0).unwrap_or(Ordering::Equal)
        } else {
            collator.as_ref().unwrap().compare_utf16(
                field(a, "createdAt").as_string().unwrap().units(),
                field(b, "createdAt").as_string().unwrap().units(),
            )
        }
    });
    Ok((bytes, read))
}

/// A single-owner JSONL adapter, using `<canonical path>.lock` advisory locking.
/// The lock file is kept so contenders always lock the same inode; Drop releases
/// the OS lock, including after crashes. TS does not honor this Rust lock:
/// TS/Rust cutover must stop the previous writer before opening this store.
/// Hard-link aliases are unsupported: each name has its own lock, and replacing
/// one name forks its history away from the other names of the original inode.
/// Appends preserve original bytes and atomically replace through a sibling file.
/// Atomic visibility does not promise power-loss durability: like TS persistence,
/// this adapter does not fsync the parent directory. A write failure requires
/// dropping/reopening the ledger; cached recoveries and no-op writes also fail.
/// This is O(transcript bytes) per commit; loading retains the TS 50 MiB ceiling.
/// Historical replacement transactions and arbitrary transcript writes are not
/// exposed by this ledger-only adapter.
pub struct JsonlLedgerStore {
    path: PathBuf,
    _lock: LedgerLock,
    bytes: Vec<u8>,
    state: WriterState,
    entry_ids: HashSet<JsString>,
    diagnostics: Vec<Diagnostic>,
    reopen_reason: Option<String>,
    #[cfg(test)]
    pub(super) fail_write: bool,
    #[cfg(test)]
    pub(super) fail_rename: bool,
    #[cfg(test)]
    pub(super) fail_after_rename: bool,
}
impl JsonlLedgerStore {
    pub fn open(
        path: impl AsRef<Path>,
        session_id: impl Into<JsString>,
    ) -> Result<SessionOperationLedger<Self>> {
        let supplied = path.as_ref();
        let parent = supplied
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        private_dirs(parent)?;
        let path = if supplied.exists() {
            supplied.canonicalize()?
        } else {
            parent.canonicalize()?.join(
                supplied
                    .file_name()
                    .ok_or_else(|| invalid("Transcript path requires a filename."))?,
            )
        };
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = private_open()
            .read(true)
            .write(true)
            .create(true)
            .open(PathBuf::from(lock_path))?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => LedgerError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Session operation ledger is locked.",
            )),
            fs::TryLockError::Error(error) => error.into(),
        })?;
        let lock = LedgerLock(lock);
        let (bytes, read) = load(&path, MAX_BYTES)?;
        // Reading too-large transcripts returns diagnostics, as TS does. Opening
        // a writer must not mistake that empty projection for a fresh session.
        if read
            .diagnostics
            .iter()
            .any(|d| d.code == "transcript_too_large")
        {
            return Err(invalid(read.diagnostics[0].message.clone()));
        }
        let (state, entry_ids) = restore_state(&read.entries)?;
        let store = Self {
            path,
            _lock: lock,
            bytes,
            state,
            entry_ids,
            diagnostics: read.diagnostics,
            reopen_reason: None,
            #[cfg(test)]
            fail_write: false,
            #[cfg(test)]
            fail_rename: false,
            #[cfg(test)]
            fail_after_rename: false,
        };
        SessionOperationLedger::new(session_id.into(), store, &read.entries)
    }
    /// Last acknowledged in-memory state, for diagnostics only if poisoned.
    /// This does not certify the disk outcome; reopen to recover after failure.
    pub fn snapshot_state(&self) -> WriterState {
        self.state.clone()
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[cfg(test)]
    pub(super) fn clone_lock_for_test(&self) -> io::Result<File> {
        self._lock.0.try_clone()
    }
    fn replace(&mut self, appended: &[u8]) -> Result<()> {
        self.check_health()?;
        if let Err(error) = self.replace_inner(appended) {
            let reason = error.to_string();
            self.reopen_reason = Some(reason.clone());
            return Err(LedgerError::ReopenRequired(reason));
        }
        Ok(())
    }
    fn replace_inner(&mut self, appended: &[u8]) -> Result<()> {
        let current = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        if current != self.bytes {
            return Err(invalid(
                "Transcript changed outside its owner; reopen before writing.",
            ));
        }
        let next = after_debris(&self.bytes, appended);
        let (mut file, cleanup) = create_temporary(&self.path)?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_write) {
            file.write_all(&next[..next.len() / 2])?;
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected partial temporary write",
            )
            .into());
        }
        file.write_all(&next)?;
        file.sync_all()?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_rename) {
            return Err(io::Error::other("injected rename failure").into());
        }
        // Rename is the visibility commit point; subsequent production memory
        // updates are infallible. No directory fsync/power-loss guarantee (TS
        // JsonlSessionPersistence.ts:24-30 does not sync ordinary ledger writes).
        fs::rename(&cleanup.0, &self.path)?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_after_rename) {
            return Err(io::Error::other("injected uncertain outcome after rename").into());
        }
        self.bytes = next;
        Ok(())
    }
}
impl TranscriptSink for JsonlLedgerStore {
    fn check_health(&self) -> Result<()> {
        match &self.reopen_reason {
            Some(reason) => Err(LedgerError::ReopenRequired(reason.clone())),
            None => Ok(()),
        }
    }
    fn append(
        &mut self,
        session_id: &JsString,
        turn_id: &JsString,
        mut event: JsObject,
    ) -> Result<()> {
        self.check_health()?;
        let sequence = self.state.sequence + 1;
        if sequence as f64 > MAX_SAFE_INTEGER {
            return Err(invalid(
                "Session event sequence must be a positive safe integer.",
            ));
        }
        let entry_id: JsString = uuid()?.into();
        if self.entry_ids.contains(&entry_id) {
            return Err(invalid("Duplicate session event entryId."));
        }
        event.insert("sessionId".into(), JsValue::String(session_id.clone()));
        event.insert("turnId".into(), JsValue::String(turn_id.clone()));
        event.insert("sequence".into(), JsValue::Number(sequence as f64));
        event.insert("createdAt".into(), timestamp()?.as_str().into());
        event.insert("entryId".into(), JsValue::String(entry_id.clone()));
        event.insert(
            "parentEntryId".into(),
            self.state
                .last_entry_id
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Null),
        );
        let mut line = stringify_json(&JsValue::Object(event))?
            .ok_or_else(|| invalid("Session event must be JSON serializable."))?;
        line.push('\n');
        self.replace(line.as_bytes())?;
        self.state = WriterState {
            sequence,
            last_entry_id: Some(entry_id.clone()),
        };
        self.entry_ids.insert(entry_id);
        Ok(())
    }
}
fn restore_state(entries: &[JsValue]) -> Result<(WriterState, HashSet<JsString>)> {
    let mut state = WriterState::default();
    let mut ids = HashSet::new();
    for entry in entries {
        let seq = field(entry, "sequence").as_number().unwrap();
        if !seq.is_finite() || seq.fract() != 0.0 || !(1.0..=MAX_SAFE_INTEGER).contains(&seq) {
            return Err(invalid(
                "Session event sequence must be a positive safe integer.",
            ));
        }
        if seq <= state.sequence as f64 {
            return Err(invalid("Session event sequence must increase strictly."));
        }
        state.sequence = seq as u64;
        let id = entry.get("entryId");
        if let Some(id) = id {
            let id = valid_id(id)?;
            if !ids.insert(id.clone()) {
                return Err(invalid("Session event entryId must be unique."));
            }
            state.last_entry_id = Some(id.clone());
        }
        if let Some(parent) = entry
            .get("parentEntryId")
            .filter(|v| !matches!(v, JsValue::Null))
        {
            valid_id(parent)?;
            if Some(parent) == id {
                return Err(invalid("Session event cannot reference itself as parent."));
            }
        }
    }
    Ok((state, ids))
}
fn valid_id(value: &JsValue) -> Result<&JsString> {
    value
        .as_string()
        .filter(|v| !js_trim(&String::from_utf16_lossy(v.units())).is_empty())
        .ok_or_else(|| invalid("Session event entryId must be a non-empty string."))
}
/// JsonlSessionPersistence.prepareTail: crash debris without a final LF is kept,
/// but the next record must not join it.
pub(super) fn after_debris(current: &[u8], appended: &[u8]) -> Vec<u8> {
    let mut next = current.to_vec();
    if !next.is_empty() && next.last() != Some(&b'\n') {
        next.push(b'\n');
    }
    next.extend_from_slice(appended);
    next
}
/// Private sibling for an atomic replace; removed on drop unless renamed away.
pub(super) fn create_temporary(path: &Path) -> Result<(File, Temporary)> {
    let temporary = path.with_file_name(format!(".ledger-{}.tmp", uuid()?));
    let file = private_open()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    Ok((file, Temporary(temporary)))
}
pub(super) fn private_open() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    options.mode(0o600);
    options
}
pub(super) fn private_dirs(dir: &Path) -> io::Result<()> {
    let mut dirs = fs::DirBuilder::new();
    dirs.recursive(true);
    #[cfg(unix)]
    dirs.mode(0o700);
    dirs.create(dir)
}
pub(super) struct Temporary(pub(super) PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct LedgerLock(File);
impl Drop for LedgerLock {
    fn drop(&mut self) {
        // Explicit unlock matters when a concurrent process spawn briefly
        // inherits a duplicate descriptor before exec closes it (CLOEXEC).
        // Closing only this File would keep that shared flock alive.
        let _ = self.0.unlock();
    }
}
pub(super) fn uuid() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
pub(super) fn timestamp() -> Result<String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid(e.to_string()))?;
    // Gregorian civil date from days since Unix epoch; no process locale/timezone.
    let z = elapsed.as_secs() / 86400 + 719468;
    let era = z / 146097;
    let day = z - era * 146097;
    let year_of_era = (day - day / 1460 + day / 36524 - day / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = year + u64::from(month <= 2);
    let seconds = elapsed.as_secs() % 86400;
    Ok(format!(
        "{year:04}-{month:02}-{date:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        elapsed.subsec_millis()
    ))
}
