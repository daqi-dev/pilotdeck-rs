//! Read-only native metadata. Filesystem facts are injectable for archived goldens.
use crate::runtime::memory::js_trim_units;
use pilotdeck_protocol::{JsObject, JsString, JsValue, json_syntax, parse_json};
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}

pub(crate) fn string(value: &JsValue) -> Option<String> {
    value.as_string()?.to_utf8().ok()
}

pub(crate) fn text_field(value: &JsValue, key: &str) -> Option<String> {
    string(value.get(key)?)
}

/// Node's primitive/constructor argument description
/// (`internal/errors.js::determineSpecificType`). The constructor-name branch
/// uses a template literal, i.e. JavaScript `ToString`: an object name becomes
/// `[object Object]`, an array name becomes its comma join, and lone
/// surrogates survive as raw UTF-16 units. `None` is reserved for shapes whose
/// description Node itself aborts on (a truthy primitive constructor makes
/// `'name' in constructor` throw a different TypeError).
pub(crate) fn argument_received(v: &JsValue) -> Option<JsString> {
    Some(match v {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Number(n) => JsString::from(format!(
            "type number ({})",
            crate::dispatch::error_number(*n)
        )),
        JsValue::Bool(b) => format!("type boolean ({b})").into(),
        JsValue::Array(_) => "an instance of Array".into(),
        JsValue::Object(o) => match o.get("constructor") {
            // A plain JSON object inherits `Object` from its prototype, so a
            // missing own `constructor` is Node's `an instance of Object`.
            None => "an instance of Object".into(),
            Some(constructor) if !js_truthy(constructor) => "[Object]".into(),
            Some(constructor @ (JsValue::Object(_) | JsValue::Array(_))) => {
                // `'name' in constructor` is false for `{}` and `[]`, which
                // falls through to `util.inspect(value, { depth: -1 })`.
                if !constructor_has_name(constructor) {
                    "[Object]".into()
                } else {
                    let name = constructor.get("name")?;
                    let text = js_to_string(name)?;
                    let mut units = "an instance of ".encode_utf16().collect::<Vec<_>>();
                    units.extend_from_slice(text.units());
                    JsString::from_utf16(units)
                }
            }
            // Truthy primitive constructors (`5`, `true`, `"abc"`) make Node's
            // `'name' in constructor` throw before a description is built.
            Some(_) => return None,
        },
        JsValue::String(_) => return None,
    })
}

/// `'name' in constructor` for the JSON-representable prototype chain. Parsed
/// JSON objects inherit no own names and arrays have no `name`.
fn constructor_has_name(constructor: &JsValue) -> bool {
    match constructor {
        JsValue::Object(o) => o.contains_key("name"),
        _ => false,
    }
}

fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Object(_) | JsValue::Array(_) => true,
    }
}

/// JavaScript `String(value)` for values that can appear in parsed JSON.
/// `None` when `ToString` throws (`Cannot convert object to primitive value`).
fn js_to_string(value: &JsValue) -> Option<JsString> {
    Some(match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(true) => "true".into(),
        JsValue::Bool(false) => "false".into(),
        JsValue::Number(n) => {
            // `String(-0)` is `"0"`, unlike Node's number diagnostic.
            if *n == 0.0 {
                "0".into()
            } else {
                crate::dispatch::error_number(*n).into()
            }
        }
        JsValue::String(text) => text.clone(),
        // Array.prototype.toString is `join(",")`; nullish elements are empty.
        JsValue::Array(items) => {
            let mut units = Vec::new();
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    units.push(b',' as u16);
                }
                if !matches!(item, JsValue::Null | JsValue::Undefined) {
                    units.extend_from_slice(js_to_string(item)?.units());
                }
            }
            JsString::from_utf16(units)
        }
        JsValue::Object(o) => {
            // Parsed JSON cannot carry a callable `toString`, so an own key
            // makes OrdinaryToPrimitive fall through to a TypeError.
            if o.contains_key("toString") {
                return None;
            }
            "[object Object]".into()
        }
    })
}

/// ECMAScript WhiteSpace + LineTerminator (`String.prototype.trim`, `\s`).
pub(crate) fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(js_space)
}

/// TS `SessionList.ts:10` `isInternalSession`: `always-on-(discovery|workspace|report)`
/// followed by `:` or `-`. Shared with the manager file catalog so the prefix
/// rule has one definition inside the gateway.
pub(crate) fn is_internal_session(session_id: &str) -> bool {
    ["discovery", "workspace", "report"].iter().any(|kind| {
        session_id
            .strip_prefix(&format!("always-on-{kind}"))
            .is_some_and(|rest| rest.starts_with([':', '-']))
    })
}

pub(crate) fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(js_space)
}

pub(crate) fn js_trim_end(value: &str) -> &str {
    value.trim_end_matches(js_space)
}

