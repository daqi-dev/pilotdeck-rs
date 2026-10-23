//! Native port of `core/file-memory.ts` at the fixed cc7c4e6 baseline.
//!
//! Candidates, edit fields and returned records use the shared JS object codec:
//! camelCase keys and optional-field presence match `core/types.ts:16-153`.
//! UTF-16 is retained by the API; Node's UTF-8 replacement happens at disk/hash
//! boundaries. Public operations serialize on each resolved root, matching synchronous
//! TS calls. Separate processes require a single writer (TS has
//! no cross-process transaction or locking either).
use super::{EdgeClawStorage, StorageError};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
pub(crate) mod helpers;
use helpers::*;
// Share the existing Node IO adapters with sibling repository modules without
// widening the private helpers module's API.
pub(super) fn io_mkdir(path: impl AsRef<Path>) -> std::io::Result<()> {
    helpers::io_mkdir(path)
}
pub(super) fn io_write(path: impl AsRef<Path>, content: impl AsRef<[u8]>) -> std::io::Result<()> {
    helpers::io_write(path, content)
}

pub const CURRENT_PROJECT_ID: &str = "current_project";
pub const TMP_PROJECT_ID: &str = "_tmp";
const PROFILE: &str = "global/UserIdentity/user-profile.md";
const GENERAL: &str = "GeneralProjects";

pub type MemoryCandidate = JsObject;
pub type MemoryFileRecord = JsObject;
pub type MemoryManifestEntry = JsObject;
pub type ProjectMetaRecord = JsObject;
pub type ManifestUserEntriesProvider =
    Arc<dyn Fn() -> Result<Vec<MemoryManifestEntry>> + Send + Sync>;
pub type FileMemoryClock = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct FileMemoryStoreOptions {
    pub workspace_mode: String,
    pub manage_project_meta: bool,
    pub manage_project_files: bool,
    pub manage_user_profile: bool,
    /// None disables the profile path; the default is the TS default path.
    pub user_profile_relative_path: Option<String>,
    pub user_notes_relative_dir: Option<String>,
    pub append_only_user_entries: bool,
    pub enable_manifest: bool,
    pub manifest_user_entries_provider: Option<ManifestUserEntriesProvider>,
}
impl Default for FileMemoryStoreOptions {
    fn default() -> Self {
        Self {
            workspace_mode: "single".into(),
            manage_project_meta: true,
            manage_project_files: true,
            manage_user_profile: true,
            user_profile_relative_path: Some(PROFILE.into()),
            user_notes_relative_dir: None,
            append_only_user_entries: false,
            enable_manifest: true,
            manifest_user_entries_provider: None,
        }
    }
}
#[derive(Debug)]
pub enum FileMemoryError {
    Io(std::io::Error),
    Invalid(String),
    InvalidUtf16(JsString),
    Collation(String),
    /// A TS-conformance `TypeError` raised before any store call, e.g. a
    /// non-array `kinds` passed to `listGlobalMemoryEntries`.
    Type(String),
    Storage(StorageError),
    /// `MemoryBundleValidationError` (`sqlite.ts:104-109`); the message is the
    /// TS error text verbatim so import validation failures compare equal.
    Bundle(String),
}
impl fmt::Display for FileMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => match e
                .get_ref()
                .and_then(|_| crate::node_io::diagnostic(e, "", None))
            {
                Some((_, message)) => write!(f, "{message}"),
                None => write!(f, "{e}"),
            },
            Self::InvalidUtf16(e) => write!(f, "{}", String::from_utf16_lossy(e.units())),
            Self::Invalid(e) | Self::Collation(e) | Self::Type(e) => write!(f, "{e}"),
            Self::Storage(e) => write!(f, "{e}"),
            Self::Bundle(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for FileMemoryError {}
impl FileMemoryError {
    pub fn io_code(&self) -> Option<&'static str> {
        match self {
            Self::Io(e) => crate::node_io::lookup(e).map(|(code, _)| code),
            _ => None,
        }
    }
    pub fn js_message(&self) -> JsString {
        match self {
            Self::InvalidUtf16(s) => s.clone(),
            Self::Storage(StorageError::Sqlite(e)) => e.to_string().into(),
            _ => self.to_string().into(),
        }
    }
}
impl From<std::io::Error> for FileMemoryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<StorageError> for FileMemoryError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
type Result<T> = std::result::Result<T, FileMemoryError>;

pub struct FileMemoryStore {
    declared_root: PathBuf,
    core: Core,
    operation: Arc<Mutex<()>>,
}
thread_local! {
    static HELD_OPERATIONS: RefCell<HashSet<usize>> = RefCell::default();
}
// TS providers run synchronously inside the calling operation. A provider may
// read another store with the same root, so only the outer call takes the mutex.
// Keeping that guard alive still excludes every other thread until it returns.
pub(crate) struct OperationGuard<'a> {
    outer: Option<MutexGuard<'a, ()>>,
    key: usize,
}
impl<'a> OperationGuard<'a> {
    pub(super) fn enter(operation: &'a Mutex<()>) -> Result<Self> {
        let key = std::ptr::from_ref(operation) as usize;
        if HELD_OPERATIONS.with(|held| held.borrow().contains(&key)) {
            return Ok(Self { outer: None, key });
        }
        let outer = operation
            .lock()
            .map_err(|_| FileMemoryError::Invalid("file memory operation mutex poisoned".into()))?;
        HELD_OPERATIONS.with(|held| held.borrow_mut().insert(key));
        Ok(Self {
            outer: Some(outer),
            key,
        })
    }
}
impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        if self.outer.is_some() {
            HELD_OPERATIONS.with(|held| held.borrow_mut().remove(&self.key));
        }
    }
}
pub(super) fn operation_lock(root: &Path) -> Result<Arc<Mutex<()>>> {
    type Locks = HashMap<PathBuf, Weak<Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| FileMemoryError::Invalid("file memory root registry mutex poisoned".into()))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(root.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}
