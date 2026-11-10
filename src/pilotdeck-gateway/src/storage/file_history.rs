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
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_MAX_SNAPSHOTS: usize = 100;

pub type FileHistoryClock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type SnapshotRecorded = Arc<dyn Fn(JsObject, &str) -> FileHistoryResult<()> + Send + Sync>;
pub type FileHistoryWarn = Arc<dyn Fn(&str) + Send + Sync>;

/// `backupFileName` after replay. The transcript is untrusted: the key can be
/// `null` (file absent at snapshot time), absent (`undefined`), a string, or
/// a non-string value from an untrusted transcript. TS keeps the raw value:
/// `getDiffStats` checks JS truthiness before `path.join`, `restoreBackup`
/// only special-cases `=== null`, and `path.join` then throws a TypeError
/// whose text depends on the value's type (`Received type number (5)`,
/// `Received an instance of Object`, ...). Keeping those states apart is
/// required to reproduce the same error text instead of silently unlinking
/// the tracked path or reporting `undefined`.
#[derive(Clone, Debug, PartialEq)]
pub enum BackupFileName {
    Null,
    Undefined,
    Named(String),
    /// A JSON value that is neither a string nor `null`/`undefined`.
    Other(JsValue),
}
impl BackupFileName {
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Named(name) => Some(name),
            Self::Null | Self::Undefined | Self::Other(_) => None,
        }
    }
    /// JS truthiness of `backup.backupFileName`, which gates `readBackupText`
    /// in `getDiffStats` (an empty string, `0`, `false`, and `NaN` are all
    /// falsy, so they preview as "no backup file").
    fn is_truthy(&self) -> bool {
        match self {
            Self::Null | Self::Undefined => false,
            Self::Named(name) => !name.is_empty(),
            Self::Other(value) => match value {
                JsValue::Bool(value) => *value,
                JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
                JsValue::String(value) => !value.units().is_empty(),
                JsValue::Object(_) | JsValue::Array(_) => true,
                JsValue::Null | JsValue::Undefined => false,
            },
        }
    }
    /// `path.join(backupDir, value)`'s TypeError message (Node 22). `None`
    /// means the value is a string and `path.join` would accept it.
    fn path_join_error(&self) -> Option<String> {
        let received = match self {
            Self::Named(_) => return None,
            Self::Null => "null",
            Self::Undefined => "undefined",
            Self::Other(JsValue::Bool(value)) => {
                return Some(path_argument_error(&format!("type boolean ({value})")));
            }
            Self::Other(JsValue::Number(value)) => {
                return Some(path_argument_error(&format!(
                    "type number ({})",
                    crate::js_value::number_string(*value)
                )));
            }
            Self::Other(JsValue::String(_)) => return None,
            Self::Other(JsValue::Array(_)) => "an instance of Array",
            Self::Other(JsValue::Object(_)) => "an instance of Object",
            Self::Other(JsValue::Null) => "null",
            Self::Other(JsValue::Undefined) => "undefined",
        };
        Some(path_argument_error(received))
    }
    fn display(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Undefined => "undefined".into(),
            Self::Named(name) => name.clone(),
            Self::Other(value) => {
                crate::js_value::display(value).unwrap_or_else(|_| "undefined".into())
            }
        }
    }
}

fn path_argument_error(received: &str) -> String {
    format!("The \"path\" argument must be of type string. Received {received}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileHistoryBackup {
    pub backup_file_name: BackupFileName,
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
    /// `None` is the TS `messageId: undefined` replay case: the snapshot is
    /// still pushed (so `snapshots.length` is non-zero), but no string lookup
    /// can ever match it, exactly like `findSnapshot`'s strict equality.
    pub message_id: Option<String>,
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
    pub backup_file_name: BackupFileName,
    pub version: u64,
    pub backup_time_ms: i64,
    pub mode: Option<u32>,
}
#[derive(Clone, Debug)]
pub struct SnapshotRecordedEntry {
    /// `messageId` may be absent in a corrupt transcript. TS keeps
    /// `undefined` and the strict comparison later never matches; `None`
    /// reproduces that instead of inventing an error.
    pub message_id: Option<String>,
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
    /// Already-rendered Node/libuv fs diagnostic. The SDK error field carries
    /// `Error.message`, so a Rust `io::Error` Display (`...(os error N)`) must
    /// never reach it.
    Fs(String),
}
impl fmt::Display for FileHistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::NoSnapshot(id) => write!(f, "No snapshot for messageId {id}"),
            Self::Fs(message) => f.write_str(message),
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

/// Native backup access remains beneath a pinned, already-authorized chat dir.
/// Open the configured home as the trust anchor, then walk every chat path
/// component without following links. The configured home itself may be a
/// symlink; canonicalize it alone, never the untrusted descendant.
#[cfg(unix)]
pub(crate) fn open_trusted_chat_dir(home: &Path, chat_dir: &Path) -> io::Result<fs::File> {
    let relative = chat_dir
        .strip_prefix(home)
        .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    let trusted_home = fs::canonicalize(home)?;
    crate::project_files::open_directory_beneath(&trusted_home, &trusted_home.join(relative))
}
#[cfg(not(unix))]
pub(crate) fn open_trusted_chat_dir(_: &Path, _: &Path) -> io::Result<fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure file history is unavailable",
    ))
}

/// Derive a fork chat directory only beneath its parent's pinned store root.
#[cfg(unix)]
pub(crate) fn open_inherited_chat_dir(
    root: &fs::File,
    root_path: &Path,
    child: &Path,
) -> io::Result<fs::File> {
    use rustix::fs::{Mode, OFlags, openat};
    let relative = child
        .strip_prefix(root_path)
        .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    crate::project_files::create_directories_at(root, relative, Mode::from_raw_mode(0o700))?;
    let mut current = root.try_clone()?;
    for part in relative.components() {
        let std::path::Component::Normal(name) = part else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        current = fs::File::from(
            openat(
                &current,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
    }
    Ok(current)
}
#[cfg(not(unix))]
pub(crate) fn open_inherited_chat_dir(_: &fs::File, _: &Path, _: &Path) -> io::Result<fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure file history is unavailable",
    ))
}

struct SecureBackup {
    root: Arc<fs::File>,
    relative: PathBuf,
}
impl SecureBackup {
    #[cfg(unix)]
    fn directory(&self, create: bool) -> io::Result<fs::File> {
        use rustix::fs::{Mode, OFlags, openat};
        if create {
            crate::project_files::create_directories_at(
                &self.root,
                &self.relative,
                Mode::from_raw_mode(0o700),
            )?;
        }
        let mut current = self.root.try_clone()?;
        for component in self.relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            };
            let fd = openat(
                &current,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?;
            current = fs::File::from(fd);
        }
        Ok(current)
    }
    #[cfg(not(unix))]
    fn directory(&self, _: bool) -> io::Result<fs::File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure file history is unavailable",
        ))
    }
    #[cfg(unix)]
    fn open_backup(&self, name: &str, write: bool) -> io::Result<fs::File> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        let mut components = Path::new(name).components();
        let Some(std::path::Component::Normal(leaf)) = components.next() else {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        };
        if components.next().is_some() {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        let directory = self.directory(write)?;
        let access = if write {
            OFlags::WRONLY | OFlags::CREATE
        } else {
            OFlags::RDONLY
        };
        let fd = openat(
            &directory,
            leaf,
            access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)?;
        let file = fs::File::from(fd);
        let stat = fstat(&file).map_err(io::Error::from)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    fn open_backup(&self, _: &str, _: bool) -> io::Result<fs::File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure file history is unavailable",
        ))
    }
    fn write_backup(&self, source: &Path, name: &str, mode: u32) -> FileHistoryResult<()> {
        let mut target = self.open_backup(name, true)?;
        target.set_len(0)?;
        io::copy(&mut fs::File::open(source)?, &mut target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            target.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
        }
        Ok(())
    }
    fn read_text(&self, name: &str) -> FileHistoryResult<Option<String>> {
        let mut file = match self.open_backup(name, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
    #[cfg(unix)]
    fn remove(&self, name: &str) -> io::Result<()> {
        use rustix::fs::{AtFlags, unlinkat};
        let mut components = Path::new(name).components();
        let Some(std::path::Component::Normal(leaf)) = components.next() else {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        };
        if components.next().is_some() {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        unlinkat(&self.directory(false)?, leaf, AtFlags::empty()).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    fn remove(&self, _: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure file history is unavailable",
        ))
    }
}

/// Native rewind authority: project root is pinned before any transcript path
/// is inspected. Session transcript paths are data, never authority.
struct SecureTarget {
    root: Arc<fs::File>,
    project: PathBuf,
    canonical_project: PathBuf,
}
impl SecureTarget {
    #[cfg(unix)]
    fn new(project: &Path) -> io::Result<Self> {
        let canonical = fs::canonicalize(project)?;
        let root = crate::project_files::open_directory_beneath(&canonical, &canonical)?;
        Ok(Self {
            root: Arc::new(root),
            project: project.to_path_buf(),
            canonical_project: canonical,
        })
    }
    #[cfg(not(unix))]
    fn new(_: &Path) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure rewind is unavailable",
        ))
    }
    fn relative(&self, path: &str) -> io::Result<PathBuf> {
        let rel = Path::new(path)
            .strip_prefix(&self.project)
            .or_else(|_| Path::new(path).strip_prefix(&self.canonical_project))
            .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
        if rel.as_os_str().is_empty()
            || rel
                .components()
                .any(|v| !matches!(v, std::path::Component::Normal(_)))
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(rel.to_path_buf())
    }
    #[cfg(not(unix))]
    fn validate(&self, _: &str) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(not(unix))]
    fn bytes(&self, _: &str) -> io::Result<Option<(Vec<u8>, u32)>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(not(unix))]
    fn delete(&self, _: &str) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(not(unix))]
    fn restore(&self, _: &str, _: &mut fs::File, _: Option<u32>) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    #[cfg(unix)]
    fn parent(&self, rel: &Path, create: bool) -> io::Result<fs::File> {
        use rustix::fs::{Mode, OFlags, openat};
        let path = rel.parent().ok_or(io::ErrorKind::InvalidInput)?;
        if create {
            crate::project_files::create_directories_at(
                &self.root,
                path,
                Mode::from_raw_mode(0o700),
            )?;
        }
        let mut dir = self.root.try_clone()?;
        for part in path.components() {
            let std::path::Component::Normal(name) = part else {
                return Err(io::ErrorKind::PermissionDenied.into());
            };
            dir = fs::File::from(
                openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?,
            );
        }
        Ok(dir)
    }
    #[cfg(unix)]
    fn open_checked(&self, rel: &Path) -> io::Result<Option<fs::File>> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        let parent = match self.parent(rel, false) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let leaf = rel.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let fd = match openat(
            &parent,
            leaf,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(f) => f,
            Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file = fs::File::from(fd);
        let st = fstat(&file).map_err(io::Error::from)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile || st.st_nlink != 1 {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(Some(file))
    }
    #[cfg(unix)]
    fn validate(&self, path: &str) -> io::Result<()> {
        let rel = self.relative(path)?;
        self.open_checked(&rel).map(|_| ())
    }
    #[cfg(unix)]
    fn bytes(&self, path: &str) -> io::Result<Option<(Vec<u8>, u32)>> {
        let rel = self.relative(path)?;
        let Some(mut f) = self.open_checked(&rel)? else {
            return Ok(None);
        };
        let mode = metadata_mode(&f.metadata()?) & 0o777;
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        Ok(Some((data, mode)))
    }
    #[cfg(unix)]
    fn delete(&self, path: &str) -> io::Result<()> {
        let rel = self.relative(path)?;
        if self.open_checked(&rel)?.is_none() {
            return Ok(());
        }
        crate::project_files::unlink_at(&self.root, &rel)
    }
    #[cfg(unix)]
    fn restore(&self, path: &str, source: &mut fs::File, mode: Option<u32>) -> io::Result<()> {
        self.restore_with_hook(path, source, mode, || {})
    }

    #[cfg(unix)]
    fn restore_with_hook(
        &self,
        path: &str,
        source: &mut fs::File,
        mode: Option<u32>,
        hook: impl FnOnce(),
    ) -> io::Result<()> {
        use rustix::fs::{
            AtFlags, FileType, Mode, OFlags, fstat, openat, renameat, statat, unlinkat,
        };
        use std::io::Write;
        let rel = self.relative(path)?;
        self.open_checked(&rel)?;
        let dir = self.parent(&rel, true)?;
        let leaf = rel.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let name = format!(".pilotdeck-rewind-{}", uuid::Uuid::new_v4());
        let fd = openat(
            &dir,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)?;
        let mut temp = fs::File::from(fd);
        let temp_identity = fstat(&temp).map_err(io::Error::from)?;
        let is_owned_temp = || -> io::Result<bool> {
            match statat(&dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(named) => Ok(
                    FileType::from_raw_mode(named.st_mode) == FileType::RegularFile
                        && temp_identity.st_dev == named.st_dev
                        && temp_identity.st_ino == named.st_ino,
                ),
                Err(rustix::io::Errno::NOENT) => Ok(false),
                Err(error) => Err(error.into()),
            }
        };
        let result = (|| {
            io::copy(source, &mut temp)?;
            temp.flush()?;
            hook();
            let current = self.parent(&rel, false)?;
            let opened = fstat(&dir).map_err(io::Error::from)?;
            let resolved = fstat(&current).map_err(io::Error::from)?;
            if opened.st_dev != resolved.st_dev || opened.st_ino != resolved.st_ino {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "rewind parent directory changed",
                ));
            }
            self.open_checked(&rel)?;
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = mode.unwrap_or(metadata_mode(&source.metadata()?) & 0o777);
                temp.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
            }
            if !is_owned_temp()? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "rewind temporary file changed",
                ));
            }
            renameat(&dir, name.as_str(), &dir, leaf).map_err(io::Error::from)
        })();
        if result.is_err() && is_owned_temp().unwrap_or(false) {
            let _ = unlinkat(&dir, name.as_str(), AtFlags::empty());
        }
        result
    }
}

