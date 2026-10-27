//! Turn-owned workspace collector. Only reported, potentially mutating tools
//! authorize a workspace diff; overlapping collectors retain explicit paths only.
use super::object;
use pilotdeck_protocol::JsValue;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

type Active = Mutex<BTreeMap<PathBuf, Vec<Weak<AtomicBool>>>>;
static ACTIVE: OnceLock<Active> = OnceLock::new();
const EXCLUDED: &[&str] = &[
    ".git",
    ".pilotdeck",
    ".cache",
    ".idea",
    ".next",
    ".nuxt",
    ".output",
    ".mypy_cache",
    ".pytest_cache",
    ".svelte-kit",
    ".temp",
    ".tmp",
    ".tox",
    ".venv",
    ".vscode",
    "__pycache__",
    "build",
    "cache",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "qa",
    "screenshots",
    "target",
    "temp",
    "tmp",
    "venv",
];
const READ_ONLY: &[&str] = &[
    "ask_user_question",
    "enter_plan_mode",
    "exit_plan_mode",
    "get_current_time",
    "glob",
    "grep",
    "list_mcp_resources",
    "read_file",
    "read_mcp_resource",
    "read_skill",
    "send_attachment",
    "structured_output",
    "task_list",
    "task_output",
    "task_stop",
    "task_wait",
    "todo_write",
    "web_fetch",
    "web_search",
];
const NO_MUTATION: &[&str] = &[
    "tool_not_found",
    "invalid_tool_input",
    "permission_denied",
    "permission_cancelled",
    "permission_required",
    "path_not_allowed",
    "file_not_found",
    "file_conflict",
    "unsupported_tool",
    "setup_required",
    "tool_unavailable",
    "plan_mode_violation",
    "ask_mode_violation",
];

