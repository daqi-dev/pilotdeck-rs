#![allow(clippy::result_large_err)]
//! Built-in file tools. Every implementation follows the fixed TS baseline
//! under `resources/PilotDeck/src/tool/builtin`; filesystem paths are resolved
//! lexically in UTF-16 first so path identity and state keys keep the original
//! wire spelling, and only the OS boundary uses an `OsStr`.
pub mod edit_file;
pub mod edit_notebook;
pub mod glob;
pub mod read_file;
pub mod time;
pub mod write_file;

use crate::{
    host::HostError,
    runtime::tools::{ExecutableTool, ToolRegistration, ToolRegistry},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

/// Register the read-only builtin family on a registry. `grep` is intentionally
/// absent from this slice (A6.5-pr4b).
pub fn register_read_only_tools(
    registry: &ToolRegistry,
) -> Result<Vec<ToolRegistration>, HostError> {
    read_only_tools()
        .into_iter()
        .map(|tool| registry.register(tool))
        .collect()
}

pub fn read_only_tools() -> Vec<Arc<dyn ExecutableTool>> {
    vec![
        Arc::new(read_file::ReadFileTool::new()),
        Arc::new(glob::GlobTool::new()),
        Arc::new(time::GetCurrentTimeTool::new()),
    ]
}

/// A6.5-pr5 write family. Registered separately from the read-only catalog so
/// A6.5-pr3's permission layer can group mutations.
pub fn write_tools() -> Vec<Arc<dyn ExecutableTool>> {
    vec![
        Arc::new(write_file::WriteFileTool::new()),
        Arc::new(edit_file::EditFileTool::new()),
        Arc::new(edit_notebook::EditNotebookTool::new()),
    ]
}

pub fn register_write_tools(registry: &ToolRegistry) -> Result<Vec<ToolRegistration>, HostError> {
    write_tools()
        .into_iter()
        .map(|tool| registry.register(tool))
        .collect()
}

pub(super) fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}

pub(super) fn string(value: impl Into<JsString>) -> JsValue {
    JsValue::String(value.into())
}

pub(super) fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

pub(super) fn schema(json: &str) -> JsObject {
    match parse_json(json) {
        Ok(value) => match &value {
            JsValue::Object(object) => object.clone(),
            _ => panic!("builtin input schema must be a JSON object"),
        },
        _ => panic!("builtin input schema must be valid JSON"),
    }
}

pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}

pub(super) fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}

pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}

/// UTF-16 lexical pieces of POSIX `path` operations. The tools only run on
/// Unix hosts today, but keeping these as units avoids a lossy round trip for
/// keys derived from the model input.
pub(crate) fn is_absolute(path: &JsString) -> bool {
    path.units()
        .first()
        .is_some_and(|unit| *unit == b'/' as u16)
}

pub(crate) fn normalize(path: &JsString) -> JsString {
    let mut parts: Vec<Vec<u16>> = Vec::new();
    let absolute = is_absolute(path);
    for part in path.units().split(|unit| *unit == b'/' as u16) {
        if part.is_empty() || part == [b'.' as u16] {
            continue;
        }
        if part == [b'.' as u16, b'.' as u16] {
            parts.pop();
        } else {
            parts.push(part.to_vec());
        }
    }
    let mut units = Vec::new();
    if absolute {
        units.push(b'/' as u16);
    }
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.push(b'/' as u16);
        }
        units.extend_from_slice(part);
    }
    if units.is_empty() {
        units.push(b'.' as u16);
    }
    JsString::from_utf16(units)
}

pub(crate) fn join(base: &JsString, child: &JsString) -> JsString {
    if is_absolute(child) {
        return normalize(child);
    }
    let mut units = base.units().to_vec();
    if units.last() != Some(&(b'/' as u16)) {
        units.push(b'/' as u16);
    }
    units.extend_from_slice(child.units());
    normalize(&JsString::from_utf16(units))
}