#[derive(Clone)]
struct HistoryTrustedRoot {
    lexical: PathBuf,
    canonical: PathBuf,
    directory: Arc<fs::File>,
}
impl HistoryTrustedRoot {
    #[cfg(unix)]
    fn open(path: &Path) -> io::Result<Self> {
        let canonical = fs::canonicalize(path)?;
        let directory = crate::project_files::open_directory_beneath(&canonical, &canonical)?;
        Ok(Self {
            lexical: path.to_path_buf(),
            canonical,
            directory: Arc::new(directory),
        })
    }
    #[cfg(not(unix))]
    fn open(_: &Path) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.lexical)
            .or_else(|_| path.strip_prefix(&self.canonical))
            .ok()
    }
}
pub(crate) struct AuthorizedHistorySource {
    parent: fs::File,
    relative: PathBuf,
    path: PathBuf,
    pinned: Mutex<Option<fs::File>>,
}
impl AuthorizedHistorySource {
    #[cfg(unix)]
    fn from_trusted(path: &Path, root: &HistoryTrustedRoot, create: bool) -> io::Result<Self> {
        use rustix::fs::{Mode, OFlags, openat};
        let relative = root.relative(path).ok_or(io::ErrorKind::PermissionDenied)?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let parent = relative.parent().ok_or(io::ErrorKind::InvalidInput)?;
        if !create {
            return Ok(Self {
                parent: root.directory.try_clone()?,
                relative: relative.to_path_buf(),
                path: path.to_path_buf(),
                pinned: Mutex::new(None),
            });
        }
        if create {
            crate::project_files::create_directories_at(
                &root.directory,
                parent,
                Mode::from_raw_mode(0o777),
            )?;
        }
        let mut directory = root.directory.try_clone()?;
        for part in parent.components() {
            let std::path::Component::Normal(name) = part else {
                return Err(io::ErrorKind::PermissionDenied.into());
            };
            directory = fs::File::from(
                openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?,
            );
        }
        Ok(Self {
            parent: directory,
            relative: PathBuf::from(relative.file_name().ok_or(io::ErrorKind::InvalidInput)?),
            path: path.to_path_buf(),
            pinned: Mutex::new(None),
        })
    }
    #[cfg(not(unix))]
    fn from_trusted(_: &Path, _: &HistoryTrustedRoot, _: bool) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(test)]
    pub(crate) fn open_with_root(path: &Path, root: &Path) -> io::Result<Self> {
        if path.strip_prefix(root).is_ok() {
            match HistoryTrustedRoot::open(root) {
                Ok(trusted) => Self::from_trusted(path, &trusted, true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Self::open(path),
                Err(error) => Err(error),
            }
        } else {
            Self::open(path)
        }
    }
    #[cfg(unix)]
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let leaf = path
            .file_name()
            .ok_or(io::ErrorKind::InvalidInput)?
            .to_owned();
        #[cfg(target_os = "macos")]
        let parent = {
            // macOS system aliases are root-owned and are the only symlink
            // ancestors normalized here. No arbitrary parent is canonicalized.
            use std::os::unix::fs::MetadataExt;
            let mut mapped = parent.to_path_buf();
            for (alias, destination) in [("/tmp", "/private/tmp"), ("/var", "/private/var")] {
                if let Ok(rest) = parent.strip_prefix(alias) {
                    let metadata = fs::symlink_metadata(alias)?;
                    let expected = Path::new(destination).strip_prefix("/").unwrap();
                    if metadata.file_type().is_symlink()
                        && metadata.uid() == 0
                        && fs::read_link(alias)? == expected
                    {
                        mapped = Path::new(destination).join(rest);
                    }
                    break;
                }
            }
            mapped
        };
        #[cfg(not(target_os = "macos"))]
        let parent = parent.to_path_buf();
        if !path.is_absolute()
            || path
                .components()
                .any(|v| matches!(v, std::path::Component::ParentDir))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        crate::project_files::create_directories_beneath(Path::new("/"), &parent)?;
        let parent = crate::project_files::open_directory_beneath(Path::new("/"), &parent)?;
        Ok(Self {
            parent,
            relative: PathBuf::from(leaf),
            path: path.to_path_buf(),
            pinned: Mutex::new(None),
        })
    }
    #[cfg(unix)]
    fn open_file(&self) -> io::Result<Option<fs::File>> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        let pinned = self.pinned.lock().unwrap();
        if let Some(pinned) = pinned.as_ref() {
            use std::io::{Seek, SeekFrom};
            let mut file = pinned.try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            return Ok(Some(file));
        }
        let mut parent = self.parent.try_clone()?;
        for part in self
            .relative
            .parent()
            .ok_or(io::ErrorKind::InvalidInput)?
            .components()
        {
            let std::path::Component::Normal(name) = part else {
                return Err(io::ErrorKind::PermissionDenied.into());
            };
            let fd = match openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            parent = fs::File::from(fd);
        }
        let fd = match openat(
            &parent,
            self.relative
                .file_name()
                .ok_or(io::ErrorKind::InvalidInput)?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file = fs::File::from(fd);
        let stat = fstat(&file).map_err(io::Error::from)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(Some(file))
    }
    pub(crate) fn open_resolved(
        path: &Path,
        resolved: &Path,
        root: Option<&Path>,
    ) -> io::Result<Self> {
        let mut source = match root {
            Some(root) => match HistoryTrustedRoot::open(root) {
                Ok(trusted) => Self::from_trusted(resolved, &trusted, true)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => Self::open(resolved)?,
                Err(error) => return Err(error),
            },
            None => Self::open(resolved)?,
        };
        source.path = path.to_path_buf();
        source.pin_for_write()?;
        Ok(source)
    }
    #[cfg(unix)]
    fn pin_for_write(&mut self) -> io::Result<()> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        let leaf = self
            .relative
            .file_name()
            .ok_or(io::ErrorKind::InvalidInput)?;
        let fd = match openat(
            &self.parent,
            leaf,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let file = fs::File::from(fd);
        if FileType::from_raw_mode(fstat(&file).map_err(io::Error::from)?.st_mode)
            != FileType::RegularFile
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        *self.pinned.get_mut().unwrap() = Some(file);
        Ok(())
    }
    #[cfg(not(unix))]
    fn pin_for_write(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    pub(crate) fn write_checked(
        &self,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<(bool, fs::Metadata)> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        use std::io::{Read, Seek, SeekFrom, Write};
        let leaf = self
            .relative
            .file_name()
            .ok_or(io::ErrorKind::InvalidInput)?;
        // Only the parent pinned by this invocation may resolve the leaf.
        let mut pinned = self.pinned.lock().unwrap();
        let (mut file, created) = if expected.is_some() {
            (
                pinned
                    .as_ref()
                    .ok_or(io::ErrorKind::NotFound)?
                    .try_clone()?,
                false,
            )
        } else {
            let fd = openat(
                &self.parent,
                leaf,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o666),
            )
            .map_err(io::Error::from)?;
            (fs::File::from(fd), true)
        };
        let stat = fstat(&file).map_err(io::Error::from)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if let Some(expected) = expected {
            file.seek(SeekFrom::Start(0))?;
            let mut previous = Vec::new();
            let limit = (expected.len() as u64).saturating_add(1);
            (&mut file).take(limit).read_to_end(&mut previous)?;
            if previous.len() > expected.len() || String::from_utf8_lossy(&previous) != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "File has changed since the last read. Read it again before writing to it.",
                ));
            }
            file.seek(SeekFrom::Start(0))?;
            file.set_len(0)?;
        }
        file.write_all(content.as_bytes())?;
        let metadata = file.metadata()?;
        if created {
            *pinned = Some(file);
        }
        Ok((created, metadata))
    }
    #[cfg(not(unix))]
    pub(crate) fn write_checked(
        &self,
        _: Option<&str>,
        _: &str,
    ) -> io::Result<(bool, fs::Metadata)> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(not(unix))]
    pub(crate) fn open(_: &Path) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(not(unix))]
    fn open_file(&self) -> io::Result<Option<fs::File>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub struct FileHistoryStore {
    options: FileHistoryOptions,
    state: Mutex<FileHistoryState>,
    mtime_cache: Mutex<Vec<(String, Option<f64>)>>,
    secure_backup: Option<SecureBackup>,
    secure_target: Option<SecureTarget>,
}