/// ECMAScript `\s` character-class body for `regex` builders. It includes BOM
/// and excludes NEL; Rust's Unicode `\s` is the opposite on both. Shared by the
/// metadata capture and SDK selector validation so the set has one definition.
pub(crate) const SPACE: &str =
    r"\t-\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";

#[derive(Clone, Debug)]
pub(crate) struct Stat {
    pub directory: bool,
    pub size: u64,
    pub birth_ms: f64,
    pub ctime_ms: f64,
    pub mtime_ms: f64,
}

// Deliberate archive safety refusals, distinct from unmeasured OS InvalidInput.
#[derive(Debug)]
struct ArchiveReadDenied;
impl std::fmt::Display for ArchiveReadDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("archive is not a regular file or exceeds the read limit")
    }
}
impl std::error::Error for ArchiveReadDenied {}
fn archive_read_denied() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, ArchiveReadDenied)
}
pub(crate) fn is_archive_read_denied(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|e| e.is::<ArchiveReadDenied>())
}

pub(crate) trait Files {
    /// UTF-8 byte order, like Node readdir; Dirent.isDirectory does not follow symlinks.
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn session_summary(&self, path: &Path, _size: u64) -> Result<bool, ()> {
        match self.read(path) {
            Ok(bytes) => session_has_summary(&bytes),
            Err(_) => Ok(false),
        }
    }
    // Project operations carry a canonical root, so native implementations can
    // pin directory descriptors and reject symlink replacements during opening.
    fn canonical(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn project_entries(&self, _root: &Path, _path: &Path) -> io::Result<Vec<(String, bool)>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn project_stat(&self, _root: &Path, _path: &Path) -> io::Result<(Stat, bool)> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn project_read(&self, _root: &Path, _path: &Path, _limit: usize) -> io::Result<Vec<u8>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Safe archive bytes, bounded by the caller's transcript/artifact policy.
    fn archive_read(&self, root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        if self.project_stat(root, path)?.0.size > 2_147_483_647 {
            return Err(archive_read_denied());
        }
        self.project_read(root, path, limit)
    }
    /// libc kill(pid, 0), through rustix's safe libc wrapper. All errors, including
    /// EPERM, mean false, matching Node's unconditional catch.
    fn process_alive(&self, pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }
    /// stat follows symlinks.
    fn stat(&self, path: &Path) -> io::Result<Stat>;
}

pub(crate) struct NativeFiles;
impl Files for NativeFiles {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
    fn project_entries(&self, root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
        crate::project_files::native_entries(root, path)
    }
    fn project_stat(&self, root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
        crate::project_files::native_stat(root, path)
    }

    fn project_read(&self, root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut file = crate::project_files::open_beneath(root, path)?;
        // Verify again on the descriptor used for reading (avoid FIFO/device races).
        if !file
            .metadata()
            .map_err(|e| crate::node_io::descriptor_error(e, "fstat"))?
            .is_file()
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        crate::project_files::read_prefix(&mut file, limit)
            .map_err(|e| crate::node_io::descriptor_error(e, "read"))
    }
    fn archive_read(&self, root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut file = crate::project_files::open_beneath(root, path)?;
        let stat = file
            .metadata()
            .map_err(|error| crate::node_io::descriptor_error(error, "fstat"))?;
        if !stat.is_file() || stat.len() > 2_147_483_647 {
            return Err(archive_read_denied());
        }
        crate::project_files::read_prefix(&mut file, limit.min(2_147_483_648))
            .map_err(|error| crate::node_io::descriptor_error(error, "read"))
            .and_then(|bytes| {
                if bytes.len() > 2_147_483_647 {
                    Err(archive_read_denied())
                } else {
                    Ok(bytes)
                }
            })
    }
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let mut entries = fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                Ok((
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.file_type()?.is_dir(),
                ))
            })
            .collect::<io::Result<Vec<_>>>()?;
        // 对齐 Node readdir（libuv uv_fs_scandir 的 strcmp 排序）。
        // 协调者实测（Node v22，APFS），同一目录：
        // ls -f: Beta ä middle zebra 10 _x alpha 2
        // readdirSync / fs/promises.readdir(withFileTypes): 10 2 Beta _x alpha middle zebra ä
        entries.sort_by(|l, r| l.0.as_bytes().cmp(r.0.as_bytes()));
        Ok(entries)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(path)?;
        let stat = file
            .metadata()
            .map_err(|e| crate::node_io::descriptor_error(e, "fstat"))?;
        let mut bytes = Vec::with_capacity(stat.len().min(1024 * 1024) as usize);
        file.read_to_end(&mut bytes)
            .map_err(|e| crate::node_io::descriptor_error(e, "read"))?;
        Ok(bytes)
    }
    fn session_summary(&self, path: &Path, size: u64) -> Result<bool, ()> {
        let Ok(mut file) = fs::File::open(path) else {
            return Ok(false);
        };
        let mut head = vec![0; size.min(LITE_BYTES as u64) as usize];
        if file.read_exact(&mut head).is_err() {
            return Ok(false);
        }
        let mut tail = head.clone();
        if size > LITE_BYTES as u64 {
            tail.resize(LITE_BYTES, 0);
            if file
                .seek(SeekFrom::Start(size - LITE_BYTES as u64))
                .is_err()
                || file.read_exact(&mut tail).is_err()
            {
                return Ok(false);
            }
        }
        let (visible, fast) = summary_preview(&head, &tail, size > LITE_BYTES as u64)?;
        if let Some(visible) = visible {
            return Ok(visible);
        }
        if file.seek(SeekFrom::Start(0)).is_err() {
            return Ok(fast);
        }
        let visible = fast || scan_metadata_summary(&mut file).unwrap_or(false);
        if visible && !fast {
            captured_field(&String::from_utf8_lossy(&head), "createdAt").map_err(|_| ())?;
        }
        Ok(visible)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let stat = fs::metadata(path)?;
        #[cfg(unix)]
        let ctime_ms = {
            use std::os::unix::fs::MetadataExt;
            stat.ctime() as f64 * 1000.0 + stat.ctime_nsec() as f64 / 1_000_000.0
        };
        #[cfg(not(unix))]
        let ctime_ms = millis(stat.modified()?);
        Ok(Stat {
            directory: stat.is_dir(),
            size: stat.len(),
            birth_ms: stat.created().map(millis).unwrap_or(0.0),
            ctime_ms,
            mtime_ms: millis(stat.modified()?),
        })
    }
}

