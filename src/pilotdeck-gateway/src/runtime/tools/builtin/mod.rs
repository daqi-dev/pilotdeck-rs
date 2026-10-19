#![allow(clippy::result_large_err)]
//! Built-in read-only file tools. Every implementation follows the fixed TS
//! baseline under `resources/PilotDeck/src/tool/builtin`; filesystem paths are
//! resolved lexically in UTF-16 first so path identity and state keys keep the
//! original wire spelling, and only the OS boundary uses an `OsStr`.
pub mod glob;
pub mod read_file;
pub mod time;

use crate::{
    host::HostError,
    runtime::tools::{ExecutableTool, ToolRegistration, ToolRegistry},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
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
pub(super) fn is_absolute(path: &JsString) -> bool {
    path.units()
        .first()
        .is_some_and(|unit| *unit == b'/' as u16)
}

pub(super) fn normalize(path: &JsString) -> JsString {
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

pub(super) fn join(base: &JsString, child: &JsString) -> JsString {
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

pub(super) fn dirname(path: &JsString) -> JsString {
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

pub(super) fn basename(path: &JsString) -> JsString {
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
pub(super) fn relative(from: &JsString, to: &JsString) -> JsString {
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