pub(crate) fn dirname(path: &JsString) -> JsString {
    let units = path.units();
    if units.is_empty() {
        return JsString::from(".");
    }
    let mut end = units.len();
    while end > 1 && units[end - 1] == b'/' as u16 {
        end -= 1;
    }
    let Some(index) = units[..end].iter().rposition(|unit| *unit == b'/' as u16) else {
        return JsString::from(".");
    };
    if index == 0 {
        return JsString::from("/");
    }
    JsString::from_utf16(units[..index].to_vec())
}

pub(crate) fn basename(path: &JsString) -> JsString {
    let units = path.units();
    let mut end = units.len();
    while end > 1 && units[end - 1] == b'/' as u16 {
        end -= 1;
    }
    let start = units[..end]
        .iter()
        .rposition(|unit| *unit == b'/' as u16)
        .map_or(0, |index| index + 1);
    JsString::from_utf16(units[start..end].to_vec())
}

pub(super) fn extname(path: &JsString) -> JsString {
    let base = basename(path);
    let units = base.units();
    let Some(index) = units.iter().rposition(|unit| *unit == b'.' as u16) else {
        return JsString::from("");
    };
    if index == 0 {
        return JsString::from("");
    }
    JsString::from_utf16(units[index..].to_vec())
}

pub(super) fn lower(value: &JsString) -> String {
    text(value).to_lowercase()
}

/// `path.relative(from, to)`; callers add `|| "."` where the baseline does.
pub(crate) fn relative(from: &JsString, to: &JsString) -> JsString {
    let from = normalize(from);
    let to = normalize(to);
    let from_parts: Vec<&[u16]> = from
        .units()
        .split(|unit| *unit == b'/' as u16)
        .filter(|part| !part.is_empty())
        .collect();
    let to_parts: Vec<&[u16]> = to
        .units()
        .split(|unit| *unit == b'/' as u16)
        .filter(|part| !part.is_empty())
        .collect();
    let shared = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts: Vec<Vec<u16>> = Vec::new();
    for _ in shared..from_parts.len() {
        parts.push(vec![b'.' as u16, b'.' as u16]);
    }
    for part in &to_parts[shared..] {
        parts.push(part.to_vec());
    }
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.push(b'/' as u16);
        }
        units.extend_from_slice(part);
    }
    JsString::from_utf16(units)
}

pub(super) fn within_root(candidate: &JsString, root: &JsString) -> bool {
    let relative = relative(root, candidate);
    let text = text(&relative);
    text.is_empty() || (!text.starts_with("..") && !text.starts_with('/'))
}

pub(super) fn os_path(path: &JsString) -> PathBuf {
    PathBuf::from(String::from_utf16_lossy(path.units()))
}

pub(super) fn js_path(path: &Path) -> JsString {
    JsString::from(path.to_string_lossy().as_ref())
}

pub(super) fn canonical(path: &JsString) -> Option<JsString> {
    std::fs::canonicalize(os_path(path))
        .ok()
        .map(|path| js_path(&path))
}

pub(super) fn cwd(context: &crate::runtime::tools::ToolRunContext) -> JsString {
    context
        .fields
        .get("cwd")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| JsString::from("."))
}

pub(super) fn permission_mode(context: &crate::runtime::tools::ToolRunContext) -> JsString {
    context
        .fields
        .get("permissionMode")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| JsString::from("default"))
}