/// Fractional milliseconds, the input to Node's `Stats.mtimeMs`.
pub(crate) fn millis(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            duration.as_secs() as f64 * 1000.0 + duration.subsec_nanos() as f64 / 1_000_000.0
        }
        Err(error) => {
            -(error.duration().as_secs() as f64 * 1000.0
                + error.duration().subsec_nanos() as f64 / 1_000_000.0)
        }
    }
}

pub(crate) struct Metadata<F = NativeFiles> {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub files: F,
}

impl Metadata {
    pub fn from_env() -> io::Result<Option<Self>> {
        let Ok(home) = std::env::var("PILOT_HOME") else {
            return Ok(None);
        };
        // Explicit PILOT_HOME only: never open the user's real default home.
        // Deployment must use the same cwd as TS process.cwd(): relative
        // project keys, .cwd markers and PILOT_HOME all resolve against it.
        let cwd = std::env::current_dir().map_err(|error| {
            let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
            io::Error::new(error.kind(), message)
        })?;
        let home = if home == "~" || home.starts_with("~/") {
            let Ok(user_home) = std::env::var("HOME") else {
                return Ok(None);
            };
            resolve(
                &cwd,
                &PathBuf::from(user_home)
                    .join(home.strip_prefix("~/").unwrap_or(""))
                    .to_string_lossy(),
            )
        } else {
            resolve(&cwd, &home)
        };
        Ok(Some(Self {
            home,
            cwd,
            files: NativeFiles,
        }))
    }
}

pub(crate) fn resolve(cwd: &Path, value: &str) -> PathBuf {
    resolve_path(cwd, Path::new(value))
}

/// Node `path.resolve` lexical normalization over an already-typed path.
/// Shared by config/session paths and the project-key registry so the copy
/// cannot drift (#301).
pub(crate) fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut output = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            component => output.push(component.as_os_str()),
        }
    }
    output
}

/// Node `path.resolve(cwd, path)` when `cwd` may itself be relative.
pub(crate) fn resolve_process_path(cwd: &Path, path: &Path) -> io::Result<PathBuf> {
    // Node resolves right to left: an absolute argument makes process cwd irrelevant.
    if path.is_absolute() {
        return Ok(resolve_path(Path::new(""), path));
    }
    if cwd.is_absolute() {
        return Ok(resolve_path(cwd, path));
    }
    let process_cwd = std::env::current_dir()
        .map_err(|error| io::Error::new(error.kind(), ProcessCwdError(error)))?;
    Ok(resolve_path(&resolve_path(&process_cwd, cwd), path))
}

// Keep the same kind/Display as from_env, and retain the native errno for
// callers whose existing error boundary also exposes a JS error code.
#[derive(Debug)]
struct ProcessCwdError(io::Error);
impl std::fmt::Display for ProcessCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::node_io::diagnostic_or_unknown(&self.0, "uv_cwd", None).1)
    }
}
impl std::error::Error for ProcessCwdError {}

pub(crate) fn process_cwd_diagnostic(error: &io::Error) -> (&'static str, String) {
    let source = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ProcessCwdError>())
        .map_or(error, |inner| &inner.0);
    crate::node_io::diagnostic_or_unknown(source, "uv_cwd", None)
}