impl FileHistoryStore {
    pub fn new(options: FileHistoryOptions) -> Self {
        Self {
            options,
            state: Mutex::new(FileHistoryState::default()),
            mtime_cache: Mutex::new(Vec::new()),
            secure_backup: None,
            secure_target: None,
        }
    }
    /// Native-only: pin the trusted chat directory and resolve every backup
    /// component beneath it without following symlinks.
    pub(crate) fn with_project_target(mut self, project: &Path) -> io::Result<Self> {
        self.secure_target = Some(SecureTarget::new(project)?);
        Ok(self)
    }

    pub(crate) fn with_trusted_root(mut self, root: fs::File, relative: PathBuf) -> Self {
        self.secure_backup = Some(SecureBackup {
            root: Arc::new(root),
            relative,
        });
        self
    }
    pub(crate) fn requires_authorized_source(&self) -> bool {
        self.secure_backup.is_some()
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
        self.track_edit_inner(file_path, message_id, None)
    }

    pub(crate) fn track_edit_authorized(
        &self,
        target: &AuthorizedHistorySource,
        message_id: &str,
    ) -> FileHistoryResult<()> {
        let path = target.path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "history target is not UTF-8")
        })?;
        self.track_edit_native(target, path, message_id)
    }

    // Prepare the backup and transcript against a private state copy. A failing
    // source or sink must not leave a partially registered native edit in memory.
    fn track_edit_native(
        &self,
        target: &AuthorizedHistorySource,
        file_path: &str,
        message_id: &str,
    ) -> FileHistoryResult<()> {
        let mut guard = self.state.lock().unwrap();
        let mut state = guard.clone();
        let abs_path = resolve_node_path(file_path)?;
        let existing = find_snapshot(&state.snapshots, message_id);
        if let Some(index) = existing
            && state.snapshots[index].backup(&abs_path).is_some()
        {
            if !state.tracked_files.contains(&abs_path) {
                state.tracked_files.push(abs_path);
                *guard = state;
            }
            return Ok(());
        }
        let source = target.open_file()?;
        let mtime = source
            .as_ref()
            .map(|file| file.metadata())
            .transpose()?
            .map(|metadata| metadata_mtime_f64(&metadata));
        let secure = self.secure_backup.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "history backup root is missing",
            )
        })?;
        let highest = state
            .snapshots
            .iter()
            .filter_map(|snapshot| snapshot.backup(&abs_path))
            .map(|backup| backup.version)
            .max()
            .unwrap_or(0);
        let version = highest.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "file-history version exhausted")
        })?;
        let result = create_native_backup(
            secure,
            Some(source),
            &abs_path,
            version,
            self.max_file_bytes(),
            &|| self.now(),
        )?;
        if result.oversize {
            self.warn(format!(
                "file-history: skipping backup for {abs_path} (size > {} bytes)",
                self.max_file_bytes()
            ));
        }
        if !state.tracked_files.contains(&abs_path) {
            state.tracked_files.push(abs_path.clone());
        }
        let index = match existing {
            Some(index) => index,
            None => {
                state.snapshots.push(FileHistorySnapshot {
                    message_id: Some(message_id.to_string()),
                    tracked_file_backups: Vec::new(),
                    expected_file_states: Vec::new(),
                    timestamp_ms: self.now(),
                });
                state.snapshots.len() - 1
            }
        };
        state.snapshots[index].set_backup(abs_path.clone(), result.backup);
        self.record_transcript(&state.snapshots[index], "update")?;
        let mut cache = self.mtime_cache.lock().unwrap();
        if let Some(entry) = cache.iter_mut().find(|(key, _)| key == &abs_path) {
            entry.1 = mtime;
        } else {
            cache.push((abs_path, mtime));
        }
        *guard = state;
        Ok(())
    }

    fn track_edit_inner(
        &self,
        file_path: &str,
        message_id: &str,
        target: Option<&AuthorizedHistorySource>,
    ) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path)?;
        if !state.tracked_files.contains(&abs_path) {
            state.tracked_files.push(abs_path.clone());
        }
        let snapshot_index = match find_snapshot(&state.snapshots, message_id) {
            Some(index) => index,
            None => {
                let timestamp = self.now();
                state.snapshots.push(FileHistorySnapshot {
                    message_id: Some(message_id.to_string()),
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
        let (source_override, source_mtime) = match target {
            Some(target) => {
                let source = target.open_file()?;
                let mtime = source
                    .as_ref()
                    .map(|file| file.metadata())
                    .transpose()?
                    .map(|metadata| metadata_mtime_f64(&metadata));
                (Some(source), Some(mtime))
            }
            None => (None, None),
        };
        let clock = || self.now();
        let result = if let Some(secure) = &self.secure_backup {
            let highest = state
                .snapshots
                .iter()
                .filter_map(|snapshot| snapshot.backup(&abs_path))
                .map(|backup| backup.version)
                .max()
                .unwrap_or(0);
            let version = highest.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "file-history version exhausted")
            })?;
            create_native_backup(
                secure,
                source_override,
                &abs_path,
                version,
                self.max_file_bytes(),
                &clock,
            )?
        } else {
            if target.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "history backup root is missing",
                )
                .into());
            }
            create_backup(
                &self.options.backup_dir,
                None,
                &abs_path,
                1,
                self.max_file_bytes(),
                &clock,
            )?
        };
        if result.oversize {
            self.warn(format!(
                "file-history: skipping backup for {abs_path} (size > {} bytes)",
                self.max_file_bytes()
            ));
        }
        let backup = result.backup;
        state.snapshots[snapshot_index].set_backup(abs_path.clone(), backup);
        if let Some(mtime) = source_mtime {
            let mut cache = self.mtime_cache.lock().unwrap();
            if let Some(entry) = cache.iter_mut().find(|(key, _)| key == &abs_path) {
                entry.1 = mtime;
            } else {
                cache.push((abs_path.clone(), mtime));
            }
        } else {
            self.cache_mtime(&abs_path);
        }
        let snapshot = state.snapshots[snapshot_index].clone();
        // TS `run()` holds the mutex across `recordTranscript`; keeping the
        // state guard here makes the sink see the committed transaction and
        // prevents an interleaved clone from writing an older snapshot last.
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
                message_id: Some(message_id.to_string()),
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
            let result = if let Some(secure) = &self.secure_backup {
                let highest = state
                    .snapshots
                    .iter()
                    .filter_map(|item| item.backup(&abs_path))
                    .map(|backup| backup.version)
                    .max()
                    .unwrap_or(0)
                    .max(previous_version);
                let version = highest.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "file-history version exhausted")
                })?;
                create_native_backup(
                    secure,
                    None,
                    &abs_path,
                    version,
                    self.max_file_bytes(),
                    &clock,
                )?
            } else {
                create_backup(
                    &self.options.backup_dir,
                    None,
                    &abs_path,
                    previous_version + 1,
                    self.max_file_bytes(),
                    &clock,
                )?
            };
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
        self.mark_edit_committed_inner(file_path, message_id, None)
    }
    pub(crate) fn mark_edit_committed_authorized(
        &self,
        target: &AuthorizedHistorySource,
        message_id: &str,
    ) -> FileHistoryResult<()> {
        let path = target.path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "history target is not UTF-8")
        })?;
        self.mark_edit_committed_inner(path, message_id, Some(target))
    }
    fn mark_edit_committed_inner(
        &self,
        file_path: &str,
        message_id: &str,
        target: Option<&AuthorizedHistorySource>,
    ) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path)?;
        let Some(index) = find_snapshot(&state.snapshots, message_id) else {
            return Ok(());
        };
        if state.snapshots[index].backup(&abs_path).is_none() {
            return Ok(());
        }
        let expected = if let Some(target) = target {
            read_expected_authorized(target)?
        } else {
            read_expected_file_state(&abs_path)?
        };
        state.snapshots[index].set_expected(abs_path, expected);
        let snapshot = state.snapshots[index].clone();
        // Same TS `run()` lock domain as `track_edit`.
        self.record_transcript(&snapshot, "update")?;
        Ok(())
    }

    pub fn get_conflict_paths(&self, message_id: &str) -> FileHistoryResult<Vec<String>> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id.as_deref() == Some(message_id))
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?;
        find_conflict_paths(snapshot, self.secure_target.as_ref())
    }

    /// F8/F9 — restore every backup of the last snapshot for `messageId`.
    /// TS wraps conflict detection and every restore in one `run()` task, so
    /// the state guard stays alive through the filesystem work.
    pub fn rewind(&self, message_id: &str) -> FileHistoryResult<RewindOutcome> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id.as_deref() == Some(message_id))
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?
            .clone();
        if let Some(backup_root) = &self.secure_backup {
            for (_, backup) in &snapshot.tracked_file_backups {
                if let BackupFileName::Named(name) = &backup.backup_file_name {
                    match backup_root.open_backup(name, false) {
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        let conflicts = find_conflict_paths(&snapshot, self.secure_target.as_ref())?;
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
            match restore_backup(
                &self.options.backup_dir,
                self.secure_backup.as_ref(),
                self.secure_target.as_ref(),
                abs_path,
                backup,
            )? {
                RestoreOutcome::Restored | RestoreOutcome::Deleted => {
                    files_changed.push(abs_path.clone());
                }
                RestoreOutcome::Missing => {
                    missing.push(abs_path.clone());
                    self.warn(format!(
                        "file-history: backup {} for {abs_path} is missing on disk; skipping",
                        backup.backup_file_name.display()
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
    /// Also inside the TS `run()` mutex (the snapshot must not move while the
    /// "after" side is being read).
    pub fn get_diff_stats(&self, message_id: &str) -> FileHistoryResult<DiffStats> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.message_id.as_deref() == Some(message_id))
            .ok_or_else(|| FileHistoryError::NoSnapshot(message_id.to_string()))?
            .clone();
        if let Some(target) = &self.secure_target {
            for (path, _) in &snapshot.tracked_file_backups {
                target.validate(path)?;
            }
            for (path, _) in &snapshot.expected_file_states {
                target.validate(path)?;
            }
        }
        let mut insertions = 0usize;
        let mut deletions = 0usize;
        let mut files_changed = 0usize;
        for (abs_path, backup) in &snapshot.tracked_file_backups {
            // TS `backup.backupFileName ? readBackupText(...) : null`. Falsy
            // values (`null`, `undefined`, "", 0, false, NaN) skip the read;
            // a truthy non-string reaches `path.join` and throws its TypeError.
            let before = if backup.backup_file_name.is_truthy() {
                match &backup.backup_file_name {
                    BackupFileName::Named(name) => {
                        if let Some(secure) = &self.secure_backup {
                            secure.read_text(name)?
                        } else {
                            safe_read_text(&self.options.backup_dir.join(name))?
                        }
                    }
                    other => {
                        let message = other
                            .path_join_error()
                            .ok_or_else(|| FileHistoryError::Fs(PATH_JOIN_UNDEFINED.to_string()))?;
                        return Err(FileHistoryError::Fs(message));
                    }
                }
            } else {
                None
            };
            let after = if let Some(target) = &self.secure_target {
                target
                    .bytes(abs_path)?
                    .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
            } else {
                safe_read_text(Path::new(abs_path))?
            };
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
            match find_snapshot_option(&state.snapshots, entry.message_id.as_deref()) {
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
                    BackupFileName::Named(name) => JsValue::from(name.as_str()),
                    BackupFileName::Null => JsValue::Null,
                    // `JSON.stringify({backupFileName: undefined})` drops the
                    // key, exactly like the TS writer.
                    BackupFileName::Undefined => JsValue::Undefined,
                    // Replayed corrupt values keep their original JSON shape.
                    BackupFileName::Other(value) => value.clone(),
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
        if let Some(message_id) = &snapshot.message_id {
            // `JSON.stringify({messageId: undefined})` drops the key, so a
            // replayed snapshot with no id is not re-emitted either.
            entry.insert("messageId".into(), JsValue::from(message_id.as_str()));
        }
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
                // TS `if (!backup.backupFileName) continue;` also skips the
                // empty string, not just null/undefined.
                let Some(name) = backup.backup_file_name.as_name() else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let still_referenced = state.snapshots.iter().any(|snapshot| {
                    snapshot
                        .tracked_file_backups
                        .iter()
                        .any(|(_, backup)| backup.backup_file_name.as_name() == Some(name))
                });
                if still_referenced {
                    continue;
                }
                if let Err(error) = if let Some(secure) = &self.secure_backup {
                    secure.remove(name)
                } else {
                    fs::remove_file(self.options.backup_dir.join(name))
                } && error.kind() != io::ErrorKind::NotFound
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

/// Reserve a distinct native backup without ever truncating a referenced or
/// orphaned name. Existing transcript formats and generic TS-facing behavior
/// remain unchanged.
#[cfg(unix)]
fn create_native_backup(
    secure: &SecureBackup,
    source_override: Option<Option<fs::File>>,
    file_path: &str,
    mut version: u64,
    max_file_bytes: u64,
    now: &dyn Fn() -> i64,
) -> FileHistoryResult<BackupResult> {
    use rustix::fs::{AtFlags, Mode, OFlags, fstat, openat, statat, unlinkat};
    const MAX_SAFE_JSON_VERSION: u64 = 9_007_199_254_740_991;
    if version > MAX_SAFE_JSON_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file-history version exceeds JSON safe integer",
        )
        .into());
    }
    let (metadata, mut source) = match source_override {
        Some(Some(file)) => (file.metadata()?, file),
        Some(None) => return Ok(null_backup(version, now(), false)),
        None => {
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
            let file = fs::File::open(file_path)?;
            (metadata, file)
        }
    };
    if !metadata.is_file() {
        return Ok(null_backup(version, now(), false));
    }
    if metadata.len() > max_file_bytes {
        return Ok(null_backup(version, now(), true));
    }
    let dir = secure.directory(true)?;
    // Bound malicious/corrupt directories rather than iterating forever.
    for _ in 0..10_000 {
        // Transcript versions are serialized as JSON Number/f64. Never
        // allocate a version that cannot be represented exactly on replay.
        if version > MAX_SAFE_JSON_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file-history version exceeds JSON safe integer",
            )
            .into());
        }
        let name = get_backup_file_name(file_path, version);
        let fd = match openat(
            &dir,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::EXIST) => {
                version = version.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "file-history version exhausted")
                })?;
                continue;
            }
            Err(error) => return Err(io::Error::from(error).into()),
        };
        let mut backup = fs::File::from(fd);
        let identity = fstat(&backup).map_err(io::Error::from)?;
        let result = (|| -> io::Result<u64> {
            let copied = io::copy(
                &mut (&mut source).take(max_file_bytes.saturating_add(1)),
                &mut backup,
            )?;
            if copied > max_file_bytes {
                return Ok(copied);
            }
            use std::os::unix::fs::PermissionsExt;
            backup.set_permissions(fs::Permissions::from_mode(metadata_mode(&metadata) & 0o777))?;
            Ok(copied)
        })();
        match result {
            Ok(copied) if copied <= max_file_bytes => {
                return Ok(BackupResult {
                    backup: FileHistoryBackup {
                        backup_file_name: BackupFileName::Named(name),
                        version,
                        backup_time_ms: now(),
                        mode: Some(metadata_mode(&metadata)),
                    },
                    oversize: false,
                });
            }
            other => {
                // A same-UID replacement after this comparison remains part
                // of the project descriptor threat-model boundary.
                if let Ok(named) = statat(&dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                    && named.st_dev == identity.st_dev
                    && named.st_ino == identity.st_ino
                {
                    let _ = unlinkat(&dir, name.as_str(), AtFlags::empty());
                }
                return match other {
                    Ok(_) => Ok(null_backup(version, now(), true)),
                    Err(error) => Err(error.into()),
                };
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "file-history could not reserve a unique backup name",
    )
    .into())
}
#[cfg(not(unix))]
fn create_native_backup(
    _: &SecureBackup,
    _: Option<Option<fs::File>>,
    _: &str,
    _: u64,
    _: u64,
    _: &dyn Fn() -> i64,
) -> FileHistoryResult<BackupResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure file history is unavailable",
    )
    .into())
}

