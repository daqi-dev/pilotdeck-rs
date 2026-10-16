//! Read-only native metadata. Filesystem facts are injectable for archived goldens.
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
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

/// Node's primitive/constructor argument description. Unsupported constructor
/// names require the host's util.inspect contract, as with describe_project.
pub(crate) fn argument_received(v: &JsValue) -> Option<String> {
    Some(match v {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Number(n) => format!("type number ({})", crate::dispatch::error_number(*n)),
        JsValue::Bool(b) => format!("type boolean ({b})"),
        JsValue::Array(_) => "an instance of Array".into(),
        JsValue::Object(o) => match o.get("constructor") {
            None => "an instance of Object".into(),
            Some(c) => match c.get("name") {
                Some(JsValue::String(s)) if !s.is_empty() => {
                    format!("an instance of {}", s.to_utf8().ok()?)
                }
                Some(JsValue::Number(n)) if *n != 0.0 && !n.is_nan() => {
                    format!("an instance of {}", crate::dispatch::error_number(*n))
                }
                Some(JsValue::Bool(true)) => "an instance of true".into(),
                Some(JsValue::Object(_) | JsValue::Array(_)) => return None,
                _ => "[Object]".into(),
            },
        },
        JsValue::String(_) => return None,
    })
}

pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}

#[derive(Clone, Debug)]
pub(crate) struct Stat {
    pub directory: bool,
    pub size: u64,
    pub birth_ms: f64,
    pub ctime_ms: f64,
    pub mtime_ms: f64,
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
    /// Complete archive bytes; kept separate from prefix reads and transcript policy.
    fn archive_read(&self, root: &Path, path: &Path) -> io::Result<Vec<u8>> {
        if self.project_stat(root, path)?.0.size > 2_147_483_647 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.project_read(root, path, usize::MAX)
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
    fn archive_read(&self, root: &Path, path: &Path) -> io::Result<Vec<u8>> {
        let mut file = crate::project_files::open_beneath(root, path)?;
        let stat = file
            .metadata()
            .map_err(|error| crate::node_io::descriptor_error(error, "fstat"))?;
        if !stat.is_file() || stat.len() > 2_147_483_647 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        crate::project_files::read_prefix(&mut file, 2_147_483_648)
            .map_err(|error| crate::node_io::descriptor_error(error, "read"))
            .and_then(|bytes| {
                if bytes.len() > 2_147_483_647 {
                    Err(io::ErrorKind::InvalidInput.into())
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
            captured_field(&String::from_utf8_lossy(&head), "createdAt")?;
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

fn millis(time: SystemTime) -> f64 {
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
    pub fn from_env() -> Option<Self> {
        let home = std::env::var("PILOT_HOME").ok()?;
        // Explicit PILOT_HOME only: never open the user's real default home.
        // Deployment must use the same cwd as TS process.cwd(): relative
        // project keys, .cwd markers and PILOT_HOME all resolve against it.
        let cwd = std::env::current_dir().ok()?;
        let home = if home == "~" || home.starts_with("~/") {
            let user_home = std::env::var("HOME").ok()?;
            resolve(
                &cwd,
                &PathBuf::from(user_home)
                    .join(home.strip_prefix("~/").unwrap_or(""))
                    .to_string_lossy(),
            )
        } else {
            resolve(&cwd, &home)
        };
        Some(Self {
            home,
            cwd,
            files: NativeFiles,
        })
    }
}

pub(crate) fn resolve(cwd: &Path, value: &str) -> PathBuf {
    let path = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        cwd.join(value)
    };
    let mut output = PathBuf::new();
    for component in path.components() {
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
        for (id, _) in self.files.entries(&projects_dir).unwrap_or_default() {
            let dir = projects_dir.join(&id);
            if !self.files.stat(&dir).is_ok_and(|stat| stat.directory) {
                continue;
            }
            let Some(root) = self.registered_path(&id) else {
                continue;
            };
            if resolve(&self.cwd, &root) == self.home {
                continue;
            }
            projects.push(self.summary(&root, Some(&dir), JsString::from(root.as_str())));
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
    fn describe_storage(&self, root: &str) -> Option<PathBuf> {
        let projects_dir = self.home.join("projects");
        let target = resolve(&self.cwd, root);
        let candidate = projects_dir.join(project_id(&target));
        let mut legacy = None;
        if self.files.stat(&candidate).is_ok_and(|stat| stat.directory) {
            match self.marker(&candidate) {
                Some(marker) if !marker.is_empty() && resolve(&self.cwd, &marker) == target => {
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
                && self.marker(&projects_dir.join(&id)).is_some_and(|marker| {
                    !marker.is_empty() && resolve(&self.cwd, &marker) == target
                })
            {
                return Some(projects_dir.join(id));
            }
        }
        legacy
    }
    pub fn describe_project(&self, root: &JsString) -> JsValue {
        let path = String::from_utf16_lossy(root.units());
        self.summary(&path, self.describe_storage(&path).as_deref(), root.clone())
    }
    pub(crate) fn chat_dir(&self, root: &str) -> PathBuf {
        let target = resolve(&self.cwd, root);
        let projects_dir = self.home.join("projects");
        for (id, directory) in self.files.entries(&projects_dir).unwrap_or_default() {
            if !directory {
                continue;
            }
            if let Some(marker) = self.marker(&projects_dir.join(&id))
                && !marker.is_empty()
                && resolve(&self.cwd, &marker) == target
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
    fn summary(&self, root: &str, storage: Option<&Path>, wire_root: JsString) -> JsValue {
        let created = storage
            .and_then(|dir| self.files.stat(dir).ok())
            .map(|stat| {
                if stat.birth_ms != 0.0 {
                    stat.birth_ms
                } else {
                    stat.ctime_ms
                }
            });
        let chat_dir = self.chat_dir(root);
        let mut activity = Vec::new();
        for (name, _) in self.files.entries(&chat_dir).unwrap_or_default() {
            let Some(session) = name.strip_suffix(".jsonl") else {
                continue;
            };
            if [
                "always-on-discovery",
                "always-on-workspace",
                "always-on-report",
            ]
            .iter()
            .any(|prefix| {
                session
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with([':', '-']))
            }) {
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

fn summary_string(value: &JsValue) -> Option<String> {
    value
        .as_string()
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn captured_field(source: &str, field: &str) -> Result<Option<String>, ()> {
    const FIELDS: [&str; 6] = [
        "title",
        "aiTitle",
        "tag",
        "parentSessionId",
        "forkedFromTurnId",
        "createdAt",
    ];
    // ECMAScript \s includes BOM and excludes NEL; regex Unicode \s differs.
    const SPACE: &str = r"[\t-\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]";
    static EXPRESSIONS: std::sync::LazyLock<[regex::Regex; 6]> = std::sync::LazyLock::new(|| {
        FIELDS.map(|field| {
            regex::Regex::new(&format!(r#""{}"{SPACE}*:{SPACE}*"((?:\\.|[^"])*)""#, field))
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
    Ok(summary_string(
        &parse_json(&format!("\"{}\"", &capture[1])).map_err(|_| ())?,
    ))
}

fn node_date_ms(ms: f64) -> f64 {
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

fn accepted_text(source: &str, last: bool) -> Option<String> {
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
                    let text = String::from_utf16_lossy(text.units());
                    if !js_trim(&text).is_empty() {
                        result = Some(js_trim(&text).to_owned());
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
        .is_some_and(|text| !js_trim(&text).is_empty())
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
        captured_field(&String::from_utf8_lossy(head), "createdAt")?;
    }
    Ok(visible)
}

fn strict_metadata(line: &str) -> Option<JsValue> {
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
    let metadata_field = |key| -> Result<Option<String>, ()> {
        let mut latest = None;
        for line in source
            .split('\n')
            .filter(|line| line.contains("\"session_metadata\""))
        {
            if let Some(value) = captured_field(line, key)? {
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
        .is_some_and(|text| !text.is_empty());
    if fast {
        captured_field(&head, "createdAt")?;
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
                        .is_some_and(|text| !js_trim(&text).is_empty())
                })
        {
            let visible = fast || metadata_summary(&latest);
            if visible && !fast {
                captured_field(&head, "createdAt")?;
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