const UTF16_SLASH: u16 = b'/' as u16;
const UTF16_DOT: u16 = b'.' as u16;

/// Node `path.resolve(base, value)` over raw UTF-16 code units: relative
/// values join `base`, `.` and `..` fold lexically, and repeated separators
/// collapse. This must run before any UTF-8 conversion because `..` can pop a
/// segment that happens to contain a lone surrogate; `path.resolve` never
/// inspects segment content, so `/fixture/project-\ud800/../project` names
/// `/fixture/project` even though the raw key is not representable. Shared by
/// the project-key identity seams (metadata/session-model) and the existing
/// workspace-snapshot/task-budget lexical resolvers.
pub(crate) fn resolve_units(base: &str, value: &JsString) -> JsString {
    let mut source = Vec::new();
    if value.units().first() != Some(&UTF16_SLASH) {
        source.extend(base.encode_utf16());
        source.push(UTF16_SLASH);
    }
    source.extend_from_slice(value.units());
    let mut segments: Vec<&[u16]> = Vec::new();
    for segment in source.split(|unit| *unit == UTF16_SLASH) {
        if segment.is_empty() || (segment.len() == 1 && segment[0] == UTF16_DOT) {
            continue;
        }
        if segment.len() == 2 && segment[0] == UTF16_DOT && segment[1] == UTF16_DOT {
            segments.pop();
        } else {
            segments.push(segment);
        }
    }
    let mut output = Vec::with_capacity(source.len().max(1));
    output.push(UTF16_SLASH);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            output.push(UTF16_SLASH);
        }
        output.extend_from_slice(segment);
    }
    JsString::from_utf16(output)
}

/// Resolve a project key the way `projectRegistry.ts:42` does, but expose it
/// as UTF-8 only after normalization. A lone surrogate that survives the
/// lexical pass is genuinely unrepresentable and cannot equal any disk-derived
/// (valid UTF-8) project key; one folded away by `.`/`..` never prevents the
/// lookup.
pub(crate) fn resolved_identity(cwd: &Path, value: &JsString) -> Option<String> {
    resolve_units(&cwd.to_string_lossy(), value).to_utf8().ok()
}

pub(crate) fn project_id(root: &Path) -> String {
    let mut result = String::new();
    let normalized = root.to_string_lossy().replace('\\', "/");
    let normalized = if normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes()[0].is_ascii_alphabetic()
    {
        &normalized[2..]
    } else {
        &normalized
    };
    let mut replacing = false;
    for ch in normalized.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            result.push(ch);
            replacing = false;
        } else if !replacing {
            result.push('-');
            replacing = true;
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        "project".into()
    } else {
        result.into()
    }
}