fn create_backup(
    backup_dir: &Path,
    secure: Option<&SecureBackup>,
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
    let backup_path = backup_dir.join(&backup_file_name);
    if let Some(secure) = secure {
        secure.write_backup(
            Path::new(file_path),
            &backup_file_name,
            metadata_mode(&metadata) & 0o777,
        )?;
    } else {
        fs::create_dir_all(backup_dir)?;
        write_private_backup(
            Path::new(file_path),
            &backup_path,
            metadata_mode(&metadata) & 0o777,
        )?;
    }
    Ok(BackupResult {
        backup: FileHistoryBackup {
            backup_file_name: BackupFileName::Named(backup_file_name),
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
            backup_file_name: BackupFileName::Null,
            version,
            backup_time_ms: backup_time,
            mode: None,
        },
        oversize,
    }
}

#[derive(Debug)]
enum RestoreOutcome {
    Restored,
    Deleted,
    Missing,
}

/// Node/libuv `Error.message` for one-path syscalls (`stat`, `access`,
/// `mkdir`, `chmod`, `unlink`). Rewind returns `error.message` verbatim, so a
/// Rust `io::Error` Display with its `(os error N)` suffix must not leak.
fn node_fs_error(error: io::Error, syscall: &str, path: &Path) -> FileHistoryError {
    match crate::node_io::diagnostic(&error, syscall, Some(&path.to_string_lossy())) {
        Some((_, message)) => FileHistoryError::Fs(message),
        // Unmeasured errno: keep a reason but drop the Rust-specific suffix.
        // Ledger: gateway accepted-diffs A6.6-FILEHISTORY-IO-001 (unapproved).
        None => FileHistoryError::Fs(without_os_error(&error)),
    }
}

/// `fs.readFile` has Node's special EISDIR rendering (`..., read`, no path).
fn node_read_error(error: io::Error, path: &str) -> FileHistoryError {
    match crate::node_io::read_file(&error, path) {
        Some((_, message)) => FileHistoryError::Fs(message),
        None => FileHistoryError::Fs(without_os_error(&error)),
    }
}

/// Two-path syscalls (`copyfile`, `rename`): `..., syscall 'from' -> 'to'`.
fn node_two_path_error(
    error: io::Error,
    syscall: &str,
    from: &Path,
    to: &Path,
) -> FileHistoryError {
    let message = match crate::node_io::lookup(&error) {
        Some((code, description)) => format!(
            "{code}: {description}, {syscall} '{}' -> '{}'",
            from.display(),
            to.display()
        ),
        None => format!(
            "{}, {syscall} '{}' -> '{}'",
            without_os_error(&error),
            from.display(),
            to.display()
        ),
    };
    FileHistoryError::Fs(message)
}

fn without_os_error(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(index) if text.ends_with(')') => text[..index].to_string(),
        _ => text,
    }
}

