//! Default CLI native archives: no authorization hook, default disk layout,
//! no retention window. Custom authorization/retention/artifact stores are A6
//! extension points, unimplemented here (not exemptions).
use crate::{
    metadata::{Files, Metadata, js_trim, object, resolve, text_field},
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{cmp::Ordering, io, path::Path};

type Error = (&'static str, JsString);
type Result<T> = std::result::Result<T, Error>;
static UNDEFINED: JsValue = JsValue::Undefined;
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
const TRANSCRIPT_BYTES: u64 = 50 * 1024 * 1024;
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn error(code: &'static str, message: &str) -> Error {
    (code, message.into())
}
fn safe_integer(value: &JsValue, min: f64, max: f64) -> bool {
    value
        .as_number()
        .is_some_and(|n| n.fract() == 0.0 && (min..=max).contains(&n))
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
/// `String(value)`. A too-deep array is V8's RangeError, surfaced as a plain
/// gateway error (real TS + WS evidence: tests/fixtures/runtime-boundaries.json).
fn display(value: &JsValue) -> Result<String> {
    crate::js_value::display(value).map_err(|_| {
        error(
            "gateway_request_failed",
            crate::js_value::StackOverflow::MESSAGE,
        )
    })
}
fn path_type(value: &JsValue, argument: &str) -> Error {
    let received = match value {
        JsValue::Number(n) => format!("type number ({})", crate::js_value::number_string(*n)),
        JsValue::Bool(b) => format!("type boolean ({b})"),
        JsValue::Array(_) => "an instance of Array".into(),
        JsValue::Null => "null".into(),
        JsValue::Undefined => "undefined".into(),
        _ => "an instance of Object".into(),
    };
    error(
        "ERR_INVALID_ARG_TYPE",
        &format!("The \"{argument}\" argument must be of type string. Received {received}"),
    )
}
fn session(params: &JsValue) -> Result<&JsString> {
    if nullish(params) {
        return Err(error(
            "gateway_request_failed",
            &format!(
                "Cannot read properties of {} (reading 'sessionKey')",
                display(params)?
            ),
        ));
    }
    match field(params, "sessionKey") {
        JsValue::Undefined | JsValue::Null => {
            Err(error("INVALID_SESSION_KEY", "sessionKey is required."))
        }
        JsValue::String(s) if js_trim(&String::from_utf16_lossy(s.units())).is_empty() => {
            Err(error("INVALID_SESSION_KEY", "sessionKey is required."))
        }
        JsValue::String(s) => Ok(s),
        _ => Err(error(
            "gateway_request_failed",
            "input.sessionKey?.trim is not a function",
        )),
    }
}
fn session_id(session: &JsString) -> String {
    let s = String::from_utf16_lossy(session.units());
    let mut output = String::new();
    let mut separator = false;
    for c in s.chars() {
        if matches!(c, '/' | '\\') {
            if !separator {
                output.push('-');
            }
            separator = true;
        } else {
            output.push(c);
            separator = false;
        }
    }
    let output = output.trim_matches('-');
    if output.is_empty() {
        "session".into()
    } else {
        output.into()
    }
}
fn validate_entries(params: &JsValue, public: bool) -> Result<usize> {
    let after = field(params, "afterSequence");
    if !after.is_undefined() && !safe_integer(after, if public { 0.0 } else { -1.0 }, MAX_SAFE) {
        return Err(error(
            "INVALID_ARCHIVE_CURSOR",
            if public {
                "afterSequence must be a non-negative safe integer."
            } else {
                "afterSequence must be an integer greater than or equal to -1."
            },
        ));
    }
    let limit = field(params, "limit");
    // Public input rejects null; callback's ??100 accepts it.
    if !(limit.is_undefined() || (!public && nullish(limit))) && !safe_integer(limit, 1.0, 500.0) {
        return Err(error(
            "INVALID_ARCHIVE_LIMIT",
            "limit must be an integer between 1 and 500.",
        ));
    }
    Ok(limit.as_number().unwrap_or(100.0) as usize)
}
fn artifact_name(params: &JsValue, public: bool) -> Result<String> {
    let value = field(params, "artifactName");
    let name = display(value)?;
    let truthy = match value {
        JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => false,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    };
    if (!public && !truthy)
        || name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    {
        return Err(error(
            "INVALID_ARCHIVE_ARTIFACT",
            "artifactName must be a single safe artifact name.",
        ));
    }
    Ok(name)
}
fn artifact_limit(params: &JsValue) -> Result<usize> {
    let limit = field(params, "maxBytes");
    if nullish(limit) {
        return Ok(1_000_000);
    }
    if !safe_integer(limit, 1.0, 10_000_000.0) {
        return Err(error(
            "INVALID_ARCHIVE_ARTIFACT_LIMIT",
            "maxBytes must be an integer between 1 and 10000000.",
        ));
    }
    Ok(limit.as_number().unwrap() as usize)
}
// Registration reads marker contents before the transcript is located. Keep
// existing registry algorithms, but never open marker bytes outside PILOT_HOME.
struct RegistryFiles<'a, F> {
    inner: &'a F,
    root: &'a Path,
}
impl<F: Files> Files for RegistryFiles<'_, F> {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.inner.entries(path)
    }
    fn stat(&self, path: &Path) -> io::Result<crate::metadata::Stat> {
        self.inner.stat(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let root = self.inner.canonical(self.root)?;
        let target = self.inner.canonical(path)?;
        if !target.starts_with(&root) {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        self.inner.archive_read(&root, &target, usize::MAX)
    }
}
fn registry<F: Files>(metadata: &Metadata<F>) -> Metadata<RegistryFiles<'_, F>> {
    Metadata {
        home: metadata.home.clone(),
        cwd: metadata.cwd.clone(),
        files: RegistryFiles {
            inner: &metadata.files,
            root: &metadata.home,
        },
    }
}
fn project<F: Files>(metadata: &Metadata<F>, params: &JsValue) -> Result<std::path::PathBuf> {
    let value = field(params, "projectKey");
    let requested = if nullish(value) {
        metadata.cwd.clone()
    } else {
        let Some(s) = value.as_string() else {
            return Err(path_type(value, "paths[0]"));
        };
        // Node path.resolve retains lone surrogates for project-key identity;
        // disk markers contain decoded UTF-8 and cannot match such a key.
        if s.to_utf8().is_err() {
            let mut units: Vec<u16> = "Unknown projectKey: ".encode_utf16().collect();
            units.extend(s.units());
            return Err(("PROJECT_NOT_FOUND", JsString::from_utf16(units)));
        }
        resolve(&metadata.cwd, &String::from_utf16_lossy(s.units()))
    };
    if requested == metadata.home {
        return Ok(requested);
    }
    if registry(metadata).has_registered_project(&requested) {
        return Ok(requested);
    }
    Err(error(
        "PROJECT_NOT_FOUND",
        &format!(
            "Unknown projectKey: {}",
            if nullish(value) {
                metadata.cwd.to_string_lossy().into_owned()
            } else {
                display(value)?
            }
        ),
    ))
}
fn fs_error(e: io::Error, method: &str, operation: &str, path: &Path) -> Error {
    if crate::project_files::nofollow_loop(&e) {
        return error(
            "PATH_NOT_ALLOWED",
            "Archive path resolves outside its storage root.",
        );
    }
    // Only this method's own Node RPC oracle admits diagnostics. Unknown ordinary
    // errno remains host-owned; NOFOLLOW refusals above can never reach TS.
    let Some((_, _)) = crate::node_io::lookup_for(method, &e) else {
        return error("TS_IO_REQUIRED", "");
    };
    let diagnostic = if operation == "open" {
        crate::node_io::read_file(&e, &path.to_string_lossy())
    } else {
        crate::node_io::diagnostic(&e, operation, Some(&path.to_string_lossy()))
    };
    diagnostic.map_or_else(
        || error("TS_IO_REQUIRED", ""),
        |(code, message)| error(code, &message),
    )
}
// A directory opened with NOFOLLOW|DIRECTORY can report ENOTDIR for a
// replaced symlink on macOS. Canonicalization errors are ordinary diagnostics;
// after containment, this checked-open failure must never fall back to TS.
// Tagged non-regular/oversized archive reads also fail locally; other unknown
// errors, including ordinary InvalidInput, retain the TS_IO_REQUIRED boundary.
fn secure_fs_error(error_value: io::Error, method: &str, operation: &str, path: &Path) -> Error {
    if error_value.raw_os_error() == Some(libc::ENOTDIR)
        || crate::metadata::is_archive_read_denied(&error_value)
    {
        error(
            "PATH_NOT_ALLOWED",
            "Archive path resolves outside its storage root.",
        )
    } else {
        fs_error(error_value, method, operation, path)
    }
}
fn canonical<F: Files>(
    metadata: &Metadata<F>,
    method: &str,
    root: &Path,
    path: &Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let canonical_root = metadata
        .files
        .canonical(root)
        .map_err(|e| fs_error(e, method, "stat", path))?;
    let canonical_path = metadata
        .files
        .canonical(path)
        .map_err(|e| fs_error(e, method, "stat", path))?;
    if canonical_path.strip_prefix(&canonical_root).is_err() {
        return Err(error(
            "PATH_NOT_ALLOWED",
            "Archive path resolves outside its storage root.",
        ));
    }
    Ok((canonical_root, canonical_path))
}
fn transcript<F: Files>(
    metadata: &Metadata<F>,
    project: &Path,
    session: &JsString,
    method: &str,
) -> Result<Option<Vec<JsValue>>> {
    let path = registry(metadata)
        .chat_dir(&project.to_string_lossy())
        .join(format!("{}.jsonl", session_id(session)));
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err((
            "ERR_INVALID_ARG_VALUE",
            crate::node_io::nul_message(&JsString::from(path.to_string_lossy().as_ref())),
        ));
    }
    let read = || -> Result<Option<Vec<JsValue>>> {
        let (root, canonical) = canonical(metadata, method, &metadata.home, &path)?;
        let (stat, _) = metadata
            .files
            .project_stat(&root, &canonical)
            .map_err(|e| secure_fs_error(e, method, "stat", &path))?;
        if stat.size > TRANSCRIPT_BYTES {
            return Err(error(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot read a native archive with invalid entries.",
            ));
        }
        if stat.directory {
            return Err(error(
                "EISDIR",
                "EISDIR: illegal operation on a directory, read",
            ));
        }
        let bytes = metadata
            .files
            .archive_read(&root, &canonical, (TRANSCRIPT_BYTES + 1) as usize)
            .map_err(|e| secure_fs_error(e, method, "open", &path))?;
        if bytes.len() as u64 > TRANSCRIPT_BYTES {
            return Err(error(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot read a native archive with invalid entries.",
            ));
        }
        parse_transcript(&bytes)
    };
    match read() {
        Err(("ENOENT", _)) => {
            let mut units: Vec<u16> = "Session not found: ".encode_utf16().collect();
            units.extend(session.units());
            Err(("SESSION_NOT_FOUND", JsString::from_utf16(units)))
        }
        other => other,
    }
}
// Separate reader until coordination with session_model_get's worktree. Payloads
// are intentionally not revalidated: the baseline only checks these five fields.
fn parse_transcript(bytes: &[u8]) -> Result<Option<Vec<JsValue>>> {
    let mut entries = vec![];
    let mut invalid = false;
    for line in String::from_utf8_lossy(bytes).split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        match parse_json(line) {
            Ok(value)
                if matches!(value, JsValue::Object(_))
                    && ["type", "sessionId", "turnId", "createdAt"]
                        .iter()
                        .all(|k| field(&value, k).as_string().is_some())
                    && field(&value, "sequence").as_number().is_some() =>
            {
                entries.push(value)
            }
            _ => invalid = true,
        }
    }
    // The TS reader sorts before its caller checks diagnostics, but malformed
    // entries have a fixed outward error independent of the sort result.
    if invalid {
        return Err(error(
            "SESSION_TRANSCRIPT_INVALID",
            "Cannot read a native archive with invalid entries.",
        ));
    }
    let mut counts = std::collections::HashMap::new();
    for entry in &entries {
        let sequence = field(entry, "sequence").as_number().unwrap();
        *counts
            .entry(if sequence == 0.0 {
                0
            } else {
                sequence.to_bits()
            })
            .or_insert(0usize) += 1;
    }
    if entries.iter().any(|entry| {
        let sequence = field(entry, "sequence").as_number().unwrap();
        counts[&if sequence == 0.0 {
            0
        } else {
            sequence.to_bits()
        }] > 1
            && has_unpaired_surrogate(field(entry, "createdAt").as_string().unwrap())
    }) {
        return Ok(None);
    }
    if entries.len() > 1 {
        let locale = node_locale(|key| std::env::var(key).ok());
        let collator = node_collator(&locale).map_err(|(code, message)| error(code, &message))?;
        entries.sort_by(|a, b| {
            let left = field(a, "sequence").as_number().unwrap();
            let right = field(b, "sequence").as_number().unwrap();
            if left != right {
                (left - right).partial_cmp(&0.0).unwrap_or(Ordering::Equal)
            } else {
                collator.compare_utf16(
                    field(a, "createdAt").as_string().unwrap().units(),
                    field(b, "createdAt").as_string().unwrap().units(),
                )
            }
        });
    }
    Ok(Some(entries))
}
fn callback<F: Files>(
    metadata: &Metadata<F>,
    method: &str,
    params: &JsValue,
    session: &JsString,
) -> Result<Option<JsValue>> {
    // These duplicate checks are intentional: the inner API differs from IPG.
    let limit = if method == "native_archive_entries" {
        validate_entries(params, false)?
    } else {
        0
    };
    if method == "native_archive_artifact" {
        artifact_name(params, false)?;
        artifact_limit(params)?;
    }
    let project = project(metadata, params)?;
    if method == "native_archive_artifact" {
        let name = field(params, "artifactName");
        let Some(s) = name.as_string() else {
            return Err(path_type(name, "path"));
        };
        let name = String::from_utf16_lossy(s.units());
        let limit = artifact_limit(params)?;
        let dir = resolve(
            &project,
            &format!(".pilotdeck/tool-results/{}", session_id(session)),
        );
        let path = dir.join(&name);
        // Local readFile failures (including candidate secure-open refusals) all
        // collapse to NOT_FOUND. Read the whole file before slicing its content.
        let bytes = canonical(metadata, method, &project, &path)
            .ok()
            .and_then(|(root, path)| metadata.files.archive_read(&root, &path, usize::MAX).ok())
            .ok_or_else(|| {
                error(
                    "ARCHIVE_ARTIFACT_NOT_FOUND",
                    &format!("Archive artifact {name} was not found."),
                )
            })?;
        return Ok(Some(object([
            ("artifactName", field(params, "artifactName").clone()),
            (
                "content",
                JsValue::String(STANDARD.encode(&bytes[..bytes.len().min(limit)]).into()),
            ),
            ("encoding", "base64".into()),
            ("bytes", JsValue::Number(bytes.len() as f64)),
            ("truncated", JsValue::Bool(bytes.len() > limit)),
        ])));
    }
    let Some(entries) = transcript(metadata, &project, session, method)? else {
        return Ok(None);
    };
    if method == "native_archive_manifest" {
        let mut fields = vec![
            ("schemaVersion", JsValue::Number(1.0)),
            ("format", "native_transcript_entries".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("entryCount", JsValue::Number(entries.len() as f64)),
        ];
        if let (Some(first), Some(last)) = (entries.first(), entries.last()) {
            fields.push(("firstSequence", field(first, "sequence").clone()));
            fields.push(("lastSequence", field(last, "sequence").clone()));
        }
        fields.push((
            "subagentCount",
            JsValue::Number(
                entries
                    .iter()
                    .filter(|e| text_field(e, "type").as_deref() == Some("subagent_started"))
                    .count() as f64,
            ),
        ));
        fields.push((
            "toolResultReferenceCount",
            JsValue::Number(
                entries
                    .iter()
                    .filter(|e| {
                        matches!(
                            text_field(e, "type").as_deref(),
                            Some("tool_result_message" | "tool_result")
                        )
                    })
                    .count() as f64,
            ),
        ));
        return Ok(Some(object(fields)));
    }
    let after = field(params, "afterSequence").as_number().unwrap_or(-1.0);
    let page: Vec<_> = entries
        .iter()
        .filter(|e| field(e, "sequence").as_number().unwrap() > after)
        .take(limit)
        .cloned()
        .collect();
    let last = page
        .last()
        .map(|e| field(e, "sequence").as_number().unwrap());
    let more = last.is_some_and(|last| {
        entries
            .iter()
            .any(|e| field(e, "sequence").as_number().unwrap() > last)
    });
    let mut fields = vec![("entries", JsValue::Array(page))];
    if more {
        fields.push(("nextSequence", JsValue::Number(last.unwrap())))
    }
    fields.push(("complete", JsValue::Bool(!more)));
    Ok(Some(object(fields)))
}
pub(crate) fn response<F: Files>(request: &JsValue, metadata: &Metadata<F>) -> Option<Vec<u8>> {
    let method = text_field(request, "method")?;
    let params = field(request, "params");
    let result = (|| {
        let session = session(params)?;
        if method == "native_archive_entries" {
            validate_entries(params, true)?;
        }
        if method == "native_archive_artifact" {
            artifact_name(params, true)?;
            artifact_limit(params)?;
        }
        callback(metadata, &method, params, session)
    })();
    let id = field(request, "id").clone();
    let response = match result {
        Ok(Some(result)) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(true)),
            ("result", result),
        ]),
        Ok(None) => return None,
        Err(("TS_IO_REQUIRED", _)) => return None,
        Err((code, message)) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                object([("code", code.into()), ("message", JsValue::String(message))]),
            ),
        ]),
    };
    stringify_json(&response)
        .ok()
        .flatten()
        .map(String::into_bytes)
}
#[cfg(test)]
pub(crate) mod tests;

#[cfg(all(test, unix))]
pub(crate) mod security_tests;

#[cfg(test)]
pub(crate) mod io_tests;
