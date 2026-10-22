//! Memory bundle export/import, staged root swaps and Dream snapshot reads.
//! Source: fixed baseline `core/storage/sqlite.ts:264-353,422-472,1232-1350,1495-1658`.
//!
//! Bundle objects are JS-owned values: key insertion order, number formatting
//! and optional-field presence mirror the TS object literals so a serialized
//! export compares byte-for-byte. Validation failures surface
//! [`FileMemoryError::Bundle`] with the TS `MemoryBundleValidationError`
//! message verbatim; a failed import leaves the live roots untouched because
//! everything is staged into sibling temp directories first.
use super::{EdgeClawStorage, StorageError};
use crate::runtime::memory::edgeclaw::file_memory::{
    EdgeClawFileStores, FileMemoryClock, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
};
use crate::{metadata::js_trim, project_files::node_compare_utf16};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, FileMemoryError>;

pub(crate) const MEMORY_EXPORT_FORMAT_VERSION: &str = "clawxmemory-memory-snapshot.v4";
const GLOBAL_MEMORY_PREFIX: &str = "global/";
const GLOBAL_USER_PROFILE_RELATIVE_PATH: &str = "UserIdentity/user-profile.md";
const GLOBAL_USER_NOTES_RELATIVE_DIR: &str = "UserIdentityNotes";
const LAST_DREAM_SNAPSHOT_DIR: &str = "last_dream";
const LAST_DREAM_SNAPSHOT_METADATA_FILE: &str = "metadata.json";
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
/// is never observable, only its uniqueness within the parent directory.
fn create_sibling_temp_path(target_dir: &Path, label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = target_dir.parent().unwrap_or_else(|| Path::new("/"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed) ^ u64::from(std::process::id());
    parent.join(format!(".{base}.{label}-{now:x}-{unique:016x}"))
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
/// `rmSync(dir, { recursive: true, force: true })`: a missing directory is
/// success, other errors propagate.
fn remove_dir_force(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
/// `robustRenameSync` (`sqlite.ts:434-448`): rename, falling back to a
/// recursive copy + delete on EPERM/EACCES.
fn robust_rename(source: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            copy_dir_all(source, destination)?;
            remove_dir_force(source)
        }
        Err(error) => Err(error),
    }
}
/// `replaceDirectoryWithStaged` (`sqlite.ts:1265-1290`): swap the staged
/// directory in for the live one through a backup, restoring the live
/// directory when the swap fails partway.
fn replace_directory_with_staged(live_root: &Path, staged_root: &Path) -> Result<()> {
    let backup_root = create_sibling_temp_path(live_root, "backup");
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
                let _ = remove_dir_force(&backup_root);
            }
            Ok(())
        }
        Err(error) => {
            if staged_root.exists() {
                let _ = remove_dir_force(staged_root);
            }
            if moved_live_root && !live_root.exists() && backup_root.exists() {
                robust_rename(&backup_root, live_root)?;
            }
            Err(error.into())
        }
    }
}

// -- snapshot file helpers (`sqlite.ts:448-472`) ----------------------------