impl<F: Files> Metadata<F> {
    fn read_text(&self, path: &Path) -> io::Result<String> {
        self.files
            .read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
    fn marker(&self, dir: &Path) -> Option<String> {
        self.read_text(&dir.join(".cwd"))
            .ok()
            .map(|text| js_trim(&text).to_owned())
    }
    fn registered_path(&self, id: &str) -> Option<String> {
        if let Some(marker) = self.marker(&self.home.join("projects").join(id)) {
            if marker.is_empty() {
                return None;
            }
            if self
                .files
                .stat(&resolve(&self.cwd, &marker))
                .is_ok_and(|stat| stat.directory)
            {
                return Some(marker);
            }
        }
        // TS legacy slug recovery on Unix; no filesystem canonicalization.
        let segments: Vec<_> = id.split('-').collect();
        for first in 0..segments.len() {
            let candidate = format!("/{}", segments[first..].join("/"));
            if project_id(Path::new(&candidate)) == id
                && self
                    .files
                    .stat(Path::new(&candidate))
                    .is_ok_and(|stat| stat.directory)
            {
                return Some(candidate);
            }
        }
        None
    }
    /// `createDialogProjectRegistry.resolveProjectKey` (projectRegistry.ts:42-
    /// 59) over the real `listProjects` catalog: the virtual General workspace
    /// is `resolve(pilotHome)`, every other key must be the registered path of
    /// a project directory. Raw UTF-16 is resolved lexically before the UTF-8
    /// conversion, so a lone surrogate folded away by `.`/`..` still resolves
    /// while one that survives cannot equal a disk-derived key.
    pub(crate) fn resolve_dialog_project(&self, value: &JsString) -> Option<String> {
        let requested = resolve_units(&self.cwd.to_string_lossy(), value);
        let general = resolve(&self.cwd, &self.home.to_string_lossy());
        let general_units = general.to_string_lossy().encode_utf16().collect::<Vec<_>>();
        if requested.units() == general_units {
            return Some(general.to_string_lossy().into_owned());
        }
        // A surviving unpaired surrogate names no registered (valid UTF-8)
        // path; the caller still keeps its units for the wire message.
        let requested = PathBuf::from(requested.to_utf8().ok()?);
        let projects = self.home.join("projects");
        for (id, _) in self.files.entries(&projects).unwrap_or_default() {
            let dir = projects.join(&id);
            if !self.files.stat(&dir).is_ok_and(|stat| stat.directory) {
                continue;
            }
            let Some(root) = self.registered_path(&id) else {
                continue;
            };
            if resolve(&self.cwd, &root) == requested {
                return Some(root);
            }
        }
        None
    }

    /// `resolveProjectStorageId(projectRoot, pilotHome)` (paths.ts:85-87 +
    /// 157-178): the first project directory whose valid `.cwd` marker
    /// resolves to the project root wins; otherwise the legacy path slug.
    pub(crate) fn project_storage_id(&self, project_root: &str) -> String {
        let target = resolve(&self.cwd, project_root);
        let projects = self.home.join("projects");
        for (id, is_directory) in self.files.entries(&projects).unwrap_or_default() {
            // `readdirSync(..., {withFileTypes:true})` uses the directory
            // entry itself (a symlink is not a directory), like NativeFiles.
            if !is_directory {
                continue;
            }
            let dir = projects.join(&id);
            let Some(marker) = self.marker(&dir) else {
                continue;
            };
            if marker.is_empty() {
                continue;
            }
            let marker_path = resolve(&self.cwd, &marker);
            // TS compares the resolved marker but calls `statSync(marker)` on
            // the raw spelling (paths.ts:137-143). A lexical `..` can fold a
            // missing or non-directory segment away, so probing `marker_path`
            // would accept a marker Node rejects. Keep identity on the
            // normalized path and existence on `process.cwd() + raw marker`.
            let marker_on_disk = self.cwd.join(&marker);
            if marker_path == target
                && self
                    .files
                    .stat(&marker_on_disk)
                    .is_ok_and(|stat| stat.directory)
            {
                return id;
            }
        }
        project_id(&target)
    }

    /// Registration membership without reading unrelated chat summaries. Archive
    /// containment must be checked before any transcript content can be opened.
    pub(crate) fn has_registered_project(&self, requested: &Path) -> bool {
        let projects = self.home.join("projects");
        self.files
            .entries(&projects)
            .unwrap_or_default()
            .into_iter()
            .any(|(id, _)| {
                self.files
                    .stat(&projects.join(&id))
                    .is_ok_and(|stat| stat.directory)
                    && self.registered_path(&id).is_some_and(|root| {
                        let root = resolve(&self.cwd, &root);
                        root != self.home && root == requested
                    })
            })
    }
    pub fn list_projects(&self) -> JsValue {
        let projects_dir = self.home.join("projects");
        let mut projects = Vec::new();
        // TS cc7c4e6 src/web/server/listProjects.ts:30-34 returns [] on every
        // readdir error, without filtering errno; preserve that RPC behavior.
        for (id, _) in self.files.entries(&projects_dir).unwrap_or_default() {
            let dir = projects_dir.join(&id);
            // Same source :39-44 also skips entries on any stat error.
            if !self.files.stat(&dir).is_ok_and(|stat| stat.directory) {
                continue;
            }
            let Some(root) = self.registered_path(&id) else {
                continue;
            };
            if resolve(&self.cwd, &root) == self.home {
                continue;
            }
            projects.push(self.summary(
                &root,
                Some(&root),
                Some(&dir),
                JsString::from(root.as_str()),
            ));
        }
        // Stable, like JS Array.sort: activity ties retain UTF-8 byte directory order.
        projects.sort_by(|left, right| {
            let activity = |project: &JsValue| {
                project
                    .get("lastActivity")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0)
            };
            activity(right)
                .partial_cmp(&activity(left))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        object([("projects", JsValue::Array(projects))])
    }
    fn describe_storage(&self, root: &str, identity: Option<&str>) -> Option<PathBuf> {
        let projects_dir = self.home.join("projects");
        let target = resolve(&self.cwd, root);
        let candidate = projects_dir.join(project_id(&target));
        let matches = |marker: &str| {
            identity
                .is_some_and(|identity| resolve(&self.cwd, marker) == resolve(&self.cwd, identity))
        };
        let mut legacy = None;
        if self.files.stat(&candidate).is_ok_and(|stat| stat.directory) {
            match self.marker(&candidate) {
                Some(marker) if !marker.is_empty() && matches(&marker) => {
                    return Some(candidate);
                }
                None => legacy = Some(candidate),
                Some(marker) if marker.is_empty() => legacy = Some(candidate),
                _ => {}
            }
        }
        // TS returns undefined on failed readdir, even if legacy was found.
        for (id, directory) in self.files.entries(&projects_dir).ok()? {
            if directory
                && self
                    .marker(&projects_dir.join(&id))
                    .is_some_and(|marker| !marker.is_empty() && matches(&marker))
            {
                return Some(projects_dir.join(id));
            }
        }
        legacy
    }
    pub fn describe_project(&self, root: &JsString) -> JsValue {
        // Node's fs layer replacement-encodes a lone surrogate, but the
        // registry identity comparison in `listProjects.ts:120-160` keeps the
        // original JS string. Resolve raw UTF-16 units first: `..` can drop a
        // surrogate-bearing segment before identity is compared, so only a
        // surrogate that survives normalization is unrepresentable. Use the
        // lossy text for filesystem paths and the fallback project id;
        // otherwise `"\ud800"` would alias onto a project whose path contains
        // literal U+FFFD.
        let path = String::from_utf16_lossy(root.units());
        let identity = resolved_identity(&self.cwd, root);
        let storage = self.describe_storage(&path, identity.as_deref());
        self.summary(&path, identity.as_deref(), storage.as_deref(), root.clone())
    }
    pub(crate) fn chat_dir(&self, root: &str) -> PathBuf {
        self.chat_dir_with_identity(root, Some(root))
    }
    fn chat_dir_with_identity(&self, root: &str, identity: Option<&str>) -> PathBuf {
        let target = resolve(&self.cwd, root);
        let projects_dir = self.home.join("projects");
        for (id, directory) in self.files.entries(&projects_dir).unwrap_or_default() {
            if !directory {
                continue;
            }
            if let Some(marker) = self.marker(&projects_dir.join(&id))
                && !marker.is_empty()
                && identity.is_some_and(|identity| {
                    resolve(&self.cwd, &marker) == resolve(&self.cwd, identity)
                })
                && self
                    .files
                    .stat(&resolve(&self.cwd, &marker))
                    .is_ok_and(|stat| stat.directory)
            {
                return projects_dir.join(id).join("chats");
            }
        }
        projects_dir.join(project_id(&target)).join("chats")
    }
    fn summary(
        &self,
        root: &str,
        identity: Option<&str>,
        storage: Option<&Path>,
        wire_root: JsString,
    ) -> JsValue {
        let created = storage
            .and_then(|dir| self.files.stat(dir).ok())
            .map(|stat| {
                if stat.birth_ms != 0.0 {
                    stat.birth_ms
                } else {
                    stat.ctime_ms
                }
            });
        let chat_dir = self.chat_dir_with_identity(root, identity);
        let mut activity = Vec::new();
        for (name, _) in self.files.entries(&chat_dir).unwrap_or_default() {
            let Some(session) = name.strip_suffix(".jsonl") else {
                continue;
            };
            if is_internal_session(session) {
                continue;
            }
            let path = chat_dir.join(name);
            let Ok(stat) = self.files.stat(&path) else {
                continue;
            };
            if stat.size == 0 {
                continue;
            }
            let Ok(has_summary) = self.files.session_summary(&path, stat.size) else {
                // The TS summarizeProject catches the entire catalog failure.
                activity.clear();
                break;
            };
            if !has_summary {
                continue;
            }
            activity.push(node_date_ms(stat.mtime_ms));
        }
        activity
            .sort_by(|left, right| right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal));
        let count = activity.len();
        let last_activity = activity.first().copied();
        let units = wire_root.units();
        let end = units
            .iter()
            .rposition(|unit| *unit != b'/' as u16)
            .map_or(0, |index| index + 1);
        let start = units[..end]
            .iter()
            .rposition(|unit| *unit == b'/' as u16)
            .map_or(0, |index| index + 1);
        let name = if start == end {
            wire_root.clone()
        } else {
            JsString::from_utf16(units[start..end].to_vec())
        };
        object([
            ("projectKey", JsValue::String(wire_root.clone())),
            ("name", JsValue::String(name)),
            ("fullPath", JsValue::String(wire_root)),
            ("sessionCount", JsValue::Number(count as f64)),
            (
                "lastActivity",
                last_activity.map_or(JsValue::Undefined, JsValue::Number),
            ),
            (
                "createdAt",
                created.map_or(JsValue::Undefined, JsValue::Number),
            ),
        ])
    }
}

fn summary_string(value: &JsValue) -> Option<JsString> {
    value.as_string().cloned()
}

/// `lastMetadataStringField`/`firstJsonStringField` capture plus
/// `unescapeJsonString`. A malformed escape is a V8 `SyntaxError` with the
/// exact `JSON.parse` message; the caller must propagate it, never treat the
/// field as absent (TS throws out of `readSessionInfo`).
pub(crate) fn captured_field(source: &str, field: &str) -> Result<Option<JsString>, JsString> {
    const FIELDS: [&str; 6] = [
        "title",
        "aiTitle",
        "tag",
        "parentSessionId",
        "forkedFromTurnId",
        "createdAt",
    ];
    // ECMAScript \s includes BOM and excludes NEL; regex Unicode \s differs.
    static EXPRESSIONS: std::sync::LazyLock<[regex::Regex; 6]> = std::sync::LazyLock::new(|| {
        FIELDS.map(|field| {
            regex::Regex::new(&format!(
                r#""{}"[{SPACE}]*:[{SPACE}]*"((?:\\.|[^"])*)""#,
                field
            ))
            .expect("fixed field regex")
        })
    });
    let expression = &EXPRESSIONS[FIELDS
        .iter()
        .position(|key| *key == field)
        .expect("fixed field")];
    let Some(capture) = expression.captures(source) else {
        return Ok(None);
    };
    if capture[1].is_empty() {
        return Ok(None);
    }
    let literal = format!("\"{}\"", &capture[1]);
    let units = literal.encode_utf16().collect::<Vec<_>>();
    match parse_json(&literal) {
        Ok(value) => Ok(value.as_string().cloned()),
        Err(_) => Err(json_syntax::parse_error_units(&units)
            .unwrap_or_else(|| JsString::from("Unexpected end of JSON input"))),
    }
}

/// Node `Date#getTime()` for a `Stats.mtimeMs` value: V8 rounds the
/// double-precision millisecond value half toward +Infinity, so a
/// sub-millisecond fraction below 0.5 still moves up when the stored double is
/// exactly `x.5` (Node v22.22.0).
pub(crate) fn node_date_ms(ms: f64) -> f64 {
    let floor = ms.floor();
    let rounded = if ms - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    if !rounded.is_finite() || rounded.abs() > 8_640_000_000_000_000.0 {
        f64::NAN
    } else {
        rounded
    }
}

pub(crate) fn accepted_text(source: &str, last: bool) -> Option<JsString> {
    let mut result = None;
    'lines: for line in source.split('\n') {
        if !line.contains("\"type\":\"accepted_input\"") {
            continue;
        }
        let Ok(entry) = parse_json(line) else {
            if !last {
                return None;
            } else {
                continue;
            }
        };
        let messages = match entry.get("messages") {
            Some(JsValue::Array(messages)) => messages,
            None | Some(JsValue::Null | JsValue::Undefined) => continue,
            _ => {
                if !last {
                    return None;
                } else {
                    continue;
                }
            }
        };
        let mut blocks = Vec::new();
        for message in messages {
            if matches!(message, JsValue::Null | JsValue::Undefined) {
                if !last {
                    return None;
                } else {
                    continue 'lines;
                }
            }
            match message.get("content") {
                Some(JsValue::Array(content)) => blocks.extend(content),
                None | Some(JsValue::Null | JsValue::Undefined) => {}
                Some(content) => blocks.push(content),
            }
        }
        for block in blocks {
            if matches!(block, JsValue::Null | JsValue::Undefined) {
                if !last {
                    return None;
                } else {
                    continue 'lines;
                }
            }
            if text_field(block, "type").as_deref() != Some("text") {
                continue;
            }
            match block.get("text") {
                Some(JsValue::String(text)) => {
                    // JS `String#trim` over raw UTF-16: a lone surrogate in
                    // the prompt stays a surrogate, never U+FFFD.
                    let trimmed = js_trim_units(text.units());
                    if !trimmed.is_empty() {
                        result = Some(JsString::from_utf16(trimmed.to_vec()));
                        if !last {
                            return result;
                        }
                    }
                }
                None | Some(JsValue::Null | JsValue::Undefined) => {}
                _ => {
                    if !last {
                        return None;
                    }
                }
            }
            break;
        }
    }
    result
}