/// Node 22 `path.join(backupDir, undefined)` error text. TS `restoreBackup`
/// only special-cases `=== null`; an absent `backupFileName` (corrupt
/// transcript or a string/array `trackedFileBackups` enumeration) reaches
/// `path.join` and throws before any filesystem write.
const PATH_JOIN_UNDEFINED: &str =
    "The \"path\" argument must be of type string. Received undefined";

fn restore_backup(
    backup_dir: &Path,
    secure: Option<&SecureBackup>,
    target_guard: Option<&SecureTarget>,
    file_path: &str,
    backup: &FileHistoryBackup,
) -> FileHistoryResult<RestoreOutcome> {
    let backup_file_name = match &backup.backup_file_name {
        BackupFileName::Null => {
            if let Some(target) = target_guard {
                target.delete(file_path)?;
                return Ok(RestoreOutcome::Deleted);
            }
            match fs::remove_file(file_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(node_fs_error(error, "unlink", Path::new(file_path))),
            }
            return Ok(RestoreOutcome::Deleted);
        }
        BackupFileName::Named(name) => name,
        // `restoreBackup` does not test truthiness: only `=== null` unlinks,
        // so even a falsy non-string (`0`, `false`, "") reaches `path.join`.
        other => {
            return Err(FileHistoryError::Fs(
                other
                    .path_join_error()
                    .unwrap_or_else(|| PATH_JOIN_UNDEFINED.to_string()),
            ));
        }
    };
    let backup_path = backup_dir.join(backup_file_name);
    let mut secured_source = if let Some(secure) = secure {
        match secure.open_backup(backup_file_name, false) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RestoreOutcome::Missing);
            }
            Err(error) => return Err(node_fs_error(error, "access", &backup_path)),
        }
    } else {
        None
    };
    match if secured_source.is_some() {
        Ok(())
    } else {
        fs::metadata(&backup_path).map(|_| ())
    } {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RestoreOutcome::Missing);
        }
        // TS probes the backup with `fs.access`.
        Err(error) => return Err(node_fs_error(error, "access", &backup_path)),
    }
    if let Some(target) = target_guard {
        let source = secured_source
            .as_mut()
            .ok_or_else(|| FileHistoryError::Fs("Secure backup source missing.".into()))?;
        target.restore(file_path, source, backup.mode)?;
        return Ok(RestoreOutcome::Restored);
    }
    let target = Path::new(file_path);
    let target_dir = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(target_dir).map_err(|error| node_fs_error(error, "mkdir", target_dir))?;
    let base = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temporary = target_dir.join(format!(".{base}.pilotdeck-rewind-{}", uuid::Uuid::new_v4()));
    let result = (|| -> FileHistoryResult<()> {
        if let Some(source) = secured_source.as_mut() {
            let mut target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| {
                    node_two_path_error(error, "copyfile", &backup_path, &temporary)
                })?;
            io::copy(source, &mut target).map_err(|error| {
                node_two_path_error(error, "copyfile", &backup_path, &temporary)
            })?;
        } else {
            fs::copy(&backup_path, &temporary).map_err(|error| {
                node_two_path_error(error, "copyfile", &backup_path, &temporary)
            })?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = backup.mode {
                fs::set_permissions(&temporary, fs::Permissions::from_mode(mode & 0o777))
                    .map_err(|error| node_fs_error(error, "chmod", &temporary))?;
            }
        }
        fs::rename(&temporary, target)
            .map_err(|error| node_two_path_error(error, "rename", &temporary, target))?;
        Ok(())
    })();
    // `rename` removes the temporary name on success; a failed copy/rename must
    // not leave a retry-visible partial artifact (F9). TS's `finally` lets a
    // non-ENOENT cleanup error override the copy/rename error, so do the same.
    let cleanup = fs::remove_file(&temporary);
    match (result, cleanup) {
        (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => {
            Err(node_fs_error(error, "unlink", &temporary))
        }
        (Err(error), _) => Err(error),
        (Ok(()), _) => Ok(RestoreOutcome::Restored),
    }
}

fn read_expected_authorized(
    target: &AuthorizedHistorySource,
) -> FileHistoryResult<ExpectedFileState> {
    let expected = match target.open_file()? {
        None => ExpectedFileState::Missing,
        Some(mut file) => {
            let metadata = file.metadata()?;
            let mut remaining = metadata.len();
            let mut digest = Sha256::new();
            let mut bytes = [0u8; 64 * 1024];
            while remaining > 0 {
                let limit =
                    usize::try_from(remaining.min(bytes.len() as u64)).unwrap_or(bytes.len());
                let count = match file.read(&mut bytes[..limit]) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "history target changed during hashing",
                    )
                    .into());
                }
                digest.update(&bytes[..count]);
                remaining -= count as u64;
            }
            let after = file.metadata()?;
            if metadata.len() != after.len()
                || metadata_mtime_f64(&metadata) != metadata_mtime_f64(&after)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "history target changed during hashing",
                )
                .into());
            }
            ExpectedFileState::Present {
                sha256: format!("{:x}", digest.finalize()),
                mode: Some(metadata_mode(&metadata) & 0o777),
            }
        }
    };
    Ok(expected)
}

fn read_expected_file_state(file_path: &str) -> FileHistoryResult<ExpectedFileState> {
    // TS runs `stat` and `readFile` concurrently and only ENOENT maps to
    // Missing. A directory's stat succeeds but readFile raises EISDIR, so the
    // read must happen even when the entry is not a regular file; a character
    // device can read successfully and still be Missing (`stat.isFile()`).
    let metadata = match fs::metadata(file_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ExpectedFileState::Missing);
        }
        Err(error) => return Err(node_fs_error(error, "stat", Path::new(file_path))),
    };
    let bytes = match fs::read(file_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ExpectedFileState::Missing);
        }
        Err(error) => return Err(node_read_error(error, file_path)),
    };
    if !metadata.is_file() {
        return Ok(ExpectedFileState::Missing);
    }
    Ok(ExpectedFileState::Present {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        mode: Some(metadata_mode(&metadata) & 0o777),
    })
}

fn find_conflict_paths(
    snapshot: &FileHistorySnapshot,
    target: Option<&SecureTarget>,
) -> FileHistoryResult<Vec<String>> {
    if let Some(target) = target {
        // Validate the complete set before any mutation or reading contents.
        for (path, _) in &snapshot.tracked_file_backups {
            target.validate(path)?;
        }
        for (path, _) in &snapshot.expected_file_states {
            target.validate(path)?;
        }
    }
    let mut conflicts = Vec::new();
    for (path, expected) in &snapshot.expected_file_states {
        let actual = if let Some(target) = target {
            match target.bytes(path)? {
                None => ExpectedFileState::Missing,
                Some((bytes, mode)) => ExpectedFileState::Present {
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    mode: Some(mode),
                },
            }
        } else {
            read_expected_file_state(path)?
        };
        if actual != *expected {
            conflicts.push(path.clone());
        }
    }
    Ok(conflicts)
}

fn find_snapshot(snapshots: &[FileHistorySnapshot], message_id: &str) -> Option<usize> {
    find_snapshot_option(snapshots, Some(message_id))
}