pub(super) fn additional_working_directories(
    context: &crate::runtime::tools::ToolRunContext,
) -> Vec<JsString> {
    context
        .fields
        .get("permissionContext")
        .and_then(|value| value.get("additionalWorkingDirectories"))
        .and_then(|value| match value {
            JsValue::Array(directories) => Some(directories),
            _ => None,
        })
        .map(|directories| {
            directories
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn tool_error(
    code: &str,
    message: impl Into<JsString>,
) -> crate::runtime::tools::ToolError {
    crate::runtime::tools::ToolError::new(code, message)
}

pub(super) fn invalid(message: impl Into<JsString>) -> crate::runtime::tools::ToolError {
    tool_error("invalid_tool_input", message)
}

pub(super) fn failed(
    error: std::io::Error,
    operation: &str,
    path: &JsString,
) -> crate::runtime::tools::ToolError {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT: no such file or directory",
        std::io::ErrorKind::PermissionDenied => "EACCES: permission denied",
        std::io::ErrorKind::AlreadyExists => "EEXIST: file already exists",
        std::io::ErrorKind::InvalidInput => "EINVAL: invalid argument",
        _ => return crate::runtime::tools::ToolError::failed(error.to_string()),
    };
    let message = format!("{kind}, {operation} '{}'", text(path));
    crate::runtime::tools::ToolError::failed(message)
}

pub(super) fn stat(path: &JsString) -> Result<Stat, crate::runtime::tools::ToolError> {
    let metadata = std::fs::metadata(os_path(path)).map_err(|error| failed(error, "stat", path))?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let milliseconds = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as f64,
        Err(error) => -(error.duration().as_millis() as f64),
    };
    Ok(Stat {
        kind: if metadata.is_file() {
            StatKind::File
        } else if metadata.is_dir() {
            StatKind::Directory
        } else {
            StatKind::Other
        },
        size: metadata.len(),
        mtime_ms: milliseconds.floor(),
    })
}

pub(super) struct Stat {
    pub kind: StatKind,
    pub size: u64,
    pub mtime_ms: f64,
}

pub(super) enum StatKind {
    File,
    Directory,
    Other,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedPath {
    pub absolute: JsString,
    pub relative: JsString,
}

/// `resolvePilotDeckWorkspacePath` for the read-only family. `allow_registered`
/// covers read_file's registered read grants; `allow_outside` is the caller's
/// explicit permission decision, never a model-controlled input.
pub(super) fn resolve_workspace_path(
    input: &JsString,
    context: &crate::runtime::tools::ToolRunContext,
    allow_registered: bool,
    allow_outside: bool,
) -> Result<ResolvedPath, crate::runtime::tools::ToolError> {
    let input_text = text(input);
    if input.is_empty() || input_text.contains('\0') {
        return Err(invalid(
            "Path must be a non-empty string without null bytes.",
        ));
    }
    let cwd = cwd(context);
    let absolute = if is_absolute(input) {
        normalize(input)
    } else {
        join(&cwd, input)
    };
    let relative_cwd = relative(&cwd, &absolute);
    let relative_cwd = if relative_cwd.is_empty() {
        JsString::from(".")
    } else {
        relative_cwd
    };
    if permission_mode(context).equals_str("bypassPermissions") {
        return Ok(ResolvedPath {
            absolute,
            relative: relative_cwd.clone(),
        });
    }

    let mut roots = vec![normalize(&cwd)];
    roots.extend(
        additional_working_directories(context)
            .iter()
            .map(normalize),
    );
    let root = roots
        .iter()
        .find(|root| within_root(&absolute, root))
        .cloned();

    let Some(root) = root else {
        if allow_registered {
            let Some(real) = canonical(&absolute) else {
                return Err(tool_error(
                    "file_not_found",
                    format!("File {} does not exist.", input_text),
                ));
            };
            let allowed =
                context.host.files.allowed_read_files.iter().any(|allowed| {
                    canonical(allowed).unwrap_or_else(|| normalize(allowed)) == real
                }) || is_managed_attachment(&real, context);
            if allowed {
                return Ok(ResolvedPath {
                    absolute,
                    relative: relative_cwd.clone(),
                });
            }
        }
        if allow_outside {
            return Ok(ResolvedPath {
                absolute,
                relative: relative_cwd.clone(),
            });
        }
        return Err(tool_error(
            "path_not_allowed",
            format!("Path {input_text} is outside the PilotDeck workspace."),
        ));
    };

    let relative = relative(&root, &absolute);
    let relative = if relative.is_empty() {
        JsString::from(".")
    } else {
        relative
    };
    if let Some(real) = canonical(&absolute) {
        let real_root = canonical(&root).unwrap_or_else(|| root.clone());
        if !within_root(&real, &real_root) {
            return Err(tool_error(
                "path_not_allowed",
                format!("Path {input_text} resolves outside the PilotDeck workspace."),
            ));
        }
    } else {
        return Err(tool_error(
            "file_not_found",
            format!("File {input_text} does not exist."),
        ));
    }
    Ok(ResolvedPath { absolute, relative })
}

fn is_managed_attachment(real: &JsString, context: &crate::runtime::tools::ToolRunContext) -> bool {
    let pilot_home = context
        .fields
        .get("env")
        .and_then(|env| env.get("PILOT_HOME"))
        .and_then(JsValue::as_string)
        .map(text)
        .or_else(|| std::env::var("PILOT_HOME").ok())
        .unwrap_or_else(|| {
            let home = context
                .fields
                .get("env")
                .and_then(|env| env.get("HOME"))
                .and_then(JsValue::as_string)
                .map(text)
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_default();
            format!("{home}/.pilotdeck")
        });
    let root = js_path(&os_path(&JsString::from(pilot_home)).join("im-attachments"));
    let root = canonical(&root).unwrap_or(root);
    within_root(real, &root)
        && std::fs::metadata(os_path(real)).is_ok_and(|metadata| metadata.is_file())
}

/// Shared bounded `String(value)` conversion. Nesting past V8's limit is the
/// same RangeError the TS template literal raises, never a Rust stack overflow.
pub(super) fn js_value_string(
    value: &JsValue,
) -> Result<JsString, crate::runtime::tools::ToolError> {
    crate::js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| {
            crate::runtime::tools::ToolError::failed(crate::js_value::StackOverflow::MESSAGE)
        })
}

// ---------------------------------------------------------------------------
// Write-family support (write_file / edit_file / edit_notebook).
//
// `writeSnapshots.ts`, `pathSafety.ts` for writes and `NodeFsPort.writeText`.
// These stay in the shared module so the three tools cannot drift apart.

const WRITE_DENY_DIRECTORIES: &[&str] = &[".git", "node_modules", "dist"];
const NOT_READ: &str = "File has not been read yet. Read it first before writing to it.";
const CHANGED: &str = "File has changed since the last read. Read it again before writing to it.";

pub(super) fn read_text_file(path: &JsString) -> Result<String, crate::runtime::tools::ToolError> {
    let metadata = fs::metadata(os_path(path)).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            tool_error(
                "file_not_found",
                format!("File {} does not exist.", text(path)),
            )
        } else {
            failed(error, "stat", path)
        }
    })?;
    if !metadata.is_file() {
        return Err(tool_error(
            "file_conflict",
            format!("{} is not a regular file.", text(path)),
        ));
    }
    let bytes = fs::read(os_path(path)).map_err(|error| failed(error, "open", path))?;
    if bytes.contains(&0) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("{} appears to be a binary file.", text(path)),
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) struct WriteResult {
    pub created: bool,
    pub mtime_ms: f64,
}

