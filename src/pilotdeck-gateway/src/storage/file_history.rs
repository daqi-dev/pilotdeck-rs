//! `FileHistoryStore` (baseline `session/filesystem/FileHistoryStore.ts`).
//!
//! Snapshot transactions are serialized by one mutex, exactly like the TS
//! mutex chain. Backups are content copies named `<sha16(path)>@v<version>`
//! under a session-scoped directory; snapshots keep insertion order so
//! transcript records are byte-stable. The optional host blob store
//! (`backupStorage`) is not ported in this slice: every fixture and host wiring
//! in this PR uses the filesystem layout. Hosts insert an `Arc<FileHistoryStore>`
//! into `ToolRuntimeContext.file_history`; the tools downcast that opaque port.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::format_epoch_iso;
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_MAX_SNAPSHOTS: usize = 100;

pub type FileHistoryClock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type SnapshotRecorded = Arc<dyn Fn(JsObject, &str) -> FileHistoryResult<()> + Send + Sync>;
pub type FileHistoryWarn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHistoryBackup {
    pub backup_file_name: Option<String>,
    pub version: u64,
    pub backup_time_ms: i64,
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectedFileState {
    Missing,
    Present { sha256: String, mode: Option<u32> },
}

#[derive(Clone, Debug)]
pub struct FileHistorySnapshot {
    pub message_id: String,
    pub tracked_file_backups: Vec<(String, FileHistoryBackup)>,
    pub expected_file_states: Vec<(String, ExpectedFileState)>,
    pub timestamp_ms: i64,
}
impl FileHistorySnapshot {
    pub fn backup(&self, path: &str) -> Option<&FileHistoryBackup> {
        self.tracked_file_backups
            .iter()
            .find(|(key, _)| key == path)
            .map(|(_, backup)| backup)
    }
    fn set_backup(&mut self, path: String, backup: FileHistoryBackup) {
        if let Some(entry) = self
            .tracked_file_backups
            .iter_mut()
            .find(|(key, _)| *key == path)
        {
            entry.1 = backup;
        } else {
            self.tracked_file_backups.push((path, backup));
        }
    }
    fn set_expected(&mut self, path: String, state: ExpectedFileState) {
        if let Some(entry) = self
            .expected_file_states
            .iter_mut()
            .find(|(key, _)| *key == path)
        {
            entry.1 = state;
        } else {
            self.expected_file_states.push((path, state));
        }
    }
}

/// Deserialized `file_snapshot_recorded` entry used by resume replay (F12).
#[derive(Clone, Debug)]
pub struct RecordedBackup {
    pub backup_file_name: Option<String>,
    pub version: u64,
    pub backup_time_ms: i64,
    pub mode: Option<u32>,
}
#[derive(Clone, Debug)]
pub struct SnapshotRecordedEntry {
    pub message_id: String,
    pub tracked_file_backups: Vec<(String, RecordedBackup)>,
    pub expected_file_states: Vec<(String, ExpectedFileState)>,
    /// `timestamp ?? snapshotTimestamp ?? createdAt ?? 0`, already parsed.
    pub timestamp_ms: Option<i64>,
}

#[derive(Clone)]
pub struct FileHistoryOptions {
    /// Absolute path under which `<sha16>@v<version>` files live.
    pub backup_dir: PathBuf,
    pub max_file_bytes: Option<u64>,
    pub max_snapshots: Option<usize>,
    pub now: Option<FileHistoryClock>,
    pub on_snapshot_recorded: Option<SnapshotRecorded>,
    pub warn: Option<FileHistoryWarn>,
}
impl FileHistoryOptions {
    pub fn new(backup_dir: impl Into<PathBuf>) -> Self {
        Self {
            backup_dir: backup_dir.into(),
            max_file_bytes: None,
            max_snapshots: None,
            now: None,
            on_snapshot_recorded: None,
            warn: None,
        }
    }
}

#[derive(Debug)]
pub enum FileHistoryError {
    Io(io::Error),
    NoSnapshot(String),
}
impl fmt::Display for FileHistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::NoSnapshot(id) => write!(f, "No snapshot for messageId {id}"),
        }
    }
}
impl std::error::Error for FileHistoryError {}
impl From<io::Error> for FileHistoryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
pub type FileHistoryResult<T> = Result<T, FileHistoryError>;