fn find_snapshot_option(
    snapshots: &[FileHistorySnapshot],
    message_id: Option<&str>,
) -> Option<usize> {
    snapshots
        .iter()
        .rposition(|snapshot| snapshot.message_id.as_deref() == message_id)
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
pub fn resolve_node_path(file_path: &str) -> FileHistoryResult<String> {
    let combined = if file_path.starts_with('/') {
        file_path.to_string()
    } else {
        let cwd = std::env::current_dir().map_err(|error| {
            FileHistoryError::Fs(crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1)
        })?;
        format!("{}/{file_path}", cwd.to_string_lossy())
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
    Ok(format!("/{}", parts.join("/")))
}

#[cfg(all(test, unix))]
#[path = "../../tests/fixtures/file-history-cwd/regression.rs"]
mod cwd_tests;

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
    use super::*;
    use std::{
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc::{RecvTimeoutError, channel},
        },
        thread,
        time::Duration,
    };

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("pdrs-file-history-{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    #[test]
    fn read_expected_file_state_renders_node_eisdir_for_a_directory() {
        let root = TempDir::new("eisdir");
        let directory = root.path().join("a.txt");
        fs::create_dir(&directory).unwrap();
        let error = read_expected_file_state(directory.to_str().unwrap()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );
    }

    #[test]
    fn restore_null_backup_on_a_directory_never_leaks_rust_os_error() {
        let root = TempDir::new("unlink-dir");
        let directory = root.path().join("a.txt");
        fs::create_dir(&directory).unwrap();
        let backup = FileHistoryBackup {
            backup_file_name: BackupFileName::Null,
            version: 1,
            backup_time_ms: 0,
            mode: None,
        };
        let error = restore_backup(
            root.path(),
            None,
            None,
            directory.to_str().unwrap(),
            &backup,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("os error"), "{message}");
        // unlink(dir) is EPERM on macOS and EISDIR on Linux; both carry the
        // same Node `unlink '<path>'` tail.
        assert!(
            message.starts_with("EPERM: operation not permitted, unlink ")
                || message.starts_with("EISDIR: illegal operation on a directory, unlink "),
            "{message}"
        );
        assert!(
            message.ends_with(&format!("unlink '{}'", directory.display())),
            "{message}"
        );
    }

    #[test]
    fn restore_mkdir_failure_uses_node_text() {
        let root = TempDir::new("mkdir-file");
        let parent = root.path().join("p");
        fs::write(&parent, "not a directory").unwrap();
        let backup_dir = root.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let backup_name = "0123456789abcdef@v1";
        fs::write(backup_dir.join(backup_name), "old\n").unwrap();
        let backup = FileHistoryBackup {
            backup_file_name: BackupFileName::Named(backup_name.into()),
            version: 1,
            backup_time_ms: 0,
            mode: Some(0o644),
        };
        let target = parent.join("a.txt");
        let error =
            restore_backup(&backup_dir, None, None, target.to_str().unwrap(), &backup).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("EEXIST: file already exists, mkdir '{}'", parent.display())
        );
    }

    #[test]
    fn snapshot_without_message_id_never_matches_a_lookup() {
        let root = TempDir::new("no-message-id");
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.path().join("backups")));
        store.replay_from_transcript(&[SnapshotRecordedEntry {
            message_id: None,
            tracked_file_backups: Vec::new(),
            expected_file_states: Vec::new(),
            timestamp_ms: Some(0),
        }]);
        assert_eq!(
            store.get_conflict_paths("m1").unwrap_err().to_string(),
            "No snapshot for messageId m1"
        );
    }

    /// C1: TS `run()` keeps `recordTranscript` inside the mutex. A second
    /// `trackEdit` must not reach its sink while the first sink is running.
    #[test]
    fn snapshot_sink_runs_inside_the_transaction_lock() {
        let root = TempDir::new("sink-lock");
        let first_file = root.path().join("a.txt");
        let second_file = root.path().join("b.txt");
        fs::write(&first_file, "a\n").unwrap();
        fs::write(&second_file, "b\n").unwrap();
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let block_once = Arc::new(AtomicBool::new(true));
        let store = Arc::new(FileHistoryStore::new(FileHistoryOptions {
            backup_dir: root.path().join("backups"),
            max_file_bytes: None,
            max_snapshots: None,
            now: Some(Arc::new(|| 0)),
            on_snapshot_recorded: Some(Arc::new({
                let block_once = block_once.clone();
                let release_rx = release_rx.clone();
                move |_, _| {
                    if block_once.swap(false, Ordering::SeqCst) {
                        entered_tx.send(()).unwrap();
                        release_rx.lock().unwrap().recv().unwrap();
                    }
                    Ok(())
                }
            })),
            warn: None,
        }));
        let first_store = store.clone();
        let first_path = first_file.to_string_lossy().into_owned();
        let first = thread::spawn(move || first_store.track_edit(&first_path, "m1"));
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first sink entered");
        let (started_tx, started_rx) = channel();
        let (done_tx, done_rx) = channel();
        let second_store = store.clone();
        let second_path = second_file.to_string_lossy().into_owned();
        let second = thread::spawn(move || {
            started_tx.send(()).unwrap();
            second_store.track_edit(&second_path, "m1").unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second thread started");
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(400)),
            Err(RecvTimeoutError::Timeout),
            "track_edit escaped the transaction lock while the sink was running"
        );
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second track_edit completed after release");
        second.join().unwrap();
    }

    /// C2: TS `rewind()` runs conflict detection and every restore inside the
    /// mutex. A concurrent `trackEdit` must stay blocked while a restore warning
    /// is being delivered.
    #[test]
    fn rewind_restore_runs_inside_the_transaction_lock() {
        let root = TempDir::new("rewind-lock");
        let first_file = root.path().join("a.txt");
        let second_file = root.path().join("b.txt");
        fs::write(&first_file, "a\n").unwrap();
        fs::write(&second_file, "b\n").unwrap();
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let block_once = Arc::new(AtomicBool::new(true));
        let store = Arc::new(FileHistoryStore::new(FileHistoryOptions {
            backup_dir: root.path().join("backups"),
            max_file_bytes: None,
            max_snapshots: None,
            now: Some(Arc::new(|| 0)),
            on_snapshot_recorded: None,
            warn: Some(Arc::new({
                let block_once = block_once.clone();
                let release_rx = release_rx.clone();
                move |_| {
                    if block_once.swap(false, Ordering::SeqCst) {
                        entered_tx.send(()).unwrap();
                        release_rx.lock().unwrap().recv().unwrap();
                    }
                }
            })),
        }));
        fs::create_dir_all(root.path().join("backups")).unwrap();
        store.replay_from_transcript(&[SnapshotRecordedEntry {
            message_id: Some("m1".into()),
            tracked_file_backups: vec![(
                first_file.to_string_lossy().into_owned(),
                RecordedBackup {
                    backup_file_name: BackupFileName::Named("0123456789abcdef@v1".into()),
                    version: 1,
                    backup_time_ms: 0,
                    mode: Some(0o644),
                },
            )],
            expected_file_states: Vec::new(),
            timestamp_ms: Some(0),
        }]);
        let first_store = store.clone();
        let first = thread::spawn(move || first_store.rewind("m1"));
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("restore warning entered");
        let (started_tx, started_rx) = channel();
        let (done_tx, done_rx) = channel();
        let second_store = store.clone();
        let second_path = second_file.to_string_lossy().into_owned();
        let second = thread::spawn(move || {
            started_tx.send(()).unwrap();
            second_store.track_edit(&second_path, "m2").unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second thread started");
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(400)),
            Err(RecvTimeoutError::Timeout),
            "rewind released the transaction lock before restoring"
        );
        release_tx.send(()).unwrap();
        let outcome = first.join().unwrap().unwrap();
        assert_eq!(outcome.missing.len(), 1);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second track_edit completed after release");
        second.join().unwrap();
    }
}

