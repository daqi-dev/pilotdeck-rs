//! Disk-only file methods, measured against the pinned Node oracle.
use crate::metadata::{Files, Metadata, Stat, js_trim, object, resolve, text_field};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use icu_casemap::CaseMapper;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::{LanguageIdentifier, Locale};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

type Error = (&'static str, String);
type Result<T> = std::result::Result<T, Error>;
const OUTSIDE: &str = "File path is outside the project workspace.";
const ESCAPE: &str = "File path resolves outside the project workspace.";
const EXCLUDED: &[&str] = &[
    ".git",
    ".pilotdeck",
    ".tmp",
    "node_modules",
    "dist",
    "build",
    "coverage",
];
fn denied(message: &str) -> Error {
    ("PATH_NOT_ALLOWED", message.into())
}
fn index_limit() -> Error {
    (
        "FILE_INDEX_LIMIT",
        "Project file scan exceeded its resource limit.".into(),
    )
}
fn encode(value: &JsValue) -> String {
    stringify_json(value).expect("JSON value").expect("defined")
}

/// Each component is opened relative to a pinned directory, never following a
/// symlink. The preceding realpath permits ordinary in-root links; a replacement
/// after that check fails closed. NONBLOCK prevents a raced-in FIFO hanging us.
pub(crate) fn open_beneath(root: &Path, canonical: &Path) -> io::Result<File> {
    open_checked(root, canonical, false)
}
fn open_checked(root: &Path, canonical: &Path, search_directory: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let relative = canonical
        .strip_prefix(root)
        .map_err(|_| io::ErrorKind::PermissionDenied)?;
    // Walk the canonical root from / as well: a raced-in link in any
    // ancestor must not redirect the root descriptor outside the project.
    let mut file = File::open("/")?;
    let components: Vec<_> = root
        .components()
        .chain(relative.components())
        .filter(|c| !matches!(c, Component::RootDir))
        .collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        let flags = if index + 1 < components.len() || search_directory {
            // Traversing a directory needs search permission, not read permission.
            #[cfg(target_os = "linux")]
            let access = OFlags::PATH;
            #[cfg(target_vendor = "apple")]
            let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
            #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
            let access = OFlags::RDONLY;
            access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
        } else {
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK
        };
        file = File::from(openat(&file, *name, flags, Mode::empty())?);
    }
    Ok(file)
}
pub(crate) fn native_stat(root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
    use rustix::fs::{AtFlags, FileType, fstat, statat};
    let stat = if root == path {
        fstat(open_checked(root, root, true)?)?
    } else {
        let parent = open_checked(
            root,
            path.parent().ok_or(io::ErrorKind::InvalidInput)?,
            true,
        )?;
        statat(
            &parent,
            path.file_name().ok_or(io::ErrorKind::InvalidInput)?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?
    };
    let kind = FileType::from_raw_mode(stat.st_mode);
    Ok((
        Stat {
            directory: kind == FileType::Directory,
            size: stat.st_size as u64,
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: stat.st_mtime as f64 * 1000.0 + stat.st_mtime_nsec as f64 / 1_000_000.0,
        },
        kind == FileType::RegularFile,
    ))
}

// Node/ICU's POSIX default locale precedence, measured in node-runtime.json.
pub(crate) fn node_locale(env: impl FnMut(&str) -> Option<String>) -> Locale {
    let value = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(env)
        .unwrap_or_else(|| "en-US".into());
    let language = value
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    if language.is_empty() {
        return Locale::UNKNOWN;
    }
    if language == "C" || language == "POSIX" {
        return "en-US".parse().unwrap();
    }
    language
        .parse()
        .unwrap_or_else(|_| "en-US".parse().unwrap())
}
fn lowercase(value: &str, locale: &LanguageIdentifier, substring: bool) -> String {
    // Node v22.22.0 / V8 12.4: Latin-1 fast casing ignores the locale only
    // for non-sliced strings. Substrings shorter than 13 UTF-16 units are
    // copied; longer ones remain slices. This is representation-dependent,
    // not a Unicode guarantee: remeasure on engine upgrades (round4/5 fixtures).
    let sliced = substring && value.encode_utf16().count() >= 13;
    let root = LanguageIdentifier::UNKNOWN;
    let locale = if !sliced && value.encode_utf16().all(|unit| unit <= 0xff) {
        &root
    } else {
        locale
    };
    CaseMapper::new()
        .lowercase_to_string(value, locale)
        .into_owned()
}

// Unsupported collation extensions fall back to the language's default in
// Node. Keep a final root fallback for unavailable locale data; never panic in
// dispatch, even if the compiled provider cannot construct either collator.
pub(crate) fn node_collator(locale: &Locale) -> Result<CollatorBorrowed<'static>> {
    let mut preferences: CollatorPreferences = locale.clone().into();
    Collator::try_new(preferences, Default::default())
        .or_else(|_| {
            preferences.collation_type = None;
            Collator::try_new(preferences, Default::default())
        })
        .or_else(|_| Collator::try_new(Default::default(), Default::default()))
        .map_err(|_| {
            (
                "gateway_request_failed",
                "Collation data unavailable.".into(),
            )
        })
}