#[derive(Clone, Debug, Default)]
pub struct FileHistoryState {
    pub snapshots: Vec<FileHistorySnapshot>,
    pub tracked_files: Vec<String>,
}

pub struct FileHistoryStore {
    options: FileHistoryOptions,
    state: Mutex<FileHistoryState>,
    mtime_cache: Mutex<Vec<(String, Option<f64>)>>,
}

impl FileHistoryStore {
    pub fn new(options: FileHistoryOptions) -> Self {
        Self {
            options,
            state: Mutex::new(FileHistoryState::default()),
            mtime_cache: Mutex::new(Vec::new()),
        }
    }
    pub fn backup_dir(&self) -> &Path {
        &self.options.backup_dir
    }
    pub fn max_file_bytes(&self) -> u64 {
        self.options
            .max_file_bytes
            .unwrap_or(DEFAULT_MAX_FILE_BYTES)
    }
    pub fn max_snapshots(&self) -> usize {
        self.options.max_snapshots.unwrap_or(DEFAULT_MAX_SNAPSHOTS)
    }
    pub fn state(&self) -> FileHistoryState {
        self.state.lock().unwrap().clone()
    }
    fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0),
        }
    }
    fn warn(&self, message: String) {
        if let Some(warn) = &self.options.warn {
            warn(&message);
        }
    }

    /// F1/F2 — capture a file before an edit; repeated calls in the same
    /// snapshot keep the existing v1 backup.
    pub fn track_edit(&self, file_path: &str, message_id: &str) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path);
        if !state.tracked_files.contains(&abs_path) {
            state.tracked_files.push(abs_path.clone());
        }
        let snapshot_index = match find_snapshot(&state.snapshots, message_id) {
            Some(index) => index,
            None => {
                let timestamp = self.now();
                state.snapshots.push(FileHistorySnapshot {
                    message_id: message_id.to_string(),
                    tracked_file_backups: Vec::new(),
                    expected_file_states: Vec::new(),
                    timestamp_ms: timestamp,
                });
                state.snapshots.len() - 1
            }
        };
        if state.snapshots[snapshot_index].backup(&abs_path).is_some() {
            return Ok(());
        }
        let clock = || self.now();
        let result = create_backup(
            &self.options.backup_dir,
            &abs_path,
            1,
            self.max_file_bytes(),
            &clock,
        )?;
        if result.oversize {
            self.warn(format!(
                "file-history: skipping backup for {abs_path} (size > {} bytes)",
                self.max_file_bytes()
            ));
        }
        let backup = result.backup;
        state.snapshots[snapshot_index].set_backup(abs_path.clone(), backup);
        self.cache_mtime(&abs_path);
        let snapshot = state.snapshots[snapshot_index].clone();
        drop(state);
        self.record_transcript(&snapshot, "update")?;
        Ok(())
    }

    /// F6 — finalize an open snapshot or create one, bumping versions for files
    /// whose mtime moved since the last record.
    pub fn make_snapshot(&self, message_id: &str) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let existing_index = find_snapshot(&state.snapshots, message_id);
        let existing = existing_index.is_some();
        let mut snapshot = match existing_index {
            Some(index) => state.snapshots[index].clone(),
            None => FileHistorySnapshot {
                message_id: message_id.to_string(),
                tracked_file_backups: Vec::new(),
                expected_file_states: Vec::new(),
                timestamp_ms: self.now(),
            },
        };
        let previous_tracked: Vec<(String, FileHistoryBackup)> = match existing_index {
            Some(index) => index
                .checked_sub(1)
                .map(|previous| state.snapshots[previous].tracked_file_backups.clone())
                .unwrap_or_default(),
            None => state
                .snapshots
                .last()
                .map(|previous| previous.tracked_file_backups.clone())
                .unwrap_or_default(),
        };

        for abs_path in state.tracked_files.clone() {
            let cached = self
                .mtime_cache
                .lock()
                .unwrap()
                .iter()
                .find(|(key, _)| *key == abs_path)
                .map(|(_, value)| *value);
            let current = match fs::metadata(&abs_path) {
                Ok(metadata) => Some(metadata_mtime_f64(&metadata)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let previous_backup = snapshot.backup(&abs_path).cloned().or_else(|| {
                previous_tracked
                    .iter()
                    .find(|(key, _)| *key == abs_path)
                    .map(|(_, backup)| backup.clone())
            });
            let previous_version = previous_backup.as_ref().map_or(0, |backup| backup.version);
            let mtime_changed = cached != Some(current);
            if !mtime_changed && let Some(previous_backup) = previous_backup {
                if snapshot.backup(&abs_path).is_none() {
                    snapshot.set_backup(abs_path.clone(), previous_backup);
                }
                continue;
            }
            let clock = || self.now();
            let result = create_backup(
                &self.options.backup_dir,
                &abs_path,
                previous_version + 1,
                self.max_file_bytes(),
                &clock,
            )?;
            if result.oversize {
                self.warn(format!(
                    "file-history: skipping backup for {abs_path} (size > {} bytes)",
                    self.max_file_bytes()
                ));
            }
            snapshot.set_backup(abs_path.clone(), result.backup);
            let mut cache = self.mtime_cache.lock().unwrap();
            if let Some(entry) = cache.iter_mut().find(|(key, _)| *key == abs_path) {
                entry.1 = current;
            } else {
                cache.push((abs_path.clone(), current));
            }
        }

        if let Some(index) = existing_index {
            state.snapshots[index] = snapshot.clone();
        } else {
            state.snapshots.push(snapshot.clone());
        }
        let kind = if existing { "update" } else { "create" };
        // TS records the snapshot before evicting; keep the callback inside the
        // transaction so a host that re-enters the store deadlocks like the TS
        // mutex chain rather than observing a half-evicted state.
        self.record_transcript(&snapshot, kind)?;
        self.evict_if_needed(&mut state);
        drop(state);
        Ok(())
    }

    /// F12 fingerprint of the post-edit state; rewind compares it first.
    pub fn mark_edit_committed(&self, file_path: &str, message_id: &str) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path);
        let Some(index) = find_snapshot(&state.snapshots, message_id) else {
            return Ok(());
        };
        if state.snapshots[index].backup(&abs_path).is_none() {
            return Ok(());
        }
        let expected = read_expected_file_state(&abs_path)?;
        state.snapshots[index].set_expected(abs_path, expected);
        let snapshot = state.snapshots[index].clone();
        drop(state);
        self.record_transcript(&snapshot, "update")?;
        Ok(())
    }

    pub fn get_conflict_paths(&self, message_id: &str) -> FileHistoryResult<Vec<String>> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id == message_id)
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?;
        find_conflict_paths(snapshot)
    }

    /// F8/F9 — restore every backup of the last snapshot for `messageId`.
    pub fn rewind(&self, message_id: &str) -> FileHistoryResult<RewindOutcome> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id == message_id)
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?
            .clone();
        drop(state);
        let conflicts = find_conflict_paths(&snapshot)?;
        if !conflicts.is_empty() {
            return Ok(RewindOutcome {
                files_changed: Vec::new(),
                missing: Vec::new(),
                conflicts,
            });
        }
        let mut files_changed = Vec::new();
        let mut missing = Vec::new();
        for (abs_path, backup) in &snapshot.tracked_file_backups {
            match restore_backup(&self.options.backup_dir, abs_path, backup)? {
                RestoreOutcome::Restored | RestoreOutcome::Deleted => {
                    files_changed.push(abs_path.clone());
                }
                RestoreOutcome::Missing => {
                    missing.push(abs_path.clone());
                    self.warn(format!(
                        "file-history: backup {} for {abs_path} is missing on disk; skipping",
                        backup.backup_file_name.as_deref().unwrap_or("null")
                    ));
                }
            }
        }
        Ok(RewindOutcome {
            files_changed,
            missing,
            conflicts: Vec::new(),
        })
    }

    /// F14 (slim) — aggregate line insertions/deletions vs the current disk.
    pub fn get_diff_stats(&self, message_id: &str) -> FileHistoryResult<DiffStats> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id == message_id)
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?
            .clone();
        drop(state);
        let mut insertions = 0usize;
        let mut deletions = 0usize;
        let mut files_changed = 0usize;
        for (abs_path, backup) in &snapshot.tracked_file_backups {
            let before = match &backup.backup_file_name {
                Some(name) => safe_read_text(&self.options.backup_dir.join(name))?,
                None => None,
            };
            let after = safe_read_text(Path::new(abs_path))?;
            match (before, after) {
                (None, None) => {}
                (None, Some(after)) => {
                    insertions += count_lines(&after);
                    files_changed += 1;
                }
                (Some(before), None) => {
                    deletions += count_lines(&before);
                    files_changed += 1;
                }
                (Some(before), Some(after)) => {
                    if before != after {
                        let delta = line_delta(&before, &after);
                        insertions += delta.0;
                        deletions += delta.1;
                        files_changed += 1;
                    }
                }
            }
        }
        Ok(DiffStats {
            files_changed,
            insertions,
            deletions,
        })
    }

    /// F12 — replay `file_snapshot_recorded` entries on resume.
    pub fn replay_from_transcript(&self, entries: &[SnapshotRecordedEntry]) {
        let mut state = self.state.lock().unwrap();
        for entry in entries {
            let tracked_file_backups: Vec<(String, FileHistoryBackup)> = entry
                .tracked_file_backups
                .iter()
                .map(|(path, backup)| {
                    (
                        path.clone(),
                        FileHistoryBackup {
                            backup_file_name: backup.backup_file_name.clone(),
                            version: backup.version,
                            backup_time_ms: backup.backup_time_ms,
                            mode: backup.mode,
                        },
                    )
                })
                .collect();
            for (path, _) in &tracked_file_backups {
                if !state.tracked_files.contains(path) {
                    state.tracked_files.push(path.clone());
                }
            }
            let snapshot = FileHistorySnapshot {
                message_id: entry.message_id.clone(),
                tracked_file_backups,
                expected_file_states: entry.expected_file_states.clone(),
                timestamp_ms: entry.timestamp_ms.unwrap_or(0),
            };
            match find_snapshot(&state.snapshots, &entry.message_id) {
                Some(index) => state.snapshots[index] = snapshot,
                None => state.snapshots.push(snapshot),
            }
        }
    }

    fn cache_mtime(&self, file_path: &str) {
        let mtime = fs::metadata(file_path)
            .map(|metadata| metadata_mtime_f64(&metadata))
            .ok();
        let mut cache = self.mtime_cache.lock().unwrap();
        if let Some(entry) = cache.iter_mut().find(|(key, _)| *key == file_path) {
            entry.1 = mtime;
        } else {
            cache.push((file_path.to_string(), mtime));
        }
    }

    fn record_transcript(
        &self,
        snapshot: &FileHistorySnapshot,
        kind: &str,
    ) -> FileHistoryResult<()> {
        let Some(sink) = &self.options.on_snapshot_recorded else {
            return Ok(());
        };
        let mut tracked = JsObject::new();
        for (path, backup) in &snapshot.tracked_file_backups {
            let mut value = JsObject::new();
            value.insert(
                "backupFileName".into(),
                match &backup.backup_file_name {
                    Some(name) => JsValue::from(name.as_str()),
                    None => JsValue::Null,
                },
            );
            value.insert("version".into(), JsValue::Number(backup.version as f64));
            if let Some(mode) = backup.mode {
                value.insert("mode".into(), JsValue::Number(mode as f64));
            }
            value.insert(
                "backupTime".into(),
                JsValue::from(format_epoch_iso(backup.backup_time_ms).as_str()),
            );
            tracked.insert(JsString::from(path.as_str()), JsValue::Object(value));
        }
        let mut entry = JsObject::new();
        entry.insert(
            "messageId".into(),
            JsValue::from(snapshot.message_id.as_str()),
        );
        entry.insert("trackedFileBackups".into(), JsValue::Object(tracked));
        if !snapshot.expected_file_states.is_empty() {
            let mut expected = JsObject::new();
            for (path, state) in &snapshot.expected_file_states {
                let mut value = JsObject::new();
                match state {
                    ExpectedFileState::Missing => {
                        value.insert("exists".into(), JsValue::Bool(false));
                    }
                    ExpectedFileState::Present { sha256, mode } => {
                        value.insert("exists".into(), JsValue::Bool(true));
                        value.insert("sha256".into(), JsValue::from(sha256.as_str()));
                        if let Some(mode) = mode {
                            value.insert("mode".into(), JsValue::Number(*mode as f64));
                        }
                    }
                }
                expected.insert(JsString::from(path.as_str()), JsValue::Object(value));
            }
            entry.insert("expectedFileStates".into(), JsValue::Object(expected));
        }
        entry.insert(
            "timestamp".into(),
            JsValue::from(format_epoch_iso(snapshot.timestamp_ms).as_str()),
        );
        sink(entry, kind)
    }

    /// F13 — drop the oldest snapshots and unreferenced backup files.
    fn evict_if_needed(&self, state: &mut FileHistoryState) {
        while state.snapshots.len() > self.max_snapshots() {
            if state.snapshots.is_empty() {
                break;
            }
            let evicted = state.snapshots.remove(0);
            for backup in evicted.tracked_file_backups.into_iter().map(|(_, b)| b) {
                let Some(name) = backup.backup_file_name else {
                    continue;
                };
                let still_referenced = state.snapshots.iter().any(|snapshot| {
                    snapshot.tracked_file_backups.iter().any(|(_, backup)| {
                        backup.backup_file_name.as_deref() == Some(name.as_str())
                    })
                });
                if still_referenced {
                    continue;
                }
                if let Err(error) = fs::remove_file(self.options.backup_dir.join(&name))
                    && error.kind() != io::ErrorKind::NotFound
                {
                    self.warn(format!("file-history: failed to evict {name}: {error}"));
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewindOutcome {
    pub files_changed: Vec<String>,
    pub missing: Vec<String>,
    pub conflicts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffStats {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

struct BackupResult {
    backup: FileHistoryBackup,
    oversize: bool,
}

/// The backup directory is process-private state, so a pre-planted hard link
/// or symlink at the destination must not be written through (decision card
/// 2026-10-19 B; same guard as the A6.6-pr6 spill copy). Open with O_NOFOLLOW
/// and fstat the handle: a symlink is ELOOP, and a second directory entry
/// (nlink > 1) is a hard link. Only then truncate, so a refused target is not
/// damaged before the check. O_NONBLOCK keeps a pre-planted FIFO from blocking
/// the open.
fn write_private_backup(source: &Path, backup_path: &Path, mode: u32) -> FileHistoryResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut destination = match options.open(backup_path) {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(private_backup_error(backup_path));
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = destination.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(private_backup_error(backup_path));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(private_backup_error(backup_path));
    }
    destination.set_len(0)?;
    let mut source = fs::File::open(source)?;
    io::copy(&mut source, &mut destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        destination.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

fn private_backup_error(path: &Path) -> FileHistoryError {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "file-history: refusing to write backup through non-private path {}",
            path.display()
        ),
    )
    .into()
}

fn create_backup(
    backup_dir: &Path,
    file_path: &str,
    version: u64,
    max_file_bytes: u64,
    now: &dyn Fn() -> i64,
) -> FileHistoryResult<BackupResult> {
    let metadata = match fs::metadata(file_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(null_backup(version, now(), false));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(null_backup(version, now(), false));
    }
    if metadata.len() > max_file_bytes {
        return Ok(null_backup(version, now(), true));
    }
    let backup_file_name = get_backup_file_name(file_path, version);
    fs::create_dir_all(backup_dir)?;
    let backup_path = backup_dir.join(&backup_file_name);
    write_private_backup(
        Path::new(file_path),
        &backup_path,
        metadata_mode(&metadata) & 0o777,
    )?;
    Ok(BackupResult {
        backup: FileHistoryBackup {
            backup_file_name: Some(backup_file_name),
            version,
            backup_time_ms: now(),
            mode: Some(metadata_mode(&metadata)),
        },
        oversize: false,
    })
}

fn null_backup(version: u64, backup_time: i64, oversize: bool) -> BackupResult {
    BackupResult {
        backup: FileHistoryBackup {
            backup_file_name: None,
            version,
            backup_time_ms: backup_time,
            mode: None,
        },
        oversize,
    }
}

enum RestoreOutcome {
    Restored,
    Deleted,
    Missing,
}

fn restore_backup(
    backup_dir: &Path,
    file_path: &str,
    backup: &FileHistoryBackup,
) -> FileHistoryResult<RestoreOutcome> {
    let Some(backup_file_name) = &backup.backup_file_name else {
        match fs::remove_file(file_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(RestoreOutcome::Deleted);
    };
    let backup_path = backup_dir.join(backup_file_name);
    match fs::metadata(&backup_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RestoreOutcome::Missing);
        }
        Err(error) => return Err(error.into()),
    }
    let target = Path::new(file_path);
    let target_dir = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(target_dir)?;
    let base = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temporary = target_dir.join(format!(".{base}.pilotdeck-rewind-{}", uuid::Uuid::new_v4()));
    let result = (|| -> FileHistoryResult<()> {
        fs::copy(&backup_path, &temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = backup.mode {
                fs::set_permissions(&temporary, fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
        fs::rename(&temporary, target)?;
        Ok(())
    })();
    // `rename` removes the temporary name on success; a failed copy/rename must
    // not leave a retry-visible partial artifact (F9).
    let _ = fs::remove_file(&temporary);
    result?;
    Ok(RestoreOutcome::Restored)
}

fn read_expected_file_state(file_path: &str) -> FileHistoryResult<ExpectedFileState> {
    let metadata = match fs::metadata(file_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ExpectedFileState::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(ExpectedFileState::Missing);
    }
    let bytes = fs::read(file_path)?;
    Ok(ExpectedFileState::Present {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        mode: Some(metadata_mode(&metadata) & 0o777),
    })
}

fn find_conflict_paths(snapshot: &FileHistorySnapshot) -> FileHistoryResult<Vec<String>> {
    let mut conflicts = Vec::new();
    for (path, expected) in &snapshot.expected_file_states {
        let actual = read_expected_file_state(path)?;
        if actual != *expected {
            conflicts.push(path.clone());
        }
    }
    Ok(conflicts)
}

fn find_snapshot(snapshots: &[FileHistorySnapshot], message_id: &str) -> Option<usize> {
    snapshots
        .iter()
        .rposition(|snapshot| snapshot.message_id == message_id)
}

fn safe_read_text(path: &Path) -> FileHistoryResult<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(_) => Ok(None),
    }
}

fn count_lines(value: &str) -> usize {
    if value.is_empty() {
        return 0;
    }
    value.bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn split_lines(value: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = value.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        lines.push(&value[start..end]);
        start = index + 1;
    }
    lines.push(&value[start..]);
    lines
}

fn line_delta(before: &str, after: &str) -> (usize, usize) {
    let mut before_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let mut after_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in split_lines(before) {
        *before_counts.entry(line).or_default() += 1;
    }
    for line in split_lines(after) {
        *after_counts.entry(line).or_default() += 1;
    }
    let mut deletions = 0;
    let mut insertions = 0;
    for (line, before_count) in &before_counts {
        let after_count = after_counts.get(line).copied().unwrap_or(0);
        if *before_count > after_count {
            deletions += before_count - after_count;
        }
    }
    for (line, after_count) in &after_counts {
        let before_count = before_counts.get(line).copied().unwrap_or(0);
        if *after_count > before_count {
            insertions += after_count - before_count;
        }
    }
    (insertions, deletions)
}

/// F4 — `<sha256(path)[..16]>@v<version>`.
pub fn get_backup_file_name(file_path: &str, version: u64) -> String {
    let hash = format!("{:x}", Sha256::digest(file_path.as_bytes()));
    format!("{}@v{version}", &hash[..16])
}

/// `path.resolve` for a POSIX absolute/relative input (lexical normalization).
pub fn resolve_node_path(file_path: &str) -> String {
    let combined = if file_path.starts_with('/') {
        file_path.to_string()
    } else {
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".into());
        format!("{cwd}/{file_path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

fn metadata_mtime_f64(metadata: &fs::Metadata) -> f64 {
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64() * 1000.0,
        Err(error) => -(error.duration().as_secs_f64() * 1000.0),
    }
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn metadata_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::line_delta;

    #[test]
    fn line_delta_counts_duplicate_lines() {
        assert_eq!(line_delta("a\na\nb\n", "a\nb\nb\n"), (1, 1));
        assert_eq!(line_delta("a\n", "a\n"), (0, 0));
    }

    #[test]
    fn line_delta_is_linear_on_ten_mib() {
        // A default-sized (10 MiB) log with a distinct line per row: the old
        // Vec + iter().find version was O(lines * unique) and hung here.
        let before: String = (0..655_360)
            .map(|index| format!("line-{index:010}\n"))
            .collect();
        assert_eq!(before.len(), 10 * 1024 * 1024);
        let after = format!("{before}tail\n");
        assert_eq!(line_delta(&before, &after), (1, 0));
        assert_eq!(line_delta(&after, &before), (0, 1));
    }
}
