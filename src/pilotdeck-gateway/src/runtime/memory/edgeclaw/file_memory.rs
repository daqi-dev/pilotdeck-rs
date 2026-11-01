//! Native port of `core/file-memory.ts` at the fixed cc7c4e6 baseline.
//!
//! Candidates, edit fields and returned records use the shared JS object codec:
//! camelCase keys and optional-field presence match `core/types.ts:16-153`.
//! UTF-16 is retained by the API; Node's UTF-8 replacement happens at disk/hash
//! boundaries. Public operations serialize on each resolved root, matching synchronous
//! TS calls. Separate processes require a single writer (TS has
//! no cross-process transaction or locking either).
use super::{
    EdgeClawStorage, StorageError,
    text::{GENERAL_FEEDBACK_MEMORY_DIR, GENERAL_PROJECT_MEMORY_DIR},
};
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
const GENERAL: &str = super::text::GENERAL_PROJECT_META_DIR;

pub type MemoryCandidate = JsObject;
pub type MemoryFileRecord = JsObject;
pub type MemoryManifestEntry = JsObject;
pub type ProjectMetaRecord = JsObject;
pub type ManifestUserEntriesProvider =
    Arc<dyn Fn() -> Result<Vec<MemoryManifestEntry>> + Send + Sync>;
pub type FileMemoryClock = Arc<dyn Fn() -> String + Send + Sync>;
/// `Date.now()` for sibling temp path names (`sqlite.ts:422`).
pub type TempNowMs = Arc<dyn Fn() -> i64 + Send + Sync>;
/// `Math.random().toString(36).slice(2, 8)` for sibling temp path names
/// (`sqlite.ts:422`). V8's shortest-round-trip radix-36 formatting stays at
/// the JS boundary: fixtures inject the exact fragment per draw, production
/// only needs the uniqueness a fresh uuid provides.
pub type TempRandomFragment = Arc<dyn Fn() -> String + Send + Sync>;

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
    /// A TS-conformance `RangeError` raised before any store call, e.g. a
    /// `String(value)` join that exceeds V8's array depth in a dropped-cluster
    /// warning.
    Range(String),
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
            Self::Invalid(e) | Self::Collation(e) | Self::Type(e) | Self::Range(e) => {
                write!(f, "{e}")
            }
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
    /// `error.name` as Node's `String(error)` prefixes it. The Dream merge
    /// planning catch only sees validation [`Self::Type`] errors; other
    /// variants fall back to the base `Error` name.
    pub fn js_name(&self) -> &'static str {
        match self {
            Self::Type(_) => "TypeError",
            Self::Range(_) => "RangeError",
            Self::Bundle(_) => "MemoryBundleValidationError",
            _ => "Error",
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
    operations: Vec<(PathBuf, Arc<Mutex<()>>)>,
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
pub(crate) type OperationScope<'a> = Vec<OperationGuard<'a>>;
impl<'a> OperationGuard<'a> {
    pub(super) fn enter(operation: &'a Mutex<()>) -> Result<Self> {
        let key = std::ptr::from_ref(operation) as usize;
        if HELD_OPERATIONS.with(|held| held.borrow().contains(&key)) {
            return Ok(Self { outer: None, key });
        }
        #[cfg(test)]
        lock_test_hook::notify(key, false);
        let outer = operation
            .lock()
            .map_err(|_| FileMemoryError::Invalid("file memory operation mutex poisoned".into()))?;
        HELD_OPERATIONS.with(|held| held.borrow_mut().insert(key));
        let guard = Self {
            outer: Some(outer),
            key,
        };
        #[cfg(test)]
        lock_test_hook::notify(key, true);
        Ok(guard)
    }
}
impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        if self.outer.is_some() {
            HELD_OPERATIONS.with(|held| held.borrow_mut().remove(&self.key));
        }
    }
}
/// Thread-local hooks run at actual mutex acquisition points, never on a
/// same-thread reentry. Production builds contain neither hooks nor callbacks.
/// `pub(crate)` since #1036: the two-thread dream tail-race regression
/// (compiled as a `#[path]` module of the crate root) uses the same hooks.
#[cfg(test)]
pub(crate) mod lock_test_hook {
    use super::*;
    pub struct Event {
        pub key: usize,
        pub acquired: bool,
        pub held: Vec<usize>,
    }
    type Hook = std::rc::Rc<dyn Fn(Event)>;
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::default();
    }
    pub fn set(hook: impl Fn(Event) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(std::rc::Rc::new(hook)));
    }
    pub(super) fn notify(key: usize, acquired: bool) {
        let hook = HOOK.with(|slot| slot.borrow().clone());
        if let Some(hook) = hook {
            let held = HELD_OPERATIONS.with(|held| held.borrow().iter().copied().collect());
            hook(Event {
                key,
                acquired,
                held,
            });
        }
    }
}
pub(crate) fn operation_lock(root: &Path) -> Result<Arc<Mutex<()>>> {
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
        options: FileMemoryStoreOptions,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        Self::with_clock_dependency(root, options, clock, None)
    }
    /// The built-in provider declares its global dependency before either
    /// root is locked. Root roles may overlap across repositories, so both
    /// must participate in the same path order, including construction.
    fn with_clock_dependency(
        root: impl AsRef<Path>,
        mut options: FileMemoryStoreOptions,
        clock: FileMemoryClock,
        dependency: Option<&FileMemoryStore>,
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
        let mut operations = vec![(core.root.clone(), operation_lock(&core.root)?)];
        if let Some(dependency) = dependency {
            operations.extend(dependency.operations.iter().cloned());
        }
        operations.sort_by(|a, b| a.0.cmp(&b.0));
        operations.dedup_by(|a, b| a.0 == b.0);
        let store = Self {
            declared_root,
            core,
            operations,
        };
        {
            let _guards = store.operation_scope()?;
            store.core.ensure_layout()?;
        }
        Ok(store)
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
        let _guards = self.operation_scope()?;
        self.core.execute(method, input)
    }
    /// Hold the complete built-in dependency set in normalized root order.
    /// Provider nesting reenters already-held locks. Every composite caller
    /// uses this same set, rather than adding a possibly lower global root
    /// after acquiring the workspace root. Keep all guards for the sequence.
    pub(crate) fn operation_scope(&self) -> Result<OperationScope<'_>> {
        self.operations
            .iter()
            .map(|(_, lock)| OperationGuard::enter(lock))
            .collect()
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
    /// The `Date.now()` / `Math.random()` halves of every sibling temp path
    /// name (`sqlite.ts:422`); see [`Self::with_temp_name_seams`].
    pub(super) temp_now_ms: TempNowMs,
    pub(super) temp_random_fragment: TempRandomFragment,
    /// #917 fix-r1: the session-clear generation survives a `close`, so a
    /// heartbeat whose L0 recheck can no longer read SQLite still learns that a
    /// `clearSession` completed after the run listed its rows. Only
    /// `clearSession` bumps it; `clear(current_project)` / `clear(all)` do not.
    /// fix-r2: the generation means "the clear completed" — it advances only
    /// after the session's L0 rows are actually deleted, never while a failed
    /// clear might still leave them behind. fix-r3 (review P0): each clear also
    /// records the ids of the L0 rows it deleted, so the heartbeat's recheck
    /// discriminates per row instead of discarding every row of the session —
    /// a turn captured after a completed clear is new data, not cleared data.
    pub(super) session_clears: SessionClears,
    /// #917 fix-r2: the `clearSession` seam for the concurrency matrix.
    /// Production never installs a hook.
    pub(super) clear_session_hook: ClearSessionHookSlot,
}