/// `NodeFsPort.writeText` with `allowOverwrite: true`; parent directories are
/// created lazily and the returned mtime is already floored like TS.
pub(super) fn write_text_file(
    path: &JsString,
    content: &str,
) -> Result<WriteResult, crate::runtime::tools::ToolError> {
    let existing = match fs::metadata(os_path(path)) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(failed(error, "stat", path)),
    };
    if let Some(metadata) = &existing
        && !metadata.is_file()
    {
        return Err(tool_error(
            "file_conflict",
            format!("{} exists and is not a regular file.", text(path)),
        ));
    }
    let os_path = os_path(path);
    if let Some(parent) = os_path.parent() {
        fs::create_dir_all(parent).map_err(|error| failed(error, "mkdir", path))?;
    }
    fs::write(&os_path, content.as_bytes()).map_err(|error| failed(error, "open", path))?;
    let written = fs::metadata(&os_path).map_err(|error| failed(error, "stat", path))?;
    Ok(WriteResult {
        created: existing.is_none(),
        mtime_ms: metadata_mtime_ms(&written),
    })
}

fn metadata_mtime_ms(metadata: &fs::Metadata) -> f64 {
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_millis() as f64).floor(),
        Err(error) => -(error.duration().as_millis() as f64).floor(),
    }
}