fn snapshot_collator() -> Result<&'static icu_collator::CollatorBorrowed<'static>> {
    super::trace_collator().map_err(FileMemoryError::from)
}
/// `readSnapshotFiles` (`sqlite.ts:458-472`): walk the directory tree with
/// entries sorted by `name.localeCompare`, returning `{relativePath, content}`
/// records.
fn read_snapshot_files(root_dir: &Path) -> Result<Vec<JsObject>> {
    let mut files = vec![];
    if root_dir.exists() {
        walk_snapshot_dir(root_dir, root_dir, &mut files)?;
    }
    Ok(files)
}
fn walk_snapshot_dir(root_dir: &Path, dir: &Path, out: &mut Vec<JsObject>) -> Result<()> {
    let mut entries = vec![];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.file_type()?,
            entry.path(),
        ));
    }
    if entries.len() >= 2 {
        let collator = snapshot_collator()?;
        entries.sort_by(|a, b| {
            node_compare_utf16(
                collator,
                &JsString::from(a.0.as_str()),
                &JsString::from(b.0.as_str()),
            )
        });
    }
    for (_name, kind, path) in entries {
        if kind.is_dir() {
            walk_snapshot_dir(root_dir, &path, out)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let relative = relative_from(root_dir, &path).replace('\\', "/");
        let content = String::from_utf8_lossy(&fs::read(&path)?).into_owned();
        out.push(obj(&[
            ("relativePath", s(relative)),
            ("content", s(content)),
        ]));
    }
    Ok(())
}
/// `snapshotVersionFromFiles` (`sqlite.ts:452-456`): sha1[:10] of the JSON
/// serialization of the snapshot files, `MEMORY.md` excluded and sorted by
/// relative path.
fn snapshot_version_from_files(files: &[JsObject]) -> Result<String> {
    let mut sorted: Vec<&JsObject> = files
        .iter()
        .filter(|file| text(file, "relativePath") != "MEMORY.md")
        .collect();
    if sorted.len() >= 2 {
        let collator = snapshot_collator()?;
        sorted.sort_by(|a, b| {
            node_compare_utf16(
                collator,
                &JsString::from(text(a, "relativePath")),
                &JsString::from(text(b, "relativePath")),
            )
        });
    }
    let payload = values(sorted.into_iter().cloned().collect());
    let json = stringify_json(&payload)
        .map_err(|error| FileMemoryError::Invalid(format!("{error:?}")))?
        .unwrap_or_default();
    Ok(hash(&json))
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
/// `normalizeMemoryBundle` (`sqlite.ts:298-353`).
fn normalize_memory_bundle(value: &JsValue, now: &str) -> Result<JsObject> {
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
    put(
        &mut metadata,
        "exportedAt",
        s(if exported_at.is_empty() {
            now
        } else {
            &exported_at
        }),
    );
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
    let mut seen_paths: HashSet<String> = HashSet::new();
    for (index, item) in files.iter().enumerate() {
        let file = normalize_snapshot_file_record(item, index)?;
        let relative_path = text(&file, "relativePath");
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
        normalized_files.push(file);
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
/// `loadLastDreamSnapshotRecord`'s result (`sqlite.ts:1311-1325`). The staged
/// file lists are read because TS reads them (a failed read surfaces) but are
/// only consumed by the Dream rollback, a pr10e write path.
#[expect(dead_code, reason = "file lists feed the pr10e Dream rollback")]
struct DreamSnapshotRecord {
    metadata: JsObject,
    workspace_files: Vec<JsObject>,
    global_files: Vec<JsObject>,
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
        let staged_workspace_root = create_sibling_temp_path(&live_workspace_root, "import");
        let staged_global_root = create_sibling_temp_path(&live_global_root, "import");
        fs::create_dir_all(&staged_workspace_root)?;
        fs::create_dir_all(&staged_global_root)?;
        let staged = (|| -> Result<StagedImport> {
            let clock = self.repository.clock().clone();
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
                let cleanup = remove_dir_force(&staged_workspace_root)
                    .and_then(|()| remove_dir_force(&staged_global_root));
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
        let now = (self.repository.clock())();
        let normalized = normalize_memory_bundle(bundle, &now)?;
        let staged = self.stage_import_bundle(&normalized)?;
        let outcome = (|| -> Result<JsValue> {
            replace_directory_with_staged(
                self.workspace_memory.get_root_dir(),
                &staged.staged_workspace_root,
            )?;
            self.workspace_memory.repair_manifests()?;
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
            put(&mut result, "importedAt", s(now));
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
        match remove_dir_force(&staged.staged_global_root) {
            Ok(()) => outcome,
            Err(error) => Err(error.into()),
        }
    }

    // -- Dream snapshot reads (`sqlite.ts:1232-1350`) ----------------------

    fn last_dream_snapshot_root(&self) -> PathBuf {
        self.storage
            .path()
            .parent()
            .map(|parent| parent.join(LAST_DREAM_SNAPSHOT_DIR))
            .unwrap_or_else(|| PathBuf::from(LAST_DREAM_SNAPSHOT_DIR))
    }
    /// `clearLastDreamSnapshot` (`sqlite.ts:1327-1329`).
    pub fn clear_last_dream_snapshot(&self) -> Result<()> {
        remove_dir_force(&self.last_dream_snapshot_root())?;
        Ok(())
    }
    /// `captureLiveMemorySnapshot` (`sqlite.ts:1232-1243`); only the version
    /// halves are observable from the read paths (`counts` and `runtimeState`
    /// feed the Dream rollback, a pr10e write path).
    fn capture_live_memory_snapshot(&self) -> Result<(String, String)> {
        let workspace_files = objects(self.workspace_memory.export_snapshot_files()?);
        let global_files = objects(self.global_user_memory.export_snapshot_files()?);
        Ok((
            snapshot_version_from_files(&workspace_files)?,
            snapshot_version_from_files(&global_files)?,
        ))
    }
    /// `loadLastDreamSnapshotRecord` (`sqlite.ts:1311-1325`).
    fn load_last_dream_snapshot_record(&self) -> Result<Option<DreamSnapshotRecord>> {
        let root = self.last_dream_snapshot_root();
        let metadata_path = root.join(LAST_DREAM_SNAPSHOT_METADATA_FILE);
        if !metadata_path.exists() {
            return Ok(None);
        }
        let raw = String::from_utf8_lossy(&fs::read(&metadata_path)?).into_owned();
        let Some(metadata) = parse_json(&raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
        else {
            return Ok(None);
        };
        if !matches!(metadata.get("version"), Some(JsValue::Number(version)) if *version == 1.0) {
            return Ok(None);
        }
        let workspace_root = root.join("workspace");
        let global_root = root.join("global");
        if !workspace_root.exists() || !global_root.exists() {
            return Ok(None);
        }
        Ok(Some(DreamSnapshotRecord {
            metadata,
            workspace_files: read_snapshot_files(&workspace_root)?,
            global_files: read_snapshot_files(&global_root)?,
        }))
    }
    /// `getLastDreamSnapshotOverview` (`sqlite.ts:1331-1350`).
    pub fn get_last_dream_snapshot_overview(&self) -> Result<Option<JsObject>> {
        let Some(record) = self.load_last_dream_snapshot_record()? else {
            return Ok(None);
        };
        let (workspace_version, global_version) = self.capture_live_memory_snapshot()?;
        let metadata = &record.metadata;
        let after = metadata
            .get("after")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default();
        let rollback_ready = text(&after, "workspaceVersion") == workspace_version
            && text(&after, "globalVersion") == global_version;
        let mut overview = obj(&[
            ("capturedAt", s(text(metadata, "capturedAt"))),
            ("sourceAction", s(text(metadata, "sourceAction"))),
            (
                "sourceWorkspaceDir",
                s(text(metadata, "sourceWorkspaceDir")),
            ),
        ]);
        for key in ["trigger", "dreamTraceId", "summary"] {
            let value = text(metadata, key);
            if !value.is_empty() {
                put(&mut overview, key, s(value));
            }
        }
        put(
            &mut overview,
            "rollbackReady",
            JsValue::Bool(rollback_ready),
        );
        if !rollback_ready {
            put(
                &mut overview,
                "warning",
                s(
                    "Current memory state no longer matches the last Dream snapshot, so rollback is unavailable.",
                ),
            );
        }
        Ok(Some(overview))
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
