//! Memory bundle export/import and staged root swaps.
//! Source: fixed baseline `core/storage/sqlite.ts:264-353,422-472,1495-1658`.
//! The Dream snapshot reads/clears the import depends on live in
//! [`super::snapshots`] (pr10e): `clear_last_dream_snapshot` /
//! `get_last_dream_snapshot_overview`.
//!
//! Bundle objects are JS-owned values: key insertion order, number formatting
//! and optional-field presence mirror the TS object literals so a serialized
//! export compares byte-for-byte. Validation failures surface
//! [`FileMemoryError::Bundle`] with the TS `MemoryBundleValidationError`
//! message verbatim; a failed import leaves the live roots untouched because
//! everything is staged into sibling temp directories first.
/// `rmSync(dir, { recursive: true, force: true })`: the shared Node-v22 port
/// in [`node_fs`], reused instead of a local reimplementation.
use super::super::node_fs::rm_sync;
use super::{EdgeClawStorage, StorageError};
use crate::metadata::js_trim;
use crate::runtime::memory::edgeclaw::file_memory::{
    EdgeClawFileStores, FileMemoryClock, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
type Result<T> = std::result::Result<T, FileMemoryError>;

pub(crate) const MEMORY_EXPORT_FORMAT_VERSION: &str = "clawxmemory-memory-snapshot.v4";
const GLOBAL_MEMORY_PREFIX: &str = "global/";
const GLOBAL_USER_PROFILE_RELATIVE_PATH: &str = "UserIdentity/user-profile.md";
const GLOBAL_USER_NOTES_RELATIVE_DIR: &str = "UserIdentityNotes";
const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";
const LAST_DREAM_AT_STATE_KEY: &str = "lastDreamAt";
const LAST_DREAM_STATUS_STATE_KEY: &str = "lastDreamStatus";
const LAST_DREAM_SUMMARY_STATE_KEY: &str = "lastDreamSummary";
const RECENT_CASE_TRACES_STATE_KEY: &str = "recentCaseTraces";
const RECENT_INDEX_TRACES_STATE_KEY: &str = "recentIndexTraces";
const RECENT_DREAM_TRACES_STATE_KEY: &str = "recentDreamTraces";

// -- local JS-object helpers (same shapes as `storage/accessors.rs`) --------

fn obj(fields: &[(&str, JsValue)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    object
}
fn s(value: impl AsRef<str>) -> JsValue {
    JsValue::from(value.as_ref())
}
fn num(value: usize) -> JsValue {
    JsValue::Number(value as f64)
}
fn text(object: &JsObject, key: &str) -> String {
    object
        .get(key)
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}
fn objects(value: JsValue) -> Vec<JsObject> {
    match &value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(|value| value.as_object().cloned())
            .collect(),
        _ => vec![],
    }
}
fn values(items: Vec<JsObject>) -> JsValue {
    JsValue::Array(items.into_iter().map(JsValue::Object).collect())
}
fn put(object: &mut JsObject, key: &str, value: JsValue) {
    object.insert(key.into(), value);
}
fn bundle_error(message: impl Into<String>) -> FileMemoryError {
    FileMemoryError::Bundle(message.into())
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha1::digest(value.as_bytes()))[..10].into()
}

// -- path helpers (`sqlite.ts:355-380,422-428`) -----------------------------

