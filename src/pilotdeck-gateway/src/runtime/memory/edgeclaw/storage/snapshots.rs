//! Dream snapshot lifecycle, in the statement order of `sqlite.ts:1179-1507`.
//! JSON-owned boundaries deliberately retain optional/unknown fields and WTF-16.
//! Operations are synchronous per live root, like a TS event-loop call. There is
//! no cross-process lock or transaction in TS; failures can leave a new inverse
//! snapshot installed before live roots or pipeline state have changed.
use super::super::file_memory::{
    EdgeClawFileStores, FileMemoryError, FileMemoryStoreOptions, OperationScope, absolute,
    io_mkdir, io_write, repository_global_options, repository_workspace_options,
};
use crate::{
    metadata::object,
    project_files::{node_collator, node_compare_utf16, node_locale},
    runtime::memory::js_trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha1::{Digest, Sha1};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};
type Result<T> = std::result::Result<T, FileMemoryError>;
const KEYS: [&str; 3] = ["lastDreamAt", "lastDreamStatus", "lastDreamSummary"];
const WARNING: &str =
    "Current memory state no longer matches the last Dream snapshot, so rollback is unavailable.";
/// A before-operation hook for hosts exercising disk faults. Returning an error
/// has the same effect as the corresponding OS operation failing. Normal hosts
/// use the default no-op hook. FileMemoryStore keeps its existing IO behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotIoOperation {
    Mkdir,
    Write,
    Rename,
    Copy,
    Remove,
}
pub type SnapshotIoHook =
    Arc<dyn Fn(SnapshotIoOperation, &Path, Option<&Path>) -> io::Result<()> + Send + Sync>;
/// The JSON shape is exactly `LiveMemorySnapshot` in TS, not an export bundle.
/// Keeping the boundary as a JS value also preserves lone surrogates in content.
pub type LiveMemorySnapshot = JsValue;
pub struct MemoryRepositoryStage {
    pub repository: EdgeClawFileStores,
    pub snapshot: LiveMemorySnapshot,
    pub staged_workspace_root: PathBuf,
    pub staged_global_root: PathBuf,
    pub staged_db_path: PathBuf,
    io: SnapshotIoHook,
}
impl MemoryRepositoryStage {
    /// Explicit, repeatable cleanup, as in TS. No implicit Drop cleanup: callers
    /// may retain a stage or inspect its partial state after a failed operation.
    pub fn dispose(&self) -> Result<()> {
        let _ = self.repository.close();
        for p in [
            &self.staged_workspace_root,
            &self.staged_global_root,
            &self.staged_db_path,
        ] {
            remove(&self.io, p)?;
        }
        Ok(())
    }
}
fn s(s: impl AsRef<str>) -> JsValue {
    JsValue::from(s.as_ref())
}
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&UNDEFINED)
}
fn required<'a>(v: &'a JsValue, k: &str) -> Result<&'a JsValue> {
    if matches!(v, JsValue::Null | JsValue::Undefined) {
        return Err(FileMemoryError::Invalid(format!(
            "Cannot read properties of {} (reading '{k}')",
            if matches!(v, JsValue::Null) {
                "null"
            } else {
                "undefined"
            }
        )));
    }
    Ok(field(v, k))
}
fn text(v: &JsValue) -> String {
    v.as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn truthy(v: &JsValue) -> bool {
    super::js_truthy(v)
}
fn array(v: &JsValue) -> Result<&[JsValue]> {
    if let JsValue::Array(a) = v {
        Ok(a)
    } else {
        Err(FileMemoryError::Invalid(
            "snapshot files is not iterable".into(),
        ))
    }
}
fn json(v: &JsValue) -> Result<String> {
    stringify_json(v)
        .map_err(|e| FileMemoryError::Invalid(e.to_string()))?
        .ok_or_else(|| FileMemoryError::Invalid("snapshot JSON is undefined".into()))
}
fn pretty(v: &JsValue) -> Result<String> {
    // Format the shared JS codec's output; never reparse through serde (which
    // would lose WTF-16 and can reorder metadata keys).
    let compact = json(v)?;
    let mut out = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escape = false;
    let mut chars = compact.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escape {
                escape = false
            } else if c == '\\' {
                escape = true
            } else if c == '"' {
                quoted = false
            };
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c)
            }
            '{' | '[' => {
                out.push(c);
                if chars.peek() == Some(&(if c == '{' { '}' } else { ']' })) {
                    out.push(chars.next().unwrap())
                } else {
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth -= 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c)
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    out.push('\n');
    Ok(out)
}
fn mkdir(h: &SnapshotIoHook, p: &Path) -> Result<()> {
    h(SnapshotIoOperation::Mkdir, p, None)
        .map_err(|e| crate::node_io::path_error(e, "mkdir", p))?;
    io_mkdir(p)?;
    Ok(())
}
fn write(h: &SnapshotIoHook, p: &Path, content: &str) -> Result<()> {
    h(SnapshotIoOperation::Write, p, None).map_err(|e| crate::node_io::path_error(e, "open", p))?;
    io_write(p, content)?;
    Ok(())
}
fn remove(h: &SnapshotIoHook, p: &Path) -> Result<()> {
    h(SnapshotIoOperation::Remove, p, None)?;
    super::super::node_fs::rm_sync(p)?;
    Ok(())
}