fn hash_text(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn write_snapshot(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
) -> Option<JsValue> {
    context
        .host
        .files
        .write_snapshots
        .lock()
        .unwrap()
        .get_key(absolute)
        .cloned()
}

pub(super) fn record_write_snapshot(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
    content: &str,
    mtime_ms: f64,
    range: Option<(Option<f64>, Option<f64>)>,
) {
    let mut entry = object([
        ("absolutePath", string(absolute.clone())),
        ("mtimeMs", JsValue::Number(mtime_ms.floor())),
        ("contentHash", string(hash_text(content))),
    ]);
    if let Some((offset, limit)) = range {
        if let Some(offset) = offset {
            entry.insert("offset".into(), JsValue::Number(offset));
        }
        if let Some(limit) = limit {
            entry.insert("limit".into(), JsValue::Number(limit));
        }
    }
    context
        .host
        .files
        .write_snapshots
        .lock()
        .unwrap()
        .insert(absolute.clone(), JsValue::Object(entry));
}

pub(super) fn invalidate_read_file_state(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
) {
    let mut prefix = absolute.units().to_vec();
    prefix.extend_from_slice(&[b':' as u16, b':' as u16]);
    let mut state = context.host.files.read_file_state.lock().unwrap();
    if state.is_empty() {
        return;
    }
    let kept: Vec<(JsString, JsValue)> = state
        .entries()
        .iter()
        .filter(|(key, _)| !key.units().starts_with(&prefix))
        .cloned()
        .collect();
    let mut next = JsObject::new();
    for (key, value) in kept {
        next.insert(key, value);
    }
    *state = next;
}

fn changed_error(
    absolute: &JsString,
    expected_mtime: f64,
    actual_mtime: f64,
) -> crate::runtime::tools::ToolError {
    let mut details = JsObject::new();
    details.insert("absolutePath".into(), string(absolute.clone()));
    details.insert("expectedMtimeMs".into(), JsValue::Number(expected_mtime));
    details.insert("actualMtimeMs".into(), JsValue::Number(actual_mtime));
    let mut error = tool_error("invalid_tool_input", CHANGED);
    error.details = Some(details);
    error
}

/// `validateWriteSnapshotFresh`: returns whether the file exists. The caller
/// only converts the two read-state messages into schema issues.
pub(super) fn validate_write_snapshot_fresh(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
) -> Result<bool, crate::runtime::tools::ToolError> {
    let metadata = match fs::metadata(os_path(absolute)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(failed(error, "stat", absolute)),
    };
    if !metadata.is_file() {
        return Err(tool_error(
            "file_conflict",
            format!("{} is not a regular file.", text(absolute)),
        ));
    }
    let Some(snapshot) = write_snapshot(context, absolute) else {
        return Err(tool_error("invalid_tool_input", NOT_READ));
    };
    let expected_mtime = snapshot
        .get("mtimeMs")
        .and_then(JsValue::as_number)
        .unwrap_or(f64::NAN);
    let actual_mtime = metadata_mtime_ms(&metadata);
    if actual_mtime == expected_mtime {
        return Ok(true);
    }
    let full_read = snapshot.get("offset").is_none() && snapshot.get("limit").is_none();
    if full_read {
        let previous = read_text_file(absolute)?;
        let recorded = snapshot
            .get("contentHash")
            .and_then(JsValue::as_string)
            .map(text)
            .unwrap_or_default();
        if hash_text(&previous) == recorded {
            return Ok(true);
        }
    }
    Err(changed_error(absolute, expected_mtime, actual_mtime))
}

pub(super) struct Freshness {
    pub exists: bool,
    pub previous_content: Option<String>,
}

/// `ensureWriteSnapshotFresh`: like validate, but also returns the content used
/// for the structured patch / diff.
pub(super) fn ensure_write_snapshot_fresh(
    context: &crate::runtime::tools::ToolRunContext,
    absolute: &JsString,
) -> Result<Freshness, crate::runtime::tools::ToolError> {
    let metadata = match fs::metadata(os_path(absolute)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Freshness {
                exists: false,
                previous_content: None,
            });
        }
        Err(error) => return Err(failed(error, "stat", absolute)),
    };
    if !metadata.is_file() {
        return Err(tool_error(
            "file_conflict",
            format!("{} is not a regular file.", text(absolute)),
        ));
    }
    let Some(snapshot) = write_snapshot(context, absolute) else {
        return Err(tool_error("invalid_tool_input", NOT_READ));
    };
    let expected_mtime = snapshot
        .get("mtimeMs")
        .and_then(JsValue::as_number)
        .unwrap_or(f64::NAN);
    let actual_mtime = metadata_mtime_ms(&metadata);
    let previous = read_text_file(absolute)?;
    let full_read = snapshot.get("offset").is_none() && snapshot.get("limit").is_none();
    if actual_mtime != expected_mtime {
        if full_read {
            let recorded = snapshot
                .get("contentHash")
                .and_then(JsValue::as_string)
                .map(text)
                .unwrap_or_default();
            if hash_text(&previous) == recorded {
                return Ok(Freshness {
                    exists: true,
                    previous_content: Some(previous),
                });
            }
        }
        return Err(changed_error(absolute, expected_mtime, actual_mtime));
    }
    if !full_read {
        return Ok(Freshness {
            exists: true,
            previous_content: Some(previous),
        });
    }
    let recorded = snapshot
        .get("contentHash")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default();
    if hash_text(&previous) != recorded {
        return Err(changed_error(absolute, expected_mtime, actual_mtime));
    }
    Ok(Freshness {
        exists: true,
        previous_content: Some(previous),
    })
}