fn normalize_relative_path(relative_path: &str) -> String {
    relative_path.replace('\\', "/")
}
fn is_global_relative_path(relative_path: &str) -> bool {
    normalize_relative_path(relative_path).starts_with(GLOBAL_MEMORY_PREFIX)
}
pub(super) fn to_exposed_global_relative_path(relative_path: &str) -> String {
    let normalized = normalize_relative_path(relative_path);
    if normalized.starts_with(GLOBAL_MEMORY_PREFIX) {
        normalized
    } else {
        format!("{GLOBAL_MEMORY_PREFIX}{normalized}")
    }
}
fn to_internal_global_relative_path(relative_path: &str) -> String {
    let normalized = normalize_relative_path(relative_path);
    match normalized.strip_prefix(GLOBAL_MEMORY_PREFIX) {
        Some(stripped) => stripped.into(),
        None => normalized,
    }
}
/// Lexical `path.resolve` (no symlink following), like the TS call sites.
fn resolve_path(root: &Path, relative: &str) -> PathBuf {
    let joined = if relative.starts_with('/') {
        PathBuf::from(relative)
    } else {
        root.join(relative)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            component => out.push(component.as_os_str()),
        }
    }
    out
}
fn relative_from(root: &Path, target: &Path) -> String {
    let root = root.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let common = root.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); root.len() - common];
    parts.extend(
        target[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}
/// `isPathWithinRoot` (`sqlite.ts:355-358`).
fn is_path_within_root(root_dir: &Path, target_path: &Path) -> bool {
    let rel = relative_from(root_dir, target_path);
    rel.is_empty() || (!rel.starts_with("..") && !rel.starts_with('/'))
}
/// `createSiblingTempPath` (`sqlite.ts:422-428`): a hidden sibling of the
/// target directory. TS mixes `Date.now()` and `Math.random()`; the exact name
/// is never observable, only its uniqueness within the parent directory. The
/// `Date.now()` half comes from the injected clock so replays observe the
/// same clock reads as TS.
fn create_sibling_temp_path(target_dir: &Path, label: &str, clock: &FileMemoryClock) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = target_dir.parent().unwrap_or_else(|| Path::new("/"));
    let now = hash(&(clock)());
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed) ^ u64::from(std::process::id());
    parent.join(format!(".{base}.{label}-{now}-{unique:016x}"))
}
fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            copy_dir_all(&path, &destination.join(entry.file_name()))?;
        } else {
            fs::copy(&path, destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}
/// `robustRenameSync` (`sqlite.ts:434-448`): rename, falling back to a
/// recursive copy + delete on EPERM/EACCES.
fn robust_rename(source: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            copy_dir_all(source, destination)?;
            rm_sync(source)
        }
        Err(error) => Err(error),
    }
}
/// `replaceDirectoryWithStaged` (`sqlite.ts:1265-1290`): swap the staged
/// directory in for the live one through a backup, restoring the live
/// directory when the swap fails partway. Registered deviation from the TS
/// restore condition (`!existsSync(liveRoot)`, which stays false whenever a
/// partially-installed live root sits at the path — recreated by a concurrent
/// writer between the two renames, or left by a copy fallback that failed
/// mid-copy or at its source removal, single-threaded included): the backup
/// is restored on every swap failure so the pre-import tree is never
/// stranded in the hidden backup directory
/// (`specs/gateway-parity/accepted-diffs.md`,
/// A6.6-EDGECLAW-IMPORT-SWAP-ROLLBACK-001, unapproved).
fn replace_directory_with_staged(
    live_root: &Path,
    staged_root: &Path,
    clock: &FileMemoryClock,
) -> Result<()> {
    let backup_root = create_sibling_temp_path(live_root, "backup", clock);
    let mut moved_live_root = false;
    let outcome: std::io::Result<()> = (|| {
        if live_root.exists() {
            robust_rename(live_root, &backup_root)?;
            moved_live_root = true;
        }
        robust_rename(staged_root, live_root)?;
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            if moved_live_root && backup_root.exists() {
                // Best-effort cleanup; the live directory has already been
                // swapped in.
                let _ = rm_sync(&backup_root);
            }
            Ok(())
        }
        Err(error) => {
            if staged_root.exists() {
                let _ = rm_sync(staged_root);
            }
            if moved_live_root && backup_root.exists() {
                // Drop whatever partially landed at the live root before
                // restoring the pre-import tree.
                if live_root.exists() {
                    let _ = rm_sync(live_root);
                }
                robust_rename(&backup_root, live_root)?;
            }
            Err(error.into())
        }
    }
}

// -- bundle normalization (`sqlite.ts:264-353`) ----------------------------