// Stage accepts one display path; two-path libuv errors render the same quoted
// suffix. This value is diagnostic text only and is never passed to the OS.
fn pair_error(error: io::Error, syscall: &'static str, src: &Path, dst: &Path) -> io::Error {
    let pair = PathBuf::from(format!("{}' -> '{}", src.display(), dst.display()));
    crate::node_io::path_error(error, syscall, &pair)
}
fn copy_read_link(src: &Path) -> io::Result<PathBuf> {
    fs::read_link(src).map_err(|e| crate::node_io::path_error(e, "readlink", src))
}
fn copy_realpath(parent: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(parent).map_err(|e| crate::node_io::path_error(e, "realpath", parent))
}
fn copy_permissions(dst: &Path, permissions: fs::Permissions) -> io::Result<()> {
    fs::set_permissions(dst, permissions).map_err(|e| crate::node_io::path_error(e, "chmod", dst))
}
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let metadata =
        fs::symlink_metadata(src).map_err(|e| crate::node_io::path_error(e, "lstat", src))?;
    if metadata.file_type().is_symlink() {
        // Node 22 cpSync(directory, {recursive:true}) uses its C++ fast path:
        // it resolves the source parent before resolving relative link targets.
        // The JS onLink lexical path applies to copying a link alone or with a
        // filter, neither of which robustRenameSync uses (real-TS probe).
        let target = copy_read_link(src)?;
        let target = if target.is_absolute() {
            target
        } else {
            absolute(&copy_realpath(src.parent().unwrap())?.join(target))
                .map_err(io::Error::other)?
        };
        if fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_symlink()) {
            fs::remove_file(dst).map_err(|e| crate::node_io::path_error(e, "unlink", dst))?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| pair_error(e, "symlink", &target, dst))?;
        #[cfg(windows)]
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(&target, dst)
                .map_err(|e| pair_error(e, "symlink", &target, dst))?;
        } else {
            std::os::windows::fs::symlink_file(&target, dst)
                .map_err(|e| pair_error(e, "symlink", &target, dst))?;
        }
    } else if metadata.is_dir() {
        io_mkdir(dst)?;
        for e in fs::read_dir(src).map_err(|e| crate::node_io::path_error(e, "scandir", src))? {
            let e = e.map_err(|e| crate::node_io::path_error(e, "scandir", src))?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        copy_permissions(dst, metadata.permissions())?;
    } else {
        fs::copy(src, dst).map_err(|e| pair_error(e, "copyfile", src, dst))?;
    }
    Ok(())
}