fn metadata_summary(value: &JsValue) -> bool {
    ["title", "aiTitle", "lastPrompt", "firstPrompt"]
        .iter()
        .find_map(|key| {
            value
                .get(key)
                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
        })
        .and_then(summary_string)
        .is_some_and(|text| !js_trim_units(text.units()).is_empty())
}

const LITE_BYTES: usize = 65_536;

fn session_has_summary(bytes: &[u8]) -> Result<bool, ()> {
    let head = &bytes[..bytes.len().min(LITE_BYTES)];
    let tail = &bytes[bytes.len().saturating_sub(LITE_BYTES)..];
    let (visible, fast) = summary_preview(head, tail, bytes.len() > LITE_BYTES)?;
    if let Some(visible) = visible {
        return Ok(visible);
    }
    let visible = fast || scan_metadata_summary(&mut io::Cursor::new(bytes)).unwrap_or(false);
    if visible && !fast {
        captured_field(&String::from_utf8_lossy(head), "createdAt").map_err(|_| ())?;
    }
    Ok(visible)
}

pub(crate) fn strict_metadata(line: &str) -> Option<JsValue> {
    if !line.contains("\"type\":\"session_metadata\"") {
        return None;
    }
    let entry = parse_json(line).ok()?;
    if text_field(&entry, "type").as_deref() != Some("session_metadata") {
        return None;
    }
    let source = entry.get("metadata")?.as_object()?;
    let mut metadata = JsObject::new();
    if source.get("isSnapshot").and_then(JsValue::as_bool) == Some(true) {
        metadata.insert("isSnapshot".into(), JsValue::Bool(true));
    }
    for key in [
        "title",
        "aiTitle",
        "tag",
        "firstPrompt",
        "lastPrompt",
        "parentSessionId",
        "forkedFromTurnId",
    ] {
        if let Some(value) = source.get(key).filter(|value| value.as_string().is_some()) {
            metadata.insert(key.into(), value.clone());
        }
    }
    Some(JsValue::Object(metadata))
}