struct Core {
    root: PathBuf,
    options: FileMemoryStoreOptions,
    clock: FileMemoryClock,
}
macro_rules! object_methods { ($($method:ident => $ts:literal),* $(,)?) => { $(pub fn $method(&self, input: &JsObject) -> Result<JsValue> { self.execute($ts, input) })* }; }
impl FileMemoryStore {
    pub fn new(root: impl AsRef<Path>, options: FileMemoryStoreOptions) -> Result<Self> {
        Self::with_clock(root, options, Arc::new(now_iso))
    }
    /// Deterministic clock injection for replay and callers already owning a clock.
    pub fn with_clock(
        root: impl AsRef<Path>,
        mut options: FileMemoryStoreOptions,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        if !options.manage_user_profile {
            options.user_profile_relative_path = None;
            options.user_notes_relative_dir = None;
            options.append_only_user_entries = false;
        }
        let declared_root = root.as_ref().to_path_buf();
        let core = Core {
            root: absolute(root.as_ref())?,
            options,
            clock,
        };
        let operation = operation_lock(&core.root)?;
        {
            let _guard = OperationGuard::enter(&operation)?;
            core.ensure_layout()?;
        }
        Ok(Self {
            declared_root,
            core,
            operation,
        })
    }
    pub(super) fn validate_entry_path(&self, path: &JsString) -> Result<()> {
        self.core.within(&String::from_utf16_lossy(path.units()))?;
        Ok(())
    }
    pub fn get_root_dir(&self) -> &Path {
        &self.declared_root
    }
    pub fn get_workspace_mode(&self) -> &str {
        &self.core.options.workspace_mode
    }
    pub fn is_general_mode(&self) -> bool {
        self.core.general()
    }
    pub fn get_user_profile_relative_path(&self) -> Option<&str> {
        self.core.options.user_profile_relative_path.as_deref()
    }
    /// Execute one synchronous TS-shaped operation. Positional TS arguments are
    /// named (`ids`, `maxLines`, `relativePaths`, `lastDreamAt`) in this adapter.
    pub fn execute(&self, method: &str, input: &JsObject) -> Result<JsValue> {
        let _guard = OperationGuard::enter(&self.operation)?;
        self.core.execute(method, input)
    }
    /// Holds this store's per-root operation lock across a multi-step
    /// sequence (import's directory swap). Reentrant with `execute` on the
    /// same thread; keep the returned guard alive for the whole sequence.
    pub(crate) fn operation_scope(&self) -> Result<OperationGuard<'_>> {
        OperationGuard::enter(&self.operation)
    }
    object_methods! {
        upsert_candidate => "upsertCandidate", upsert_user_profile => "upsertUserProfile",
        list_memory_entries => "listMemoryEntries", count_memory_entries => "countMemoryEntries",
        get_memory_records_by_ids => "getMemoryRecordsByIds", edit_entry => "editEntry",
        mark_entries_deprecated => "markEntriesDeprecated", restore_entries => "restoreEntries",
        delete_entries => "deleteEntries", reassign_project_entries => "reassignProjectEntries",
        upsert_project_meta => "upsertProjectMeta", upsert_general_project_meta => "upsertGeneralProjectMeta",
        ensure_project_meta => "ensureProjectMeta", edit_project_meta => "editProjectMeta",
        get_project_meta => "getProjectMeta", has_visible_project_memory => "hasVisibleProjectMemory",
        list_project_identity_hints => "listProjectIdentityHints", get_overview => "getOverview",
        get_snapshot_version => "getSnapshotVersion", clear_all_data => "clearAllData",
        merge_duplicate_entries => "mergeDuplicateEntries", archive_tmp_entries => "archiveTmpEntries"
    }
    pub fn repair_manifests(&self) -> Result<JsValue> {
        self.execute("repairManifests", &JsObject::new())
    }
    pub fn get_user_summary(&self) -> Result<JsValue> {
        self.execute("getUserSummary", &JsObject::new())
    }
    pub fn list_project_metas(&self) -> Result<JsValue> {
        self.execute("listProjectMetas", &JsObject::new())
    }
    pub fn list_tmp_entries(&self) -> Vec<MemoryManifestEntry> {
        vec![]
    }
    pub fn export_bundle_records(&self) -> Result<JsValue> {
        self.execute("exportBundleRecords", &JsObject::new())
    }
    pub fn export_snapshot_files(&self) -> Result<JsValue> {
        self.execute("exportSnapshotFiles", &JsObject::new())
    }
    pub fn to_candidate(record: &MemoryFileRecord) -> MemoryCandidate {
        to_candidate(record)
    }
}

/// `sqlite.ts:1179-1198`; these materialization options omit the live
/// repository's manifest provider. Constructor wiring adds that provider below.
pub(super) fn repository_workspace_options(mode: &str) -> FileMemoryStoreOptions {
    FileMemoryStoreOptions {
        workspace_mode: mode.into(),
        manage_user_profile: false,
        ..Default::default()
    }
}
pub(super) fn repository_global_options() -> FileMemoryStoreOptions {
    FileMemoryStoreOptions {
        manage_project_meta: false,
        manage_project_files: false,
        user_profile_relative_path: Some("UserIdentity/user-profile.md".into()),
        user_notes_relative_dir: Some("UserIdentityNotes".into()),
        append_only_user_entries: true,
        enable_manifest: false,
        ..Default::default()
    }
}