fn rename(h: &SnapshotIoHook, src: &Path, dst: &Path) -> Result<()> {
    let r = h(SnapshotIoOperation::Rename, src, Some(dst)).and_then(|()| fs::rename(src, dst));
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            h(SnapshotIoOperation::Copy, src, Some(dst))?;
            copy_tree(src, dst)?;
            remove(h, src)
        }
        Err(e) => Err(pair_error(e, "rename", src, dst).into()),
    }
}
/// `createSiblingTempPath(targetDir, label)` (`sqlite.ts:422-429`):
/// `.${basename}.${label}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`.
/// Each call consumes one `Math.random` draw in TS, so the injected fragment
/// seam must draw from the same source the caller's fixtures froze.
fn temp(p: &Path, label: &str, now_ms: i64, fragment: &str) -> PathBuf {
    p.with_file_name(format!(
        ".{}.{}-{}-{}",
        p.file_name().unwrap_or_default().to_string_lossy(),
        label,
        base36(now_ms),
        fragment
    ))
}

/// `Number.prototype.toString(36)` for the integer `Date.now()` — the same
/// digit alphabet and `-` sign V8 emits.
fn base36(mut value: i64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let negative = value < 0;
    let mut out = Vec::new();
    while value != 0 {
        let digit = (value % 36).unsigned_abs() as usize;
        out.push(DIGITS[digit]);
        value /= 36;
    }
    if negative {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ASCII")
}
fn write_files(h: &SnapshotIoHook, root: &Path, files: &JsValue) -> Result<()> {
    mkdir(h, root)?;
    let root = absolute(root)?;
    for record in array(files)? {
        let rel = text(field(record, "relativePath"));
        let path = absolute(&root.join(&rel))?;
        // TS relative().startsWith('..') rejects even '..name' in the root.
        let within = path
            .strip_prefix(&root)
            .ok()
            .is_some_and(|p| !p.as_os_str().is_empty() && !p.to_string_lossy().starts_with(".."));
        if !within {
            return Err(FileMemoryError::Invalid(format!(
                "Invalid imported snapshot file path: {rel}"
            )));
        }
        mkdir(h, path.parent().unwrap())?;
        write(h, &path, &text(field(record, "content")))?;
    }
    Ok(())
}
fn read_files(root: &Path) -> Result<JsValue> {
    fn walk(root: &Path, p: &Path, files: &mut Vec<JsValue>) -> Result<()> {
        let mut entries = fs::read_dir(p)
            .map_err(|e| crate::node_io::path_error(e, "scandir", p))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| crate::node_io::path_error(e, "scandir", p))?;
        if entries.len() > 1 {
            let col = node_collator(&node_locale(|k| std::env::var(k).ok()))
                .map_err(|e| FileMemoryError::Collation(format!("{e:?}")))?;
            entries.sort_by(|a, b| {
                node_compare_utf16(
                    &col,
                    &JsString::from(a.file_name().to_string_lossy().as_ref()),
                    &JsString::from(b.file_name().to_string_lossy().as_ref()),
                )
            });
        }
        for e in entries {
            let p = e.path();
            let ty = e.file_type().map_err(|e| {
                crate::node_io::path_error(e, "scandir", p.parent().unwrap_or(root))
            })?;
            if ty.is_dir() {
                walk(root, &p, files)?
            } else if ty.is_file() {
                files.push(object([
                    (
                        "relativePath",
                        s(p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/")),
                    ),
                    (
                        "content",
                        s(String::from_utf8_lossy(
                            &fs::read(&p).map_err(|e| crate::node_io::path_error(e, "open", &p))?,
                        )),
                    ),
                ]));
            }
        }
        Ok(())
    }
    let mut files = vec![];
    if root.exists() {
        walk(root, root, &mut files)?;
    }
    Ok(JsValue::Array(files))
}
fn version(files: &JsValue) -> Result<JsValue> {
    let mut files = array(files)?
        .iter()
        .filter(|f| field(f, "relativePath") != &s("MEMORY.md"))
        .cloned()
        .collect::<Vec<_>>();
    if files.len() > 1 {
        let col = node_collator(&node_locale(|k| std::env::var(k).ok()))
            .map_err(|e| FileMemoryError::Collation(format!("{e:?}")))?;
        files.sort_by(|a, b| {
            node_compare_utf16(
                &col,
                field(a, "relativePath").as_string().unwrap(),
                field(b, "relativePath").as_string().unwrap(),
            )
        });
    }
    Ok(s(&format!(
        "{:x}",
        Sha1::digest(json(&JsValue::Array(files))?.as_bytes())
    )[..10]))
}
impl EdgeClawFileStores {
    pub fn workspace_store_options(&self) -> FileMemoryStoreOptions {
        repository_workspace_options(self.get_workspace_mode())
    }
    pub fn global_store_options(&self) -> FileMemoryStoreOptions {
        repository_global_options()
    }

    pub fn set_snapshot_io_hook(&mut self, hook: SnapshotIoHook) {
        self.repository.snapshot_io = hook;
    }
    pub(super) fn snapshot_operation<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _scope = self.snapshot_operation_scope()?;
        f()
    }

    /// A holdable [`Self::snapshot_operation`]: composite callers that must
    /// keep the root operation locks across several repository calls (the
    /// dream success tail spans install, replace and the state writes in one
    /// TS-synchronous segment) acquire the guards once here. No LLM await may
    /// happen while the returned scopes are alive. Same-thread calls below
    /// reenter the already-held locks, so the guards compose with every
    /// `snapshot_operation` inside the held region.
    pub(crate) fn snapshot_operation_scope(
        &self,
    ) -> Result<(OperationScope<'_>, OperationScope<'_>)> {
        // The workspace store predeclares the provider's global root and
        // acquires the complete set in root order, even if another repository
        // uses one of these paths in the opposite role.
        let workspace = self.workspace_memory.operation_scope()?;
        let global = self.global_user_memory.operation_scope()?;
        Ok((workspace, global))
    }
    fn runtime_state(&self) -> Result<JsValue> {
        let mut out = JsObject::new();
        for k in KEYS {
            let v = self
                .storage
                .get_pipeline_state(k)?
                .unwrap_or(JsValue::Undefined);
            let valid = if k == "lastDreamStatus" {
                matches!(
                    text(&v).as_str(),
                    "running" | "success" | "skipped" | "failed"
                )
            } else {
                v.as_string()
                    .is_some_and(|s| !js_trim_units(s.units()).is_empty())
            };
            if valid {
                out.insert(k.into(), v);
            }
        }
        Ok(JsValue::Object(out))
    }
    fn restore_runtime(&self, v: &JsValue) -> Result<()> {
        for k in KEYS {
            self.storage.delete_pipeline_state(k)?;
        }
        for k in KEYS {
            let v = required(v, k)?;
            if truthy(v) {
                self.storage.set_pipeline_state(k, v)?;
            }
        }
        Ok(())
    }
    fn counts(&self) -> Result<JsValue> {
        let ws = self.workspace_memory.export_bundle_records()?;
        let gl = self.global_user_memory.export_bundle_records()?;
        let memory = array(field(&ws, "memoryFiles"))?
            .iter()
            .chain(array(field(&gl, "memoryFiles"))?)
            .collect::<Vec<_>>();
        let managed = array(&self.workspace_memory.export_snapshot_files()?)?.len()
            + array(&self.global_user_memory.export_snapshot_files()?)?.len();
        Ok(object([
            ("managedFiles", JsValue::Number(managed as f64)),
            ("memoryFiles", JsValue::Number(memory.len() as f64)),
            (
                "project",
                JsValue::Number(
                    memory
                        .iter()
                        .filter(|v| field(v, "type") == &s("project"))
                        .count() as f64,
                ),
            ),
            (
                "feedback",
                JsValue::Number(
                    memory
                        .iter()
                        .filter(|v| field(v, "type") == &s("feedback"))
                        .count() as f64,
                ),
            ),
            (
                "user",
                JsValue::Number(
                    memory
                        .iter()
                        .filter(|v| field(v, "type") == &s("user"))
                        .count() as f64,
                ),
            ),
            ("tmp", JsValue::Number(0.0)),
            (
                "projectMetas",
                JsValue::Number(array(field(&ws, "projectMetas"))?.len() as f64),
            ),
        ]))
    }
    pub fn capture_current_memory_snapshot(&self) -> Result<LiveMemorySnapshot> {
        self.snapshot_operation(|| {
            let ws = self.workspace_memory.export_snapshot_files()?;
            let gl = self.global_user_memory.export_snapshot_files()?;
            let counts = self.counts()?;
            let wv = version(&ws)?;
            let gv = version(&gl)?;
            Ok(object([
                ("workspaceFiles", ws),
                ("globalFiles", gl),
                ("counts", counts),
                ("workspaceVersion", wv),
                ("globalVersion", gv),
                ("runtimeState", self.runtime_state()?),
            ]))
        })
    }
    fn last_root(&self) -> PathBuf {
        self.storage
            .path()
            .parent()
            .unwrap_or(Path::new("."))
            .join("last_dream")
    }
    fn replace_dir(&self, live: &Path, staged: &Path) -> Result<()> {
        let h = &self.repository.snapshot_io;
        let backup = temp(
            live,
            "backup",
            (self.temp_now_ms)(),
            &(self.temp_random_fragment)(),
        );
        let mut moved = false;
        let result = (|| {
            if live.exists() {
                rename(h, live, &backup)?;
                moved = true;
            }
            rename(h, staged, live)
        })();
        if let Err(e) = result {
            if staged.exists() {
                remove(h, staged)?;
            }
            if moved && !live.exists() && backup.exists() {
                rename(h, &backup, live)?;
            }
            return Err(e);
        }
        if moved && backup.exists() {
            let _ = remove(h, &backup);
        }
        Ok(())
    }
    fn load_last(&self) -> Result<Option<(JsValue, JsValue, JsValue)>> {
        let root = self.last_root();
        let path = root.join("metadata.json");
        if !path.exists() {
            return Ok(None);
        }
        // sqlite.ts:1315 reads the metadata through `readFileSync`: an open
        // error names its path, a descriptor read error (EISDIR after a
        // directory swap, EIO, ...) is `read` without one (node v22.22.0,
        // `_tmp/20261025-issue-656/node-probe/read-stages.out`).
        let mut file =
            fs::File::open(&path).map_err(|e| crate::node_io::path_error(e, "open", &path))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .map_err(|e| crate::node_io::descriptor_error(e, "read"))?;
        let metadata = parse_json(&String::from_utf8_lossy(&bytes)).unwrap_or(JsValue::Null);
        if field(&metadata, "version") != &JsValue::Number(1.0)
            || !root.join("workspace").exists()
            || !root.join("global").exists()
        {
            return Ok(None);
        }
        Ok(Some((
            metadata,
            read_files(&root.join("workspace"))?,
            read_files(&root.join("global"))?,
        )))
    }
    pub fn clear_last_dream_snapshot(&self) -> Result<()> {
        self.snapshot_operation(|| remove(&self.repository.snapshot_io, &self.last_root()))
    }
    pub fn get_last_dream_snapshot_overview(&self) -> Result<JsValue> {
        self.snapshot_operation(|| {
            let Some((meta, _, _)) = self.load_last()? else {
                return Ok(JsValue::Undefined);
            };
            let current = self.capture_current_memory_snapshot()?;
            let ready = ready(&meta, &current)?;
            let mut out = JsObject::new();
            for k in ["capturedAt", "sourceAction", "sourceWorkspaceDir"] {
                out.insert(k.into(), field(&meta, k).clone());
            }
            for k in ["trigger", "dreamTraceId", "summary"] {
                if truthy(field(&meta, k)) {
                    out.insert(k.into(), field(&meta, k).clone());
                }
            }
            out.insert("rollbackReady".into(), JsValue::Bool(ready));
            if !ready {
                out.insert("warning".into(), s(WARNING));
            }
            Ok(JsValue::Object(out))
        })
    }
    pub fn create_dream_stage(&self, label: &str) -> Result<MemoryRepositoryStage> {
        self.snapshot_operation(|| self.stage_from(&self.capture_current_memory_snapshot()?, label))
    }
    fn stage_from(&self, snapshot: &JsValue, label: &str) -> Result<MemoryRepositoryStage> {
        let h = &self.repository.snapshot_io;
        let now_ms = (self.temp_now_ms)();
        // Three separate `Math.random` draws, one per path, like TS.
        let ws = temp(
            self.workspace_memory.get_root_dir(),
            label,
            now_ms,
            &(self.temp_random_fragment)(),
        );
        let gl = temp(
            self.global_user_memory.get_root_dir(),
            label,
            now_ms,
            &(self.temp_random_fragment)(),
        );
        let db = temp(
            self.storage.path(),
            label,
            now_ms,
            &(self.temp_random_fragment)(),
        );
        // Deliberately outside try, matching TS's partial mkdir failure state.
        mkdir(h, &ws)?;
        mkdir(h, &gl)?;
        let mut repo = None;
        let result = (|| {
            write_files(h, &ws, field(snapshot, "workspaceFiles"))?;
            write_files(h, &gl, field(snapshot, "globalFiles"))?;
            repo = Some(
                Self::with_workspace_clock(
                    &db,
                    &ws,
                    &gl,
                    self.current_workspace()?,
                    &self.repository.general_workspace,
                    self.repository.clock.clone(),
                )?
                .with_temp_name_seams(self.temp_now_ms.clone(), self.temp_random_fragment.clone()),
            );
            repo.as_ref().unwrap().repair_workspace_manifest()?;
            Ok(())
        })();
        if let Err(e) = result {
            if let Some(r) = &repo {
                let _ = r.close();
            }
            remove(h, &ws)?;
            remove(h, &gl)?;
            remove(h, &db)?;
            return Err(e);
        }
        Ok(MemoryRepositoryStage {
            repository: repo.unwrap(),
            snapshot: snapshot.clone(),
            staged_workspace_root: ws,
            staged_global_root: gl,
            staged_db_path: db,
            io: h.clone(),
        })
    }
    pub fn replace_live_roots_with_stage(
        &self,
        stage: &MemoryRepositoryStage,
        fallback: &LiveMemorySnapshot,
    ) -> Result<()> {
        self.snapshot_operation(|| {
            let mut swapped = false;
            let result = (|| {
                self.replace_dir(
                    self.workspace_memory.get_root_dir(),
                    &stage.staged_workspace_root,
                )?;
                swapped = true;
                self.replace_dir(
                    self.global_user_memory.get_root_dir(),
                    &stage.staged_global_root,
                )
            })();
            if let Err(e) = result {
                if swapped {
                    let restore = self.stage_from(fallback, "restore")?;
                    let result = (|| {
                        restore.repository.close()?;
                        self.replace_dir(
                            self.workspace_memory.get_root_dir(),
                            &restore.staged_workspace_root,
                        )
                    })();
                    let cleanup = restore.dispose();
                    cleanup?;
                    result?;
                }
                return Err(e);
            }
            Ok(())
        })
    }
    pub fn install_last_dream_snapshot(
        &self,
        snapshot: &LiveMemorySnapshot,
        metadata: &JsValue,
    ) -> Result<()> {
        self.snapshot_operation(|| {
            let h = &self.repository.snapshot_io;
            let stage = temp(
                &self.last_root(),
                "snapshot",
                (self.temp_now_ms)(),
                &(self.temp_random_fragment)(),
            );
            mkdir(h, &stage)?;
            let result = (|| {
                write_files(
                    h,
                    &stage.join("workspace"),
                    field(snapshot, "workspaceFiles"),
                )?;
                write_files(h, &stage.join("global"), field(snapshot, "globalFiles"))?;
                write(h, &stage.join("metadata.json"), &pretty(metadata)?)
            })();
            if let Err(e) = result {
                remove(h, &stage)?;
                return Err(e);
            }
            self.replace_dir(&self.last_root(), &stage)
        })
    }
    pub fn rollback_last_dream_snapshot(&self) -> Result<JsValue> {
        self.snapshot_operation(|| {
            let Some((meta, ws, gl)) = self.load_last()? else {
                return Err(FileMemoryError::Invalid(
                    "No last Dream snapshot is available for rollback.".into(),
                ));
            };
            let current = self.capture_current_memory_snapshot()?;
            if !ready(&meta, &current)? {
                return Err(FileMemoryError::Invalid(WARNING.into()));
            }
            let at = (self.repository.clock)();
            let before = field(&meta, "before");
            let workspace = self
                .storage
                .get_pipeline_state("workspaceDir")?
                .unwrap_or(JsValue::Undefined);
            let workspace = if workspace
                .as_string()
                .is_some_and(|s| !js_trim_units(s.units()).is_empty())
            {
                workspace
            } else {
                s("")
            };
            let next = object([
                ("version", JsValue::Number(1.0)),
                ("capturedAt", s(&at)),
                ("sourceAction", s("rollback")),
                ("sourceWorkspaceDir", workspace),
                (
                    "summary",
                    field(field(&current, "runtimeState"), "lastDreamSummary").clone(),
                ),
                ("before", boundary(&current)),
                (
                    "after",
                    object([
                        (
                            "workspaceVersion",
                            required(before, "workspaceVersion")?.clone(),
                        ),
                        ("globalVersion", required(before, "globalVersion")?.clone()),
                        ("counts", required(before, "counts")?.clone()),
                        ("runtimeState", required(before, "runtimeState")?.clone()),
                    ]),
                ),
            ]);
            self.install_last_dream_snapshot(&current, &next)?;
            let snapshot = object([
                ("workspaceFiles", ws),
                ("globalFiles", gl),
                ("counts", field(before, "counts").clone()),
                (
                    "workspaceVersion",
                    field(before, "workspaceVersion").clone(),
                ),
                ("globalVersion", field(before, "globalVersion").clone()),
                ("runtimeState", field(before, "runtimeState").clone()),
            ]);
            let stage = self.stage_from(&snapshot, "rollback")?;
            let result = (|| {
                stage.repository.close()?;
                self.replace_live_roots_with_stage(&stage, &current)
            })();
            let cleanup = stage.dispose();
            cleanup?;
            result?;
            self.restore_runtime(field(before, "runtimeState"))?;
            let mut out = object([
                ("rolledBackAt", s(at)),
                ("snapshotCapturedAt", field(&meta, "capturedAt").clone()),
                ("restored", field(before, "counts").clone()),
            ])
            .as_object()
            .unwrap()
            .clone();
            for k in KEYS {
                let v = required(field(before, "runtimeState"), k)?;
                if truthy(v) {
                    out.insert(k.into(), v.clone());
                }
            }
            Ok(JsValue::Object(out))
        })
    }
}
fn ready(meta: &JsValue, current: &JsValue) -> Result<bool> {
    let after = field(meta, "after");
    Ok(
        required(after, "workspaceVersion")? == field(current, "workspaceVersion")
            && required(after, "globalVersion")? == field(current, "globalVersion"),
    )
}
fn boundary(v: &JsValue) -> JsValue {
    object([
        ("workspaceVersion", field(v, "workspaceVersion").clone()),
        ("globalVersion", field(v, "globalVersion").clone()),
        ("counts", field(v, "counts").clone()),
        ("runtimeState", field(v, "runtimeState").clone()),
    ])
}

