//! Repository accessors, external workspace discovery and readable catalogs.
//! Source: fixed baseline `core/storage/sqlite.ts:592-909`.
use super::js_truthy;
use crate::metadata::js_trim;
use crate::runtime::memory::edgeclaw::file_memory::{
    EdgeClawFileStores, FileMemoryClock, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
    absolute,
    helpers::{extract_objects, hash, map_global, objects, s, sort_manifest_entries, text},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};
type Result<T> = std::result::Result<T, FileMemoryError>;

pub(crate) struct RepositoryContext {
    workspace: PathBuf,
    pub(super) general_workspace: PathBuf,
    workspaces_root: PathBuf,
    mode: String,
    pub(super) clock: FileMemoryClock,
    external: Mutex<HashMap<String, Snapshot>>,
    pub(super) snapshot_io: super::snapshots::SnapshotIoHook,
}
#[derive(Clone)]
struct Snapshot {
    workspace: PathBuf,
    store: Arc<FileMemoryStore>,
}
impl RepositoryContext {
    pub(crate) fn new(memory: &Path, mode: &str, clock: FileMemoryClock) -> Result<Self> {
        Self::with_general(memory, mode, general_workspace()?, clock)
    }
    pub(crate) fn with_general(
        memory: &Path,
        mode: &str,
        general_workspace: PathBuf,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        let memory = absolute(memory)?;
        let root = memory
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        Ok(Self {
            workspace: root.clone(),
            general_workspace: absolute(&general_workspace)?,
            workspaces_root: root,
            mode: mode.into(),
            clock,
            external: Mutex::default(),
            snapshot_io: Arc::new(|_, _, _| Ok(())),
        })
    }
}
/// `os.homedir()`: `$HOME` wins even when empty; otherwise libuv falls back to
/// `getpwuid_r` (NSS). The workspace forbids `unsafe`, so the fallback asks the
/// platform passwd lookup instead of calling libc directly.
fn homedir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    passwd_home().ok_or_else(|| FileMemoryError::Invalid("os.homedir() is unavailable".into()))
}
#[cfg(target_os = "macos")]
fn passwd_home() -> Option<PathBuf> {
    // `id -P` prints the BSD passwd entry; `pw_dir` is the second-to-last field.
    let output = Command::new("/usr/bin/id").arg("-P").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?;
    let fields: Vec<&str> = line.trim_end().split(':').collect();
    let home = PathBuf::from(fields.get(fields.len().checked_sub(2)?)?);
    home.is_absolute().then_some(home)
}
#[cfg(all(unix, not(target_os = "macos")))]
fn passwd_home() -> Option<PathBuf> {
    // `getent` follows NSS like getpwuid_r; `/etc/passwd` is the last resort.
    let uid = Command::new("id").arg("-u").output().ok()?;
    if !uid.status.success() {
        return None;
    }
    let uid = String::from_utf8(uid.stdout).ok()?;
    let uid = uid.trim();
    if let Ok(output) = Command::new("getent").args(["passwd", uid]).output()
        && output.status.success()
        && let Ok(line) = String::from_utf8(output.stdout)
        && let Some(home) = line.trim_end().split(':').nth(5)
    {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            return Some(home);
        }
    }
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        fields.next()?;
        fields.next()?;
        if fields.next()? != uid {
            return None;
        }
        fields.next()?;
        fields.next()?;
        let home = PathBuf::from(fields.next()?);
        home.is_absolute().then_some(home)
    })
}
#[cfg(not(unix))]
fn passwd_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}
fn general_workspace() -> Result<PathBuf> {
    absolute(&homedir()?.join(".pilotdeck"))
}
fn put(o: &mut JsObject, k: &str, v: JsValue) {
    o.insert(k.into(), v);
}
fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
fn external_id(workspace: &Path, project: &str) -> String {
    format!(
        "external-project:{}",
        hash(&format!("{}::{project}", workspace.display()))
    )
}
/// `options.includeExternal ?? true`, then JS truthiness. Only nullish values
/// default to true; `0`, `""`, `false` and `NaN` skip external entries.
fn include_external(options: &JsObject) -> bool {
    match options.get("includeExternal") {
        None | Some(JsValue::Undefined | JsValue::Null) => true,
        Some(value) => js_truthy(value),
    }
}
/// `options.kinds ?? ["project", "feedback"]` is forwarded verbatim to the
/// project store, which evaluates `new Set(kinds)`: arrays are iterated for
/// matching entry types and strings iterate into single characters (never a
/// real type, so no matches). Numbers, booleans and plain objects are not
/// iterable, so the store call itself throws `TypeError`; `null`/`undefined`
/// select the default kinds.
fn project_entry_kinds(options: &JsObject) -> Result<JsValue> {
    match options.get("kinds") {
        None | Some(JsValue::Undefined | JsValue::Null) => {
            Ok(JsValue::Array(vec![s("project"), s("feedback")]))
        }
        Some(value @ (JsValue::Array(_) | JsValue::String(_))) => Ok(value.clone()),
        Some(JsValue::Number(number)) => Err(not_iterable(format!(
            "number {}",
            crate::js_value::number_string(*number)
        ))),
        Some(JsValue::Bool(value)) => Err(not_iterable(format!("boolean {value}"))),
        Some(JsValue::Object(_)) => Err(not_iterable("object".into())),
    }
}
/// The V8 `TypeError` text from the store's `new Set(kinds)`; Rust raises it at
/// the same point, immediately before the store call.
fn not_iterable(value: String) -> FileMemoryError {
    FileMemoryError::Type(format!(
        "{value} is not iterable (cannot read property Symbol(Symbol.iterator))"
    ))
}
/// `listReadableProjectEntries` builds a fresh store input: caller limit/offset
/// are ignored (TS hardcodes 5000/0) and only the documented keys are forwarded.
fn project_entry_input(options: &JsObject, kinds: JsValue, project_id: &str) -> JsObject {
    let mut input = JsObject::new();
    put(&mut input, "kinds", kinds);
    put(&mut input, "scope", s("project"));
    put(&mut input, "projectId", s(project_id));
    if let Some(value) = options.get("includeDeprecated") {
        put(&mut input, "includeDeprecated", value.clone());
    }
    if let Some(query) = options.get("query").filter(|v| js_truthy(v)) {
        put(&mut input, "query", query.clone());
    }
    put(&mut input, "limit", JsValue::Number(5000.0));
    put(&mut input, "offset", JsValue::Number(0.0));
    input
}
/// `options.kinds?.filter(kind => kind === "user")`: non-array values throw
/// `TypeError` before the `scope` check, exactly like the TS optional chain.
fn global_entry_kinds(options: &JsObject) -> Result<Option<Vec<JsValue>>> {
    match options.get("kinds") {
        None | Some(JsValue::Undefined | JsValue::Null) => Ok(None),
        Some(JsValue::Array(values)) => Ok(Some(
            values
                .iter()
                .filter(|value| **value == s("user"))
                .cloned()
                .collect(),
        )),
        Some(_) => Err(FileMemoryError::Type(
            "options.kinds?.filter is not a function".into(),
        )),
    }
}
/// `listGlobalMemoryEntries` forwards only its whitelisted keys to the global
/// store; extra keys such as `projectId` never leak into `listMemoryEntries`.
fn global_entry_input(options: &JsObject, kinds: Option<Vec<JsValue>>) -> JsObject {
    let mut input = JsObject::new();
    put(
        &mut input,
        "kinds",
        JsValue::Array(kinds.unwrap_or_else(|| vec![s("user")])),
    );
    if let Some(query) = options.get("query").filter(|v| js_truthy(v)) {
        put(&mut input, "query", query.clone());
    }
    if let Some(JsValue::Number(limit)) = options.get("limit") {
        put(&mut input, "limit", JsValue::Number(*limit));
    }
    if let Some(JsValue::Number(offset)) = options.get("offset") {
        put(&mut input, "offset", JsValue::Number(*offset));
    }
    put(&mut input, "scope", s("global"));
    if let Some(value) = options.get("includeDeprecated") {
        put(&mut input, "includeDeprecated", value.clone());
    }
    input
}
impl EdgeClawFileStores {
    /// Repository constructor with TS path defaults and workspace mode detection.
    pub fn open_repository(
        db: impl AsRef<Path>,
        memory: Option<&Path>,
        global: Option<&Path>,
        workspace: Option<&Path>,
    ) -> Result<Self> {
        let db = absolute(db.as_ref())?;
        let memory = absolute(
            &memory
                .map(Path::to_path_buf)
                .unwrap_or_else(|| db.parent().unwrap().join("memory")),
        )?;
        let root = memory
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/"));
        let global = absolute(
            &global
                .map(Path::to_path_buf)
                .unwrap_or_else(|| root.parent().unwrap_or(Path::new("/")).join("global")),
        )?;
        let workspace = absolute(workspace.unwrap_or(root))?;
        let general = general_workspace()?;
        Self::with_workspace_clock(
            db,
            memory,
            global,
            workspace,
            general,
            Arc::new(|| {
                pilotdeck_transport::system_clock::format_epoch_iso(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                )
            }),
        )
    }
    /// Explicit home/workspace and clock dependencies for isolated hosts and replay.
    /// The injected general workspace means construction never reads `$HOME`.
    pub fn with_workspace_clock(
        db: impl AsRef<Path>,
        memory: impl AsRef<Path>,
        global: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
        general_workspace: impl AsRef<Path>,
        clock: FileMemoryClock,
    ) -> Result<Self> {
        let workspace = absolute(workspace.as_ref())?;
        let general_workspace = absolute(general_workspace.as_ref())?;
        let mode = if workspace == general_workspace {
            "general"
        } else {
            "single"
        };
        let mut repository =
            Self::with_clock_general(db, memory, global, mode, Some(general_workspace), clock)?;
        repository.repository.workspace = workspace;
        Ok(repository)
    }
    pub fn close(&self) -> Result<()> {
        self.repository
            .external
            .lock()
            .expect("external cache poisoned")
            .clear();
        self.storage.close()?;
        Ok(())
    }
    pub fn get_file_memory_store(&self) -> &FileMemoryStore {
        &self.workspace_memory
    }
    pub fn get_global_user_store(&self) -> &FileMemoryStore {
        &self.global_user_memory
    }
    pub fn get_workspace_mode(&self) -> &str {
        &self.repository.mode
    }
    pub(super) fn current_workspace(&self) -> Result<PathBuf> {
        let value = self.storage.get_pipeline_state("workspaceDir")?;
        Ok(value
            .and_then(|v| v.as_string().cloned())
            .filter(|s| !js_trim(&String::from_utf16_lossy(s.units())).is_empty())
            .map(|s| PathBuf::from(String::from_utf16_lossy(s.units())))
            .unwrap_or_else(|| self.repository.workspace.clone()))
    }
    fn external_snapshots(&self) -> Result<Vec<Snapshot>> {
        if self.get_workspace_mode() != "general" {
            return Ok(vec![]);
        }
        let current = absolute(&self.current_workspace()?)?;
        let memory = absolute(self.workspace_memory.get_root_dir())?;
        let mut snapshots = vec![];
        let mut cache = self
            .repository
            .external
            .lock()
            .expect("external cache poisoned");
        let mut next = HashMap::new();
        if self.repository.workspaces_root.exists() {
            let root = &self.repository.workspaces_root;
            let mut dirs = fs::read_dir(root)
                .map_err(|e| crate::node_io::path_error(e, "scandir", root))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| crate::node_io::path_error(e, "scandir", root))?;
            // Node readdir is in filename order on supported hosts; stable order
            // also determines which of duplicate-workspace stores is cached.
            dirs.sort_by_key(|e| e.file_name());
            for dir in dirs {
                if !dir
                    .file_type()
                    .map_err(|e| crate::node_io::path_error(e, "scandir", root))?
                    .is_dir()
                {
                    continue;
                }
                let memory_dir = dir.path().join("memory");
                let db = dir.path().join("control.sqlite");
                if !memory_dir.exists() || !db.exists() || absolute(&memory_dir)? == memory {
                    continue;
                }
                let Some(workspace) = read_workspace(&db) else {
                    continue;
                };
                if workspace == current || workspace == self.repository.general_workspace {
                    continue;
                }
                let key = hash(&path_text(&workspace));
                let snapshot =
                    if let Some(cached) = cache.get(&key).filter(|s| s.workspace == workspace) {
                        cached.clone()
                    } else {
                        Snapshot {
                            workspace,
                            store: Arc::new(FileMemoryStore::with_clock(
                                memory_dir,
                                FileMemoryStoreOptions {
                                    workspace_mode: "single".into(),
                                    manage_user_profile: false,
                                    ..Default::default()
                                },
                                self.repository.clock.clone(),
                            )?),
                        }
                    };
                next.insert(key, snapshot.clone());
                snapshots.push(snapshot);
            }
        }
        *cache = next;
        Ok(snapshots)
    }
    pub fn list_readable_project_catalog(&self) -> Result<JsValue> {
        if self.get_workspace_mode() != "general" {
            let meta = self.workspace_memory.get_project_meta(&JsObject::new())?;
            let Some(meta) = meta.as_object() else {
                return Ok(objects(vec![]));
            };
            let workspace = self.current_workspace()?;
            return Ok(objects(vec![catalog_entry(
                meta.clone(),
                self.get_workspace_mode(),
                &workspace,
                "general_local",
                &text(meta, "projectId"),
                false,
                false,
                summary(&self.workspace_memory, &text(meta, "projectId"))?,
            )]));
        }
        let workspace = self.current_workspace()?;
        let locals = extract_objects(&self.workspace_memory.list_project_metas()?);
        let mut mirrors = HashMap::new();
        for meta in &locals {
            if text(meta, "sourceKind") == "workspace_external_mirror"
                && !text(meta, "sourceWorkspacePath").is_empty()
                && !text(meta, "sourceProjectId").is_empty()
            {
                mirrors.insert(
                    (
                        absolute(Path::new(&text(meta, "sourceWorkspacePath")))?,
                        text(meta, "sourceProjectId"),
                    ),
                    meta,
                );
            }
        }
        let mut catalog = vec![];
        for snapshot in self.external_snapshots()? {
            let meta = snapshot.store.get_project_meta(&JsObject::new())?;
            let Some(meta) = meta.as_object() else {
                continue;
            };
            let id = text(meta, "projectId");
            let logical = external_id(&snapshot.workspace, &id);
            let external_summary = summary(&snapshot.store, &id)?;
            let mut entry = if let Some(mirror) = mirrors.get(&(snapshot.workspace.clone(), id)) {
                let mirror_id = text(mirror, "projectId");
                let local_summary = summary(&self.workspace_memory, &mirror_id)?;
                let mut merged = external_summary;
                for key in ["totalEntries", "projectEntries", "feedbackEntries"] {
                    let n = |o: &JsObject| match o.get(key) {
                        Some(JsValue::Number(n)) => *n,
                        _ => 0.0,
                    };
                    let total = n(&merged) + n(&local_summary);
                    put(&mut merged, key, JsValue::Number(total));
                }
                let latest = [
                    text(&merged, "latestMemoryAt"),
                    text(&local_summary, "latestMemoryAt"),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .max_by_key(|s| s.encode_utf16().collect::<Vec<_>>());
                put(
                    &mut merged,
                    "latestMemoryAt",
                    latest.map(s).unwrap_or(JsValue::Undefined),
                );
                let mut e = catalog_entry(
                    (*mirror).clone(),
                    "general",
                    &workspace,
                    "workspace_external_mirror",
                    &mirror_id,
                    false,
                    true,
                    merged,
                );
                put(&mut e, "localMirrorProjectId", s(mirror_id));
                e
            } else {
                catalog_entry(
                    meta.clone(),
                    "single",
                    &snapshot.workspace,
                    "workspace_external",
                    &logical,
                    true,
                    false,
                    external_summary,
                )
            };
            put(&mut entry, "externalLogicalProjectId", s(logical));
            catalog.push(entry);
        }
        for meta in locals
            .into_iter()
            .filter(|m| text(m, "sourceKind") != "workspace_external_mirror")
        {
            let id = text(&meta, "projectId");
            let summary = summary(&self.workspace_memory, &id)?;
            catalog.push(catalog_entry(
                meta,
                "general",
                &workspace,
                "general_local",
                &id,
                false,
                false,
                summary,
            ));
        }
        sort_manifest_entries(
            &mut catalog,
            |e| {
                e.get("summary")
                    .and_then(JsValue::as_object)
                    .map(|o| text(o, "latestMemoryAt"))
                    .unwrap_or_default()
            },
            "projectName",
        )?;
        Ok(objects(catalog))
    }
    pub fn get_readable_project(&self, logical: &str) -> Result<JsValue> {
        Ok(extract_objects(&self.list_readable_project_catalog()?)
            .into_iter()
            .find(|e| text(e, "logicalProjectId") == logical)
            .map(JsValue::Object)
            .unwrap_or(JsValue::Undefined))
    }
    pub fn list_readable_project_entries(
        &self,
        logical: &str,
        options: &JsObject,
    ) -> Result<JsValue> {
        let project = self.get_readable_project(logical)?;
        let Some(project) = project.as_object() else {
            return Ok(objects(vec![]));
        };
        // TS forwards `options.kinds ?? ["project", "feedback"]` to the store,
        // which builds `new Set(kinds)`; a non-iterable value therefore throws
        // only when that store call is reached. An external-only project with
        // `includeExternal: false` never reaches it.
        let include_external = include_external(options);
        let source = text(project, "sourceType");
        let mut entries = vec![];
        if source == "general_local" || source == "workspace_external_mirror" {
            let input = project_entry_input(
                options,
                project_entry_kinds(options)?,
                &text(project, "projectId"),
            );
            entries.extend(extract_objects(
                &self.workspace_memory.list_memory_entries(&input)?,
            ));
        }
        if include_external
            && (source == "workspace_external" || source == "workspace_external_mirror")
        {
            let target = text(project, "sourceWorkspacePath");
            let target = if target.is_empty() {
                text(project, "workspacePath")
            } else {
                target
            };
            for snapshot in self.external_snapshots()? {
                if snapshot.workspace != absolute(Path::new(&target))? {
                    continue;
                }
                let source_id = text(project, "sourceProjectId");
                let project_id = if source_id.is_empty() {
                    text(project, "projectId")
                } else {
                    source_id
                };
                let input =
                    project_entry_input(options, project_entry_kinds(options)?, &project_id);
                entries.extend(
                    extract_objects(&snapshot.store.list_memory_entries(&input)?)
                        .into_iter()
                        .map(|mut e| {
                            let path = text(&e, "relativePath").replace('\\', "/");
                            put(
                                &mut e,
                                "relativePath",
                                s(format!(
                                    "external:{}:{path}",
                                    hash(&path_text(&snapshot.workspace))
                                )),
                            );
                            e
                        }),
                );
                break;
            }
        }
        sort_manifest_entries(&mut entries, |o| text(o, "updatedAt"), "relativePath")?;
        Ok(objects(entries))
    }
    pub fn repair_workspace_manifest(&self) -> Result<JsValue> {
        self.workspace_memory.repair_manifests()
    }
    pub fn get_user_summary(&self) -> Result<JsValue> {
        let summary = self.global_user_memory.get_user_summary()?;
        let mut summary = summary.as_object().cloned().unwrap_or_default();
        let files = extract_objects(
            &summary
                .get("files")
                .cloned()
                .unwrap_or(JsValue::Array(vec![])),
        );
        put(
            &mut summary,
            "files",
            objects(files.into_iter().map(map_global).collect()),
        );
        Ok(JsValue::Object(summary))
    }
    /// Internal TS helper, exposed for repository consumers needing the same filter.
    pub fn list_global_memory_entries(&self, options: &JsObject) -> Result<JsValue> {
        // The TS filter runs before the scope check, so a non-array `kinds`
        // throws even when `scope` is `"project"`.
        let kinds = global_entry_kinds(options)?;
        if text(options, "scope") == "project" {
            return Ok(objects(vec![]));
        }
        if matches!(&kinds, Some(kinds) if kinds.is_empty()) {
            return Ok(objects(vec![]));
        }
        let input = global_entry_input(options, kinds);
        Ok(objects(
            extract_objects(&self.global_user_memory.list_memory_entries(&input)?)
                .into_iter()
                .map(map_global)
                .collect(),
        ))
    }
}
fn read_workspace(db: &Path) -> Option<PathBuf> {
    // Match TS: open without initializing/migrating and swallow all read errors.
    // node:sqlite's `DatabaseSync` defaults to timeout 0 (measured: a locked
    // read returns "database is locked" in 0 ms), so a concurrent writer makes
    // TS skip the workspace too.
    let conn = Connection::open(db).ok()?;
    conn.busy_timeout(std::time::Duration::ZERO).ok()?;
    let value: Option<String> = conn
        .query_row(
            "SELECT state_json FROM pipeline_state WHERE state_key = ?",
            ["workspaceDir"],
            |row| row.get(0),
        )
        .optional()
        .ok()?;
    let parsed = parse_json(value?.as_str()).ok()?;
    let value = parsed.as_string()?;
    let value = js_trim(&String::from_utf16_lossy(value.units())).to_string();
    if value.is_empty() {
        None
    } else {
        absolute(Path::new(&value)).ok()
    }
}
fn summary(store: &FileMemoryStore, id: &str) -> Result<JsObject> {
    let mut options = JsObject::new();
    put(
        &mut options,
        "kinds",
        JsValue::Array(vec![s("project"), s("feedback")]),
    );
    put(&mut options, "scope", s("project"));
    put(&mut options, "projectId", s(id));
    put(&mut options, "includeDeprecated", JsValue::Bool(false));
    put(&mut options, "limit", JsValue::Number(5000.0));
    put(&mut options, "offset", JsValue::Number(0.0));
    let entries = extract_objects(&store.list_memory_entries(&options)?);
    let mut out = JsObject::new();
    for (key, n) in [
        ("totalEntries", entries.len()),
        (
            "projectEntries",
            entries
                .iter()
                .filter(|e| text(e, "type") == "project")
                .count(),
        ),
        (
            "feedbackEntries",
            entries
                .iter()
                .filter(|e| text(e, "type") == "feedback")
                .count(),
        ),
    ] {
        put(&mut out, key, JsValue::Number(n as f64));
    }
    if let Some(latest) = entries.first().filter(|e| !text(e, "updatedAt").is_empty()) {
        put(
            &mut out,
            "latestMemoryAt",
            latest.get("updatedAt").unwrap().clone(),
        );
    }
    Ok(out)
}
#[allow(clippy::too_many_arguments)]
fn catalog_entry(
    mut meta: JsObject,
    mode: &str,
    workspace: &Path,
    source: &str,
    logical: &str,
    read_only: bool,
    mirror: bool,
    summary: JsObject,
) -> JsObject {
    for (k, v) in [
        ("workspaceMode", s(mode)),
        ("workspacePath", s(path_text(workspace))),
        (
            "workspaceName",
            s(workspace.file_name().unwrap_or_default().to_string_lossy()),
        ),
        ("sourceType", s(source)),
        ("logicalProjectId", s(logical)),
        ("readOnly", JsValue::Bool(read_only)),
        ("hasLocalMirror", JsValue::Bool(mirror)),
        ("summary", JsValue::Object(summary)),
    ] {
        put(&mut meta, k, v);
    }
    meta
}