#[cfg(all(test, unix))]
mod secure_backup_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("pdrs-file-history-secure-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn descriptor_root_rejects_symlink_and_hardlink_without_changing_victim() {
        let temp = TempDir::new();
        let root_path = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root_path).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim");
        let source = temp.path().join("source");
        fs::write(&victim, b"protected").unwrap();
        fs::write(&source, b"new").unwrap();
        let secure = SecureBackup {
            root: Arc::new(fs::File::open(&root_path).unwrap()),
            relative: PathBuf::from("session/file-history"),
        };
        symlink(&outside, root_path.join("session")).unwrap();
        assert!(secure.write_backup(&source, "victim", 0o600).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"protected");
        fs::remove_file(root_path.join("session")).unwrap();
        fs::create_dir_all(root_path.join("session/file-history")).unwrap();
        let leaf = root_path.join("session/file-history/victim");
        symlink(&victim, &leaf).unwrap();
        assert!(secure.write_backup(&source, "victim", 0o600).is_err());
        assert!(secure.open_backup("victim", false).is_err());
        fs::remove_file(&leaf).unwrap();
        fs::hard_link(&victim, &leaf).unwrap();
        assert!(secure.write_backup(&source, "victim", 0o600).is_err());
        assert!(secure.open_backup("victim", false).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"protected");
    }

    #[test]
    fn native_rewind_preserves_project_alias_and_canonical_paths() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        let alias = temp.path().join("alias");
        fs::create_dir_all(&project).unwrap();
        symlink(&project, &alias).unwrap();
        let canonical = fs::canonicalize(&project).unwrap();
        let alias_history = alias.join("alias-history");
        let canonical_history = canonical.join("canonical-history");
        fs::write(&alias_history, b"first").unwrap();
        fs::write(&canonical_history, b"second").unwrap();
        let backup = FileHistoryBackup {
            backup_file_name: BackupFileName::Null,
            version: 1,
            backup_time_ms: 0,
            mode: None,
        };
        let store = FileHistoryStore::new(FileHistoryOptions::new(temp.path().join("backups")))
            .with_project_target(&alias)
            .unwrap();
        for (id, path) in [
            ("canonical", canonical.join("alias-history")),
            ("alias", alias.join("canonical-history")),
        ] {
            store
                .state
                .lock()
                .unwrap()
                .snapshots
                .push(FileHistorySnapshot {
                    message_id: Some(id.into()),
                    tracked_file_backups: vec![(
                        path.to_string_lossy().into_owned(),
                        backup.clone(),
                    )],
                    expected_file_states: vec![],
                    timestamp_ms: 0,
                });
            assert!(store.rewind(id).unwrap().missing.is_empty());
            assert!(!path.exists());
        }
        let spoof = temp.path().join("alias-suffix").join("victim");
        fs::create_dir_all(spoof.parent().unwrap()).unwrap();
        fs::write(&spoof, b"keep").unwrap();
        assert!(
            store
                .secure_target
                .as_ref()
                .unwrap()
                .relative(spoof.to_str().unwrap())
                .is_err()
        );
        assert_eq!(fs::read(spoof).unwrap(), b"keep");
    }

    #[test]
    fn native_rewind_checks_entire_snapshot_before_any_delete() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let first = project.join("first");
        let outside = temp.path().join("outside");
        fs::write(&first, b"keep first").unwrap();
        fs::write(&outside, b"keep outside").unwrap();
        let backup = FileHistoryBackup {
            backup_file_name: BackupFileName::Null,
            version: 1,
            backup_time_ms: 0,
            mode: None,
        };
        let store = FileHistoryStore::new(FileHistoryOptions::new(temp.path().join("backups")))
            .with_project_target(&project)
            .unwrap();
        store
            .state
            .lock()
            .unwrap()
            .snapshots
            .push(FileHistorySnapshot {
                message_id: Some("m".into()),
                tracked_file_backups: vec![
                    (first.to_string_lossy().into_owned(), backup.clone()),
                    (outside.to_string_lossy().into_owned(), backup),
                ],
                expected_file_states: vec![],
                timestamp_ms: 0,
            });
        assert!(store.rewind("m").is_err());
        assert_eq!(fs::read(first).unwrap(), b"keep first");
        assert_eq!(fs::read(outside).unwrap(), b"keep outside");
    }

    #[test]
    fn native_store_blocks_forged_expected_and_link_targets() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, b"private").unwrap();
        let symlink_path = project.join("symlink");
        let hardlink_path = project.join("hardlink");
        symlink(&victim, &symlink_path).unwrap();
        fs::hard_link(&victim, &hardlink_path).unwrap();
        let store = FileHistoryStore::new(FileHistoryOptions::new(temp.path().join("backups")))
            .with_project_target(&project)
            .unwrap();
        for (id, path, expected_only) in [
            ("outside", &victim, true),
            ("symlink", &symlink_path, false),
            ("hardlink", &hardlink_path, false),
        ] {
            let backup = FileHistoryBackup {
                backup_file_name: BackupFileName::Null,
                version: 1,
                backup_time_ms: 0,
                mode: None,
            };
            let path = path.to_string_lossy().into_owned();
            let mut snapshot = FileHistorySnapshot {
                message_id: Some(id.into()),
                tracked_file_backups: vec![],
                expected_file_states: vec![],
                timestamp_ms: 0,
            };
            if expected_only {
                snapshot
                    .expected_file_states
                    .push((path, ExpectedFileState::Missing));
            } else {
                snapshot.tracked_file_backups.push((path, backup));
            }
            store.state.lock().unwrap().snapshots.push(snapshot);
            assert!(store.get_conflict_paths(id).is_err());
            assert!(store.get_diff_stats(id).is_err());
            assert!(store.rewind(id).is_err());
            assert_eq!(fs::read(&victim).unwrap(), b"private");
        }
    }

    #[test]
    fn native_rewind_mode_none_preserves_backup_mode_for_existing_and_missing() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let backup = temp.path().join("source");
        fs::write(&backup, b"restored").unwrap();
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o640)).unwrap();
        let guard = SecureTarget::new(&project).unwrap();
        for old_mode in [Some(0o400), Some(0o640), None] {
            let destination = project.join(format!("file-{old_mode:?}"));
            if let Some(mode) = old_mode {
                fs::write(&destination, b"previous").unwrap();
                fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).unwrap();
            }
            guard
                .restore(
                    destination.to_str().unwrap(),
                    &mut fs::File::open(&backup).unwrap(),
                    None,
                )
                .unwrap();
            assert_eq!(
                fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn native_backup_rejects_unrepresentable_json_version() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("file");
        fs::write(&source, b"original").unwrap();
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.join("history")))
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        let path = source.to_string_lossy().into_owned();
        store
            .state
            .lock()
            .unwrap()
            .snapshots
            .push(FileHistorySnapshot {
                message_id: Some("old".into()),
                tracked_file_backups: vec![(
                    path.clone(),
                    FileHistoryBackup {
                        backup_file_name: BackupFileName::Named("old".into()),
                        version: 9_007_199_254_740_991,
                        backup_time_ms: 0,
                        mode: None,
                    },
                )],
                expected_file_states: vec![],
                timestamp_ms: 0,
            });
        let error = store.track_edit(&path, "new").unwrap_err();
        assert!(error.to_string().contains("JSON safe integer"));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        fs::remove_file(&source).unwrap();
        assert!(
            store
                .track_edit(&path, "missing")
                .unwrap_err()
                .to_string()
                .contains("JSON safe integer")
        );
    }

    #[test]
    fn authorized_resolved_symlinks_and_hardlinks_keep_file_identity() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let dir = root.join("real");
        fs::create_dir(&dir).unwrap();
        let real = dir.join("file");
        fs::write(&real, "\u{feff}before\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let hard = root.join("hard");
        fs::hard_link(&real, &hard).unwrap();
        symlink(&dir, root.join("dir-link")).unwrap();
        symlink("file", dir.join("leaf-link")).unwrap();
        let spelling = root.join("dir-link/leaf-link");
        let cap = AuthorizedHistorySource::open_resolved(
            &spelling,
            &fs::canonicalize(&spelling).unwrap(),
            Some(&root),
        )
        .unwrap();
        let (created, metadata) = cap
            .write_checked(Some("\u{feff}before\n"), "\u{feff}after\n")
            .unwrap();
        assert!(!created);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        assert_eq!(fs::read_to_string(&hard).unwrap(), "\u{feff}after\n");
        assert!(
            fs::symlink_metadata(&spelling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn authorized_write_handles_existing_new_and_stale_content() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let path = root.join("existing");
        fs::write(&path, "before\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let cap = AuthorizedHistorySource::open_resolved(&path, &path, Some(&root)).unwrap();
        assert!(cap.write_checked(Some("stale\n"), "wrong").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"before\n");
        fs::write(&path, vec![b'x'; 1_000_000]).unwrap();
        assert!(cap.write_checked(Some("before\n"), "wrong").is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), 1_000_000);
        fs::write(&path, "before\n").unwrap();
        let (created, metadata) = cap.write_checked(Some("before\n"), "after\n").unwrap();
        assert!(!created);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        assert_eq!(fs::read(&path).unwrap(), b"after\n");
        let new_path = root.join("new");
        let new_cap = AuthorizedHistorySource::open_with_root(&new_path, &root).unwrap();
        assert!(new_cap.write_checked(None, "new\n").unwrap().0);
        assert_eq!(fs::read(&new_path).unwrap(), b"new\n");
        assert!(new_cap.write_checked(None, "wrong").is_err());
    }

    #[test]
    fn authorized_new_file_tracks_and_marks_using_created_descriptor() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let path = root.join("new");
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.join("history")))
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        let cap = AuthorizedHistorySource::open_resolved(&path, &path, Some(&root)).unwrap();
        store.track_edit_authorized(&cap, "new-message").unwrap();
        assert!(cap.write_checked(None, "created\n").unwrap().0);
        assert!(cap.pinned.lock().unwrap().is_some());
        store
            .mark_edit_committed_authorized(&cap, "new-message")
            .unwrap();
        let state = store.state();
        assert!(matches!(
            state.snapshots[0].expected_file_states[0].1,
            ExpectedFileState::Present { .. }
        ));
        assert_eq!(fs::read(path).unwrap(), b"created\n");
    }

    #[test]
    fn native_track_failure_does_not_publish_state_or_cache() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("source");
        fs::write(&source, b"before").unwrap();
        let mut options = FileHistoryOptions::new(root.join("history"));
        options.on_snapshot_recorded =
            Some(Arc::new(|_, _| Err(io::Error::other("sink failed").into())));
        let store = FileHistoryStore::new(options)
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        let cap = AuthorizedHistorySource::open_with_root(&source, &root).unwrap();
        assert!(store.track_edit_authorized(&cap, "m1").is_err());
        assert!(store.state().snapshots.is_empty());
        assert!(store.state().tracked_files.is_empty());
        assert!(store.mtime_cache.lock().unwrap().is_empty());
        fs::remove_file(&source).unwrap();
        symlink(root.join("missing"), &source).unwrap();
        assert!(store.track_edit_authorized(&cap, "m2").is_err());
        assert!(store.state().snapshots.is_empty());
        assert!(store.state().tracked_files.is_empty());
        assert!(store.mtime_cache.lock().unwrap().is_empty());
    }

    #[test]
    fn per_call_history_authority_tracks_normal_and_approved_outside_files() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let source = project.join("file");
        let outside = root.join("outside");
        fs::write(&source, b"before").unwrap();
        fs::write(&outside, b"outside-before").unwrap();
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.join("history")))
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        for (id, path) in [("inside", &source), ("outside", &outside)] {
            let cap = AuthorizedHistorySource::open_with_root(path, &project).unwrap();
            store.track_edit_authorized(&cap, id).unwrap();
            fs::write(path, b"after").unwrap();
            store.mark_edit_committed_authorized(&cap, id).unwrap();
        }
        let state = store.state();
        assert_eq!(state.snapshots.len(), 2);
        assert!(
            state
                .snapshots
                .iter()
                .all(|s| s.expected_file_states.len() == 1)
        );
    }

    #[test]
    fn per_call_history_parent_creation_matches_normal_workspace_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let reference = root.join("reference");
        fs::create_dir(&reference).unwrap();
        let expected = fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
        let source = root.join("new-parent/file");
        let _cap = AuthorizedHistorySource::open_with_root(&source, &root).unwrap();
        assert_eq!(
            fs::metadata(root.join("new-parent"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            expected
        );
    }

    #[test]
    fn per_call_history_allows_new_configured_working_directory() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let additional = root.join("new-additional");
        let source = additional.join("nested/file");
        assert!(!additional.exists());
        let cap = AuthorizedHistorySource::open_with_root(&source, &additional).unwrap();
        assert!(additional.join("nested").is_dir());
        assert!(cap.open_file().unwrap().is_none());
    }

    #[test]
    fn two_per_call_authorities_preserve_original_and_intermediate_versions() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("file");
        fs::write(&source, b"original").unwrap();
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.join("history")))
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        for (id, value) in [("one", b"first".as_slice()), ("two", b"second".as_slice())] {
            let capability = AuthorizedHistorySource::open_with_root(&source, &root).unwrap();
            store.track_edit_authorized(&capability, id).unwrap();
            fs::write(&source, value).unwrap();
            store
                .mark_edit_committed_authorized(&capability, id)
                .unwrap();
            drop(capability);
        }
        assert!(store.rewind("two").unwrap().conflicts.is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"first");
        assert!(store.rewind("one").unwrap().conflicts.is_empty());
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[test]
    fn per_call_history_authority_handles_missing_source() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("missing");
        let store = FileHistoryStore::new(FileHistoryOptions::new(root.join("history")))
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        let cap = AuthorizedHistorySource::open_with_root(&source, &root).unwrap();
        store.track_edit_authorized(&cap, "m").unwrap();
        assert!(matches!(
            store.state().snapshots[0].tracked_file_backups[0]
                .1
                .backup_file_name,
            BackupFileName::Null
        ));
        fs::write(&source, b"new").unwrap();
        store.mark_edit_committed_authorized(&cap, "m").unwrap();
    }

    #[test]
    fn native_versions_preserve_two_turns_across_replay_and_rewind() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("file");
        fs::write(&source, b"original").unwrap();
        let history = root.join("history");
        let make_store = || {
            FileHistoryStore::new(FileHistoryOptions::new(&history))
                .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"))
        };
        let store = make_store();
        let path = source.to_str().unwrap();
        store.track_edit(path, "one").unwrap();
        fs::write(&source, b"first").unwrap();
        store.mark_edit_committed(path, "one").unwrap();
        store.track_edit(path, "two").unwrap();
        fs::write(&source, b"second").unwrap();
        store.mark_edit_committed(path, "two").unwrap();
        let before = store.state();
        assert_eq!(before.snapshots.len(), 2);
        assert_eq!(before.snapshots[0].tracked_file_backups[0].1.version, 1);
        assert_eq!(before.snapshots[1].tracked_file_backups[0].1.version, 2);
        let entries: Vec<_> = before
            .snapshots
            .iter()
            .map(|snap| SnapshotRecordedEntry {
                message_id: snap.message_id.clone(),
                tracked_file_backups: snap
                    .tracked_file_backups
                    .iter()
                    .map(|(path, backup)| {
                        (
                            path.clone(),
                            RecordedBackup {
                                backup_file_name: backup.backup_file_name.clone(),
                                version: backup.version,
                                backup_time_ms: backup.backup_time_ms,
                                mode: backup.mode,
                            },
                        )
                    })
                    .collect(),
                expected_file_states: snap.expected_file_states.clone(),
                timestamp_ms: Some(snap.timestamp_ms),
            })
            .collect();
        drop(store);
        let resumed = make_store();
        resumed.replay_from_transcript(&entries);
        assert!(resumed.rewind("two").unwrap().conflicts.is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"first");
        assert!(resumed.rewind("one").unwrap().conflicts.is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"original");
    }

    #[test]
    fn native_versions_skip_orphans_and_keep_retained_backups_on_eviction() {
        let temp = TempDir::new();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("file");
        let history = root.join("history");
        fs::create_dir(&history).unwrap();
        fs::write(&source, b"original").unwrap();
        let mut options = FileHistoryOptions::new(&history);
        options.max_snapshots = Some(2);
        let store = FileHistoryStore::new(options)
            .with_trusted_root(fs::File::open(&root).unwrap(), PathBuf::from("history"));
        let path = source.to_str().unwrap();
        store.track_edit(path, "one").unwrap();
        fs::write(&source, b"first").unwrap();
        store.track_edit(path, "two").unwrap();
        fs::write(&source, b"second").unwrap();
        let orphan = history.join(get_backup_file_name(path, 3));
        fs::write(&orphan, b"do-not-replace").unwrap();
        store.track_edit(path, "three").unwrap();
        // Eviction belongs to explicit snapshot finalization, not track_edit.
        store.make_snapshot("three").unwrap();
        let state = store.state();
        assert_eq!(state.snapshots.len(), 2);
        assert_eq!(state.snapshots[0].message_id.as_deref(), Some("two"));
        assert_eq!(state.snapshots[0].tracked_file_backups[0].1.version, 2);
        assert_eq!(state.snapshots[1].tracked_file_backups[0].1.version, 4);
        assert_eq!(fs::read(orphan).unwrap(), b"do-not-replace");
        let retained = history.join(get_backup_file_name(path, 2));
        assert_eq!(fs::read(retained).unwrap(), b"first");
    }

    #[test]
    fn rewind_does_not_commit_or_remove_replaced_temporary() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let target = project.join("target");
        let backup = temp.path().join("backup");
        let saved = project.join("owned-saved");
        fs::write(&target, b"old").unwrap();
        fs::write(&backup, b"secret").unwrap();
        let guard = SecureTarget::new(&project).unwrap();
        let mut replacement = None;
        let result = guard.restore_with_hook(
            target.to_str().unwrap(),
            &mut fs::File::open(&backup).unwrap(),
            Some(0o600),
            || {
                let name = fs::read_dir(&project)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".pilotdeck-rewind-")
                    })
                    .unwrap();
                fs::rename(&name, &saved).unwrap();
                fs::write(&name, b"foreign").unwrap();
                replacement = Some(name);
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
        assert_eq!(fs::read(replacement.unwrap()).unwrap(), b"foreign");
        assert_eq!(fs::read(saved).unwrap(), b"secret");
    }

    #[test]
    fn rewind_temporary_is_private_and_parent_swap_aborts() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let project = temp.path().join("project");
        let parent = project.join("sub");
        let moved = temp.path().join("moved");
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("target");
        let backup = temp.path().join("backup");
        fs::write(&target, b"old").unwrap();
        fs::write(&backup, b"secret").unwrap();
        let guard = SecureTarget::new(&project).unwrap();
        let result = guard.restore_with_hook(
            target.to_str().unwrap(),
            &mut fs::File::open(&backup).unwrap(),
            Some(0o644),
            || {
                let entry = fs::read_dir(&parent)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".pilotdeck-rewind-")
                    })
                    .unwrap();
                assert_eq!(
                    fs::metadata(entry).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                fs::rename(&parent, &moved).unwrap();
                fs::create_dir(&parent).unwrap();
                fs::write(parent.join("target"), b"replacement").unwrap();
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(moved.join("target")).unwrap(), b"old");
        assert_eq!(fs::read(parent.join("target")).unwrap(), b"replacement");
        assert!(fs::read_dir(moved).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pilotdeck-rewind-")
        }));
    }

    #[test]
    fn native_rewind_replays_real_alias_and_canonical_edit_history() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        let alias = temp.path().join("alias");
        fs::create_dir_all(&project).unwrap();
        symlink(&project, &alias).unwrap();
        let canonical = fs::canonicalize(&project).unwrap();
        let store = FileHistoryStore::new(FileHistoryOptions::new(temp.path().join("history")))
            .with_trusted_root(
                fs::File::open(temp.path()).unwrap(),
                PathBuf::from("history"),
            )
            .with_project_target(&alias)
            .unwrap();
        for (id, path) in [
            ("alias-edit", alias.join("first")),
            ("canonical-edit", canonical.join("second")),
        ] {
            fs::write(&path, b"before").unwrap();
            store.track_edit(path.to_str().unwrap(), id).unwrap();
            fs::write(&path, b"after").unwrap();
            store
                .mark_edit_committed(path.to_str().unwrap(), id)
                .unwrap();
            assert!(store.rewind(id).unwrap().missing.is_empty());
            assert_eq!(fs::read(path).unwrap(), b"before");
        }
    }

    #[test]
    fn rewind_target_rejects_untrusted_paths_and_preserves_all_targets() {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim");
        let first = project.join("first");
        fs::write(&victim, b"outside").unwrap();
        fs::write(&first, b"inside").unwrap();
        let guard = SecureTarget::new(&project).unwrap();
        let backup = FileHistoryBackup {
            backup_file_name: BackupFileName::Null,
            version: 1,
            backup_time_ms: 0,
            mode: None,
        };
        let snapshot = FileHistorySnapshot {
            message_id: Some("m".into()),
            tracked_file_backups: vec![
                (first.to_string_lossy().into_owned(), backup.clone()),
                (victim.to_string_lossy().into_owned(), backup.clone()),
            ],
            expected_file_states: vec![],
            timestamp_ms: 0,
        };
        assert!(find_conflict_paths(&snapshot, Some(&guard)).is_err());
        assert_eq!(fs::read(&first).unwrap(), b"inside");
        assert_eq!(fs::read(&victim).unwrap(), b"outside");
        let ancestor = project.join("linked");
        symlink(&outside, &ancestor).unwrap();
        assert!(
            guard
                .bytes(ancestor.join("victim").to_str().unwrap())
                .is_err()
        );
        let leaf = project.join("leaf");
        symlink(&victim, &leaf).unwrap();
        assert!(guard.validate(leaf.to_str().unwrap()).is_err());
        assert!(guard.delete(leaf.to_str().unwrap()).is_err());
        fs::remove_file(&leaf).unwrap();
        fs::hard_link(&victim, &leaf).unwrap();
        assert!(guard.bytes(leaf.to_str().unwrap()).is_err());
        assert!(guard.delete(leaf.to_str().unwrap()).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"outside");
    }

    #[test]
    fn rewind_target_normal_restore_and_null_delete() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let target = project.join("nested/file");
        let backup = temp.path().join("backup");
        fs::write(&backup, b"restored").unwrap();
        let guard = SecureTarget::new(&project).unwrap();
        guard
            .restore(
                target.to_str().unwrap(),
                &mut fs::File::open(&backup).unwrap(),
                Some(0o640),
            )
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"restored");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        guard.delete(target.to_str().unwrap()).unwrap();
        guard.delete(target.to_str().unwrap()).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn inherited_history_root_is_descriptor_bounded_for_forks() {
        let temp = TempDir::new();
        let parent = temp.path().join("parent");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let pinned = fs::File::open(&parent).unwrap();
        let child = parent.join("session/subagents");
        let opened = open_inherited_chat_dir(&pinned, &parent, &child).unwrap();
        assert!(opened.metadata().unwrap().is_dir());
        assert!(open_inherited_chat_dir(&pinned, &parent, &outside).is_err());
        let link = parent.join("linked");
        symlink(&outside, &link).unwrap();
        assert!(open_inherited_chat_dir(&pinned, &parent, &link.join("subagents")).is_err());
        assert!(!outside.join("subagents").exists());
    }

    #[test]
    fn chat_root_rejects_symlink_leaf_and_ancestor() {
        let temp = TempDir::new();
        let home = temp.path().join("home");
        let outside = temp.path().join("outside");
        fs::create_dir_all(home.join("projects/p/chats")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let chats = home.join("projects/p/chats");
        assert!(open_trusted_chat_dir(&home, &chats).is_ok());
        fs::remove_dir(&chats).unwrap();
        symlink(&outside, &chats).unwrap();
        assert!(open_trusted_chat_dir(&home, &chats).is_err());
        fs::remove_file(&chats).unwrap();
        fs::remove_dir(home.join("projects/p")).unwrap();
        symlink(&outside, home.join("projects/p")).unwrap();
        assert!(open_trusted_chat_dir(&home, &chats).is_err());
        assert!(open_trusted_chat_dir(&home, &outside).is_err());
    }

    #[test]
    fn descriptor_root_writes_and_reads_normal_backup() {
        let temp = TempDir::new();
        let source = temp.path().join("source");
        fs::write(&source, b"snapshot").unwrap();
        let secure = SecureBackup {
            root: Arc::new(fs::File::open(temp.path()).unwrap()),
            relative: PathBuf::from("session/file-history"),
        };
        secure.write_backup(&source, "backup", 0o600).unwrap();
        assert_eq!(
            secure.read_text("backup").unwrap().as_deref(),
            Some("snapshot")
        );
        secure.remove("backup").unwrap();
        assert!(secure.read_text("backup").unwrap().is_none());
    }
}