/// `sanitizeDreamStatus` (`sqlite.ts:239-243`).
pub(super) fn sanitize_dream_status(value: Option<&JsValue>) -> Option<JsString> {
    let status = value.and_then(JsValue::as_string)?;
    (status.equals_str("running")
        || status.equals_str("success")
        || status.equals_str("skipped")
        || status.equals_str("failed"))
    .then(|| status.clone())
}
/// `normalizeSnapshotRelativePath` (`sqlite.ts:264-280`).
fn normalize_snapshot_relative_path(value: Option<&JsValue>, index: usize) -> Result<String> {
    let raw = value
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    let invalid = || bundle_error(format!("Invalid files[{index}].relativePath"));
    let raw = js_trim(&raw).replace('\\', "/");
    if raw.is_empty() || raw.starts_with('/') {
        return Err(invalid());
    }
    let segments: Vec<&str> = raw
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| *segment == "." || *segment == "..")
    {
        return Err(invalid());
    }
    Ok(segments.join("/"))
}
/// `normalizeSnapshotFileRecord` (`sqlite.ts:282-291`); the content check runs
/// before the path check, exactly like TS.
fn normalize_snapshot_file_record(value: &JsValue, index: usize) -> Result<JsObject> {
    let Some(object) = value.as_object() else {
        return Err(bundle_error(format!("Invalid files[{index}]")));
    };
    let Some(content) = object.get("content").and_then(JsValue::as_string) else {
        return Err(bundle_error(format!("Invalid files[{index}].content")));
    };
    let relative_path = normalize_snapshot_relative_path(object.get("relativePath"), index)?;
    Ok(obj(&[
        ("relativePath", s(relative_path)),
        ("content", JsValue::String(content.clone())),
    ]))
}
/// `hasLegacyMultiProjectPath` (`sqlite.ts:293-296`).
fn has_legacy_multi_project_path(relative_path: &str) -> bool {
    relative_path.starts_with("projects/") || relative_path.contains("/project.meta.md")
}
/// `normalizeMemoryBundle` (`sqlite.ts:298-353`). The clock is read only
/// for the `exportedAt` fallback, exactly like the TS `||` short-circuit, so
/// a progressing clock observes the same call order as TS.
fn normalize_memory_bundle(value: &JsValue, clock: &FileMemoryClock) -> Result<JsObject> {
    let Some(record) = value.as_object() else {
        return Err(bundle_error("Invalid memory bundle"));
    };
    let scope = text(record, "scope");
    if !scope.is_empty() && scope != "current_project" {
        return Err(bundle_error(
            "Unsupported memory bundle scope. Expected current_project.",
        ));
    }
    let mut metadata = JsObject::new();
    let exported_at = js_trim(&text(record, "exportedAt")).to_owned();
    if exported_at.is_empty() {
        put(&mut metadata, "exportedAt", s((clock)()));
    } else {
        put(&mut metadata, "exportedAt", s(&exported_at));
    }
    for key in ["lastIndexedAt", "lastDreamAt"] {
        let trimmed = js_trim(&text(record, key)).to_owned();
        if !trimmed.is_empty() {
            put(&mut metadata, key, s(&trimmed));
        }
    }
    if let Some(status) = sanitize_dream_status(record.get("lastDreamStatus")) {
        put(&mut metadata, "lastDreamStatus", JsValue::String(status));
    }
    let summary = js_trim(&text(record, "lastDreamSummary")).to_owned();
    if !summary.is_empty() {
        put(&mut metadata, "lastDreamSummary", s(&summary));
    }
    for (key, id_key) in [
        ("recentCaseTraces", "caseId"),
        ("recentIndexTraces", "indexTraceId"),
        ("recentDreamTraces", "dreamTraceId"),
    ] {
        let value = record.get(key).cloned().unwrap_or(JsValue::Undefined);
        let sanitized = super::sanitize_trace_array(&value, id_key, "startedAt")
            .map_err(FileMemoryError::from)?;
        if !sanitized.is_empty() {
            put(&mut metadata, key, JsValue::Array(sanitized));
        }
    }
    let version_matches = record
        .get("formatVersion")
        .and_then(JsValue::as_string)
        .is_some_and(|version| version.equals_str(MEMORY_EXPORT_FORMAT_VERSION));
    if !version_matches {
        return Err(bundle_error(format!(
            "Unsupported memory bundle formatVersion. Expected {MEMORY_EXPORT_FORMAT_VERSION}."
        )));
    }
    let Some(JsValue::Array(files)) = record.get("files") else {
        return Err(bundle_error("Invalid memory snapshot bundle files"));
    };
    let mut normalized_files = Vec::new();
    for (index, item) in files.iter().enumerate() {
        // TS maps every record before any duplicate or legacy check
        // (`files.map(...)` then the `for` loop), so a later record's
        // validation error outranks an earlier record's duplicate path.
        normalized_files.push(normalize_snapshot_file_record(item, index)?);
    }
    let mut seen_paths: HashSet<String> = HashSet::new();
    for file in &normalized_files {
        let relative_path = text(file, "relativePath");
        if !seen_paths.insert(relative_path.clone()) {
            return Err(bundle_error(format!(
                "Duplicate imported snapshot file path: {relative_path}"
            )));
        }
        if has_legacy_multi_project_path(&relative_path) {
            return Err(bundle_error(
                "Legacy multi-project memory bundles are not supported in current-project memory mode",
            ));
        }
    }
    let mut bundle = JsObject::new();
    put(
        &mut bundle,
        "formatVersion",
        s(MEMORY_EXPORT_FORMAT_VERSION),
    );
    put(&mut bundle, "scope", s("current_project"));
    for (key, value) in metadata.entries() {
        bundle.insert(key.clone(), value.clone());
    }
    put(&mut bundle, "files", values(normalized_files));
    Ok(bundle)
}