pub(crate) fn native_entries(root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
    let file = open_beneath(root, path)?;
    let dir = rustix::fs::Dir::read_from(&file)?;
    let mut entries = Vec::new();
    for entry in dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        entries.push((name, entry.file_type() == rustix::fs::FileType::Directory));
        // Enough entries to force the method's existing FILE_INDEX_LIMIT.
        if entries.len() > 100_000 {
            break;
        }
    }
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(entries)
}
pub(crate) fn read_prefix(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    while output.len() < limit {
        let count = (limit - output.len()).min(chunk.len());
        let read = match reader.read(&mut chunk[..count]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if read == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..read]);
    }
    Ok(output)
}

// Invalid JS argument shapes with util.inspect-specific errors remain TS-owned.
// Never forward a string path on security grounds: that check is local below.
pub(crate) fn execute<F: Files>(
    metadata: &Metadata<F>,
    method: &str,
    params: &JsValue,
) -> Option<Result<JsValue>> {
    let project = text_field(params, "projectKey")?;
    if method == "project_file_read" {
        // Node encodes lone UTF-16 surrogates as U+FFFD for filesystem access.
        // Keep all string paths on the local containment/NOFOLLOW path, while
        // preserving the original JS string in the response.
        let path = params.get("path")?.as_string()?;
        let path = String::from_utf16_lossy(path.units());
        return local_result(read(metadata, params, &project, &path));
    }
    let query = match params.get("query") {
        None | Some(JsValue::Null | JsValue::Undefined) => String::new(),
        Some(value) => crate::metadata::string(value)?,
    };
    // A non-string truthy cursor has Node-specific Buffer argument diagnostics.
    if let Some(cursor) = params.get("cursor")
        && truthy(cursor)
        && cursor.as_string().is_none()
    {
        return None;
    }
    local_result(list(metadata, params, &project, &query))
}
// Node owns unmeasured platform-specific IO diagnostics. Keep those errors
// upstream rather than manufacturing a different errno/message. Security errors
// are already resolved locally and never take this fallback.
fn local_result(result: Result<JsValue>) -> Option<Result<JsValue>> {
    if matches!(&result, Err(("gateway_request_failed", _))) {
        None
    } else {
        Some(result)
    }
}
fn registered<F: Files>(metadata: &Metadata<F>, project: &str, general: bool) -> Result<PathBuf> {
    let requested = resolve(&metadata.cwd, project);
    if general && requested == metadata.home {
        return Ok(requested);
    }
    if let Some(JsValue::Array(projects)) = metadata.list_projects().get("projects") {
        for item in projects {
            if let Some(key) = text_field(item, "projectKey")
                && resolve(&metadata.cwd, &key) == requested
            {
                return Ok(resolve(&metadata.cwd, &key));
            }
        }
    }
    Err((
        "PROJECT_NOT_FOUND",
        format!("Unknown projectKey: {project}"),
    ))
}
fn read<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    project: &str,
    path: &str,
) -> Result<JsValue> {
    let root = registered(metadata, project, true)?;
    let root = metadata
        .files
        .canonical(&root)
        .map_err(|e| fs_error(e, "realpath", &root))?;
    let absolute = resolve(&root, path);
    // TS pathSafety also rejects '..name' prefixes, unlike listProjectFiles.
    if !within_read(&root, &absolute) {
        return Err(denied(OUTSIDE));
    }
    let canonical = metadata
        .files
        .canonical(&absolute)
        .map_err(|_| denied(ESCAPE))?;
    if !within_read(&root, &canonical) {
        return Err(denied(ESCAPE));
    }
    let Ok((stat, true)) = metadata.files.project_stat(&root, &canonical) else {
        return Ok(JsValue::Null);
    };
    // Node readFile opens before checking size, then slices to maxBytes.
    // A zero-byte prefix performs our safe open without reading the huge file,
    // so EACCES (and NOFOLLOW failures) take precedence over the size error.
    if stat.size > 2_147_483_647 {
        metadata
            .files
            .project_read(&root, &canonical, 0)
            .map_err(|e| fs_error(e, "open", &canonical))?;
        return Err((
            "ERR_FS_FILE_TOO_LARGE",
            format!("File size ({}) is greater than 2 GiB", stat.size),
        ));
    }
    let number = match params.get("maxBytes") {
        None | Some(JsValue::Null | JsValue::Undefined) => 1_000_000.0,
        Some(value) => js_number(value),
    };
    let limit = if number.is_nan() {
        0
    } else {
        number.clamp(1.0, 10_000_000.0) as usize
    };
    let bytes = metadata
        .files
        .project_read(&root, &canonical, limit)
        .map_err(|e| fs_error(e, "open", &canonical))?;
    let (content, encoding) = if text_field(params, "encoding").as_deref() == Some("base64") {
        (STANDARD.encode(bytes), "base64")
    } else {
        (String::from_utf8_lossy(&bytes).into_owned(), "utf-8")
    };
    Ok(object([
        (
            "path",
            params.get("path").cloned().unwrap_or_else(|| path.into()),
        ),
        ("content", content.as_str().into()),
        ("encoding", encoding.into()),
    ]))
}
fn within_read(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|p| !p.to_string_lossy().starts_with(".."))
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn js_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Number(n) => *n,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        JsValue::String(_) | JsValue::Array(_) => number_from_text(&js_coercion_text(value)),
        _ => f64::NAN,
    }
}
fn js_coercion_text(value: &JsValue) -> String {
    match value {
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        JsValue::Array(items) => items
            .iter()
            .map(|item| {
                if matches!(item, JsValue::Null | JsValue::Undefined) {
                    String::new()
                } else {
                    js_coercion_text(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(_) => "[object Object]".into(),
        JsValue::Undefined => "undefined".into(),
        _ => encode(value),
    }
}
fn number_from_text(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = text.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            return digits
                .chars()
                .try_fold(0.0, |value, c| {
                    c.to_digit(radix)
                        .map(|digit| value * radix as f64 + digit as f64)
                })
                .unwrap_or(f64::NAN);
        }
    }
    // Rust also accepts inf; JavaScript Number does not.
    if text
        .chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return f64::NAN;
    }
    text.parse().unwrap_or(f64::NAN)
}
fn fs_error(error: io::Error, operation: &str, path: &Path) -> Error {
    // NOFOLLOW detects a link installed after canonicalization. Never send that
    // request to the TS fallback, whose check/open sequence is not race-safe.
    if error.raw_os_error() == Some(libc::ELOOP) {
        return denied(ESCAPE);
    }
    // Node v22 raw fs errors AND their WS responses: node-runtime-round3.json.
    let (code, description) = match error.kind() {
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => ("gateway_request_failed", "filesystem operation failed"),
    };
    (
        code,
        format!("{code}: {description}, {operation} '{}'", path.display()),
    )
}
fn list<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    project: &str,
    query: &str,
) -> Result<JsValue> {
    let registered = registered(metadata, project, false)?;
    let root = metadata.files.canonical(&registered).map_err(|_| {
        (
            "PROJECT_NOT_FOUND",
            format!("Project does not exist: {project}"),
        )
    })?;
    if !metadata
        .files
        .project_stat(&root, &root)
        .map_err(|e| fs_error(e, "stat", &root))?
        .0
        .directory
    {
        return Err((
            "PROJECT_NOT_FOUND",
            format!("Project is not a directory: {project}"),
        ));
    }
    let locale = node_locale(|key| std::env::var(key).ok());
    let trimmed = js_trim(query);
    let query = lowercase(trimmed, &locale.id, trimmed != query);
    if query.encode_utf16().count() > 256 {
        return Err((
            "INVALID_QUERY",
            "query must not exceed 256 characters.".into(),
        ));
    }
    let include_dirs = !matches!(params.get("includeDirs"), Some(JsValue::Bool(false)));
    let limit = match params.get("limit") {
        None | Some(JsValue::Undefined) => 100,
        Some(JsValue::Number(n)) if n.fract() == 0.0 && (1.0..=500.0).contains(n) => *n as usize,
        _ => {
            return Err((
                "INVALID_LIMIT",
                "limit must be an integer between 1 and 500.".into(),
            ));
        }
    };
    let signature = URL_SAFE_NO_PAD.encode(Sha256::digest(encode(&object([
        ("root", root.to_string_lossy().as_ref().into()),
        ("query", query.as_str().into()),
        ("includeDirs", JsValue::Bool(include_dirs)),
    ]))));
    let invalid_cursor = || {
        (
            "INVALID_CURSOR",
            "cursor is invalid or does not match the query.".into(),
        )
    };
    let offset = match params
        .get("cursor")
        .and_then(JsValue::as_string)
        .filter(|v| !v.is_empty())
    {
        None => 0.0,
        Some(cursor) => {
            let bytes = decode_base64(cursor.units()).ok_or_else(invalid_cursor)?;
            let parsed =
                parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid_cursor())?;
            let offset = parsed
                .get("offset")
                .and_then(JsValue::as_number)
                .ok_or_else(invalid_cursor)?;
            if parsed.get("version").and_then(JsValue::as_number) != Some(1.0)
                || text_field(&parsed, "signature").as_deref() != Some(signature.as_str())
                || offset < 0.0
                || offset.fract() != 0.0
            {
                return Err(invalid_cursor());
            }
            offset
        }
    };
    let mut scan = Scan {
        metadata,
        root: &root,
        collator: node_collator(&locale)?,
        query: &query,
        locale: locale.id,
        include_dirs,
        started: Instant::now(),
        count: 0,
        visited: HashSet::from([root.clone()]),
        entries: Vec::new(),
    };
    scan.walk(&root, 0)?;
    scan.entries.sort_by(|a, b| {
        scan.collator.compare(
            &text_field(a, "relativePath").unwrap(),
            &text_field(b, "relativePath").unwrap(),
        )
    });
    let start = (offset as usize).min(scan.entries.len());
    let end = (start + limit).min(scan.entries.len());
    let next = if end < scan.entries.len() {
        URL_SAFE_NO_PAD.encode(encode(&object([
            ("version", JsValue::Number(1.0)),
            ("offset", JsValue::Number(end as f64)),
            ("signature", signature.as_str().into()),
        ])))
    } else {
        String::new()
    };
    let mut fields = vec![
        (
            "items",
            JsValue::Array(scan.entries.drain(start..end).collect()),
        ),
        ("projectKey", root.to_string_lossy().as_ref().into()),
    ];
    if !next.is_empty() {
        fields.push(("nextCursor", next.as_str().into()));
    }
    Ok(object(fields))
}
fn decode_base64(value: &[u16]) -> Option<Vec<u8>> {
    // Buffer.from(base64url) accepts standard alphabet, whitespace, missing
    // padding, and ignores other characters. A trailing sextet is discarded.
    // Buffer uses each UTF-16 code unit's low byte, including lone surrogates.
    let mut clean: String = value
        .iter()
        .map(|unit| char::from(*unit as u8))
        .take_while(|c| *c != '=')
        .filter_map(|c| match c {
            '+' => Some('-'),
            '/' => Some('_'),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect();
    if clean.len() % 4 == 1 {
        clean.pop();
    }
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
    .decode(clean)
    .ok()
}
struct Scan<'a, F> {
    metadata: &'a Metadata<F>,
    root: &'a Path,
    collator: CollatorBorrowed<'static>,
    query: &'a str,
    locale: LanguageIdentifier,
    include_dirs: bool,
    started: Instant,
    count: usize,
    visited: HashSet<PathBuf>,
    entries: Vec<JsValue>,
}
impl<F: Files> Scan<'_, F> {
    fn walk(&mut self, directory: &Path, depth: usize) -> Result<()> {
        if depth > 64 || self.count >= 100_000 || self.started.elapsed() > Duration::from_secs(5) {
            return Err(index_limit());
        }
        let mut children = self
            .metadata
            .files
            .project_entries(self.root, directory)
            .map_err(|e| fs_error(e, "scandir", directory))?;
        children.sort_by(|a, b| self.collator.compare(&a.0, &b.0));
        for (name, is_directory) in children {
            self.count += 1;
            if self.count > 100_000 || self.started.elapsed() > Duration::from_secs(5) {
                return Err(index_limit());
            }
            if is_directory && EXCLUDED.contains(&name.as_str()) {
                continue;
            }
            let Ok(canonical) = self.metadata.files.canonical(&directory.join(&name)) else {
                continue;
            };
            let Ok(relative) = canonical.strip_prefix(self.root) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let Ok((stat, is_file)) = self.metadata.files.project_stat(self.root, &canonical)
            else {
                continue;
            };
            if !stat.directory && !is_file {
                continue;
            }
            let relative = relative.to_string_lossy();
            let mut matches = Vec::new();
            if !self.query.is_empty() {
                append_matches(
                    &mut matches,
                    "name",
                    &name,
                    self.query,
                    &self.locale,
                    self.root,
                );
                append_matches(
                    &mut matches,
                    "relativePath",
                    &relative,
                    self.query,
                    &self.locale,
                    self.root,
                );
            }
            if (self.query.is_empty() || !matches.is_empty()) && (is_file || self.include_dirs) {
                let mut hash = Sha256::new();
                hash.update(self.root.to_string_lossy().as_bytes());
                hash.update([0]);
                hash.update(relative.as_bytes());
                let id = format!("sha256:{:x}", hash.finalize());
                let mut fields = vec![
                    ("id", id.as_str().into()),
                    (
                        "name",
                        canonical
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .as_ref()
                            .into(),
                    ),
                    ("relativePath", relative.as_ref().into()),
                    (
                        "kind",
                        if stat.directory {
                            "directory".into()
                        } else {
                            "file".into()
                        },
                    ),
                    (
                        "size",
                        JsValue::Number(if stat.directory {
                            0.0
                        } else {
                            stat.size as f64
                        }),
                    ),
                    ("mtimeMs", JsValue::Number(stat.mtime_ms)),
                ];
                if !matches.is_empty() {
                    fields.push(("matches", JsValue::Array(matches)));
                }
                self.entries.push(object(fields));
            }
            if stat.directory && self.visited.insert(canonical.clone()) {
                self.walk(&canonical, depth + 1)?;
            }
        }
        Ok(())
    }
}
fn append_matches(
    output: &mut Vec<JsValue>,
    field: &str,
    value: &str,
    query: &str,
    locale: &LanguageIdentifier,
    root: &Path,
) {
    // The baseline's relative(root, canonical).split(sep).join("/") keeps
    // a top-level substring. With a two-byte canonical root, any segment
    // slice of >=13 UTF-16 units also makes the joined path use locale casing.
    // Dirent.name is not a slice. These are measured V8 representation rules,
    // not exhaustive guarantees (see the fixture README's residual risk).
    let substring = field == "relativePath"
        && (!value.contains('/')
            || (root.to_string_lossy().chars().any(|c| c > '\u{ff}')
                && value
                    .split('/')
                    .any(|segment| segment.encode_utf16().count() >= 13)));
    let lower = lowercase(value, locale, substring);
    for (byte, _) in lower.match_indices(query) {
        let start = lower[..byte].encode_utf16().count();
        output.push(object([
            ("field", field.into()),
            ("start", JsValue::Number(start as f64)),
            (
                "end",
                JsValue::Number((start + query.encode_utf16().count()) as f64),
            ),
        ]));
    }
}
#[cfg(test)]
mod tests;