/// #917 fix-r2: deterministic stage names for the `clearSession` seam, the
/// storage-side twin of [`HeartbeatStage`](super::heartbeat::indexer::HeartbeatStage).
/// The callback runs synchronously inside `clear_session_memory_data`'s
/// snapshot scope and may abort the clear by returning an error, which stands
/// in for the mid-clear storage failure the #917 fix-r2 regression needs (the
/// clear generation must not have advanced at that point).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearSessionStage {
    /// After the empty-session guard, before the L0 count and before any data
    /// is removed (the pre-fix-r2 bump position): the clear has committed to
    /// removing the session but has not removed a single row yet.
    BeforeRemoveData,
    /// #917 fix-r3 (review P2): after the session's memory files are removed
    /// by `delete_memory_entries` but before its L0 rows are selected and
    /// deleted (the "bump after the file removal, before the L0 DELETE"
    /// half-fix position). A hook that parks here observes the partial-clear
    /// state; one that errors aborts with the files already gone.
    BeforeDeleteL0,
    /// #1207: after the session's L0 row ids are selected and before the
    /// DELETE that removes them, with the connection guard held — the
    /// guarded segment the selection and the deletion must share. A hook
    /// that parks here keeps the guard open (a concurrent capture blocks
    /// until the clear releases it); one that errors aborts the clear with
    /// the L0 rows still present and the generation untouched. A hook must
    /// not touch the same store's connection here: the conn mutex is not
    /// reentrant on one thread, so it would self-deadlock. Production
    /// never installs a hook.
    L0IdsSelected,
}

#[doc(hidden)]
pub type ClearSessionHook = std::sync::Arc<
    dyn Fn(ClearSessionStage, &str) -> std::result::Result<(), FileMemoryError> + Send + Sync,
>;

type ClearSessionHookSlot = std::sync::Mutex<Option<ClearSessionHook>>;

/// #917 fix-r1: in-memory per-store clear counter. `generation` is this
/// `EdgeClawFileStores`' monotone counter; `sessions` records the value each
/// cleared session key last reached together with the generation at which
/// each of its clears deleted a given L0 row. A reader that captured a
/// baseline before a clear observes a different value after it, without
/// touching SQLite.
pub(crate) struct SessionClears {
    generation: std::sync::atomic::AtomicU64,
    sessions: std::sync::Mutex<std::collections::HashMap<String, SessionClearState>>,
}
/// #917 fix-r3 (review P0): the rows a session's clears deleted, mapped to
/// the generation of the clear that deleted each one. #1207: the per-row
/// generation replaces an accumulating set — membership alone cannot tell
/// *when* a row was deleted, so once any clear of the session had touched a
/// row id, every later unrelated clear made that id read as "cleared since" a
/// baseline that already predates the deleting clear (a re-captured turn was
/// silently dropped). A row deleted by an earlier clear can no longer be
/// listed, so each id's entry is only ever re-stamped by a later clear that
/// deletes the row again.
struct SessionClearState {
    generation: u64,
    removed: std::collections::HashMap<String, u64>,
}
impl SessionClears {
    pub(crate) fn new() -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::new(0),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
    /// Record that `clearSession` finished removing this session's data
    /// (#917 fix-r2: only after the L0 rows are gone, so a clear that fails
    /// midway leaves the generation — and the rows — untouched; fix-r3: the
    /// rows it deleted are recorded with the bump, in one map insert, so a
    /// reader sees both or neither; #1207: each deleted id is stamped with
    /// this bump's generation, so the record can answer "deleted *after* the
    /// reader's baseline", not merely "deleted at some point").
    pub(crate) fn bump(&self, session: &str, removed: &[String]) {
        let next = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = sessions
            .entry(session.to_string())
            .or_insert(SessionClearState {
                generation: 0,
                removed: std::collections::HashMap::new(),
            });
        state.generation = next;
        for id in removed {
            state.removed.insert(id.clone(), next);
        }
    }
    /// The generation this session key last reached (0 when never cleared).
    pub(crate) fn generation_of(&self, session: &str) -> u64 {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .map(|state| state.generation)
            .unwrap_or(0)
    }
    /// #917 fix-r3 (review P0): did a clear complete after `baseline` and
    /// delete this exact L0 row? The generation and the removed map are read
    /// under one lock, so the answer is atomic with the bump. A clear that
    /// completed after the baseline but never deleted this row (the row was
    /// captured after the clear finished) answers false: that turn is new
    /// data, not cleared data. #1207: a clear that deleted this row *before*
    /// the baseline (`cleared_at <= baseline` — the row was re-captured, or
    /// the baseline was read after that clear) answers false as well: the
    /// reader's own listing of the row is not stale, so persisting it is not
    /// a resurrection.
    pub(crate) fn removed_since(&self, session: &str, baseline: u64, l0_index_id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .is_some_and(|state| {
                state
                    .removed
                    .get(l0_index_id)
                    .is_some_and(|cleared_at| *cleared_at > baseline)
            })
    }
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
        // sqlite.ts creates these directories before constructing either file
        // store. Besides ordering side effects, this preserves mkdir's first
        // failing path when rootDir is occupied by a file (#778).
        if let Some(parent) = db.as_ref().parent().filter(|p| !p.as_os_str().is_empty()) {
            io_mkdir(parent)?;
        }
        io_mkdir(&memory)?;
        io_mkdir(&global)?;
        let storage = if crate::mutants::on("native-database-open-late") {
            None
        } else {
            Some(EdgeClawStorage::connect(&db, clock.clone())?)
        };
        let global_user_memory = Arc::new(FileMemoryStore::with_clock(
            global,
            repository_global_options(),
            clock.clone(),
        )?);
        let provider = global_user_memory.clone();
        let workspace_memory = FileMemoryStore::with_clock_dependency(
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
            Some(&global_user_memory),
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
        let storage = match storage {
            Some(storage) => storage,
            None => EdgeClawStorage::connect(db, clock)?,
        };
        storage.initialize()?;
        Ok(Self {
            repository,
            storage,
            workspace_memory,
            global_user_memory,
            temp_now_ms: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64
            }),
            temp_random_fragment: Arc::new(default_temp_fragment),
            session_clears: SessionClears::new(),
            clear_session_hook: std::sync::Mutex::new(None),
        })
    }
    /// Override the sibling temp-path naming seams. `sqlite.ts:422` reads
    /// `Date.now()` and one `Math.random()` draw per path, so replay hosts
    /// inject the frozen clock and the exact base-36 fragment of each draw.
    pub fn with_temp_name_seams(
        mut self,
        now_ms: TempNowMs,
        random_fragment: TempRandomFragment,
    ) -> Self {
        self.temp_now_ms = now_ms;
        self.temp_random_fragment = random_fragment;
        self
    }
}