// -- store options (`sqlite.ts:1179-1199`) ---------------------------------

fn workspace_store_options(mode: &str) -> FileMemoryStoreOptions {
    FileMemoryStoreOptions {
        workspace_mode: mode.into(),
        manage_project_meta: true,
        manage_project_files: true,
        manage_user_profile: false,
        enable_manifest: true,
        ..Default::default()
    }
}
fn global_store_options() -> FileMemoryStoreOptions {
    FileMemoryStoreOptions {
        manage_project_meta: false,
        manage_project_files: false,
        manage_user_profile: true,
        user_profile_relative_path: Some(GLOBAL_USER_PROFILE_RELATIVE_PATH.into()),
        user_notes_relative_dir: Some(GLOBAL_USER_NOTES_RELATIVE_DIR.into()),
        append_only_user_entries: true,
        enable_manifest: false,
        ..Default::default()
    }
}

// -- import staging (`sqlite.ts:1495-1597`) --------------------------------

/// `buildTransferCounts` (`sqlite.ts:1495-1508`).
fn build_transfer_counts(
    workspace_store: &FileMemoryStore,
    global_store: &FileMemoryStore,
) -> Result<JsObject> {
    let bundle_records = |store: &FileMemoryStore| -> Result<(Vec<JsObject>, usize)> {
        let exported = store.export_bundle_records()?;
        let object = exported.as_object().cloned().unwrap_or_default();
        let files = objects(
            object
                .get("memoryFiles")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        let metas = objects(
            object
                .get("projectMetas")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        )
        .len();
        Ok((files, metas))
    };
    let (workspace_files, workspace_metas) = bundle_records(workspace_store)?;
    let (global_files, _) = bundle_records(global_store)?;
    let memory_files = [workspace_files, global_files].concat();
    let managed_files = objects(workspace_store.export_snapshot_files()?).len()
        + objects(global_store.export_snapshot_files()?).len();
    let count = |kind: &str| {
        memory_files
            .iter()
            .filter(|file| text(file, "type") == kind)
            .count()
    };
    Ok(obj(&[
        ("managedFiles", num(managed_files)),
        ("memoryFiles", num(memory_files.len())),
        ("project", num(count("project"))),
        ("feedback", num(count("feedback"))),
        ("user", num(count("user"))),
        ("tmp", num(0)),
        ("projectMetas", num(workspace_metas)),
    ]))
}
/// `writeSnapshotFilesToRoot` (`sqlite.ts:1521-1531`).
fn write_snapshot_files_to_root(root_dir: &Path, files: &[JsObject]) -> Result<()> {
    fs::create_dir_all(root_dir)?;
    for record in files {
        let relative_path = text(record, "relativePath");
        let absolute_path = resolve_path(root_dir, &relative_path);
        if !is_path_within_root(root_dir, &absolute_path)
            || absolute_path == resolve_path(root_dir, "")
        {
            return Err(bundle_error(format!(
                "Invalid imported snapshot file path: {relative_path}"
            )));
        }
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&absolute_path, text(record, "content"))?;
    }
    Ok(())
}
/// `materializeSnapshotBundle` (`sqlite.ts:1510-1519`).
fn materialize_snapshot_bundle(
    root_dir: &Path,
    files: &[JsObject],
    options: FileMemoryStoreOptions,
    clock: FileMemoryClock,
) -> Result<FileMemoryStore> {
    write_snapshot_files_to_root(root_dir, files)?;
    let store = FileMemoryStore::with_clock(root_dir, options, clock)?;
    store.repair_manifests()?;
    Ok(store)
}
struct StagedImport {
    staged_workspace_root: PathBuf,
    staged_global_root: PathBuf,
    counts: JsObject,
    ignored_global_file_count: usize,
}
impl EdgeClawFileStores {
    /// `stageImportBundle` (`sqlite.ts:1533-1575`).
    fn stage_import_bundle(&self, bundle: &JsObject) -> Result<StagedImport> {
        let files = objects(bundle.get("files").cloned().unwrap_or(JsValue::Undefined));
        let workspace_files: Vec<JsObject> = files
            .iter()
            .filter(|file| !is_global_relative_path(&text(file, "relativePath")))
            .cloned()
            .collect();
        let global_files: Vec<JsObject> = files
            .iter()
            .filter(|file| is_global_relative_path(&text(file, "relativePath")))
            .map(|file| {
                let mut mapped = file.clone();
                put(
                    &mut mapped,
                    "relativePath",
                    s(to_internal_global_relative_path(&text(
                        file,
                        "relativePath",
                    ))),
                );
                mapped
            })
            .collect();
        let live_workspace_root = self.workspace_memory.get_root_dir().to_path_buf();
        let live_global_root = self.global_user_memory.get_root_dir().to_path_buf();
        let clock = self.repository.clock().clone();
        let staged_workspace_root =
            create_sibling_temp_path(&live_workspace_root, "import", &clock);
        let staged_global_root = create_sibling_temp_path(&live_global_root, "import", &clock);
        fs::create_dir_all(&staged_workspace_root)?;
        fs::create_dir_all(&staged_global_root)?;
        let staged = (|| -> Result<StagedImport> {
            let workspace_store = materialize_snapshot_bundle(
                &staged_workspace_root,
                &workspace_files,
                workspace_store_options(self.get_workspace_mode()),
                clock.clone(),
            )?;
            let global_store = materialize_snapshot_bundle(
                &staged_global_root,
                &global_files,
                global_store_options(),
                clock,
            )?;
            Ok(StagedImport {
                staged_workspace_root: staged_workspace_root.clone(),
                staged_global_root: staged_global_root.clone(),
                counts: build_transfer_counts(&workspace_store, &global_store)?,
                ignored_global_file_count: global_files.len(),
            })
        })();
        match staged {
            Ok(staged) => Ok(staged),
            Err(error) => {
                // Both staged directories are removed before rethrowing, like
                // the TS catch block; a failing removal replaces the error.
                let cleanup =
                    rm_sync(&staged_workspace_root).and_then(|()| rm_sync(&staged_global_root));
                match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(cleanup.into()),
                }
            }
        }
    }
    /// `resetImportedRuntimeState` (`sqlite.ts:1577-1597`).
    fn reset_imported_runtime_state(&self, bundle: &JsObject) -> Result<()> {
        self.storage.clear_l0_sessions()?;
        for key in [
            RECENT_CASE_TRACES_STATE_KEY,
            RECENT_INDEX_TRACES_STATE_KEY,
            RECENT_DREAM_TRACES_STATE_KEY,
            LAST_INDEXED_AT_STATE_KEY,
            LAST_DREAM_AT_STATE_KEY,
            LAST_DREAM_STATUS_STATE_KEY,
            LAST_DREAM_SUMMARY_STATE_KEY,
        ] {
            self.storage.delete_pipeline_state(key)?;
        }
        for (bundle_key, state_key) in [
            ("lastIndexedAt", LAST_INDEXED_AT_STATE_KEY),
            ("lastDreamAt", LAST_DREAM_AT_STATE_KEY),
            ("lastDreamStatus", LAST_DREAM_STATUS_STATE_KEY),
            ("lastDreamSummary", LAST_DREAM_SUMMARY_STATE_KEY),
            ("recentCaseTraces", RECENT_CASE_TRACES_STATE_KEY),
            ("recentIndexTraces", RECENT_INDEX_TRACES_STATE_KEY),
            ("recentDreamTraces", RECENT_DREAM_TRACES_STATE_KEY),
        ] {
            if let Some(value) = bundle.get(bundle_key) {
                self.storage.set_pipeline_state(state_key, value)?;
            }
        }
        Ok(())
    }

    // -- export / import (`sqlite.ts:1599-1658`) ----------------------------

    /// `exportMemoryBundle` (`sqlite.ts:1599-1625`). `MEMORY.md` is a derived
    /// manifest regenerated on import, so it is excluded.
    pub fn export_memory_bundle(&self) -> Result<JsValue> {
        let string_state = |key: &str| -> Result<Option<JsString>> {
            Ok(
                match self
                    .storage
                    .get_pipeline_state(key)
                    .map_err(FileMemoryError::from)?
                {
                    Some(JsValue::String(ref value)) => Some(value.clone()),
                    _ => None,
                },
            )
        };
        let mut bundle = JsObject::new();
        put(
            &mut bundle,
            "formatVersion",
            s(MEMORY_EXPORT_FORMAT_VERSION),
        );
        put(&mut bundle, "scope", s("current_project"));
        put(&mut bundle, "exportedAt", s((self.repository.clock())()));
        if let Some(value) = string_state(LAST_INDEXED_AT_STATE_KEY)? {
            put(&mut bundle, "lastIndexedAt", JsValue::String(value));
        }
        if let Some(value) = string_state(LAST_DREAM_AT_STATE_KEY)? {
            put(&mut bundle, "lastDreamAt", JsValue::String(value));
        }
        if let Some(status) = sanitize_dream_status(
            self.storage
                .get_pipeline_state(LAST_DREAM_STATUS_STATE_KEY)
                .map_err(FileMemoryError::from)?
                .as_ref(),
        ) {
            put(&mut bundle, "lastDreamStatus", JsValue::String(status));
        }
        if let Some(value) = string_state(LAST_DREAM_SUMMARY_STATE_KEY)? {
            put(&mut bundle, "lastDreamSummary", JsValue::String(value));
        }
        for (key, id_key, limit) in [
            ("recentCaseTraces", "caseId", 200),
            ("recentIndexTraces", "indexTraceId", 200),
            ("recentDreamTraces", "dreamTraceId", 200),
        ] {
            let traces = match id_key {
                "caseId" => self.storage.list_recent_case_traces(limit)?,
                "indexTraceId" => self.storage.list_recent_index_traces(limit)?,
                _ => self.storage.list_recent_dream_traces(limit)?,
            };
            if !traces.is_empty() {
                put(&mut bundle, key, JsValue::Array(traces));
            }
        }
        let files = objects(self.workspace_memory.export_snapshot_files()?)
            .into_iter()
            .filter(|file| text(file, "relativePath") != "MEMORY.md")
            .collect::<Vec<_>>();
        put(&mut bundle, "files", values(files));
        Ok(JsValue::Object(bundle))
    }
    /// `importMemoryBundle` (`sqlite.ts:1627-1658`).
    pub fn import_memory_bundle(&self, bundle: &JsValue) -> Result<JsValue> {
        let normalized = normalize_memory_bundle(bundle, self.repository.clock())?;
        let staged = self.stage_import_bundle(&normalized)?;
        // The directory swap, manifest repair and runtime reset hold both live
        // stores' per-root operation locks (workspace first, then global — the
        // provider nesting order): a concurrent writer could otherwise rebuild
        // the live root after it was moved to the backup, fail the staged
        // rename with ENOTEMPTY and strand the pre-import tree.
        let _workspace_lock = self.workspace_memory.operation_scope()?;
        let _global_lock = self.global_user_memory.operation_scope()?;
        let outcome = (|| -> Result<JsValue> {
            replace_directory_with_staged(
                self.workspace_memory.get_root_dir(),
                &staged.staged_workspace_root,
                self.repository.clock(),
            )?;
            self.workspace_memory.repair_manifests()?;
            // `clearLastDreamSnapshot` (sqlite.ts:1333) — `storage::snapshots`
            // (pr10e). Its `snapshot_operation` locks are reentrant with the
            // two `operation_scope` guards held above, so the clear stays
            // inside this locked window.
            self.clear_last_dream_snapshot()?;
            self.reset_imported_runtime_state(&normalized)?;
            let mut result = JsObject::new();
            put(
                &mut result,
                "formatVersion",
                s(MEMORY_EXPORT_FORMAT_VERSION),
            );
            put(&mut result, "scope", s("current_project"));
            put(
                &mut result,
                "imported",
                JsValue::Object(staged.counts.clone()),
            );
            // TS reads `nowIso()` here, after the swap and reset, not the
            // normalization timestamp.
            put(&mut result, "importedAt", s((self.repository.clock())()));
            if staged.ignored_global_file_count > 0 {
                put(
                    &mut result,
                    "warnings",
                    JsValue::Array(vec![s(format!(
                        "Ignored {} global user memory file(s) from a legacy current-project bundle.",
                        staged.ignored_global_file_count
                    ))]),
                );
            }
            for key in [
                "lastIndexedAt",
                "lastDreamAt",
                "lastDreamStatus",
                "lastDreamSummary",
                "recentCaseTraces",
                "recentIndexTraces",
                "recentDreamTraces",
            ] {
                if let Some(value) = normalized.get(key) {
                    put(&mut result, key, value.clone());
                }
            }
            Ok(JsValue::Object(result))
        })();
        // `finally { rmSync(stagedGlobalRoot) }`: a failing removal masks the
        // body's result exactly like the TS finally block.
        match rm_sync(&staged.staged_global_root) {
            Ok(()) => outcome,
            Err(error) => Err(error.into()),
        }
    }
}
impl EdgeClawStorage {
    /// `sqlite.ts:1578` — `this.db.exec("DELETE FROM l0_sessions;")` at the
    /// start of `resetImportedRuntimeState`.
    fn clear_l0_sessions(&self) -> std::result::Result<(), StorageError> {
        self.conn()?.execute("DELETE FROM l0_sessions", [])?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Fix rv-codex-r1 P0 / rv-grok-r1 #1: when the staged swap fails after
    /// the live root was moved to the backup, the pre-import tree must be
    /// restored even though a partially-installed directory already sits at
    /// the live root (the old `!live_root.exists()` condition skipped the
    /// restore and stranded the tree in the orphaned backup). `chflags uchg`
    /// on the staged root makes the second rename fall back to a copy and
    /// then fail the source removal — the only deterministic route into that
    /// error path with a populated live root.
    #[test]
    #[cfg(target_os = "macos")]
    fn failed_swap_restores_live_root_even_when_recreated() {
        fn chflags(path: &Path, flags: &str) {
            let status = std::process::Command::new("chflags")
                .arg(flags)
                .arg(path)
                .status()
                .expect("chflags runs");
            assert!(status.success(), "chflags {flags} {}", path.display());
        }
        let parent = std::env::temp_dir().join(format!(
            "pdrs-bundle-swap-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&parent).unwrap();
        let live_root = parent.join("live");
        let staged_root = parent.join("staged");
        fs::create_dir_all(&live_root).unwrap();
        fs::write(live_root.join("original.md"), "original").unwrap();
        fs::create_dir_all(&staged_root).unwrap();
        fs::write(staged_root.join("imported.md"), "imported").unwrap();
        chflags(&staged_root, "uchg");
        let clock: FileMemoryClock = std::sync::Arc::new(|| "unit-test".into());
        let swap = replace_directory_with_staged(&live_root, &staged_root, &clock);
        chflags(&staged_root, "nouchg");
        assert!(swap.is_err(), "the flagged staged root must fail the swap");
        assert_eq!(
            fs::read_to_string(live_root.join("original.md")).unwrap(),
            "original",
            "the pre-import tree must be restored at the live root"
        );
        assert!(
            !live_root.join("imported.md").exists(),
            "the partially-installed copy must not survive the rollback"
        );
        // The flagged staged root survives its best-effort removal, but the
        // pre-import backup must not be left behind as an orphan.
        let backups: Vec<String> = fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".live.backup"))
            .collect();
        assert!(
            backups.is_empty(),
            "no orphaned backup directories may remain: {backups:?}"
        );
        fs::remove_dir_all(&parent).unwrap();
    }

    /// A6.6-EDGECLAW-IMPORT-SWAP-ROLLBACK-001, single-threaded half: a swap
    /// whose copy fallback fails mid-copy. The staged root lives in a
    /// read-only parent, so after the live root moved to the backup the
    /// second rename fails with EACCES and `robust_rename` falls back to a
    /// copy that dies at the unreadable file — leaving a partially-installed
    /// live root, exactly the state whose restore the TS `!existsSync`
    /// condition skips. This is the rollback path's only coverage on Linux
    /// (no `chflags` there).
    #[test]
    #[cfg(unix)]
    fn failed_copy_fallback_restores_live_root() {
        use std::os::unix::fs::PermissionsExt;
        fn set_mode(path: &Path, mode: u32) {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let parent = std::env::temp_dir().join(format!(
            "pdrs-bundle-copy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&parent).unwrap();
        // Restore permissions and remove the scratch tree even when an
        // assertion fails: the read-only staged parent would otherwise
        // survive the cleanup.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::process::Command::new("chmod")
                    .arg("-R")
                    .arg("u+rwx")
                    .arg(&self.0)
                    .status();
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(parent.clone());
        let live_root = parent.join("live");
        let staged_parent = parent.join("staged-parent");
        let staged_root = staged_parent.join("staged");
        fs::create_dir_all(&live_root).unwrap();
        fs::create_dir_all(&staged_root).unwrap();
        fs::write(live_root.join("original.md"), "original").unwrap();
        fs::write(staged_root.join("copied.md"), "copied").unwrap();
        fs::write(staged_root.join("unreadable.md"), "unreadable").unwrap();
        // The read-only staged parent fails the second rename with EACCES
        // (and the fallback's source removal); the unreadable file kills the
        // fallback copy after `copied.md` already landed at the live root.
        set_mode(&staged_root.join("unreadable.md"), 0o000);
        set_mode(&staged_parent, 0o555);
        let clock: FileMemoryClock = std::sync::Arc::new(|| "unit-test".into());
        let swap = replace_directory_with_staged(&live_root, &staged_root, &clock);
        set_mode(&staged_parent, 0o755);
        assert!(
            swap.is_err(),
            "the unreadable staged file must fail the swap: {:?}",
            swap.err()
        );
        assert_eq!(
            fs::read_to_string(live_root.join("original.md")).unwrap(),
            "original",
            "the pre-import tree must be restored at the live root"
        );
        assert!(
            !live_root.join("copied.md").exists(),
            "the partially-installed copy must not survive the rollback"
        );
        // The staged root survives its best-effort removal (its parent is
        // still read-only), but the pre-import backup must not be left
        // behind as an orphan.
        let backups: Vec<String> = fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".live.backup"))
            .collect();
        assert!(
            backups.is_empty(),
            "no orphaned backup directories may remain: {backups:?}"
        );
    }
}