fn write_denied(relative: &JsString) -> bool {
    let first: Vec<u16> = relative
        .units()
        .split(|unit| *unit == b'/' as u16)
        .next()
        .map(<[u16]>::to_vec)
        .unwrap_or_default();
    WRITE_DENY_DIRECTORIES
        .iter()
        .any(|denied| first == denied.encode_utf16().collect::<Vec<_>>())
}

fn denied_error(relative: &JsString) -> crate::runtime::tools::ToolError {
    tool_error(
        "path_not_allowed",
        format!("Writing to {} is not allowed by default.", text(relative)),
    )
}

/// Extra containment guard for mutations. TS `pathSafety` is lexical unless
/// `mustExist`; the task requires writes to stay inside an allowed root, so a
/// symlinked target or symlinked parent that resolves outside is rejected, as
/// is an existing multiply-linked file (a hard link aliases bytes owned by
/// another path). This is deliberately stricter than the TS NodeFsPort.
fn guard_write_containment(
    input: &JsString,
    absolute: &JsString,
    root: &JsString,
    relative: &JsString,
) -> Result<(), crate::runtime::tools::ToolError> {
    let target = os_path(absolute);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let link =
                fs::read_link(&target).map_err(|error| failed(error, "readlink", absolute))?;
            let target = if link.is_absolute() {
                link
            } else {
                target.parent().unwrap_or(Path::new("/")).join(link)
            };
            let target = normalize(&JsString::from(target.to_string_lossy().as_ref()));
            let real = canonical(&target).unwrap_or_else(|| {
                nearest_existing_ancestor(&target).unwrap_or_else(|| target.clone())
            });
            let real_root = canonical(root).unwrap_or_else(|| root.clone());
            if !within_root(&real, &real_root) {
                return Err(tool_error(
                    "path_not_allowed",
                    format!(
                        "Path {} resolves outside the PilotDeck workspace.",
                        text(input)
                    ),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if fs::metadata(os_path(&target))
                    .is_ok_and(|metadata| metadata.is_file() && metadata.nlink() > 1)
                {
                    return Err(denied_error(relative));
                }
            }
        }
        Ok(metadata) => {
            let real = canonical(absolute).unwrap_or_else(|| absolute.clone());
            let real_root = canonical(root).unwrap_or_else(|| root.clone());
            if !within_root(&real, &real_root) {
                return Err(tool_error(
                    "path_not_allowed",
                    format!(
                        "Path {} resolves outside the PilotDeck workspace.",
                        text(input)
                    ),
                ));
            }
            if metadata.is_file() {
                let followed =
                    fs::metadata(&target).map_err(|error| failed(error, "stat", absolute))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if followed.nlink() > 1 {
                        return Err(denied_error(relative));
                    }
                }
                #[cfg(not(unix))]
                let _ = followed;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let probe = nearest_existing_ancestor(absolute).unwrap_or_else(|| normalize(root));
            let real = canonical(&probe).unwrap_or(probe);
            let real_root = canonical(root).unwrap_or_else(|| root.clone());
            if !within_root(&real, &real_root) {
                return Err(tool_error(
                    "path_not_allowed",
                    format!(
                        "Path {} resolves outside the PilotDeck workspace.",
                        text(input)
                    ),
                ));
            }
        }
        Err(error) => return Err(failed(error, "stat", absolute)),
    }
    Ok(())
}