/// File-store wiring from `sqlite.ts:523-540`, using only pr10a's public API.
/// Repository snapshots are in `storage::snapshots`; routing and retrieval
/// remain separate work items.
pub struct EdgeClawFileStores {
    pub(super) repository: super::storage::accessors::RepositoryContext,
    pub storage: EdgeClawStorage,
    pub workspace_memory: FileMemoryStore,
    pub global_user_memory: Arc<FileMemoryStore>,
}
impl EdgeClawFileStores {
    pub fn open(
        db: impl AsRef<Path>,
        memory: impl AsRef<Path>,
        global: impl AsRef<Path>,
        mode: &str,
    ) -> Result<Self> {
        Self::with_clock(db, memory, global, mode, Arc::new(now_iso))
    }
    pub fn with_clock(
        db: impl AsRef<Path>,
        memory: impl AsRef<Path>,
        global: impl AsRef<Path>,
        mode: &str,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        Self::with_clock_general(db, memory, global, mode, None, clock)
    }
    /// Shared constructor. An explicit general workspace skips the `$HOME`
    /// lookup entirely, so repository callers that already know it do not
    /// acquire an environment dependency (`with_workspace_clock`).
    pub(crate) fn with_clock_general(
        db: impl AsRef<Path>,
        memory: impl AsRef<Path>,
        global: impl AsRef<Path>,
        mode: &str,
        general_workspace: Option<PathBuf>,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        let global_user_memory = Arc::new(FileMemoryStore::with_clock(
            global,
            repository_global_options(),
            clock.clone(),
        )?);
        let provider = global_user_memory.clone();
        let workspace_memory = FileMemoryStore::with_clock(
            &memory,
            FileMemoryStoreOptions {
                manifest_user_entries_provider: Some(Arc::new(move || {
                    let value = provider.list_memory_entries(&obj(&[
                        ("kinds", strings(&["user".into()])),
                        ("scope", s("global")),
                    ]))?;
                    Ok(match &value {
                        JsValue::Array(v) => v
                            .iter()
                            .filter_map(|v| v.as_object().cloned())
                            .map(map_global)
                            .collect(),
                        _ => vec![],
                    })
                })),
                ..repository_workspace_options(mode)
            },
            clock.clone(),
        )?;
        let repository = match general_workspace {
            Some(general) => super::storage::accessors::RepositoryContext::with_general(
                memory.as_ref(),
                mode,
                general,
                clock.clone(),
            )?,
            None => super::storage::accessors::RepositoryContext::new(
                memory.as_ref(),
                mode,
                clock.clone(),
            )?,
        };
        Ok(Self {
            repository,
            storage: EdgeClawStorage::with_clock(db, clock)?,
            workspace_memory,
            global_user_memory,
        })
    }
}
fn now_iso() -> String {
    let ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    };
    pilotdeck_transport::system_clock::format_epoch_iso(ms)
}
pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}
impl Core {
    fn now(&self) -> String {
        (self.clock)()
    }
    fn general(&self) -> bool {
        self.options.workspace_mode == "general"
    }
    fn path(&self, relative: &str) -> Result<PathBuf> {
        self.within(relative)?;
        absolute(&self.root.join(relative))
    }
    fn within(&self, relative: &str) -> Result<bool> {
        let denied = || FileMemoryError::Invalid(format!("Memory entry not found: {relative}"));
        if Path::new(relative).is_absolute() || relative.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(denied());
        }
        let path = absolute(&self.root.join(relative))?;
        if !path.starts_with(&self.root) {
            return Err(denied());
        }
        // Resolve existing ancestors as well as the leaf: a missing file beneath
        // an escaping directory symlink is unsafe too. Reject dangling links.
        fn resolved(path: &Path, boundary: &Path, root: &Path) -> std::io::Result<PathBuf> {
            if path == boundary {
                return Ok(root.to_owned());
            }
            match fs::symlink_metadata(path) {
                Ok(_) => fs::canonicalize(path),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::NotADirectory
                            | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    // A non-searchable directory cannot be traversed by this
                    // process. Validate its accessible ancestors, then let the
                    // actual exists/read/write boundary retain Node semantics.
                    let parent = path.parent().ok_or(e)?;
                    Ok(
                        resolved(parent, boundary, root)?
                            .join(path.file_name().unwrap_or_default()),
                    )
                }
                Err(e) => Err(e),
            }
        }
        // The root itself may be a symlink (including a dangling root during
        // clear). Treat it as the trust boundary; only descendants are inputs.
        let root = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        match resolved(&path, &self.root, &root) {
            Ok(path) if path.starts_with(root) => Ok(true),
            Ok(_) => Err(denied()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound) => Err(denied()),
            Err(e) => Err(crate::node_io::path_error(e, "realpath", &path).into()),
        }
    }
    fn ensure_layout(&self) -> Result<()> {
        io_mkdir(&self.root)?;
        if self.options.manage_user_profile {
            if let Some(path) = &self.options.user_profile_relative_path {
                io_mkdir(self.path(path)?.parent().expect("absolute parent"))?;
            }
            if let Some(path) = &self.options.user_notes_relative_dir {
                io_mkdir(self.path(path)?)?;
            }
        }
        if self.options.manage_project_files {
            for dir in ["Project", "Feedback"] {
                io_mkdir(self.root.join(dir))?;
            }
        }
        if self.options.manage_project_meta && self.general() {
            io_mkdir(self.root.join(GENERAL))?;
        }
        Ok(())
    }
    fn parsed(&self, path: &str) -> Result<Option<(JsObject, String)>> {
        if !self.within(path)? {
            return Ok(None);
        }
        let path = self.path(path)?;
        if !path.exists() {
            return Ok(None);
        }
        // The clock is read only for the `updated_at` fallback, like TS's
        // lazy `??` operand, so a progressing clock advances identically.
        Ok(parse(&read(&path)?, || self.now()))
    }
    fn entry(&self, path: &str) -> Result<Option<JsObject>> {
        let Some((mut fm, _)) = self.parsed(path)? else {
            return Ok(None);
        };
        if text(&fm, "scope") == "project" {
            fm.insert(
                "projectId".into(),
                s(first(&[n(&fm, "projectId"), CURRENT_PROJECT_ID.into()])),
            );
        }
        fm.insert("file".into(), s(path.rsplit('/').next().unwrap_or(path)));
        fm.insert("relativePath".into(), s(path));
        fm.insert("absolutePath".into(), s(self.path(path)?.to_string_lossy()));
        Ok(Some(fm))
    }
    fn directory(&self, dir: &str) -> Result<Vec<JsObject>> {
        let path = self.root.join(dir);
        if !path.exists() {
            return Ok(vec![]);
        }
        let mut paths = vec![];
        for entry in
            fs::read_dir(&path).map_err(|e| crate::node_io::path_error(e, "scandir", &path))?
        {
            let entry = entry.map_err(|e| crate::node_io::path_error(e, "scandir", &path))?;
            let file = entry.file_name().to_string_lossy().into_owned();
            if entry
                .file_type()
                .map_err(|e| crate::node_io::path_error(e, "scandir", &path))?
                .is_file()
                && file.ends_with(".md")
                && file != "MEMORY.md"
                && file != "project.meta.md"
            {
                paths.push(file);
            }
        }
        paths.sort_by_key(|p| p.encode_utf16().collect::<Vec<_>>());
        let mut records = vec![];
        for file in paths {
            if let Some(entry) = self.entry(&format!("{dir}/{file}"))? {
                records.push(entry);
            }
        }
        Ok(records)
    }
    fn sort(&self, entries: &mut [JsObject]) -> Result<()> {
        sort_manifest_entries(entries, |e| text(e, "updatedAt"), "relativePath")
    }
    fn all(&self) -> Result<Vec<JsObject>> {
        let mut entries = vec![];
        if self.options.manage_project_meta && self.general() {
            entries.extend(self.directory(GENERAL)?);
        }
        if self.options.manage_project_files {
            entries.extend(self.directory("Project")?);
            entries.extend(self.directory("Feedback")?);
        }
        if self.options.manage_user_profile {
            if let Some(path) = &self.options.user_profile_relative_path
                && let Some(entry) = self.entry(path)?
            {
                entries.push(entry);
            }
            if let Some(dir) = &self.options.user_notes_relative_dir {
                entries.extend(self.directory(dir)?);
            }
        }
        self.sort(&mut entries)?;
        Ok(entries)
    }
    fn records(&self, ids: &[String], max_lines: f64) -> Result<Vec<JsObject>> {
        let mut records = vec![];
        for id in ids {
            let Some(mut e) = self.entry(id)? else {
                continue;
            };
            let Some((_, body)) = self.parsed(id)? else {
                continue;
            };
            let full = crate::metadata::js_trim(&body);
            let normalized = full.replace("\r\n", "\n");
            let lines: Vec<_> = normalized.split('\n').collect();
            let content = if max_lines <= 0.0 {
                String::new()
            } else if max_lines.is_nan() || lines.len() as f64 > max_lines {
                format!(
                    "{}\n...",
                    lines[..max_lines as usize].join("\n").replace("\r\n", "\n")
                )
            } else {
                full.into()
            };
            e.insert("content".into(), s(content));
            e.insert("preview".into(), preview(&body));
            records.push(e);
        }
        Ok(records)
    }
    fn api_path(&self, record: &mut JsObject, path: &JsString) {
        if path.to_utf8().is_ok() {
            return;
        }
        let mut parts = if path.units().first() == Some(&47) {
            vec![]
        } else {
            self.root
                .to_string_lossy()
                .encode_utf16()
                .collect::<Vec<_>>()
                .split(|u| *u == 47)
                .filter(|p| !p.is_empty())
                .map(|p| p.to_vec())
                .collect::<Vec<_>>()
        };
        for part in path.units().split(|u| *u == 47) {
            if part.is_empty() || part == [46] {
                continue;
            }
            if part == [46, 46] {
                parts.pop();
            } else {
                parts.push(part.to_vec());
            }
        }
        let mut absolute = vec![47];
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                absolute.push(47);
            }
            absolute.extend(part);
        }
        record.insert("relativePath".into(), JsValue::String(path.clone()));
        record.insert(
            "absolutePath".into(),
            JsValue::String(JsString::from_utf16(absolute)),
        );
        record.insert(
            "file".into(),
            JsValue::String(JsString::from_utf16(
                path.units()
                    .rsplit(|u| *u == 47)
                    .next()
                    .unwrap_or(path.units())
                    .to_vec(),
            )),
        );
    }
    fn write(&self, path: &str, fm: &JsObject, body: &str) -> Result<JsObject> {
        let absolute = self.path(path)?;
        io_mkdir(absolute.parent().expect("absolute parent"))?;
        io_write(absolute, render(fm, body))?;
        self.repair()?;
        self.records(&[path.into()], 5000.0)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                FileMemoryError::Invalid(format!("Written memory entry cannot be read: {path}"))
            })
    }
    fn require_profile(&self) -> Result<&str> {
        self.options
            .user_profile_relative_path
            .as_deref()
            .filter(|p| self.options.manage_user_profile && !p.is_empty())
            .ok_or_else(|| {
                FileMemoryError::Invalid(
                    "Global user profile storage is disabled for this store".into(),
                )
            })
    }
    fn single_meta(&self) -> Result<Option<JsObject>> {
        if !self.options.manage_project_meta || self.general() {
            return Ok(None);
        }
        let path = self.path("project.meta.md")?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = read(&path)?;
        let header = raw
            .strip_prefix("---\n")
            .and_then(|r| r.find("\n---\n").map(|i| &r[..i]))
            .map(header)
            .unwrap_or_default();
        // `const parsed = parseFrontmatterBlock(raw)` (TS 548) runs before the
        // project-name check, and only its result is shared by the two date
        // fields: each terminal `nowIso()` fallback reads the clock on its own
        // (TS 568-569), so a header without either date observes two distinct
        // timestamps, one clock read per field.
        let parsed_updated_at = parse(&raw, || self.now()).map(|(p, _)| text(&p, "updatedAt"));
        let name = n(&header, "project_name");
        if name.is_empty() {
            return Ok(None);
        }
        let date_of = |key: &str| -> String {
            if header.contains_key(key) {
                text(&header, key)
            } else {
                parsed_updated_at.clone().unwrap_or_else(|| self.now())
            }
        };
        let created_at = date_of("created_at");
        let updated_at = date_of("updated_at");
        let mut meta = obj(&[
            ("projectId", s(CURRENT_PROJECT_ID)),
            ("projectName", s(&name)),
            ("description", s(desc(&text(&header, "description"), &name))),
            (
                "status",
                s(first(&[n(&header, "status"), "in_progress".into()])),
            ),
            ("createdAt", s(&created_at)),
            ("updatedAt", s(&updated_at)),
            ("relativePath", s("project.meta.md")),
            ("absolutePath", s(path.to_string_lossy())),
        ]);
        if !text(&header, "dream_updated_at").is_empty() {
            meta.insert(
                "dreamUpdatedAt".into(),
                s(text(&header, "dream_updated_at")),
            );
        }
        Ok(Some(meta))
    }
    fn general_meta(&self, record: &JsObject) -> JsObject {
        let mut meta = obj(&[
            (
                "projectId",
                s(first(&[n(record, "projectId"), CURRENT_PROJECT_ID.into()])),
            ),
            ("projectName", s(text(record, "name"))),
            (
                "description",
                s(desc(&text(record, "description"), &text(record, "name"))),
            ),
            (
                "status",
                s(first(&[
                    para(&sections(&text(record, "content")), "status"),
                    "in_progress".into(),
                ])),
            ),
            (
                "createdAt",
                s(first(&[
                    text(record, "capturedAt"),
                    text(record, "updatedAt"),
                ])),
            ),
        ]);
        copy(
            &mut meta,
            record,
            &[
                "sourceKind",
                "sourceWorkspacePath",
                "sourceProjectId",
                "updatedAt",
                "dreamUpdatedAt",
                "relativePath",
                "absolutePath",
            ],
        );
        meta
    }
    fn metas(&self) -> Result<Vec<JsObject>> {
        if !self.options.manage_project_meta {
            return Ok(vec![]);
        }
        if !self.general() {
            return Ok(self.single_meta()?.into_iter().collect());
        }
        let ids = self
            .all()?
            .iter()
            .filter(|e| text(e, "type") == "general_project_meta" && !flag(e, "deprecated"))
            .map(|e| text(e, "relativePath"))
            .collect::<Vec<_>>();
        Ok(self
            .records(&ids, 5000.0)?
            .iter()
            .map(|r| self.general_meta(r))
            .collect())
    }
    fn meta(&self, id: &str) -> Result<Option<JsObject>> {
        let id = norm(id);
        if !self.general() {
            if !id.is_empty() && id != CURRENT_PROJECT_ID {
                return Ok(None);
            }
            return self.single_meta();
        }
        Ok(self
            .metas()?
            .into_iter()
            .find(|m| text(m, "projectId") == id))
    }
    fn upsert_meta(&self, input: &JsObject, general_only: bool) -> Result<JsObject> {
        if !self.options.manage_project_meta || (general_only && !self.general()) {
            return Err(FileMemoryError::Invalid(
                if general_only {
                    "General project metadata is disabled for this store"
                } else {
                    "Project metadata is disabled for this store"
                }
                .into(),
            ));
        }
        if self.general() {
            let input_name = if general_only {
                text(input, "projectName")
            } else {
                first(&[n(input, "projectName"), "Current Project".into()])
            };
            let supplied_id = norm_field(input, "projectId");
            let id_js = if supplied_id.is_empty() {
                JsString::from(hash(&format!(
                    "{}:{}:{}:{}",
                    norm(&input_name),
                    n(input, "sourceWorkspacePath"),
                    n(input, "sourceProjectId"),
                    self.now()
                )))
            } else {
                supplied_id
            };
            let id = String::from_utf16_lossy(id_js.units());
            let old = if id_js.to_utf8().is_ok() {
                self.meta(&id)?.unwrap_or_default()
            } else {
                JsObject::new()
            };
            let path = first(&[
                text(&old, "relativePath"),
                format!(
                    "{GENERAL}/{}-{}.md",
                    slug(&first(&[input_name.clone(), id.clone()])),
                    utf16_prefix(&id, 12)
                ),
            ]);
            let name = first(&[
                norm(&input_name),
                text(&old, "projectName"),
                "Current Project".into(),
            ]);
            let description = desc(
                &text(input, "description"),
                &first(&[text(&old, "description"), input_name.clone()]),
            );
            let mut fm = obj(&[
                ("name", s(&name)),
                ("description", s(&description)),
                ("type", s("general_project_meta")),
                ("scope", s("project")),
                ("projectId", s(id)),
                ("updatedAt", s(self.now())),
            ]);
            for key in ["sourceKind", "sourceWorkspacePath", "sourceProjectId"] {
                let value = first(&[text(input, key), text(&old, key)]);
                if !value.is_empty() {
                    fm.insert(key.into(), s(value));
                }
            }
            if !text(&old, "createdAt").is_empty() {
                fm.insert("capturedAt".into(), s(text(&old, "createdAt")));
            }
            let dream = norm(&first(&[
                text(input, "dreamUpdatedAt"),
                text(&old, "dreamUpdatedAt"),
            ]));
            if !dream.is_empty() {
                fm.insert("dreamUpdatedAt".into(), s(dream));
            }
            let mut candidate = fm.clone();
            candidate.insert(
                "stage".into(),
                s(normalize_status(&first(&[
                    text(input, "status"),
                    text(&old, "status"),
                    "in_progress".into(),
                ]))),
            );
            let mut api_path = JsString::from(path.clone());
            if text(&old, "relativePath").is_empty() {
                let mut units = format!(
                    "{GENERAL}/{}-",
                    slug(&first(&[input_name.clone(), text(&fm, "projectId")]))
                )
                .encode_utf16()
                .collect::<Vec<_>>();
                units.extend(id_js.units().iter().take(12).copied());
                units.extend(".md".encode_utf16());
                api_path = JsString::from_utf16(units);
            }
            let mut record = self.write(&path, &fm, &body(&candidate))?;
            self.api_path(&mut record, &api_path);
            return Ok(self.general_meta(&record));
        }
        let old = self.single_meta()?.unwrap_or_default();
        let active = self
            .all()?
            .into_iter()
            .filter(|e| text(e, "scope") == "project" && !flag(e, "deprecated"))
            .collect::<Vec<_>>();
        let project = active.iter().find(|e| text(e, "type") == "project");
        let feedback = active.iter().find(|e| text(e, "type") == "feedback");
        let seed_name = first(&[
            project.map(|e| text(e, "name")).unwrap_or_default(),
            "Current Project".into(),
        ]);
        let seed_desc = project
            .or(feedback)
            .map(|e| first(&[text(e, "description"), "Current Project".into()]))
            .unwrap_or_else(|| "Current project memory.".into());
        let name = first(&[
            n(input, "projectName"),
            text(&old, "projectName"),
            seed_name,
        ]);
        let description = desc(
            &text(input, "description"),
            &first(&[text(&old, "description"), seed_desc, name.clone()]),
        );
        let mut meta = obj(&[
            ("projectId", s(CURRENT_PROJECT_ID)),
            ("projectName", s(&name)),
            ("description", s(&description)),
            (
                "status",
                s(normalize_status(&first(&[
                    text(input, "status"),
                    text(&old, "status"),
                    "in_progress".into(),
                ]))),
            ),
            (
                "createdAt",
                s(if old.contains_key("createdAt") {
                    text(&old, "createdAt")
                } else {
                    self.now()
                }),
            ),
            ("updatedAt", s(self.now())),
            ("relativePath", s("project.meta.md")),
            (
                "absolutePath",
                s(self.path("project.meta.md")?.to_string_lossy()),
            ),
        ]);
        let dream = norm(&first(&[
            text(input, "dreamUpdatedAt"),
            text(&old, "dreamUpdatedAt"),
        ]));
        if !dream.is_empty() {
            meta.insert("dreamUpdatedAt".into(), s(dream));
        }
        let mut lines = vec!["---".into()];
        for (disk, key) in [
            ("project_id", "projectId"),
            ("project_name", "projectName"),
            ("description", "description"),
            ("status", "status"),
            ("created_at", "createdAt"),
            ("updated_at", "updatedAt"),
        ] {
            lines.push(format!("{disk}: {}", text(&meta, key)));
        }
        if !text(&meta, "dreamUpdatedAt").is_empty() {
            lines.push(format!(
                "dream_updated_at: {}",
                text(&meta, "dreamUpdatedAt")
            ));
        }
        lines.extend([
            "---".into(),
            "".into(),
            "## Summary".into(),
            description,
            "".into(),
        ]);
        io_write(
            self.path("project.meta.md")?,
            format!("{}\n", crate::metadata::js_trim(&lines.join("\n"))),
        )?;
        self.repair()?;
        Ok(meta)
    }
    fn ensure_meta(&self, input: &JsObject) -> Result<JsObject> {
        if self.general() {
            if !n(input, "projectId").is_empty()
                && norm_field(input, "projectId").to_utf8().is_ok()
                && let Some(meta) = self.meta(&n(input, "projectId"))?
            {
                return Ok(meta);
            }
        } else if let Some(meta) = self.single_meta()? {
            return Ok(meta);
        }
        self.upsert_meta(input, false)
    }
    fn manifest(&self) -> Result<String> {
        // `buildManifest` (file-memory.ts:941-974) computes the project meta,
        // the general metas, every entry and the user-entry provider before
        // the `Updated: ${nowIso()}` line, so that timestamp observes those
        // clock reads — a dateless project.meta.md ticks twice ahead of it.
        let project_meta = self.single_meta()?;
        let general_metas = if self.general() {
            self.metas()?
        } else {
            vec![]
        };
        let all = self.all()?;
        let user_entries = if !self.options.manage_user_profile {
            match &self.options.manifest_user_entries_provider {
                Some(p) => {
                    let mut entries: Vec<JsObject> = p()?
                        .into_iter()
                        .filter(|e| !flag(e, "deprecated"))
                        .collect();
                    self.sort(&mut entries)?;
                    entries
                }
                None => vec![],
            }
        } else {
            all.iter()
                .filter(|e| !flag(e, "deprecated") && text(e, "type") == "user")
                .cloned()
                .collect()
        };
        let mut lines = vec![
            "# EdgeClaw Memory".into(),
            "".into(),
            format!("Updated: {}", self.now()),
            "".into(),
        ];
        if let Some(meta) = project_meta {
            lines.extend([
                "## Current Project Meta".into(),
                format!(
                    "- [{}](project.meta.md) — {}",
                    text(&meta, "projectName"),
                    text(&meta, "description")
                ),
                "".into(),
            ]);
        }
        if self.general() && !general_metas.is_empty() {
            lines.push("## General Projects".into());
            for m in general_metas {
                lines.push(format!(
                    "- [{}]({}) — {}",
                    text(&m, "projectName"),
                    text(&m, "relativePath"),
                    text(&m, "description")
                ));
            }
            lines.push("".into());
        }
        for (title, kind) in [
            ("Project Memory", "project"),
            ("Feedback Memory", "feedback"),
            ("User Memory", "user"),
            ("Deprecated", ""),
        ] {
            let entries = if kind == "user" && !self.options.manage_user_profile {
                user_entries.clone()
            } else {
                all.iter()
                    .filter(|e| {
                        if kind.is_empty() {
                            flag(e, "deprecated")
                        } else {
                            !flag(e, "deprecated") && text(e, "type") == kind
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if entries.is_empty() {
                continue;
            }
            lines.push(format!("## {title}"));
            for e in entries {
                let path = if kind == "user" {
                    let rel = relative_path(&self.root, &PathBuf::from(text(&e, "absolutePath")))
                        .replace('\\', "/");
                    if rel.is_empty() || rel.starts_with("..") {
                        first(&[rel, text(&e, "relativePath")])
                    } else {
                        text(&e, "relativePath")
                    }
                } else {
                    text(&e, "relativePath")
                };
                lines.push(format!(
                    "- [{}]({path}) — {}",
                    text(&e, "name"),
                    text(&e, "description")
                ));
            }
            lines.push("".into());
        }
        Ok(format!("{}\n", crate::metadata::js_trim(&lines.join("\n"))))
    }
    fn repair(&self) -> Result<JsObject> {
        self.ensure_layout()?;
        let (changed, summary) = if !self.options.enable_manifest {
            (0, "Manifest management is disabled for this store.")
        } else {
            let next = self.manifest()?;
            let path = self.path("MEMORY.md")?;
            let prev = if path.exists() {
                read(&path)?
            } else {
                String::new()
            };
            if next != prev {
                io_write(path, next)?;
                (1, "Rebuilt workspace memory manifest.")
            } else {
                (0, "Workspace memory manifest already up to date.")
            }
        };
        Ok(obj(&[
            ("changed", num(changed)),
            ("summary", s(summary)),
            ("memoryFileCount", num(self.all()?.len())),
        ]))
    }
    fn list(&self, input: &JsObject) -> Result<Vec<JsObject>> {
        let project = n(input, "projectId");
        if !project.is_empty() && !self.general() && project != CURRENT_PROJECT_ID {
            return Ok(vec![]);
        }
        let kinds = if input.contains_key("kinds") {
            array(input, "kinds")
        } else {
            vec!["user".into(), "feedback".into(), "project".into()]
        };
        // TS normalizes the query eagerly, before `collectAllEntries`, so a
        // truthy non-string throws even when no entry would have matched.
        let query = normalized_query(input)?;
        let entries = self
            .all()?
            .into_iter()
            .filter(|e| {
                kinds.contains(&text(e, "type"))
                    && (text(input, "scope").is_empty() || text(e, "scope") == text(input, "scope"))
                    && (project.is_empty()
                        || (text(e, "type") != "general_project_meta"
                            && text(e, "scope") != "project")
                        || (norm_field(input, "projectId").to_utf8().is_ok()
                            && text(e, "projectId") == project))
                    && (truthy(input, "includeDeprecated") || !flag(e, "deprecated"))
                    && query_matches(e, query.as_ref())
            })
            .collect::<Vec<_>>();
        let offset = input
            .get("offset")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let limit = input
            .get("limit")
            .and_then(JsValue::as_number)
            .unwrap_or(entries.len().max(1) as f64);
        if offset.is_nan() || limit.is_nan() {
            return Ok(vec![]);
        }
        let offset = offset.max(0.0);
        let limit = limit.max(1.0);
        let start = (offset as usize).min(entries.len());
        let end = ((offset + limit) as usize).min(entries.len());
        Ok(entries
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect())
    }

    fn upsert(&self, candidate: &JsObject, profile: bool) -> Result<JsObject> {
        let mut c = candidate.clone();
        let kind = text(&c, "type");
        if profile {
            let path = self.require_profile()?;
            let old = self.entry(path)?.unwrap_or_default();
            c.insert("type".into(), s("user"));
            c.insert("scope".into(), s("global"));
            c.insert(
                "name".into(),
                s(first(&[n(candidate, "name"), "user-profile".into()])),
            );
            c.insert(
                "description".into(),
                s(desc(
                    &text(candidate, "description"),
                    &first(&[text(candidate, "name"), "User profile".into()]),
                )),
            );
            return self.write(path, &frontmatter(&c, &old, &self.now()), &body(candidate));
        }
        if kind != "user" && self.options.manage_project_meta {
            let meta_input = obj(&[
                (
                    "projectName",
                    s(if kind == "project" {
                        text(&c, "name")
                    } else {
                        first(&[text(&c, "description"), text(&c, "name")])
                    }),
                ),
                ("description", s(text(&c, "description"))),
            ]);
            if self.general() {
                let id = if !n(&c, "projectId").is_empty() {
                    n(&c, "projectId")
                } else {
                    text(&self.ensure_meta(&meta_input)?, "projectId")
                };
                c.insert(
                    "projectId".into(),
                    if !norm_field(&c, "projectId").is_empty() {
                        JsValue::String(norm_field(&c, "projectId"))
                    } else {
                        s(id)
                    },
                );
            } else {
                let mut meta_input = meta_input;
                if kind != "project" {
                    meta_input = obj(&[("description", s(text(&c, "description")))]);
                }
                self.ensure_meta(&meta_input)?;
            }
        }
        let all = self.all()?;
        let old = all
            .iter()
            .find(|e| {
                !text(&c, "capturedAt").is_empty()
                    && !text(&c, "sourceSessionKey").is_empty()
                    && e.get("capturedAt") == c.get("capturedAt")
                    && e.get("sourceSessionKey") == c.get("sourceSessionKey")
                    && text(e, "type") == kind
            })
            .or_else(|| {
                if kind == "user" && !self.options.append_only_user_entries {
                    all.iter().find(|e| {
                        Some(text(e, "relativePath")).as_ref()
                            == self.options.user_profile_relative_path.as_ref()
                    })
                } else {
                    None
                }
            })
            .cloned()
            .unwrap_or_default();
        let path = if !text(&old, "relativePath").is_empty() {
            text(&old, "relativePath")
        } else if kind == "user" && !self.options.append_only_user_entries {
            self.require_profile()?.into()
        } else {
            let tail = first(&[text(&c, "sourceSessionKey"), self.now()]);
            let seed = if kind == "user" {
                format!(
                    "{kind}:{}:{}:{}:{tail}",
                    text(&c, "name"),
                    text(&c, "description"),
                    text(&c, "capturedAt")
                )
            } else {
                format!(
                    "{kind}:{}:{}:{}:{}:{tail}",
                    text(&c, "projectId"),
                    text(&c, "name"),
                    text(&c, "description"),
                    text(&c, "capturedAt")
                )
            };
            let dir = if kind == "user" {
                self.options
                    .user_notes_relative_dir
                    .as_deref()
                    .unwrap_or("UserIdentityNotes")
            } else if kind == "feedback" {
                "Feedback"
            } else {
                "Project"
            };
            format!("{dir}/{}-{}.md", slug(&text(&c, "name")), hash(&seed))
        };
        let r = self.write(&path, &frontmatter(&c, &old, &self.now()), &body(&c))?;
        if kind == "project"
            && !self.general()
            && let Some(meta) = self.single_meta()?
        {
            let seed_like = n(&meta, "projectName").eq_ignore_ascii_case("Current Project")
                || text(&meta, "description")
                    .to_lowercase()
                    .ends_with("workspace memory")
                || n(&meta, "description").to_lowercase() == "current project memory.";
            let mut input = obj(&[("status", s(text(&meta, "status")))]);
            if seed_like {
                input.insert("projectName".into(), s(text(&c, "name")));
                if !text(&c, "description").is_empty() {
                    input.insert("description".into(), s(text(&c, "description")));
                }
            }
            self.upsert_meta(&input, false)?;
        }
        Ok(r)
    }
    fn execute(&self, method: &str, input: &JsObject) -> Result<JsValue> {
        Ok(match method {
            "toCandidate" => JsValue::Object(to_candidate(
                input
                    .get("record")
                    .and_then(JsValue::as_object)
                    .ok_or_else(|| FileMemoryError::Invalid("record is required".into()))?,
            )),
            "upsertCandidate" => JsValue::Object(self.upsert(input, false)?),
            "upsertUserProfile" => JsValue::Object(self.upsert(input, true)?),
            "repairManifests" => JsValue::Object(self.repair()?),
            "listMemoryEntries" => objects(self.list(input)?),
            "countMemoryEntries" => num(self.list(input)?.len()),
            "getMemoryRecordsByIds" => {
                let mut result = vec![];
                if let Some(JsValue::Array(ids)) = input.get("ids") {
                    for id in ids.iter().filter_map(JsValue::as_string) {
                        if let Some(mut record) = self
                            .records(
                                &[String::from_utf16_lossy(id.units())],
                                input
                                    .get("maxLines")
                                    .and_then(JsValue::as_number)
                                    .unwrap_or(80.0),
                            )?
                            .into_iter()
                            .next()
                        {
                            self.api_path(&mut record, id);
                            result.push(record);
                        }
                    }
                }
                objects(result)
            }
            "getUserSummary" => {
                let records = match self
                    .options
                    .user_profile_relative_path
                    .as_deref()
                    .filter(|_| self.options.manage_user_profile)
                {
                    Some(path) => self.records(&[path.into()], 5000.0)?,
                    None => vec![],
                };
                let values = records
                    .first()
                    .map(|r| facts(&sections(&text(r, "content"))))
                    .unwrap_or_default();
                JsValue::Object(obj(&[
                    ("identityBackground", strings(&values)),
                    ("files", objects(records)),
                ]))
            }
            "upsertProjectMeta" => JsValue::Object(self.upsert_meta(input, false)?),
            "upsertGeneralProjectMeta" => JsValue::Object(self.upsert_meta(input, true)?),
            "ensureProjectMeta" => JsValue::Object(self.ensure_meta(input)?),
            "editProjectMeta" => {
                if !self.options.manage_project_meta {
                    return Err(FileMemoryError::Invalid(
                        "Project metadata is disabled for this store".into(),
                    ));
                }
                let id = n(input, "projectId");
                let mut next = input.clone();
                if self.general() {
                    if id.is_empty() {
                        return Err(FileMemoryError::Invalid(
                            "projectId is required for General project metadata edits".into(),
                        ));
                    }
                    let old = if norm_field(input, "projectId").to_utf8().is_ok() {
                        self.meta(&id)?
                    } else {
                        None
                    }
                    .ok_or_else(|| input_error("Unknown projectId: ", input, "projectId"))?;
                    copy(
                        &mut next,
                        &old,
                        &[
                            "sourceKind",
                            "sourceWorkspacePath",
                            "sourceProjectId",
                            "dreamUpdatedAt",
                        ],
                    );
                } else {
                    if !id.is_empty() && id != CURRENT_PROJECT_ID {
                        return Err(input_error("Unknown projectId: ", input, "projectId"));
                    }
                    next = JsObject::new();
                    copy(&mut next, input, &["projectName", "description", "status"]);
                }
                JsValue::Object(self.upsert_meta(&next, self.general())?)
            }
            "getProjectMeta" => {
                if input.contains_key("projectId")
                    && norm_field(input, "projectId").to_utf8().is_err()
                {
                    JsValue::Null
                } else {
                    self.meta(&if input.contains_key("projectId") {
                        text(input, "projectId")
                    } else {
                        CURRENT_PROJECT_ID.into()
                    })?
                    .map(JsValue::Object)
                    .unwrap_or(JsValue::Null)
                }
            }
            "listProjectMetas" => objects(self.metas()?),
            "listProjectIdentityHints" => objects(
                self.metas()?
                    .iter()
                    .map(|m| {
                        obj(&[
                            ("identityKey", s(text(m, "projectId"))),
                            ("projectId", s(text(m, "projectId"))),
                            ("projectName", s(text(m, "projectName"))),
                            ("description", s(text(m, "description"))),
                            ("updatedAt", s(text(m, "updatedAt"))),
                            ("scope", s("formal")),
                        ])
                    })
                    .collect(),
            ),
            "hasVisibleProjectMemory" => {
                let id = if input.contains_key("projectId") {
                    n(input, "projectId")
                } else {
                    CURRENT_PROJECT_ID.into()
                };
                JsValue::Bool(
                    self.options.manage_project_files
                        && norm_field(input, "projectId").to_utf8().is_ok()
                        && (id.is_empty() || self.general() || id == CURRENT_PROJECT_ID)
                        && self.all()?.iter().any(|e| {
                            text(e, "scope") == "project"
                                && text(e, "type") != "general_project_meta"
                                && !flag(e, "deprecated")
                                && (id.is_empty()
                                    || first(&[text(e, "projectId"), CURRENT_PROJECT_ID.into()])
                                        == id)
                        }),
                )
            }
            "listTmpEntries" => JsValue::Array(vec![]),
            "archiveTmpEntries" => {
                return Err(FileMemoryError::Invalid(
                    "archive_tmp is not supported in EdgeClaw current-project memory mode".into(),
                ));
            }
            "editEntry" => {
                let path = text(input, "relativePath");
                let old = self
                    .records(std::slice::from_ref(&path), 5000.0)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        input_error("Memory entry not found: ", input, "relativePath")
                    })?;
                let mut candidate = to_candidate(&old);
                candidate.insert(
                    "name".into(),
                    s(first(&[n(input, "name"), text(&candidate, "name")])),
                );
                candidate.insert(
                    "description".into(),
                    s(desc(
                        &text(input, "description"),
                        &text(&candidate, "description"),
                    )),
                );
                if let Some(fields) = input.get("fields").and_then(JsValue::as_object) {
                    remove(&mut candidate, "body");
                    for key in ["stage", "rule", "why", "howToApply"] {
                        if fields.get(key).and_then(JsValue::as_string).is_some() {
                            candidate.insert(key.into(), s(n(fields, key)));
                        }
                    }
                    for key in [
                        "decisions",
                        "constraints",
                        "nextSteps",
                        "blockers",
                        "timeline",
                        "notes",
                    ] {
                        if fields.contains_key(key) {
                            candidate.insert(key.into(), unique_array_value(fields, key));
                        }
                    }
                }
                let mut fm = old;
                fm.insert("name".into(), s(text(&candidate, "name")));
                fm.insert("description".into(), s(candidate_description(&candidate)));
                fm.insert("updatedAt".into(), s(self.now()));
                JsValue::Object(self.write(&path, &fm, &body(&candidate))?)
            }
            "markEntriesDeprecated" | "restoreEntries" => {
                let mut changed = vec![];
                for path in array(input, "relativePaths") {
                    if let Some(mut r) = self
                        .records(std::slice::from_ref(&path), 5000.0)?
                        .into_iter()
                        .next()
                    {
                        r.insert("updatedAt".into(), s(self.now()));
                        r.insert(
                            "deprecated".into(),
                            JsValue::Bool(method == "markEntriesDeprecated"),
                        );
                        self.write(&path, &r, &text(&r, "content"))?;
                        changed.push(path);
                    }
                }
                JsValue::Object(obj(&[
                    ("mutatedIds", strings(&changed)),
                    ("deletedProjectIds", JsValue::Array(vec![])),
                ]))
            }
            "deleteEntries" => {
                let mut changed = vec![];
                for path in array(input, "relativePaths") {
                    if self.within(&path)? && self.path(&path)?.exists() {
                        let absolute = self.path(&path)?;
                        fs::remove_file(&absolute)
                            .map_err(|e| crate::node_io::path_error(e, "unlink", &absolute))?;
                        changed.push(path);
                    }
                }
                self.repair()?;
                JsValue::Object(obj(&[
                    ("mutatedIds", strings(&changed)),
                    ("deletedProjectIds", JsValue::Array(vec![])),
                ]))
            }
            "reassignProjectEntries" => {
                let from = n(input, "fromProjectId");
                let to = n(input, "toProjectId");
                let mut changed = vec![];
                if !from.is_empty() && !to.is_empty() && from != to {
                    let options = obj(&[
                        ("kinds", strings(&["project".into(), "feedback".into()])),
                        ("scope", s("project")),
                        (
                            "projectId",
                            JsValue::String(norm_field(input, "fromProjectId")),
                        ),
                        ("includeDeprecated", JsValue::Bool(true)),
                        ("limit", num(5000)),
                    ]);
                    for e in self.list(&options)? {
                        let path = text(&e, "relativePath");
                        if let Some(mut r) = self
                            .records(std::slice::from_ref(&path), 5000.0)?
                            .into_iter()
                            .next()
                        {
                            r.insert("projectId".into(), s(&to));
                            r.insert("updatedAt".into(), s(self.now()));
                            self.write(&path, &r, &text(&r, "content"))?;
                            changed.push(path);
                        }
                    }
                }
                JsValue::Object(obj(&[("mutatedIds", strings(&changed))]))
            }
            "exportBundleRecords" => {
                let ids = self
                    .all()?
                    .iter()
                    .map(|e| text(e, "relativePath"))
                    .collect::<Vec<_>>();
                let records = self
                    .records(&ids, 5000.0)?
                    .into_iter()
                    .map(|r| {
                        let mut export = JsObject::new();
                        copy(
                            &mut export,
                            &r,
                            &[
                                "name",
                                "description",
                                "type",
                                "scope",
                                "projectId",
                                "sourceKind",
                                "sourceWorkspacePath",
                                "sourceProjectId",
                                "updatedAt",
                                "dreamUpdatedAt",
                                "capturedAt",
                                "sourceSessionKey",
                                "deprecated",
                                "dreamAttempts",
                                "file",
                                "relativePath",
                                "content",
                            ],
                        );
                        if text(&r, "scope") != "project" {
                            remove(&mut export, "projectId");
                        }
                        export
                    })
                    .collect();
                let metas = self
                    .metas()?
                    .into_iter()
                    .map(|mut m| {
                        remove(&mut m, "absolutePath");
                        for key in ["sourceKind", "sourceWorkspacePath", "sourceProjectId"] {
                            remove(&mut m, key);
                        }
                        m
                    })
                    .collect();
                JsValue::Object(obj(&[
                    ("memoryFiles", objects(records)),
                    ("projectMetas", objects(metas)),
                ]))
            }
            "exportSnapshotFiles" => objects(self.snapshot()?),
            "clearAllData" => {
                super::node_fs::rm_sync(&self.root)?;
                self.ensure_layout()?;
                if !matches!(input.get("rebuildManifest"), Some(JsValue::Bool(false))) {
                    self.repair()?;
                }
                JsValue::Null
            }
            "getOverview" => {
                let entries = self
                    .all()?
                    .into_iter()
                    .filter(|e| !flag(e, "deprecated"))
                    .collect::<Vec<_>>();
                let count = |kind: &str| entries.iter().filter(|e| text(e, "type") == kind).count();
                let last = text(input, "lastDreamAt");
                let mut overview = obj(&[
                    ("totalFiles", num(entries.len())),
                    ("projectMemories", num(count("project"))),
                    ("feedbackMemories", num(count("feedback"))),
                    ("userProfiles", num(count("user"))),
                    (
                        "changedFilesSinceLastDream",
                        num(entries
                            .iter()
                            .filter(|e| last.is_empty() || text(e, "updatedAt") > last)
                            .count()),
                    ),
                    ("tmpTotalFiles", num(0)),
                    ("tmpFeedbackMemories", num(0)),
                    ("tmpProjectMemories", num(0)),
                    (
                        "projectMetaCount",
                        num(if self.general() {
                            count("general_project_meta")
                        } else {
                            usize::from(self.single_meta()?.is_some())
                        }),
                    ),
                ]);
                if self.general() {
                    overview.insert(
                        "generalProjectMetaCount".into(),
                        num(count("general_project_meta")),
                    );
                }
                if let Some(e) = entries.first()
                    && !text(e, "updatedAt").is_empty()
                {
                    overview.insert("latestMemoryAt".into(), s(text(e, "updatedAt")));
                }
                JsValue::Object(overview)
            }
            "getSnapshotVersion" => {
                let payload = JsValue::Object(obj(&[
                    ("lastDreamAt", s(text(input, "lastDreamAt"))),
                    ("files", objects(self.snapshot()?)),
                ]));
                s(hash(
                    &stringify_json(&payload)
                        .map_err(|e| FileMemoryError::Invalid(format!("{e:?}")))?
                        .unwrap_or_default(),
                ))
            }
            "mergeDuplicateEntries" => {
                let entries = match input.get("entries") {
                    Some(JsValue::Array(a)) => {
                        a.iter().filter_map(|v| v.as_object().cloned()).collect()
                    }
                    _ => self.all()?,
                };
                let mut groups: Vec<(String, Vec<JsObject>)> = vec![];
                for entry in entries {
                    let kind = text(&entry, "type");
                    if flag(&entry, "deprecated")
                        || kind == "user"
                        || kind == "general_project_meta"
                    {
                        continue;
                    }
                    let key = format!("{kind}:{}", slug(&text(&entry, "name")));
                    if let Some((_, bucket)) = groups.iter_mut().find(|(k, _)| *k == key) {
                        bucket.push(entry);
                    } else {
                        groups.push((key, vec![entry]));
                    }
                }
                let mut merged = 0;
                let mut changed = vec![];
                let mut deleted = vec![];
                for (_, bucket) in groups {
                    if bucket.len() < 2 {
                        continue;
                    }
                    let ids = bucket
                        .iter()
                        .map(|e| text(e, "relativePath"))
                        .collect::<Vec<_>>();
                    let mut records = self.records(&ids, 5000.0)?;
                    // Stable updatedAt-only sorting: ties retain caller's order.
                    if records.len() > 1 {
                        let collator = crate::project_files::node_collator(
                            &crate::project_files::node_locale(|key| std::env::var(key).ok()),
                        )
                        .map_err(|e| FileMemoryError::Collation(format!("{e:?}")))?;
                        records.sort_by(|a, b| {
                            crate::project_files::node_compare_utf16(
                                &collator,
                                &JsString::from(text(b, "updatedAt")),
                                &JsString::from(text(a, "updatedAt")),
                            )
                        });
                    }
                    let Some(primary) = records.first() else {
                        continue;
                    };
                    let path = text(primary, "relativePath");
                    let mut candidate = to_candidate(primary);
                    for r in records.iter().skip(1) {
                        candidate = merge(&candidate, &to_candidate(r));
                    }
                    let mut fm = primary.clone();
                    fm.insert("updatedAt".into(), s(self.now()));
                    fm.insert("description".into(), s(candidate_description(&candidate)));
                    fm.insert(
                        "dreamAttempts".into(),
                        JsValue::Number(
                            primary
                                .get("dreamAttempts")
                                .and_then(JsValue::as_number)
                                .unwrap_or(0.0)
                                + 1.0,
                        ),
                    );
                    self.write(&path, &fm, &body(&candidate))?;
                    changed.push(path);
                    for r in records.iter().skip(1) {
                        let path = text(r, "relativePath");
                        if self.path(&path)?.exists() {
                            let absolute = self.path(&path)?;
                            fs::remove_file(&absolute)
                                .map_err(|e| crate::node_io::path_error(e, "unlink", &absolute))?;
                            deleted.push(path);
                        }
                    }
                    merged += 1;
                }
                if merged > 0 || !deleted.is_empty() {
                    self.repair()?;
                }
                JsValue::Object(obj(&[
                    ("merged", num(merged)),
                    ("changedFiles", strings(&changed)),
                    ("deletedFiles", strings(&deleted)),
                ]))
            }
            _ => {
                return Err(FileMemoryError::Invalid(format!(
                    "Unknown FileMemoryStore operation: {method}"
                )));
            }
        })
    }
    fn snapshot(&self) -> Result<Vec<JsObject>> {
        if self.options.enable_manifest {
            self.repair()?;
        }
        let mut paths = vec![];
        if self.options.enable_manifest {
            paths.push("MEMORY.md".into());
        }
        if !self.general() && self.single_meta()?.is_some() {
            paths.push("project.meta.md".into());
        }
        paths.extend(self.all()?.iter().map(|e| text(e, "relativePath")));
        paths
            .iter()
            .map(|path| {
                Ok(obj(&[
                    ("relativePath", s(path)),
                    ("content", s(read(&self.path(path)?)?)),
                ]))
            })
            .collect()
    }
}
fn relative_path(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|p| p.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

#[cfg(test)]
mod entry_write_security_tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn direct_entry_write_rejects_unsafe_paths() {
        use std::os::unix::fs::symlink;
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/edgeclaw-entries/oracle.json"
        ))
        .unwrap();
        let missing = &oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "invalid-entry-operations")
            .unwrap()["observations"][0]["result"]["message"];
        let scratch =
            std::env::temp_dir().join(format!("pdrs-entry-write-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&scratch).unwrap();
        for name in ["memory", "global"] {
            let store = FileMemoryStore::new(scratch.join(name), FileMemoryStoreOptions::default())
                .unwrap();
            let victim = scratch.join("victim.md");
            fs::write(&victim, "valuable contents").unwrap();
            symlink(&victim, store.core.root.join("escape.md")).unwrap();
            symlink(&scratch, store.core.root.join("escape-dir")).unwrap();
            for path in [
                "../victim.md",
                victim.to_str().unwrap(),
                "escape.md",
                "escape-dir/victim.md",
                "escape-dir/new.md",
                "Project/../safe.md",
            ] {
                let e = store
                    .core
                    .write(path, &JsObject::new(), "changed")
                    .expect_err("unsafe write accepted");
                assert_eq!(
                    e.to_string(),
                    missing.as_str().unwrap().replace("missing", path)
                );
                assert_eq!(fs::read_to_string(&victim).unwrap(), "valuable contents");
                assert!(!scratch.join("new.md").exists());
            }
        }
        fs::remove_dir_all(scratch).unwrap();
    }
}

#[cfg(all(test, unix))]
#[path = "../../../../tests/fixtures/edgeclaw-entries/io_replay.rs"]
pub(super) mod round6_io;
#[cfg(all(test, unix))]
mod round6_tests {
    use super::*;
    #[test]
    fn file_scandir_matches_real_ts() {
        round6_io::check("file-scandir", |repo, root| {
            round6_io::deny(&root.join("current/memory/Project"), true);
            repo.workspace_memory
                .list_memory_entries(&JsObject::new())
                .unwrap_err()
        });
    }
}