// Entries uses the same discovery/cache and collation as readable catalogs.
impl EdgeClawFileStores {
    pub(super) fn entry_external_records(
        &self,
        ids: &[JsString],
        max_lines: f64,
    ) -> Result<Vec<JsValue>> {
        let mut groups: Vec<(String, Vec<JsValue>)> = vec![];
        for id in ids {
            let Some(payload) = id
                .units()
                .strip_prefix(&[101, 120, 116, 101, 114, 110, 97, 108, 58])
            else {
                continue;
            };
            let Some(separator) = payload.iter().position(|u| *u == 58) else {
                continue;
            };
            let key = crate::runtime::memory::js_trim_units(&payload[..separator]);
            let path = crate::runtime::memory::js_trim_units(&payload[separator + 1..]);
            if key.is_empty() || path.is_empty() {
                continue;
            }
            let key = String::from_utf16_lossy(key);
            let path = JsValue::String(JsString::from_utf16(
                path.iter()
                    .map(|u| if *u == 92 { 47 } else { *u })
                    .collect(),
            ));
            if let Some((_, paths)) = groups.iter_mut().find(|(k, _)| k == &key) {
                paths.push(path);
            } else {
                groups.push((key, vec![path]));
            }
        }
        let mut records = vec![];
        if groups.is_empty() {
            return Ok(records);
        }
        let snapshots = self.external_snapshots()?;
        for (key, paths) in groups {
            let Some(snapshot) = snapshots
                .iter()
                .find(|s| hash(&path_text(&s.workspace)) == key)
            else {
                continue;
            };
            let mut input = JsObject::new();
            put(&mut input, "ids", JsValue::Array(paths));
            put(&mut input, "maxLines", JsValue::Number(max_lines));
            for mut record in extract_objects(&snapshot.store.get_memory_records_by_ids(&input)?) {
                let mut path = format!("external:{key}:")
                    .encode_utf16()
                    .collect::<Vec<_>>();
                path.extend(
                    external_record_path(&record)?
                        .units()
                        .iter()
                        .map(|u| if *u == 92 { 47 } else { *u }),
                );
                put(
                    &mut record,
                    "relativePath",
                    JsValue::String(JsString::from_utf16(path)),
                );
                records.push(JsValue::Object(record));
            }
        }
        Ok(records)
    }
}
pub(super) fn sort_entry_values(entries: &mut Vec<JsValue>) -> Result<()> {
    let mut records = entries
        .iter()
        .filter_map(|v| v.as_object().cloned())
        .collect::<Vec<_>>();
    sort_manifest_entries(&mut records, |o| text(o, "updatedAt"), "relativePath")?;
    *entries = records.into_iter().map(JsValue::Object).collect();
    Ok(())
}

fn external_record_path(record: &JsObject) -> Result<&JsString> {
    record
        .get("relativePath")
        .and_then(JsValue::as_string)
        .ok_or_else(|| {
            FileMemoryError::Invalid(
                "Internal error: external record relativePath is not a string".into(),
            )
        })
}
#[cfg(test)]
mod round6_tests {
    use super::*;
    #[test]
    fn malformed_external_record_returns_internal_error() {
        for value in [None, Some(JsValue::Null), Some(JsValue::Number(1.0))] {
            let mut record = JsObject::new();
            if let Some(value) = value {
                record.insert("relativePath".into(), value);
            }
            assert!(
                matches!(external_record_path(&record), Err(FileMemoryError::Invalid(message))
                if message == "Internal error: external record relativePath is not a string")
            );
        }
    }
}

#[cfg(all(test, unix))]
use super::super::file_memory::round6_io;
#[cfg(all(test, unix))]
mod round6_io_tests {
    use super::*;
    #[test]
    fn external_scandir_matches_real_ts() {
        round6_io::check("external-scandir", |repo, root| {
            round6_io::deny(root, true);
            repo.external_snapshots()
                .err()
                .expect("external scandir must fail")
        });
    }
}