#[cfg(all(test, unix))]
use super::super::file_memory::round6_io;
#[cfg(all(test, unix))]
mod round6_tests {
    use super::*;
    fn last(root: &Path) -> PathBuf {
        let last = root.join("last_dream");
        fs::create_dir_all(last.join("workspace")).unwrap();
        fs::create_dir_all(last.join("global")).unwrap();
        fs::write(last.join("metadata.json"), "{\"version\":1}").unwrap();
        last
    }
    #[test]
    fn copy_tree_unlink_matches_real_ts_delete_boundary() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        round6_io::check("copy-unlink", |_repo, root| {
            let target = root.join("unlink-target");
            fs::write(&target, "target").unwrap();
            let source = root.join("source-link");
            symlink(&target, &source).unwrap();
            let parent = root.join("current/memory/Project");
            let destination = parent.join("copied.md");
            symlink(&target, &destination).unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
            copy_tree(&source, &destination).unwrap_err().into()
        });
    }
    #[test]
    fn copy_tree_scandir_matches_node_boundary() {
        round6_io::check("snapshot-scandir", |_repo, root| {
            let path = last(root).join("workspace");
            round6_io::deny(&path, true);
            copy_tree(&path, &root.join("copy")).unwrap_err().into()
        });
    }
    #[test]
    fn copy_tree_mkdir_matches_node_boundary() {
        round6_io::check("snapshot-mkdir", |_repo, root| {
            let source = root.join("source");
            fs::create_dir(&source).unwrap();
            let path = root.join("blocked");
            fs::create_dir(&path).unwrap();
            round6_io::deny(&path, true);
            copy_tree(&source, &path.join("new")).unwrap_err().into()
        });
    }
    #[test]
    fn mkdir_hook_matches_real_node_error() {
        round6_io::check("snapshot-mkdir", |_repo, root| {
            let hook: SnapshotIoHook = Arc::new(|_, _, _| Err(io::Error::from_raw_os_error(13)));
            mkdir(&hook, &root.join("blocked/new")).unwrap_err()
        });
    }
    #[test]
    fn write_hook_matches_real_node_error() {
        round6_io::check("snapshot-write", |_repo, root| {
            let hook: SnapshotIoHook = Arc::new(|_, _, _| Err(io::Error::from_raw_os_error(13)));
            write(&hook, &root.join("write/locked.md"), "new").unwrap_err()
        });
    }
    #[test]
    fn snapshot_scandir_matches_real_ts() {
        round6_io::check("snapshot-scandir", |repo, root| {
            let last = last(root);
            round6_io::deny(&last.join("workspace"), true);
            repo.get_last_dream_snapshot_overview().unwrap_err()
        });
    }
    #[test]
    fn snapshot_read_matches_real_ts() {
        round6_io::check("snapshot-read", |repo, root| {
            let path = last(root).join("workspace/locked.md");
            fs::write(&path, "saved").unwrap();
            round6_io::deny(&path, false);
            repo.get_last_dream_snapshot_overview().unwrap_err()
        });
    }
    #[test]
    fn metadata_read_matches_real_ts() {
        round6_io::check("metadata-read", |repo, root| {
            round6_io::deny(&last(root).join("metadata.json"), false);
            repo.get_last_dream_snapshot_overview().unwrap_err()
        });
    }
    /// sqlite.ts:1315 reads the metadata through `readFileSync`, so a
    /// directory occupying metadata.json (the exists check passes, the
    /// read-only open succeeds on POSIX) must report the descriptor read
    /// stage like Node: `EISDIR: illegal operation on a directory, read`
    /// without the path (node v22.22.0,
    /// `_tmp/20261025-issue-656/node-probe/read-stages.out`), not the open
    /// label with it.
    #[test]
    fn metadata_directory_reports_the_read_stage() {
        let root = std::env::temp_dir().join(format!(
            "pdrs-snapshot-eisdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let repo = EdgeClawFileStores::with_workspace_clock(
            root.join("control.sqlite"),
            root.join("current/memory"),
            root.join("global"),
            root.join("workspace"),
            root.join("workspace"),
            Arc::new(|| "2026-10-23T00:00:00.000Z".into()),
        )
        .unwrap();
        let last = root.join("last_dream");
        fs::create_dir_all(last.join("workspace")).unwrap();
        fs::create_dir_all(last.join("global")).unwrap();
        fs::create_dir(last.join("metadata.json")).unwrap();
        let error = repo.get_last_dream_snapshot_overview().unwrap_err();
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            error.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );
        assert_eq!(error.io_code(), Some("EISDIR"));
    }
    #[test]
    fn snapshot_mkdir_matches_real_ts() {
        round6_io::check("snapshot-mkdir", |repo, root| {
            let path = root.join("blocked");
            fs::create_dir(&path).unwrap();
            round6_io::deny(&path, true);
            write_files(
                &repo.repository.snapshot_io,
                &path.join("new"),
                &JsValue::Array(vec![]),
            )
            .unwrap_err()
        });
    }
    #[test]
    fn snapshot_write_matches_real_ts() {
        round6_io::check("snapshot-write", |repo, root| {
            let path = root.join("write");
            fs::create_dir(&path).unwrap();
            fs::write(path.join("locked.md"), "old").unwrap();
            round6_io::deny(&path.join("locked.md"), false);
            let files = JsValue::Array(vec![object([
                ("relativePath", s("locked.md")),
                ("content", s("new")),
            ])]);
            write_files(&repo.repository.snapshot_io, &path, &files).unwrap_err()
        });
    }
}

#[cfg(all(test, unix))]
#[path = "../../../../../tests/fixtures/edgeclaw-entries/copy_io_replay.rs"]
mod round7_tests;