fn summary_preview(head: &[u8], tail: &[u8], large: bool) -> Result<(Option<bool>, bool), ()> {
    let head = String::from_utf8_lossy(head);
    let tail = String::from_utf8_lossy(tail);
    let source = format!("{head}\n{tail}");
    let metadata_field = |key| -> Result<Option<JsString>, ()> {
        let mut latest = None;
        for line in source
            .split('\n')
            .filter(|line| line.contains("\"session_metadata\""))
        {
            if let Some(value) = captured_field(line, key).map_err(|_| ())? {
                latest = Some(value);
            }
        }
        Ok(latest)
    };
    let title = metadata_field("title")?;
    let ai_title = metadata_field("aiTitle")?;
    for key in ["tag", "parentSessionId", "forkedFromTurnId"] {
        metadata_field(key)?;
    }
    let fast = title
        .or(ai_title)
        .or_else(|| accepted_text(&tail, true))
        .or_else(|| accepted_text(&head, false))
        .is_some_and(|text| !js_trim_units(text.units()).is_empty());
    if fast {
        captured_field(&head, "createdAt").map_err(|_| ())?;
    }
    if fast && !large {
        return Ok((Some(true), fast));
    }
    if large {
        let mut latest = None;
        for line in tail.split('\n').skip(1) {
            if !line.contains("\"type\":\"session_metadata\"") {
                continue;
            }
            match strict_metadata(line) {
                Some(metadata) => latest = Some(metadata),
                None => {
                    latest = None;
                    break;
                }
            }
        }
        if let Some(latest) = latest
            && latest.get("isSnapshot").and_then(JsValue::as_bool) == Some(true)
            && ["title", "aiTitle", "lastPrompt", "firstPrompt"]
                .iter()
                .any(|key| {
                    latest
                        .get(key)
                        .and_then(summary_string)
                        .is_some_and(|text| !js_trim_units(text.units()).is_empty())
                })
        {
            let visible = fast || metadata_summary(&latest);
            if visible && !fast {
                captured_field(&head, "createdAt").map_err(|_| ())?;
            }
            return Ok((Some(visible), fast));
        }
    }
    Ok((None, fast))
}