pub(super) struct Collector {
    cwd: PathBuf,
    allowed: Vec<PathBuf>,
    baseline: BTreeMap<PathBuf, (u64, String)>,
    explicit: Vec<PathBuf>,
    mutation: bool,
    overlap: Arc<AtomicBool>,
}
fn resolve(root: &Path, path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in root.join(path).components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(part),
        }
    }
    out
}
fn fingerprint(path: &Path) -> io::Result<(u64, String)> {
    // Nonblocking open plus descriptor type check avoids hanging on a tool
    // replacing an explicit path with a FIFO between discovery and hashing.
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok((meta.len(), format!("{:x}", hash.finalize())))
}
fn hard_internal(relative: &Path) -> bool {
    relative.components().any(|p| {
        [".git", ".pilotdeck", "node_modules"]
            .contains(&p.as_os_str().to_string_lossy().to_lowercase().as_str())
    }) || [
        ".pilotdeck_build.js",
        ".pilotdeck_build.cjs",
        ".pilotdeck_build.mjs",
    ]
    .contains(
        &relative
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .as_str(),
    )
}
fn sensitive(name: &str) -> bool {
    if name == ".env.example" {
        return false;
    }
    name == ".env"
        || name
            .strip_prefix(".env.")
            .is_some_and(|suffix| !suffix.is_empty())
        || [
            "credential",
            "credentials",
            "secret",
            "secrets",
            "token",
            "tokens",
        ]
        .iter()
        .any(|p| {
            name == *p
                || name
                    .strip_prefix(p)
                    .is_some_and(|s| s.starts_with(['-', '_', '.']))
        })
        || ["id_dsa", "id_ecdsa", "id_ed25519", "id_rsa"]
            .iter()
            .any(|p| name == *p || name == format!("{p}.pub"))
        || [".key", ".p12", ".pfx", ".pem"]
            .iter()
            .any(|p| name.ends_with(p))
}
impl Collector {
    pub(super) fn start(cwd: PathBuf, allowed: &[PathBuf]) -> Self {
        let cwd = resolve(&std::env::current_dir().unwrap_or_default(), &cwd);
        let mut this = Self {
            allowed: allowed.iter().map(|p| resolve(&cwd, p)).collect(),
            cwd,
            baseline: BTreeMap::new(),
            explicit: vec![],
            mutation: false,
            overlap: Arc::new(AtomicBool::new(false)),
        };
        this.baseline = this.scan().into_iter().collect();
        let key = fs::canonicalize(&this.cwd).unwrap_or_else(|_| this.cwd.clone());
        let mut active = ACTIVE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        active.retain(|_, peers| {
            peers.retain(|p| p.strong_count() > 0);
            !peers.is_empty()
        });
        let peers = active.entry(key).or_default();
        for peer in peers.iter().filter_map(Weak::upgrade) {
            peer.store(true, Ordering::SeqCst);
            this.overlap.store(true, Ordering::SeqCst);
        }
        peers.push(Arc::downgrade(&this.overlap));
        this
    }
    fn allowed(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.cwd) else {
            return false;
        };
        if relative.as_os_str().is_empty()
            || relative.to_string_lossy().starts_with("..")
            || hard_internal(relative)
        {
            return false;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if sensitive(&name) {
            return false;
        }
        let internal = relative
            .components()
            .any(|p| EXCLUDED.contains(&p.as_os_str().to_string_lossy().to_lowercase().as_str()))
            || name == ".ds_store"
            || [
                ".log",
                ".pid",
                ".sock",
                ".db-shm",
                ".db-wal",
                ".sqlite-shm",
                ".sqlite-wal",
            ]
            .iter()
            .any(|p| name.ends_with(p));
        !internal || self.allowed.iter().any(|allowed| allowed == path)
    }
    fn scan(&self) -> Vec<(PathBuf, (u64, String))> {
        let mut out = vec![];
        let mut pending = vec![self.cwd.clone()];
        while let Some(path) = pending.pop() {
            let stat = if path == self.cwd {
                fs::metadata(&path)
            } else {
                fs::symlink_metadata(&path)
            };
            let Ok(meta) = stat else {
                continue;
            };
            if meta.is_dir() {
                if path != self.cwd
                    && EXCLUDED.contains(
                        &path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase()
                            .as_str(),
                    )
                {
                    continue;
                }
                if let Ok(entries) = fs::read_dir(&path) {
                    let children: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
                    pending.extend(children.into_iter().rev());
                }
            } else if meta.is_file()
                && self.allowed(&path)
                && let Ok(fp) = fingerprint(&path)
            {
                out.push((path, fp));
            }
        }
        for path in &self.allowed {
            if self.allowed(path)
                && let Ok(fp) = fingerprint(path)
            {
                if let Some(existing) = out.iter_mut().find(|(p, _)| p == path) {
                    existing.1 = fp;
                } else {
                    out.push((path.clone(), fp));
                }
            }
        }
        out
    }
    fn add(&mut self, value: &JsValue) {
        if let Some(path) = value.as_string().and_then(|s| s.to_utf8().ok()) {
            let path = resolve(&self.cwd, Path::new(&path));
            if self.allowed(&path) && !self.explicit.contains(&path) {
                self.explicit.push(path);
            }
        }
    }
    fn known_paths(&mut self, value: &JsValue, depth: usize) {
        if depth > 4 {
            return;
        }
        match value {
            JsValue::Array(items) => {
                for item in items {
                    self.known_paths(item, depth + 1);
                }
            }
            JsValue::Object(fields) => {
                for (key, value) in fields.entries() {
                    if ["filepath", "outputfile", "outputpath", "artifactpath"].contains(
                        &String::from_utf16_lossy(key.units())
                            .to_lowercase()
                            .as_str(),
                    ) && matches!(value, JsValue::String(_))
                    {
                        self.add(value);
                    } else {
                        self.known_paths(value, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
    pub(super) fn observe(&mut self, result: &JsValue) {
        let name = result
            .get("toolName")
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
            .unwrap_or_default();
        let success = result.get("type") == Some(&"success".into());
        let excluded = result
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(JsValue::as_string)
            .is_some_and(|s| NO_MUTATION.iter().any(|v| s.equals_str(v)));
        self.mutation |=
            !READ_ONLY.contains(&name.to_lowercase().as_str()) && (success || !excluded);
        if !success {
            return;
        }
        if let Some(JsValue::Array(items)) = result.get("content") {
            for item in items {
                if item.get("type") == Some(&"file".into())
                    && let Some(path) = item.get("path")
                {
                    self.add(path);
                }
            }
        }
        if ["write_file", "edit_file", "edit_notebook"].contains(&name.as_str())
            && let Some(data) = result.get("data")
        {
            self.known_paths(data, 0);
        }
    }
    pub(super) fn finish(
        self,
        status: &str,
        now: &dyn Fn() -> pilotdeck_protocol::JsString,
    ) -> Vec<JsValue> {
        let mut candidates: Vec<_> = self
            .explicit
            .iter()
            .map(|p| (p.clone(), "tool", None))
            .collect();
        if self.mutation && !self.overlap.load(Ordering::SeqCst) {
            for (path, fp) in self.scan() {
                if self
                    .baseline
                    .get(&path)
                    .is_none_or(|before| before.1 != fp.1)
                {
                    if let Some(existing) = candidates.iter_mut().find(|(p, _, _)| p == &path) {
                        existing.2 = Some(fp);
                    } else {
                        candidates.push((path, "workspace_diff", Some(fp)));
                    }
                }
            }
        }
        let mut artifacts = vec![];
        for (path, source, fingerprint_at_scan) in candidates {
            let Ok((size, hash)) = fingerprint_at_scan.map_or_else(|| fingerprint(&path), Ok)
            else {
                continue;
            };
            let before = self.baseline.get(&path);
            if before.is_some_and(|b| b.1 == hash) {
                continue;
            }
            let relative = path
                .strip_prefix(&self.cwd)
                .expect("collector contained path")
                .to_string_lossy()
                .replace('\\', "/");
            let id =
                format!("{:x}", Sha256::digest(format!("{relative}\0{hash}")))[..24].to_owned();
            let mut value = object([
                ("id", id.as_str().into()),
                (
                    "name",
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref()
                        .into(),
                ),
                ("path", relative.as_str().into()),
                (
                    "operation",
                    if before.is_some() {
                        "updated"
                    } else {
                        "created"
                    }
                    .into(),
                ),
                ("source", source.into()),
                ("status", status.into()),
                ("size", JsValue::Number(size as f64)),
                ("sha256", hash.as_str().into()),
            ]);
            let JsValue::Object(fields) = &mut value else {
                unreachable!()
            };
            if let Some(mime) = mime(&path) {
                fields.insert("mimeType".into(), mime.into());
            }
            fields.insert("createdAt".into(), JsValue::String(now()));
            artifacts.push(value);
        }
        let collator = crate::project_files::node_collator(&icu_locale_core::locale!("en-US"))
            .expect("built-in locale");
        artifacts.sort_by(|a, b| {
            crate::project_files::node_compare_utf16(
                &collator,
                a.get("path")
                    .and_then(JsValue::as_string)
                    .expect("artifact path"),
                b.get("path")
                    .and_then(JsValue::as_string)
                    .expect("artifact path"),
            )
        });
        artifacts
    }
}
fn mime(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_string_lossy().to_lowercase().as_str() {
            "csv" => "text/csv",
            "css" => "text/css",
            "doc" => "application/msword",
            "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "dps" | "ppt" => "application/vnd.ms-powerpoint",
            "et" | "xls" => "application/vnd.ms-excel",
            "gif" => "image/gif",
            "htm" | "html" => "text/html",
            "js" | "mjs" => "text/javascript",
            "jpeg" | "jpg" => "image/jpeg",
            "json" => "application/json",
            "md" => "text/markdown",
            "odt" => "application/vnd.oasis.opendocument.text",
            "ods" => "application/vnd.oasis.opendocument.spreadsheet",
            "odp" => "application/vnd.oasis.opendocument.presentation",
            "pdf" => "application/pdf",
            "png" => "image/png",
            "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "rtf" => "application/rtf",
            "svg" => "image/svg+xml",
            "tsv" => "text/tab-separated-values",
            "ts" | "tsx" | "txt" => "text/plain",
            "webp" => "image/webp",
            "wps" => "application/vnd.ms-works",
            "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xml" => "application/xml",
            "yaml" | "yml" => "application/yaml",
            "zip" => "application/zip",
            _ => return None,
        },
    )
}