fn nearest_existing_ancestor(path: &JsString) -> Option<JsString> {
    let mut current = os_path(path);
    while current.pop() {
        if fs::symlink_metadata(&current).is_ok() {
            return Some(js_path(&current));
        }
    }
    None
}

/// Write variant of `resolvePilotDeckWorkspacePath`. `must_exist` mirrors the
/// notebook resolver; `allow_outside` is the host permission decision, never a
/// model-controlled input.
pub(super) fn resolve_write_path(
    input: &JsString,
    context: &crate::runtime::tools::ToolRunContext,
    must_exist: bool,
    allow_outside: bool,
) -> Result<ResolvedPath, crate::runtime::tools::ToolError> {
    let input_text = text(input);
    if input.is_empty() || input_text.contains('\0') {
        return Err(invalid(
            "Path must be a non-empty string without null bytes.",
        ));
    }
    let cwd = cwd(context);
    let absolute = if is_absolute(input) {
        normalize(input)
    } else {
        join(&cwd, input)
    };
    let relative_cwd = relative(&cwd, &absolute);
    let relative_cwd = if relative_cwd.is_empty() {
        JsString::from(".")
    } else {
        relative_cwd
    };
    if permission_mode(context).equals_str("bypassPermissions") {
        if write_denied(&relative_cwd) {
            return Err(denied_error(&relative_cwd));
        }
        return Ok(ResolvedPath {
            absolute,
            relative: relative_cwd,
        });
    }

    let mut roots = vec![normalize(&cwd)];
    roots.extend(
        additional_working_directories(context)
            .iter()
            .map(normalize),
    );
    let root = roots
        .iter()
        .find(|root| within_root(&absolute, root))
        .cloned();
    let Some(root) = root else {
        if allow_outside {
            if write_denied(&relative_cwd) {
                return Err(denied_error(&relative_cwd));
            }
            return Ok(ResolvedPath {
                absolute,
                relative: relative_cwd,
            });
        }
        return Err(tool_error(
            "path_not_allowed",
            format!("Path {input_text} is outside the PilotDeck workspace."),
        ));
    };
    let relative_path = relative(&root, &absolute);
    let relative_path = if relative_path.is_empty() {
        JsString::from(".")
    } else {
        relative_path
    };
    if write_denied(&relative_path) {
        return Err(denied_error(&relative_path));
    }
    if must_exist {
        let real = canonical(&absolute).ok_or_else(|| {
            tool_error(
                "file_not_found",
                format!("File {input_text} does not exist."),
            )
        })?;
        let real_root = canonical(&root).unwrap_or_else(|| root.clone());
        if !within_root(&real, &real_root) {
            return Err(tool_error(
                "path_not_allowed",
                format!("Path {input_text} resolves outside the PilotDeck workspace."),
            ));
        }
    }
    if !allow_outside {
        guard_write_containment(input, &absolute, &root, &relative_path)?;
    }
    Ok(ResolvedPath {
        absolute,
        relative: relative_path,
    })
}

/// `context.messageId ?? context.turnId`, already a host-set string.
pub(super) fn message_id(context: &crate::runtime::tools::ToolRunContext) -> String {
    context
        .fields
        .get("messageId")
        .and_then(JsValue::as_string)
        .or_else(|| context.fields.get("turnId").and_then(JsValue::as_string))
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}

pub(super) fn file_history(
    context: &crate::runtime::tools::ToolRunContext,
) -> Option<&crate::storage::file_history::FileHistoryStore> {
    context.host.file_history.as_ref()?.downcast_ref()
}