/// Production `Math.random().toString(36).slice(2, 8)`.
fn default_temp_fragment() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect()
}
pub(crate) fn now_iso() -> String {
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
        self.checked_path(relative).map(|_| true)
    }
    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {
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
            Ok(path) if path.starts_with(&root) => Ok((root, path)),
            Ok(_) => Err(denied()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound) => Err(denied()),
            Err(e) => Err(crate::node_io::path_error(e, "realpath", &path).into()),
        }
    }
    fn mutation_error(
        &self,
        relative: &str,
        syscall: &'static str,
        error: std::io::Error,
    ) -> FileMemoryError {
        self.mutation_error_at(relative, None, syscall, error)
    }
    /// Like [`mutation_error`], with an explicit display path for the syscall's
    /// own target. TS's `ensureDir(dirname(absolutePath))` reports mkdir on the
    /// parent directory, not on the file being written.
    fn mutation_error_at(
        &self,
        relative: &str,
        display: Option<&Path>,
        syscall: &'static str,
        error: std::io::Error,
    ) -> FileMemoryError {
        if crate::project_files::nofollow_loop(&error) {
            FileMemoryError::Invalid(format!("Memory entry not found: {relative}"))
        } else {
            let display = display.map_or_else(
                // Normalize like the old path-based display: join("") would add
                // a trailing separator and "./" would stay verbatim.
                || absolute(&self.root.join(relative)).unwrap_or_else(|_| self.root.join(relative)),
                |path| path.to_path_buf(),
            );
            crate::node_io::path_error(error, syscall, &display).into()
        }
    }
    /// Check and resolve ordinary in-root links, then pin the root before the
    /// test seam (or another process) can replace a checked descendant.
    #[cfg(unix)]
    fn mutation_target(&self, relative: &str) -> Result<(fs::File, PathBuf)> {
        let (root, target) = self.checked_path(relative)?;
        let directory = crate::project_files::open_directory_beneath(&root, &root)
            .map_err(|e| self.mutation_error(relative, "open", e))?;
        let target = target
            .strip_prefix(root)
            .expect("checked boundary")
            .to_path_buf();
        #[cfg(test)]
        mutation_test_hook::notify(relative);
        Ok((directory, target))
    }
    /// The read leg of [`Self::mutation_target`]: pin the same root below the
    /// boundary check, then read the canonical target without following a
    /// link raced in after that check. The pin is opened for reading (not
    /// only search) so an empty relative can enumerate the root itself.
    #[cfg(unix)]
    fn read_target(&self, relative: &str) -> Result<(fs::File, PathBuf)> {
        let (root, target) = self.checked_path(relative)?;
        // checked_path resolves a valid root symlink as the trust boundary.
        // Probe that resolved root without following another link, matching
        // the NOFOLLOW pin below. Missing, dangling, or non-directory roots
        // still mean "no file", like the exists/read pair this replaces.
        if !fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
            return Err(FileMemoryError::Io(std::io::Error::from_raw_os_error(
                libc::ENOENT,
            )));
        }
        let directory = crate::project_files::open_beneath(&root, &root)
            .map_err(|e| self.mutation_error(relative, "open", e))?;
        let target = target
            .strip_prefix(root)
            .expect("checked boundary")
            .to_path_buf();
        #[cfg(test)]
        read_test_hook::notify(relative);
        Ok((directory, target))
    }
    /// Read a memory file below the pinned root. A missing file is `None`,
    /// like the exists/read pair it replaces; every other failure keeps the
    /// measured Node open/read diagnostics for the display path.
    fn read_file(&self, relative: &str) -> Result<Option<String>> {
        #[cfg(unix)]
        {
            let (root, target) = match self.read_target(relative) {
                Ok(pinned) => pinned,
                // Includes the missing or dangling memory root from the probe.
                Err(FileMemoryError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };
            let bytes = match crate::project_files::read_at(&root, &target) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                // Opening a directory succeeds on POSIX, like TS readFile.
                Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => {
                    return Err(crate::node_io::descriptor_error(e, "read").into());
                }
                Err(e) => return Err(self.mutation_error(relative, "open", e)),
                Ok(bytes) => bytes,
            };
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
        // Non-Unix keeps the origin/main exists/read pair on the checked path.
        #[cfg(not(unix))]
        {
            if !self.within(relative)? {
                return Ok(None);
            }
            let path = self.path(relative)?;
            if !path.exists() {
                return Ok(None);
            }
            Ok(Some(read(&path)?))
        }
    }
    fn mkdir(&self, relative: &str) -> Result<()> {
        #[cfg(unix)]
        {
            let (root, target) = self.mutation_target(relative)?;
            crate::project_files::create_directories_at(
                &root,
                &target,
                rustix::fs::Mode::from_raw_mode(0o777),
            )
            .map_err(|e| self.mutation_error(relative, "mkdir", e))
        }
        // Non-Unix keeps the origin/main path-based behavior.
        #[cfg(not(unix))]
        {
            io_mkdir(self.path(relative)?)?;
            Ok(())
        }
    }
    fn write_bytes(&self, relative: &str, content: impl AsRef<[u8]>) -> Result<()> {
        #[cfg(unix)]
        {
            let (root, target) = self.mutation_target(relative)?;
            // A root-addressed relative has no parent: Node's writeFileSync on
            // a directory reports EISDIR at open, not a panic here.
            let parent = target.parent().ok_or_else(|| {
                self.mutation_error(
                    relative,
                    "open",
                    std::io::Error::from_raw_os_error(libc::EISDIR),
                )
            })?;
            let display = self.parent_display(parent);
            crate::project_files::create_directories_at(
                &root,
                parent,
                rustix::fs::Mode::from_raw_mode(0o777),
            )
            .map_err(|e| self.mutation_error_at(relative, Some(&display), "mkdir", e))?;
            crate::project_files::overwrite_at(&root, &target, content.as_ref())
                .map_err(|e| self.mutation_error(relative, "open", e))
        }
        // Non-Unix keeps the origin/main path-based behavior.
        #[cfg(not(unix))]
        {
            let target = self.path(relative)?;
            io_mkdir(target.parent().expect("absolute parent"))?;
            io_write(target, content)?;
            Ok(())
        }
    }
    fn unlink(&self, relative: &str) -> Result<()> {
        #[cfg(unix)]
        {
            self.within(relative)?;
            // unlink removes an allowed leaf symlink, not its canonical target.
            let lexical = Path::new(relative);
            if lexical.parent().is_none() {
                // "" addresses the memory root itself; Node unlinkSync(2) on a
                // directory reports EPERM (EISDIR on Linux) against the root path.
                return self.unlink_root();
            }
            let parent = lexical.parent().expect("non-root parent");
            let (canonical, target) = self.checked_path(parent.to_str().expect("UTF-8 path"))?;
            let root = crate::project_files::open_directory_beneath(&canonical, &canonical)
                .map_err(|e| self.mutation_error(relative, "unlink", e))?;
            let target = target
                .strip_prefix(canonical)
                .expect("checked boundary")
                .join(lexical.file_name().ok_or_else(|| {
                    FileMemoryError::Invalid(format!("Memory entry not found: {relative}"))
                })?);
            #[cfg(test)]
            mutation_test_hook::notify(relative);
            crate::project_files::unlink_at(&root, &target)
                .map_err(|e| self.mutation_error(relative, "unlink", e))
        }
        // Non-Unix keeps the origin/main unlink: a root-addressed remove_file
        // reports the platform's directory error, like unlink_root.
        #[cfg(not(unix))]
        {
            self.within(relative)?;
            let target = self.path(relative)?;
            fs::remove_file(&target)
                .map_err(|e| crate::node_io::path_error(e, "unlink", &target))?;
            Ok(())
        }
    }
    /// The display path of a mutation's parent directory, used by
    /// ensureDir-style diagnostics. An empty relative denotes the root itself,
    /// without the trailing separator that join("") would introduce.
    fn parent_display(&self, parent: &Path) -> PathBuf {
        let joined = if parent.as_os_str().is_empty() {
            self.root.clone()
        } else {
            self.root.join(parent)
        };
        absolute(&joined).unwrap_or(joined)
    }
    /// Unlink the pinned memory root itself, keeping Node's directory error.
    #[cfg(unix)]
    fn unlink_root(&self) -> Result<()> {
        let parent = self
            .root
            .parent()
            .ok_or_else(|| FileMemoryError::Invalid("Cannot unlink filesystem root".into()))?;
        let canonical = fs::canonicalize(parent)?;
        let directory = crate::project_files::open_directory_beneath(&canonical, &canonical)
            .map_err(|e| self.mutation_error("", "unlink", e))?;
        #[cfg(test)]
        mutation_test_hook::notify("");
        crate::project_files::unlink_at(
            &directory,
            Path::new(self.root.file_name().expect("root name")),
        )
        .map_err(|e| self.mutation_error("", "unlink", e))
    }
    fn ensure_layout(&self) -> Result<()> {
        #[cfg(unix)]
        {
            // Bootstrap a missing trusted root from its nearest existing canonical
            // ancestor, using #596's mkdirat walk rather than recursive path mkdir.
            let mut ancestor = self.root.as_path();
            while !ancestor.exists() {
                ancestor = ancestor.parent().ok_or_else(|| {
                    FileMemoryError::Invalid("Memory root has no existing ancestor".into())
                })?;
            }
            let canonical = fs::canonicalize(ancestor)?;
            let directory = crate::project_files::open_directory_beneath(&canonical, &canonical)?;
            crate::project_files::create_directories_at(
                &directory,
                self.root.strip_prefix(ancestor).expect("ancestor"),
                rustix::fs::Mode::from_raw_mode(0o777),
            )
            .map_err(|e| self.mutation_error("", "mkdir", e))?;
        }
        // Non-Unix keeps the origin/main path-based recursive mkdir.
        #[cfg(not(unix))]
        io_mkdir(&self.root)?;
        if self.options.manage_user_profile {
            // An empty configured path is absent in TS (falsy) and is skipped,
            // instead of panicking on its missing parent.
            if let Some(path) = self
                .options
                .user_profile_relative_path
                .as_deref()
                .filter(|path| !path.is_empty())
            {
                // Retain validation of the full configured path, not only its parent.
                self.within(path)?;
                self.mkdir(
                    Path::new(path)
                        .parent()
                        .unwrap_or(Path::new(""))
                        .to_str()
                        .expect("UTF-8 path"),
                )?;
            }
            if let Some(path) = &self.options.user_notes_relative_dir {
                self.mkdir(path)?;
            }
        }
        if self.options.manage_project_files {
            for dir in [GENERAL_PROJECT_MEMORY_DIR, GENERAL_FEEDBACK_MEMORY_DIR] {
                self.mkdir(dir)?;
            }
        }
        if self.options.manage_project_meta && self.general() {
            self.mkdir(GENERAL)?;
        }
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        #[cfg(unix)]
        {
            // The root is a trusted input and TS unlinks a root symlink itself.
            // Pin its parent, then remove the lexical leaf without following links.
            let parent = self
                .root
                .parent()
                .ok_or_else(|| FileMemoryError::Invalid("Cannot clear filesystem root".into()))?;
            let canonical = fs::canonicalize(parent)?;
            let directory = crate::project_files::open_directory_beneath(&canonical, &canonical)?;
            #[cfg(test)]
            mutation_test_hook::notify("");
            crate::project_files::remove_tree_at(
                &directory,
                Path::new(self.root.file_name().expect("root name")),
                &self.root,
            )
            .map_err(|e| self.mutation_error("", "rmdir", e))
        }
        // Non-Unix keeps the origin/main node_fs::rm_sync removal.
        #[cfg(not(unix))]
        {
            super::node_fs::rm_sync(&self.root)?;
            Ok(())
        }
    }
    fn parsed(&self, path: &str) -> Result<Option<(JsObject, String)>> {
        let Some(raw) = self.read_file(path)? else {
            return Ok(None);
        };
        // The clock is read only for the `updated_at` fallback, like TS's
        // lazy `??` operand, so a progressing clock advances identically.
        Ok(parse(&raw, || self.now()))
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
        let mut paths: Vec<String> = self
            .read_dir_files(dir)?
            .into_iter()
            .filter(|file| {
                file.ends_with(".md") && file != "MEMORY.md" && file != "project.meta.md"
            })
            .collect();
        paths.sort_by_key(|p| p.encode_utf16().collect::<Vec<_>>());
        let mut records = vec![];
        for file in paths {
            if let Some(entry) = self.entry(&format!("{dir}/{file}"))? {
                records.push(entry);
            }
        }
        Ok(records)
    }
    /// The regular files directly below a memory directory, in readdir order.
    /// Unix walks from the pinned root without following a raced-in link and
    /// reports the directory itself on failures, like the scandir it replaces.
    fn read_dir_files(&self, dir: &str) -> Result<Vec<String>> {
        #[cfg(unix)]
        {
            let (root, target) = match self.read_target(dir) {
                Ok(pinned) => pinned,
                // Includes the missing or dangling memory root from the probe.
                Err(FileMemoryError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(vec![]);
                }
                Err(e) => return Err(e),
            };
            match crate::project_files::regular_files_at(&root, &target) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
                Err(e) => Err(self.mutation_error(dir, "scandir", e)),
                Ok(files) => Ok(files),
            }
        }
        // Non-Unix keeps the origin/main exists/read_dir pair.
        #[cfg(not(unix))]
        {
            let path = self.root.join(dir);
            if !path.exists() {
                return Ok(vec![]);
            }
            let mut paths = vec![];
            for entry in
                fs::read_dir(&path).map_err(|e| crate::node_io::path_error(e, "scandir", &path))?
            {
                let entry = entry.map_err(|e| crate::node_io::path_error(e, "scandir", &path))?;
                if entry
                    .file_type()
                    .map_err(|e| crate::node_io::path_error(e, "scandir", &path))?
                    .is_file()
                {
                    paths.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
            Ok(paths)
        }
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
            entries.extend(self.directory(GENERAL_PROJECT_MEMORY_DIR)?);
            entries.extend(self.directory(GENERAL_FEEDBACK_MEMORY_DIR)?);
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
        self.records_raw(ids, &JsValue::Number(max_lines))
    }
    fn records_raw(&self, ids: &[String], max_lines: &JsValue) -> Result<Vec<JsObject>> {
        let mut records = vec![];
        for id in ids {
            let Some(mut e) = self.entry(id)? else {
                continue;
            };
            let Some((_, body)) = self.parsed(id)? else {
                continue;
            };
            // `trimContentLines` is reached only after both file reads succeed.
            let max_lines = if crate::mutants::on("file-get-maxlines-skip") {
                80.0
            } else {
                super::storage::overview::to_number(max_lines)?
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
        self.write_bytes(path, render(fm, body))?;
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
        let Some(raw) = self.read_file("project.meta.md")? else {
            return Ok(None);
        };
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
            (
                "absolutePath",
                s(self.path("project.meta.md")?.to_string_lossy()),
            ),
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
        self.write_bytes(
            "project.meta.md",
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
            let prev = self.read_file("MEMORY.md")?.unwrap_or_default();
            if next != prev {
                self.write_bytes("MEMORY.md", next)?;
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
                GENERAL_FEEDBACK_MEMORY_DIR
            } else {
                GENERAL_PROJECT_MEMORY_DIR
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
                // Defaulting does not convert: missing records never inspect maxLines.
                let max_lines = match input.get("maxLines") {
                    None | Some(JsValue::Undefined) => JsValue::Number(80.0),
                    Some(value) if crate::mutants::on("file-memory-nonnumber-maxlines") => {
                        JsValue::Number(value.as_number().unwrap_or(80.0))
                    }
                    Some(value) => value.clone(),
                };
                if crate::mutants::on("file-get-maxlines-early") {
                    super::storage::overview::to_number(&max_lines)?;
                }
                let undefined = JsValue::Undefined;
                let ids = match input.get("ids").unwrap_or(&undefined) {
                    JsValue::Array(ids) => ids,
                    _ if crate::mutants::on("file-get-ids-unchecked") => {
                        return Ok(objects(vec![]));
                    }
                    JsValue::Undefined => {
                        return Err(FileMemoryError::Type(
                            "Cannot read properties of undefined (reading 'map')".into(),
                        ));
                    }
                    JsValue::Null => {
                        return Err(FileMemoryError::Type(
                            "Cannot read properties of null (reading 'map')".into(),
                        ));
                    }
                    _ => return Err(FileMemoryError::Type("ids.map is not a function".into())),
                };
                let mut result = vec![];
                for id in ids.iter().filter_map(JsValue::as_string) {
                    if let Some(mut record) = self
                        .records_raw(&[String::from_utf16_lossy(id.units())], &max_lines)?
                        .into_iter()
                        .next()
                    {
                        self.api_path(&mut record, id);
                        result.push(record);
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
                        self.unlink(&path)?;
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
                self.clear()?;
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
                            self.unlink(&path)?;
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
                // A listed entry that vanishes before the read keeps Node's
                // own open diagnostics, like the read it replaces.
                let raw = self.read_file(path)?.ok_or_else(|| {
                    self.mutation_error(
                        path,
                        "open",
                        std::io::Error::from_raw_os_error(libc::ENOENT),
                    )
                })?;
                Ok(obj(&[("relativePath", s(path)), ("content", s(raw))]))
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

/// Per-thread, path-filtered seam after validation and root pinning. RAII
/// removes callbacks even when an assertion panics; tests never share hooks.
#[cfg(test)]
mod mutation_test_hook {
    use super::*;
    type Hook = Box<dyn Fn(&str)>;
    thread_local! { static HOOK: RefCell<Option<Hook>> = RefCell::default(); }
    pub struct Guard;
    pub fn set(hook: impl Fn(&str) + 'static) -> Guard {
        HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        Guard
    }
    pub fn notify(path: &str) {
        HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook(path);
            }
        });
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// The read leg of `mutation_test_hook`: the same per-thread, path-filtered
/// seam after validation and root pinning, for reads and scandirs. A separate
/// slot keeps mutation races and read races independently reproducible.
#[cfg(test)]
mod read_test_hook {
    use super::*;
    type Hook = Box<dyn Fn(&str)>;
    thread_local! { static HOOK: RefCell<Option<Hook>> = RefCell::default(); }
    pub struct Guard;
    pub fn set(hook: impl Fn(&str) + 'static) -> Guard {
        HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        Guard
    }
    pub fn notify(path: &str) {
        HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook(path);
            }
        });
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

#[cfg(all(test, unix))]
mod mutation_security_tests {
    use super::*;
    use std::{
        cell::Cell,
        os::unix::fs::symlink,
        rc::Rc,
        time::{Duration, Instant},
    };
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("pdrs-597-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(fs::canonicalize(p).unwrap())
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // A failing assertion (negative control) can leave 0300
            // directories remove_dir_all cannot remove; restore first,
            // like the io_replay fixture's Scratch.
            fn restore(path: &Path) {
                use std::os::unix::fs::PermissionsExt;
                if path.is_symlink() {
                    return;
                }
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        restore(&entry.path());
                    }
                }
            }
            restore(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn store(root: &Path) -> FileMemoryStore {
        FileMemoryStore::new(
            root,
            FileMemoryStoreOptions {
                enable_manifest: false,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn install(
        root: &Path,
        outside: &Path,
        target: &str,
        component: &str,
    ) -> (mutation_test_hook::Guard, Rc<Cell<bool>>) {
        let fired = Rc::new(Cell::new(false));
        let (root, outside, target, component) = (
            root.to_owned(),
            outside.to_owned(),
            target.to_owned(),
            component.to_owned(),
        );
        let flag = fired.clone();
        let guard = mutation_test_hook::set(move |path| {
            if path != target || flag.replace(true) {
                return;
            }
            fs::rename(root.join(&component), root.join("saved")).unwrap();
            symlink(&outside, root.join(&component)).unwrap();
        });
        (guard, fired)
    }
    /// The read-seam twin of [`install`]: the same component swap, fired from
    /// `read_test_hook` so read races stay independently reproducible.
    fn install_read(
        root: &Path,
        outside: &Path,
        target: &str,
        component: &str,
    ) -> (read_test_hook::Guard, Rc<Cell<bool>>) {
        let fired = Rc::new(Cell::new(false));
        let (root, outside, target, component) = (
            root.to_owned(),
            outside.to_owned(),
            target.to_owned(),
            component.to_owned(),
        );
        let flag = fired.clone();
        let guard = read_test_hook::set(move |path| {
            if path != target || flag.replace(true) {
                return;
            }
            fs::rename(root.join(&component), root.join("saved")).unwrap();
            symlink(&outside, root.join(&component)).unwrap();
        });
        (guard, fired)
    }
    fn denied(error: FileMemoryError, path: &str) {
        assert!(
            matches!(error, FileMemoryError::Invalid(_)),
            "unexpected error: {error:?}"
        );
        // The error message anchor is generated by the real TS editEntry call.
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/edgeclaw-memory-write/oracle.json"
        ))
        .unwrap();
        assert_eq!(
            error.to_string(),
            oracle["missingMessage"]
                .as_str()
                .unwrap()
                .replace("missing", path)
        );
    }
    #[test]
    fn mutation_matches_real_ts_writes_and_unlink() {
        let raw = include_str!("../../../../tests/fixtures/edgeclaw-memory-write/oracle.json");
        crate::test_baseline::oracle(raw);
        let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
        let scratch = Scratch::new();
        let mut store = store(&scratch.0.join("memory"));
        store.core.clock = Arc::new(|| "2026-10-22T00:00:00.000Z".into());
        for write in oracle["writes"].as_array().unwrap() {
            let path = write["relativePath"].as_str().unwrap();
            if path.ends_with("alias.md") {
                symlink("note.md", store.core.root.join(path)).unwrap();
            }
            let fm = pilotdeck_protocol::parse_json(&write["frontmatter"].to_string()).unwrap();
            store
                .core
                .write(
                    path,
                    fm.as_object().unwrap(),
                    write["body"].as_str().unwrap(),
                )
                .unwrap();
            assert_eq!(
                fs::read_to_string(store.core.root.join(path)).unwrap(),
                write["bytes"].as_str().unwrap(),
                "real TS bytes: {path}"
            );
        }
        store
            .delete_entries(&obj(&[(
                "relativePaths",
                strings(&["Project/nested/alias.md".into()]),
            )]))
            .unwrap();
        assert!(!store.core.root.join("Project/nested/alias.md").is_symlink());
        assert_eq!(
            fs::read_to_string(store.core.root.join("Project/nested/note.md")).unwrap(),
            oracle["afterUnlink"].as_str().unwrap()
        );
        // Replay the generator's clear leg: the oracle's clearSentinel proves
        // TS's rmSync did not follow the in-root link, and neither do we.
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), "outside sentinel").unwrap();
        symlink(&outside, store.core.root.join("Project").join("outside")).unwrap();
        store
            .clear_all_data(&obj(&[("rebuildManifest", JsValue::Bool(false))]))
            .unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).unwrap(),
            oracle["clearSentinel"].as_str().unwrap()
        );
        assert!(store.core.root.join("Project").is_dir());
    }
    #[test]
    fn mutation_race_write() {
        for existing in [false, true] {
            let scratch = Scratch::new();
            let store = store(&scratch.0.join("memory"));
            let root = &store.core.root;
            let outside = scratch.0.join("outside");
            fs::create_dir(&outside).unwrap();
            let path = "Project/new.md";
            if existing {
                fs::write(root.join(path), "inside").unwrap();
            }
            fs::write(outside.join("new.md"), "outside sentinel").unwrap();
            let (_hook, fired) = install(root, &outside, path, "Project");
            let result = store.core.write(path, &JsObject::new(), "replacement");
            assert!(fired.get(), "target-filtered hook did not fire");
            assert_eq!(
                fs::read_to_string(outside.join("new.md")).unwrap(),
                "outside sentinel",
                "race wrote outside memory root"
            );
            denied(result.unwrap_err(), path);
            assert!(!outside.join("MEMORY.md").exists());
        }
    }
    #[test]
    fn mutation_race_new_file() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let (_hook, fired) = install(
            &store.core.root,
            &outside,
            "Project/nested/new.md",
            "Project",
        );
        let result = store
            .core
            .write("Project/nested/new.md", &JsObject::new(), "new");
        assert!(fired.get());
        assert!(
            !outside.join("nested").exists(),
            "race created directories outside memory root"
        );
        denied(result.unwrap_err(), "Project/nested/new.md");
    }
    #[test]
    fn mutation_race_leaf_write() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::write(&outside, "outside sentinel").unwrap();
        // A file leaf swap, independent of the directory-walk test.
        let root = store.core.root.clone();
        let victim = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = mutation_test_hook::set(move |path| {
            if path == "project.meta.md" && !flag.replace(true) {
                symlink(&victim, root.join(path)).unwrap();
            }
        });
        let result = store
            .core
            .upsert_meta(&obj(&[("projectName", s("Race"))]), false);
        assert!(fired.get());
        assert_eq!(
            fs::read_to_string(outside).unwrap(),
            "outside sentinel",
            "meta wrote through leaf link"
        );
        denied(result.unwrap_err(), "project.meta.md");
    }
    #[test]
    fn mutation_race_manifest() {
        let scratch = Scratch::new();
        let mut store = store(&scratch.0.join("memory"));
        store.core.options.enable_manifest = true;
        let outside = scratch.0.join("outside");
        fs::write(&outside, "outside sentinel").unwrap();
        let root = store.core.root.clone();
        let victim = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = mutation_test_hook::set(move |path| {
            if path == "MEMORY.md" && !flag.replace(true) {
                symlink(&victim, root.join(path)).unwrap();
            }
        });
        let result = store.repair_manifests();
        assert!(fired.get());
        assert_eq!(
            fs::read_to_string(outside).unwrap(),
            "outside sentinel",
            "manifest wrote through leaf link"
        );
        denied(result.unwrap_err(), "MEMORY.md");
    }
    #[test]
    fn mutation_race_mkdir() {
        let scratch = Scratch::new();
        let mut store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        store.core.options.user_notes_relative_dir = Some("Project/nested".into());
        let (_hook, fired) = install(&store.core.root, &outside, "Project/nested", "Project");
        let result = store.core.ensure_layout();
        assert!(fired.get());
        assert!(
            !outside.join("nested").exists(),
            "layout mkdir escaped memory root"
        );
        denied(result.unwrap_err(), "Project/nested");
    }
    #[test]
    fn mutation_race_delete() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(store.core.root.join("Project/victim.md"), "inside").unwrap();
        fs::write(outside.join("victim.md"), "outside sentinel").unwrap();
        let (_hook, fired) = install(&store.core.root, &outside, "Project/victim.md", "Project");
        let result = store.delete_entries(&obj(&[(
            "relativePaths",
            strings(&["Project/victim.md".into()]),
        )]));
        assert!(fired.get());
        assert_eq!(
            fs::read_to_string(outside.join("victim.md")).unwrap(),
            "outside sentinel",
            "delete escaped memory root"
        );
        denied(result.unwrap_err(), "Project/victim.md");
    }
    #[test]
    fn mutation_race_clear_parent() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("holder/memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir_all(outside.join("memory")).unwrap();
        fs::write(outside.join("memory/victim.md"), "outside sentinel").unwrap();
        let (_hook, fired) = install(&scratch.0, &outside, "", "holder");
        let result = store.core.clear();
        assert!(fired.get());
        assert_eq!(
            fs::read_to_string(outside.join("memory/victim.md")).unwrap(),
            "outside sentinel",
            "clear escaped pinned parent"
        );
        result.unwrap();
        // The pinned parent handle followed the rename, so clear removed the
        // real (renamed) memory root instead of chasing the raced link target.
        assert!(!scratch.0.join("saved/memory").exists());
    }
    #[test]
    fn mutation_clear_does_not_follow_links() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim.md"), "outside sentinel").unwrap();
        let (_hook, fired) = install(&store.core.root, &outside, "", "Project");
        store
            .clear_all_data(&obj(&[("rebuildManifest", JsValue::Bool(false))]))
            .unwrap();
        assert!(fired.get());
        assert_eq!(
            fs::read_to_string(outside.join("victim.md")).unwrap(),
            "outside sentinel"
        );
        assert!(store.core.root.join("Project").is_dir());
        assert!(!store.core.root.join("Project").is_symlink());
    }
    #[test]
    fn mutation_write_rejects_a_hardlinked_leaf() {
        // TS writeRecord writes through a hardlink (real-store probe), which
        // would alias an outside inode into the memory root. Rust refuses the
        // write before truncating: diffs/A6.6-EDGECLAW-WRITE-INODE-001.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside.md");
        fs::write(&outside, "outside sentinel").unwrap();
        fs::hard_link(&outside, store.core.root.join("Project/hard.md")).unwrap();
        let error = store
            .core
            .write("Project/hard.md", &JsObject::new(), "changed")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, open '{}'",
                store.core.root.join("Project/hard.md").display()
            )
        );
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "outside sentinel",
            "write truncated a hardlinked outside inode"
        );
    }
    #[test]
    fn mutation_race_hardlink_write() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::write(&outside, "outside sentinel").unwrap();
        let root = store.core.root.clone();
        let victim = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = mutation_test_hook::set(move |path| {
            if path == "Project/hard.md" && !flag.replace(true) {
                fs::hard_link(&victim, root.join("Project/hard.md")).unwrap();
            }
        });
        let error = store
            .core
            .write("Project/hard.md", &JsObject::new(), "changed")
            .unwrap_err();
        assert!(fired.get(), "target-filtered hook did not fire");
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "outside sentinel",
            "race wrote through a swapped-in hardlink"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, open '{}'",
                store.core.root.join("Project/hard.md").display()
            )
        );
    }
    #[test]
    fn mutation_race_fifo_write() {
        // A no-reader FIFO open blocks (the TS behavior); with a held reader
        // the writer open succeeds, so only the inode recheck can refuse it.
        use std::os::unix::fs::OpenOptionsExt;
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let root = store.core.root.clone();
        let fifo = root.join("Project/fifo.md");
        let hooked = fifo.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let reader = Rc::new(std::cell::RefCell::new(None));
        let held = reader.clone();
        let _hook = mutation_test_hook::set(move |path| {
            if path == "Project/fifo.md" && !flag.replace(true) {
                std::process::Command::new("mkfifo")
                    .arg(&hooked)
                    .status()
                    .unwrap();
                *held.borrow_mut() = Some(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&hooked)
                        .unwrap(),
                );
            }
        });
        // write_bytes, not write(): a broken recheck would let this succeed,
        // and the wrapper's record read-back would then block on the FIFO.
        let error = store
            .core
            .write_bytes("Project/fifo.md", "changed")
            .unwrap_err();
        assert!(fired.get(), "target-filtered hook did not fire");
        assert_eq!(
            error.to_string(),
            format!("EACCES: permission denied, open '{}'", fifo.display())
        );
        use std::os::unix::fs::FileTypeExt;
        assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo());
    }
    #[test]
    fn mutation_pin_precedes_hook() {
        // The root handle is pinned before the test seam fires, so replacing
        // the root itself afterwards redirects nothing: the write lands in the
        // renamed directory and the raced link target stays untouched.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let root = store.core.root.clone();
        let moved = scratch.0.join("saved");
        let renamed = moved.clone();
        let linked = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = mutation_test_hook::set(move |path| {
            if path == "Project/pinned.md" && !flag.replace(true) {
                fs::rename(&root, &renamed).unwrap();
                symlink(&linked, &root).unwrap();
            }
        });
        store
            .core
            .write_bytes("Project/pinned.md", "pinned body")
            .unwrap();
        assert!(fired.get(), "target-filtered hook did not fire");
        assert_eq!(
            fs::read_to_string(moved.join("Project/pinned.md")).unwrap(),
            "pinned body"
        );
        assert!(
            !outside.join("Project").exists(),
            "write escaped through the raced root link"
        );
    }
    #[test]
    fn mutation_race_read_leaf() {
        // The read leg pins the same root the write leg does: a leaf swapped
        // for an outside symlink after the boundary check is not followed.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside.md");
        fs::write(&outside, "outside sentinel").unwrap();
        fs::write(store.core.root.join("Project/note.md"), "inside body").unwrap();
        let (_hook, fired) = install_read(
            &store.core.root,
            &outside,
            "Project/note.md",
            "Project/note.md",
        );
        let result = store.get_memory_records_by_ids(&obj(&[
            ("ids", strings(&["Project/note.md".into()])),
            ("maxLines", num(5000)),
        ]));
        assert!(fired.get(), "target-filtered hook did not fire");
        denied(result.unwrap_err(), "Project/note.md");
    }
    #[test]
    fn mutation_race_read_component() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(store.core.root.join("Project/note.md"), "inside body").unwrap();
        fs::write(outside.join("note.md"), "outside sentinel").unwrap();
        let (_hook, fired) = install_read(&store.core.root, &outside, "Project/note.md", "Project");
        let result = store.get_memory_records_by_ids(&obj(&[
            ("ids", strings(&["Project/note.md".into()])),
            ("maxLines", num(5000)),
        ]));
        assert!(fired.get(), "target-filtered hook did not fire");
        let error = result.unwrap_err();
        assert_eq!(error.io_code(), Some("ELOOP"), "{error:?}");
        assert_eq!(
            fs::read_to_string(outside.join("note.md")).unwrap(),
            "outside sentinel"
        );
    }
    #[test]
    fn mutation_read_parent_symlink_reports_eloop() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        fs::write(store.core.root.join("Project/note.md"), "inside body").unwrap();
        let root = store.core.root.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = read_test_hook::set(move |path| {
            if path == "Project/note.md" && !flag.replace(true) {
                fs::rename(root.join("Project"), root.join("saved")).unwrap();
                symlink("Project", root.join("Project")).unwrap();
            }
        });
        let error = store.core.read_file("Project/note.md").unwrap_err();
        assert!(fired.get(), "target-filtered hook did not fire");
        assert_eq!(error.io_code(), Some("ELOOP"), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!(
                "ELOOP: too many symbolic links encountered, open '{}'",
                store.core.root.join("Project/note.md").display()
            )
        );
        assert_eq!(
            fs::read_to_string(store.core.root.join("saved/note.md")).unwrap(),
            "inside body"
        );
    }
    #[test]
    fn mutation_read_follows_existing_symlink_root() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("directory");
        fs::create_dir(&directory).unwrap();
        let alias = scratch.0.join("memory");
        // Both a relative and an absolute root link are valid trust boundaries.
        for target in [Path::new("directory"), directory.as_path()] {
            symlink(target, &alias).unwrap();
            let store = store(&alias);
            fs::write(directory.join("Project/note.md"), "root link body").unwrap();
            assert!(
                store.core.root.is_symlink(),
                "exercise the lexical root link"
            );
            assert_eq!(
                store.core.read_file("Project/note.md").unwrap().as_deref(),
                Some("root link body")
            );
            let (pinned, relative) = store.core.read_target("Project/note.md").unwrap();
            assert_eq!(relative, Path::new("Project/note.md"));
            assert_eq!(
                crate::project_files::read_at(&pinned, &relative).unwrap(),
                b"root link body"
            );
            fs::remove_file(&alias).unwrap();
        }
    }
    #[test]
    fn mutation_race_scandir() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(store.core.root.join("Project/a.md"), "a").unwrap();
        fs::write(outside.join("b.md"), "outside sentinel").unwrap();
        let (_hook, fired) = install_read(&store.core.root, &outside, "Project", "Project");
        let result = store.list_memory_entries(&JsObject::new());
        assert!(fired.get(), "target-filtered hook did not fire");
        denied(result.unwrap_err(), "Project");
    }
    #[test]
    fn mutation_read_pin_precedes_hook() {
        // The read's root handle is pinned before the test seam fires, so
        // replacing the root itself afterwards redirects nothing: the read
        // returns the renamed directory's bytes and the raced link target
        // stays unread.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(
            store.core.root.join("Project/pinned.md"),
            "---\nname: Pinned\ntype: project\nscope: project\n---\npinned body",
        )
        .unwrap();
        let root = store.core.root.clone();
        let moved = scratch.0.join("saved");
        let renamed = moved.clone();
        let linked = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let _hook = read_test_hook::set(move |path| {
            if path == "Project/pinned.md" && !flag.replace(true) {
                fs::rename(&root, &renamed).unwrap();
                symlink(&linked, &root).unwrap();
            }
        });
        let (_, body) = store
            .core
            .parsed("Project/pinned.md")
            .unwrap()
            .expect("entry read from the pinned root");
        assert!(fired.get(), "target-filtered hook did not fire");
        assert_eq!(body, "pinned body");
        assert!(
            !outside.join("Project").exists(),
            "read escaped through the raced root link"
        );
    }
    #[test]
    fn mutation_fifo_write_without_reader_reports_enxio() {
        // A no-reader FIFO cannot be opened for writing at all (POSIX
        // O_NONBLOCK): the write refuses at open with Node's ENXIO
        // diagnostics instead of hanging like TS writeFileSync (real-TS
        // probe, round 1). With a held reader, mutation_race_fifo_write
        // proves the fstat recheck refuses the inode itself.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let fifo = store.core.root.join("Project/fifo.md");
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        let error = store
            .core
            .write_bytes("Project/fifo.md", "changed")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "ENXIO: no such device or address, open '{}'",
                fifo.display()
            )
        );
    }
    #[test]
    fn mutation_directory_inaccessible_ancestor_is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let ancestor = store.core.root.join("Project/blocked");
        fs::create_dir_all(ancestor.join("nested")).unwrap();
        fs::write(ancestor.join("nested/note.md"), "body").unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o600)).unwrap();
        // Independent native probe: do not let the wrapper certify its own
        // permission fixture. Scratch restores permissions even on unwind.
        assert_eq!(
            fs::metadata(ancestor.join("nested"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(
            store
                .core
                .directory("Project/blocked/nested")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn mutation_directory_non_directory_ancestor_is_empty() {
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let ancestor = store.core.root.join("Project/file");
        fs::write(&ancestor, "body").unwrap();
        assert_eq!(
            fs::metadata(ancestor.join("nested"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOTDIR)
        );
        assert!(
            store
                .core
                .directory("Project/file/nested")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn mutation_directory_unreadable_leaf_keeps_scandir_error() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let leaf = store.core.root.join("Project");
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o300)).unwrap();
        assert_eq!(
            fs::read_dir(&leaf).unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        let error = store.core.directory("Project").unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("EACCES: permission denied, scandir '{}'", leaf.display())
        );
    }
    #[test]
    fn mutation_read_inaccessible_parent_is_none() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let parent = store.core.root.join("Project/blocked");
        fs::create_dir_all(&parent).unwrap();
        fs::write(parent.join("note.md"), "body").unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o600)).unwrap();
        // Native probe: the parent dir is not searchable.
        assert_eq!(
            fs::metadata(parent.join("note.md"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        // TS existsSync swallows EACCES to false → readFile yields no record.
        assert!(
            store
                .core
                .read_file("Project/blocked/note.md")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn mutation_read_unreadable_leaf_keeps_open_error() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let leaf = store.core.root.join("Project/note.md");
        fs::write(&leaf, "body").unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o000)).unwrap();
        // Native probe: the leaf file is not readable.
        assert_eq!(
            fs::read(&leaf).unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        // Parent is searchable, so statat probe succeeds → the leaf's own
        // open EACCES is preserved as an error, matching TS readFile.
        let error = store.core.read_file("Project/note.md").unwrap_err();
        assert!(error.to_string().contains("EACCES"), "{}", error);
        assert!(error.to_string().contains("open"), "{}", error);
    }
    #[test]
    fn mutation_clear_deep_tree_with_low_fd_limit() {
        const CHILD_ENV: &str = "PDRS_597_CLEAR_LOW_FD_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ENV) {
            use rustix::process::{Resource, getrlimit, setrlimit};
            let mut limit = getrlimit(Resource::Nofile);
            limit.current = Some(64);
            setrlimit(Resource::Nofile, limit).unwrap();
            assert_eq!(getrlimit(Resource::Nofile).current, Some(64));
            // Both the internal clear and public clearAllData must remove a
            // tree deeper than the descriptor limit without touching links.
            for (name, public) in [("internal", false), ("public", true)] {
                let store = store(&Path::new(&root).join(name));
                if public {
                    store
                        .clear_all_data(&obj(&[("rebuildManifest", JsValue::Bool(false))]))
                        .unwrap();
                } else {
                    store.core.clear().unwrap();
                }
                assert!(!store.core.root.join("Project/deep").exists());
                assert!(!store.core.root.join("Project/z.md").exists());
                assert_eq!(store.core.root.join("Project").is_dir(), public);
            }
            return;
        }
        let scratch = Scratch::new();
        let sentinel = scratch.0.join("sentinel.md");
        fs::write(&sentinel, "outside sentinel").unwrap();
        for name in ["internal", "public"] {
            let store = store(&scratch.0.join(name));
            let mut deep = store.core.root.join("Project/deep");
            for _ in 0..96 {
                deep.push("d");
            }
            fs::create_dir_all(&deep).unwrap();
            fs::write(deep.join("note.md"), "deep body").unwrap();
            symlink(&sentinel, deep.join("link.md")).unwrap();
            // Keep a sibling to check traversal resumes after descending.
            fs::write(store.core.root.join("Project/z.md"), "sibling").unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::memory::edgeclaw::file_memory::mutation_security_tests::mutation_clear_deep_tree_with_low_fd_limit", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &scratch.0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("low-fd clear child exceeded 30 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        // #273: same libtest receipt criterion as the exact-rerun sites — a
        // renamed filter would otherwise pass vacuously with exit 0.
        assert!(
            crate::test_exact_receipt::single_pass(
                &output.status,
                "runtime::memory::edgeclaw::file_memory::mutation_security_tests::mutation_clear_deep_tree_with_low_fd_limit",
                &stdout
            ),
            "low-fd clear: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "outside sentinel");
        for name in ["internal", "public"] {
            assert!(!scratch.0.join(name).join("Project/deep").exists());
        }
    }
    #[test]
    fn mutation_clear_reports_ts_scandir_errors() {
        // TS clearAllData on a 0300 directory reports scandir on that
        // directory (real-store probe); the descriptor walk keeps the shape.
        use std::os::unix::fs::PermissionsExt;
        // Restore on unwind too: a failing assertion (negative control) must
        // not leave a 0300 directory Scratch::drop cannot remove.
        struct Unrestrict(PathBuf);
        impl Drop for Unrestrict {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            }
        }
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let secret = store.core.root.join("secret");
        fs::create_dir(&secret).unwrap();
        fs::write(secret.join("victim.md"), "v").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o300)).unwrap();
        let _unrestrict = Unrestrict(secret.clone());
        let error = store.core.clear().unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("EACCES: permission denied, scandir '{}'", secret.display())
        );
    }
    #[test]
    fn mutation_mkdir_error_keeps_the_parent_display_path() {
        // TS's ensureDir(dirname(absolutePath)) reports mkdir on the parent
        // directory (real-store probe), not on the file being written.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        fs::write(store.core.root.join("Project/file.md"), "file").unwrap();
        let error = store
            .core
            .write_bytes("Project/file.md/entry.md", "body")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "EEXIST: file already exists, mkdir '{}'",
                store.core.root.join("Project/file.md").display()
            )
        );
    }
    #[test]
    fn mutation_write_to_root_reports_eisdir() {
        // A root-addressed write is Node's writeFileSync on a directory.
        let scratch = Scratch::new();
        let store = store(&scratch.0.join("memory"));
        let error = store.core.write_bytes("", b"changed").unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                store.core.root.display()
            )
        );
    }
    #[test]
    fn mutation_layout_skips_an_empty_profile_path() {
        // TS treats an empty userProfileRelativePath as absent (falsy): the
        // layout is built without touching the profile parent at all.
        let scratch = Scratch::new();
        let mut store = store(&scratch.0.join("memory"));
        store.core.options.user_profile_relative_path = Some(String::new());
        store.core.ensure_layout().unwrap();
        assert!(store.core.root.join("Project").is_dir());
    }
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