fn merge_summary_line(line: &[u8], merged: &mut JsObject) {
    let Some(metadata) = strict_metadata(&String::from_utf8_lossy(line)) else {
        return;
    };
    for key in ["title", "aiTitle", "firstPrompt", "lastPrompt"] {
        if let Some(value) = metadata
            .get(key)
            .filter(|value| value.as_string().is_some())
        {
            merged.insert(key.into(), value.clone());
        }
    }
}

fn scan_metadata_summary(reader: &mut impl Read) -> io::Result<bool> {
    // Same 64KiB chunks / 128KiB non-metadata line cap as SessionList.ts.
    // Large media records are skipped without retaining an entire transcript.
    let mut buffer = [0; LITE_BYTES];
    let mut line = Vec::new();
    let mut discarded = false;
    let mut metadata_line = false;
    let mut merged = JsObject::new();
    loop {
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        let chunk = &buffer[..length];
        let mut start = 0;
        for end in chunk
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
            .chain(std::iter::once(length))
        {
            let segment = &chunk[start..end];
            if !discarded {
                line.extend_from_slice(segment);
                if !metadata_line {
                    metadata_line =
                        String::from_utf8_lossy(&line).contains("\"type\":\"session_metadata\"");
                }
                if !metadata_line && line.len() > 128 * 1024 {
                    line.clear();
                    discarded = true;
                }
            }
            if end < length {
                if !discarded {
                    merge_summary_line(&line, &mut merged);
                }
                line.clear();
                discarded = false;
                metadata_line = false;
                start = end + 1;
            }
        }
    }
    if !discarded && !line.is_empty() {
        merge_summary_line(&line, &mut merged);
    }
    Ok(metadata_summary(&JsValue::Object(merged)))
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(all(test, unix))]
#[path = "../tests/fixtures/metadata-from-env/regression.rs"]
mod from_env_tests;

#[cfg(all(test, unix))]
#[path = "../tests/fixtures/metadata-process-cwd/regression.rs"]
mod process_cwd_tests;
