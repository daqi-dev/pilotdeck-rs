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
    collections::{BTreeMap, HashMap},
    fmt, fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
mod helpers;
use helpers::*;

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
    Storage(StorageError),
}
impl fmt::Display for FileMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidUtf16(e) => write!(f, "{}", String::from_utf16_lossy(e.units())),
            Self::Invalid(e) | Self::Collation(e) => write!(f, "{e}"),
            Self::Storage(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for FileMemoryError {}
impl FileMemoryError {
    pub fn js_message(&self) -> JsString {
        match self {
            Self::InvalidUtf16(s) => s.clone(),
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
fn operation_lock(root: &Path) -> Result<Arc<Mutex<()>>> {
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
            let _guard = operation.lock().map_err(|_| {
                FileMemoryError::Invalid("file memory operation mutex poisoned".into())
            })?;
            core.ensure_layout()?;
        }
        Ok(Self {
            declared_root,
            core,
            operation,
        })
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
        let _guard = self
            .operation
            .lock()
            .map_err(|_| FileMemoryError::Invalid("file memory operation mutex poisoned".into()))?;
        self.core.execute(method, input)
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

/// File-store wiring from `sqlite.ts:523-540`, using only pr10a's public API.
/// Repository-level snapshots, routing and retrieval remain separate work items.
pub struct EdgeClawFileStores {
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
        let global_user_memory = Arc::new(FileMemoryStore::with_clock(
            global,
            FileMemoryStoreOptions {
                manage_project_meta: false,
                manage_project_files: false,
                user_profile_relative_path: Some("UserIdentity/user-profile.md".into()),
                user_notes_relative_dir: Some("UserIdentityNotes".into()),
                append_only_user_entries: true,
                enable_manifest: false,
                ..Default::default()
            },
            clock.clone(),
        )?);
        let provider = global_user_memory.clone();
        let workspace_memory = FileMemoryStore::with_clock(
            memory,
            FileMemoryStoreOptions {
                workspace_mode: mode.into(),
                manage_user_profile: false,
                manifest_user_entries_provider: Some(Arc::new(move || {
                    let value = provider.list_memory_entries(&obj(&[
                        ("kinds", strings(&["user".into()])),
                        ("scope", s("global")),
                    ]))?;
                    Ok(match &value {
                        JsValue::Array(v) => v
                            .iter()
                            .filter_map(|v| v.as_object().cloned())
                            .map(|mut e| {
                                let path = text(&e, "relativePath").replace('\\', "/");
                                e.insert(
                                    "relativePath".into(),
                                    s(if path.starts_with("global/") {
                                        path
                                    } else {
                                        format!("global/{path}")
                                    }),
                                );
                                e
                            })
                            .collect(),
                        _ => vec![],
                    })
                })),
                ..Default::default()
            },
            clock,
        )?;
        Ok(Self {
            storage: EdgeClawStorage::open(db)?,
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
fn absolute(path: &Path) -> Result<PathBuf> {
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
        absolute(&self.root.join(relative))
    }
    fn within(&self, relative: &str) -> Result<bool> {
        let absolute = self.path(relative)?;
        let rel = relative_path(&self.root, &absolute);
        // Preserve TS's unusual first disjunct, including canonical ../ paths.
        Ok(rel == relative || (!rel.starts_with("..") && !rel.contains("..")))
    }
    fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        if self.options.manage_user_profile {
            if let Some(path) = &self.options.user_profile_relative_path {
                fs::create_dir_all(self.path(path)?.parent().expect("absolute parent"))?;
            }
            if let Some(path) = &self.options.user_notes_relative_dir {
                fs::create_dir_all(self.path(path)?)?;
            }
        }
        if self.options.manage_project_files {
            for dir in ["Project", "Feedback"] {
                fs::create_dir_all(self.root.join(dir))?;
            }
        }
        if self.options.manage_project_meta && self.general() {
            fs::create_dir_all(self.root.join(GENERAL))?;
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
        Ok(parse(&read(&path)?, &self.now()))
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
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file()
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
        if entries.len() < 2 {
            return Ok(());
        }
        let collator =
            crate::project_files::node_collator(&crate::project_files::node_locale(|key| {
                std::env::var(key).ok()
            }))
            .map_err(|e| FileMemoryError::Collation(format!("{e:?}")))?;
        entries.sort_by(|a, b| {
            let a_time = text(a, "updatedAt");
            let b_time = text(b, "updatedAt");
            if a_time != b_time {
                crate::project_files::node_compare_utf16(
                    &collator,
                    &JsString::from(b_time),
                    &JsString::from(a_time),
                )
            } else {
                crate::project_files::node_compare_utf16(
                    &collator,
                    &JsString::from(text(a, "relativePath")),
                    &JsString::from(text(b, "relativePath")),
                )
            }
        });
        Ok(())
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
        fs::create_dir_all(absolute.parent().expect("absolute parent"))?;
        fs::write(absolute, render(fm, body))?;
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
        let name = n(&header, "project_name");
        if name.is_empty() {
            return Ok(None);
        }
        let parsed = parse(&raw, &self.now())
            .map(|(p, _)| text(&p, "updatedAt"))
            .unwrap_or_else(|| self.now());
        let mut meta = obj(&[
            ("projectId", s(CURRENT_PROJECT_ID)),
            ("projectName", s(&name)),
            ("description", s(desc(&text(&header, "description"), &name))),
            (
                "status",
                s(first(&[n(&header, "status"), "in_progress".into()])),
            ),
            (
                "createdAt",
                s(if header.contains_key("created_at") {
                    text(&header, "created_at")
                } else {
                    parsed.clone()
                }),
            ),
            (
                "updatedAt",
                s(if header.contains_key("updated_at") {
                    text(&header, "updated_at")
                } else {
                    parsed
                }),
            ),
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
        fs::write(
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
        let mut lines = vec![
            "# EdgeClaw Memory".into(),
            "".into(),
            format!("Updated: {}", self.now()),
            "".into(),
        ];
        if let Some(meta) = self.single_meta()? {
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
        if self.general() {
            let metas = self.metas()?;
            if !metas.is_empty() {
                lines.push("## General Projects".into());
                for m in metas {
                    lines.push(format!(
                        "- [{}]({}) — {}",
                        text(&m, "projectName"),
                        text(&m, "relativePath"),
                        text(&m, "description")
                    ));
                }
                lines.push("".into());
            }
        }
        let all = self.all()?;
        for (title, kind) in [
            ("Project Memory", "project"),
            ("Feedback Memory", "feedback"),
            ("User Memory", "user"),
            ("Deprecated", ""),
        ] {
            let mut entries = all
                .iter()
                .filter(|e| {
                    if kind.is_empty() {
                        flag(e, "deprecated")
                    } else {
                        !flag(e, "deprecated") && text(e, "type") == kind
                    }
                })
                .cloned()
                .collect::<Vec<_>>();
            if kind == "user" && !self.options.manage_user_profile {
                entries = match &self.options.manifest_user_entries_provider {
                    Some(p) => p()?
                        .into_iter()
                        .filter(|e| !flag(e, "deprecated"))
                        .collect(),
                    None => vec![],
                };
                self.sort(&mut entries)?;
            }
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
                fs::write(path, next)?;
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
                    && (flag(input, "includeDeprecated") || !flag(e, "deprecated"))
                    && query_matches(e, input)
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
                } else if !id.is_empty() && id != CURRENT_PROJECT_ID {
                    return Err(input_error("Unknown projectId: ", input, "projectId"));
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
                        fs::remove_file(self.path(&path)?)?;
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
                if self.root.exists() {
                    fs::remove_dir_all(&self.root)?;
                }
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
                            fs::remove_file(self.path(&path)?)?;
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