/// The two staged whole-file write ports behind memory mutations:
/// `project_files::overwrite_at` (the unix write path) and `helpers::io_write`
/// (the snapshot writes and the non-unix leg). Both must report the
/// descriptor write stage like Node's `writeFileSync`/`writeFile` —
/// `EFBIG: file too large, write` with no path (node v22.22.0,
/// `_tmp/20261025-issue-656/node-probe/write-stages.out`) — which is also how
/// their staged errors render through `FileMemoryError` on the mutation path.
/// Both children realpath the scratch root before writing: `overwrite_at`'s
/// NOFOLLOW walk starts at `/` and cannot cross macOS' /var symlink, and
/// `io_write` treats the root the same way so the two ports are judged at one
/// physical location.
#[cfg(all(test, unix))]
mod write_stage_tests {
    use super::*;

    #[test]
    #[ignore = "spawned by overwrite_reports_the_write_stage_like_node"]
    fn overwrite_child_over_file_size_limit() {
        let Ok(root_path) = std::env::var(crate::test_write_stage::CHILD_PATH_ENV) else {
            panic!(
                "{} not set; spawn via overwrite_reports_the_write_stage_like_node",
                crate::test_write_stage::CHILD_PATH_ENV
            );
        };
        crate::test_write_stage::ignore_sigxfsz_like_gateway();
        // The same pinned root `mutation_target` hands to `overwrite_at`:
        // realpath-resolved, because the NOFOLLOW walk starts at `/`.
        let root_path = fs::canonicalize(&root_path).unwrap();
        let root = crate::project_files::open_directory_beneath(&root_path, &root_path).unwrap();
        let error = crate::project_files::overwrite_at(
            &root,
            std::path::Path::new("sub/entry.md"),
            &[b'a'; 4096],
        )
        .expect_err("a write past RLIMIT_FSIZE must fail");
        println!("WRITE_ERROR={}", FileMemoryError::Io(error));
    }

    #[test]
    fn overwrite_reports_the_write_stage_like_node() {
        let scratch = crate::test_scratch::Scratch::new("overwrite-write-stage");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        let line = crate::test_write_stage::child_write_error(
            "runtime::memory::edgeclaw::file_memory::write_stage_tests::overwrite_child_over_file_size_limit",
            1,
            scratch.path(),
        );
        assert_eq!(line, "EFBIG: file too large, write");
    }

    #[test]
    #[ignore = "spawned by io_write_reports_the_write_stage_like_node"]
    fn io_write_child_over_file_size_limit() {
        let Ok(path) = std::env::var(crate::test_write_stage::CHILD_PATH_ENV) else {
            panic!(
                "{} not set; spawn via io_write_reports_the_write_stage_like_node",
                crate::test_write_stage::CHILD_PATH_ENV
            );
        };
        crate::test_write_stage::ignore_sigxfsz_like_gateway();
        // Same root treatment as overwrite_child: realpath the parent so
        // both children write at one physical location (the shared scratch
        // sits behind macOS' /var symlink). `io_write` itself opens through
        // `File::create`, which follows symlinks, so this is fixture
        // uniformity, not a NOFOLLOW requirement; the printed WRITE_ERROR
        // below is the descriptor write's EFBIG either way.
        let target = std::path::PathBuf::from(path);
        let root = fs::canonicalize(target.parent().expect("scratch root")).unwrap();
        let error = io_write(
            root.join(target.file_name().expect("entry file name")),
            [b'a'; 4096],
        )
        .expect_err("a write past RLIMIT_FSIZE must fail");
        println!("WRITE_ERROR={}", FileMemoryError::Io(error));
    }

    #[test]
    fn io_write_reports_the_write_stage_like_node() {
        let scratch = crate::test_scratch::Scratch::new("io-write-write-stage");
        let target = scratch.path().join("entry.md");
        let line = crate::test_write_stage::child_write_error(
            "runtime::memory::edgeclaw::file_memory::write_stage_tests::io_write_child_over_file_size_limit",
            1,
            &target,
        );
        assert_eq!(line, "EFBIG: file too large, write");
    }
}
