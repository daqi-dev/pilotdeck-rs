//! Native `/api/web/projects` and workspace file endpoints.
use crate::metadata::{Metadata, NativeFiles, resolve, resolve_path};
use base64::{Engine, engine::general_purpose::STANDARD};
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};
use url::Url;

const MAX_READ_BYTES: usize = 1024 * 1024;
const TEXT_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "json", "md", "mdx", "yml", "yaml", "txt", "css", "html", "py", "go",
    "rs", "java", "c", "cpp", "h", "hpp", "sh", "bash", "toml", "ini", "csv", "tsv", "env", "lock",
];

#[derive(Debug)]
struct WebError(StatusCode, &'static str, String);
type Result<T> = std::result::Result<T, WebError>;

impl WebError {
    fn response(self) -> Response<crate::Body> {
        super::json(
            self.0,
            json!({"error": {"code": self.1, "message": self.2}}),
        )
    }
}
fn internal(message: impl Into<String>) -> WebError {
    WebError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        message.into(),
    )
}
fn boundary(path: &str) -> WebError {
    WebError(
        StatusCode::FORBIDDEN,
        "workspace_boundary",
        format!("Path escapes workspace root: {path}"),
    )
}
fn file_error(error: io::Error, syscall: &str, path: &Path) -> WebError {
    if crate::project_files::nofollow_loop(&error) {
        return boundary(&path.to_string_lossy());
    }
    internal(
        crate::node_io::diagnostic(&error, syscall, Some(&path.to_string_lossy()))
            .map_or_else(|| error.to_string(), |(_, message)| message),
    )
}

enum Route {
    Browse,
    Projects,
    Project(String),
    Tree(String, String),
    Read(String, String),
    Write(String),
    NotFound,
}

fn decode_project(value: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut source = value.bytes();
    while let Some(byte) = source.next() {
        bytes.push(if byte == b'%' {
            let hi = source.next().and_then(|b| char::from(b).to_digit(16));
            let lo = source.next().and_then(|b| char::from(b).to_digit(16));
            match (hi, lo) {
                (Some(hi), Some(lo)) => (hi * 16 + lo) as u8,
                _ => return Err(internal("URI malformed")),
            }
        } else {
            byte
        });
    }
    String::from_utf8(bytes).map_err(|_| internal("URI malformed"))
}

fn route(method: &Method, url: &Url) -> Result<Route> {
    if url.path() == "/api/browse-filesystem" && *method == Method::GET {
        return Ok(Route::Browse);
    }
    if url.path() == "/api/web/projects" && *method == Method::GET {
        return Ok(Route::Projects);
    }
    let Some(rest) = url.path().strip_prefix("/api/web/projects/") else {
        return Ok(Route::NotFound);
    };
    let (key, subpath) = rest
        .split_once('/')
        .map_or((rest, ""), |(key, path)| (key, path));
    if key.is_empty() {
        return Ok(Route::NotFound);
    }
    if subpath.is_empty() && *method == Method::GET {
        return Ok(Route::Project(decode_project(key).map_err(|error| {
            WebError(StatusCode::NOT_FOUND, "project_not_found", error.2)
        })?));
    }
    if !rest.contains('/') {
        return Ok(Route::NotFound);
    }
    let key = decode_project(key)?;
    let path = || {
        url.query_pairs()
            .find(|(key, _)| key == "path")
            .map(|(_, value)| value.into_owned())
    };
    Ok(match (method, subpath) {
        (&Method::GET, "files/tree") => Route::Tree(key, path().unwrap_or_else(|| ".".into())),
        (&Method::GET, "files/read") => {
            let path = path().filter(|path| !path.is_empty()).ok_or_else(|| {
                WebError(
                    StatusCode::BAD_REQUEST,
                    "missing_path",
                    "?path required".into(),
                )
            })?;
            Route::Read(key, path)
        }
        (&Method::POST, "files/write") => Route::Write(key),
        _ => Route::NotFound,
    })
}

pub(super) async fn handle(
    request: Request<Incoming>,
    state: &super::HttpState,
) -> Response<crate::Body> {
    if !super::authorized(request.headers(), &state.token) {
        return WebError(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid token".into(),
        )
        .response();
    }
    let base = Url::parse("http://127.0.0.1").expect("static URL");
    let url = match Url::options()
        .base_url(Some(&base))
        .parse(&request.uri().to_string())
    {
        Ok(url) => url,
        Err(error) => return internal(error.to_string()).response(),
    };
    let route = match route(request.method(), &url) {
        Ok(route) => route,
        Err(error) => return error.response(),
    };
    let browse = matches!(&route, Route::Browse);
    let body = if matches!(&route, Route::Write(_)) {
        // httpRouter.ts:127-140 collects the complete JSON body without a cap.
        match super::read_body(request.into_body(), usize::MAX).await {
            Ok(body) => body,
            Err(response) => return *response,
        }
    } else {
        Vec::new()
    };
    let home = state.home.clone();
    let cwd = state.cwd.clone();
    let workspaces_root = state.workspaces_root.clone();
    match tokio::task::spawn_blocking(move || {
        execute(route, &url, home, cwd, workspaces_root, &body)
    })
    .await
    {
        Ok(Ok(value)) => super::json(StatusCode::OK, value),
        Ok(Err(error)) if browse => {
            let message = if error.0 == StatusCode::INTERNAL_SERVER_ERROR {
                "Failed to browse filesystem".into()
            } else {
                error.2
            };
            super::json(error.0, json!({"error": message}))
        }
        Ok(Err(error)) => error.response(),
        Err(error) => internal(error.to_string()).response(),
    }
}

fn execute(
    route: Route,
    url: &Url,
    home: Option<PathBuf>,
    cwd: PathBuf,
    workspaces_root: Option<PathBuf>,
    body: &[u8],
) -> Result<Value> {
    match route {
        Route::Browse => browse_filesystem(url, &cwd, workspaces_root.as_deref()),
        Route::Projects | Route::Project(_) => {
            let home = home.ok_or_else(|| match &route {
                Route::Project(_) => WebError(
                    StatusCode::NOT_FOUND,
                    "project_not_found",
                    "PILOT_HOME is required".into(),
                ),
                _ => internal("PILOT_HOME is required"),
            })?;
            let metadata = Metadata {
                home,
                cwd,
                files: NativeFiles,
            };
            let result = match route {
                Route::Projects => metadata
                    .list_projects()
                    .map_err(|error| internal(error.to_string()))?,
                Route::Project(key) => metadata
                    .describe_project(&JsString::from(key.as_str()))
                    .map_err(|error| internal(error.to_string()))?,
                _ => unreachable!(),
            };
            let text = stringify_json(&result)
                .map_err(|error| internal(error.to_string()))?
                .ok_or_else(|| internal("Undefined project response"))?;
            serde_json::from_str(&text).map_err(|error| internal(error.to_string()))
        }
        Route::Tree(key, path) => WebFiles::new(&cwd, &key).tree(&path),
        Route::Read(key, path) => WebFiles::new(&cwd, &key).read(&path),
        Route::Write(key) => {
            let (path, bytes) = write_body(body)?;
            WebFiles::new(&cwd, &key).write(&path, &bytes)?;
            Ok(json!({"ok": true}))
        }
        Route::NotFound => Err(WebError(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("No /api/web route for {}", url.path()),
        )),
    }
}

fn expand_workspace_path(input: &str, root: &Path) -> PathBuf {
    if input == "~" {
        root.to_path_buf()
    } else if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        root.join(rest)
    } else {
        PathBuf::from(input)
    }
}

fn browse_filesystem(url: &Url, cwd: &Path, configured_root: Option<&Path>) -> Result<Value> {
    let root = configured_root.unwrap_or(cwd);
    let requested = url
        .query_pairs()
        .find(|(key, _)| key == "path")
        .map(|(_, value)| value.into_owned());
    let target = requested
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(|path| expand_workspace_path(path, root))
        .unwrap_or_else(|| root.to_path_buf());
    let target = resolve_path(cwd, &target);
    let stat = std::fs::metadata(&target).map_err(|_| {
        WebError(
            StatusCode::NOT_FOUND,
            "directory_not_accessible",
            "Directory not accessible".into(),
        )
    })?;
    if !stat.is_dir() {
        return Err(WebError(
            StatusCode::BAD_REQUEST,
            "not_a_directory",
            "Path is not a directory".into(),
        ));
    }
    let show_hidden = url
        .query_pairs()
        .find(|(key, _)| key == "showHidden")
        .is_some_and(|(_, value)| value == "true");
    let mut suggestions = Vec::new();
    for entry in
        std::fs::read_dir(&target).map_err(|error| file_error(error, "scandir", &target))?
    {
        let entry = entry.map_err(|error| file_error(error, "scandir", &target))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if std::fs::metadata(&path).is_ok_and(|stat| stat.is_dir()) {
            suggestions.push(json!({"name": name, "path": path, "type": "directory"}));
        }
    }
    let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
    let mut preferences: icu_collator::CollatorPreferences = locale.into();
    preferences.numeric_ordering = Some(icu_collator::preferences::CollationNumericOrdering::True);
    let collator = icu_collator::Collator::try_new(preferences, Default::default())
        .map_err(|error| internal(error.to_string()))?;
    suggestions.sort_by(|left, right| {
        collator.compare(
            left["name"].as_str().unwrap_or_default(),
            right["name"].as_str().unwrap_or_default(),
        )
    });
    let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if target == resolved_root {
        const COMMON: &[&str] = &[
            "Desktop",
            "Documents",
            "Projects",
            "Development",
            "Dev",
            "Code",
            "workspace",
        ];
        suggestions.sort_by_key(|entry| {
            (!COMMON.contains(&entry["name"].as_str().unwrap_or_default())) as u8
        });
    }
    Ok(json!({"path": target, "suggestions": suggestions}))
}

fn write_body(body: &[u8]) -> Result<(String, Vec<u8>)> {
    let invalid = || {
        WebError(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "Expected { path, content[, encoding] }".into(),
        )
    };
    let body = parse_json(&String::from_utf8_lossy(body)).map_err(|_| invalid())?;
    let path = body
        .get("path")
        .and_then(JsValue::as_string)
        .filter(|path| !path.is_empty())
        .ok_or_else(invalid)?;
    let content = body
        .get("content")
        .and_then(JsValue::as_string)
        .ok_or_else(invalid)?;
    let encoding = body.get("encoding");
    let bytes = match encoding {
        None => String::from_utf16_lossy(content.units()).into_bytes(),
        Some(JsValue::String(value)) if value == &JsString::from("utf8") => {
            String::from_utf16_lossy(content.units()).into_bytes()
        }
        Some(JsValue::String(value)) if value == &JsString::from("base64") => {
            // Buffer.from(base64) skips junk, accepts base64url and discards a trailing sextet.
            let mut bytes = Vec::new();
            let mut bits = 0u32;
            let mut count = 0;
            for unit in content.units() {
                let value = match *unit as u8 {
                    b'A'..=b'Z' => (*unit as u8 - b'A') as u32,
                    b'a'..=b'z' => (*unit as u8 - b'a' + 26) as u32,
                    b'0'..=b'9' => (*unit as u8 - b'0' + 52) as u32,
                    b'+' | b'-' => 62,
                    b'/' | b'_' => 63,
                    b'=' => break,
                    _ => continue,
                };
                bits = (bits << 6) | value;
                count += 6;
                if count >= 8 {
                    count -= 8;
                    bytes.push((bits >> count) as u8);
                }
            }
            bytes
        }
        _ => return Err(invalid()),
    };
    Ok((String::from_utf16_lossy(path.units()), bytes))
}

struct WebFiles {
    root: PathBuf,
}
impl WebFiles {
    fn new(cwd: &Path, key: &str) -> Self {
        let root = resolve(cwd, key);
        Self {
            root: std::fs::canonicalize(&root).unwrap_or(root),
        }
    }
    fn relative(&self, path: &Path) -> String {
        let relative = path
            .strip_prefix(&self.root)
            .expect("checked workspace path")
            .to_string_lossy()
            .replace(char::from(92), "/");
        if relative.is_empty() {
            ".".into()
        } else {
            relative
        }
    }
    fn safe(&self, path: &str) -> Result<PathBuf> {
        if Path::new(path).is_absolute() {
            return Err(boundary(path));
        }
        let mut normalized: Vec<&std::ffi::OsStr> = Vec::new();
        for component in Path::new(path).components() {
            match component {
                Component::ParentDir if normalized.last().is_some_and(|name| *name != "..") => {
                    normalized.pop();
                }
                Component::ParentDir => normalized.push(std::ffi::OsStr::new("..")),
                Component::Normal(name) => normalized.push(name),
                _ => {}
            }
        }
        if normalized
            .first()
            .is_some_and(|name| name.to_string_lossy().starts_with(".."))
        {
            return Err(boundary(path));
        }
        let target = resolve(&self.root, path);
        let relative = target
            .strip_prefix(&self.root)
            .map_err(|_| boundary(path))?;
        if relative.to_string_lossy().starts_with("..") {
            return Err(boundary(path));
        }
        let mut ancestor = target.as_path();
        loop {
            match std::fs::canonicalize(ancestor) {
                Ok(real) => {
                    // A missing project root is created by the descriptor walk in write().
                    if ancestor.starts_with(&self.root) {
                        if !real.starts_with(&self.root) {
                            return Err(boundary(path));
                        }
                        return Ok(resolve_path(
                            &real,
                            target.strip_prefix(ancestor).expect("ancestor"),
                        ));
                    }
                    return Ok(target);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .ok_or_else(|| file_error(error, "realpath", &target))?;
                }
                Err(error) => return Err(file_error(error, "realpath", &target)),
            }
        }
    }
    fn read(&self, path: &str) -> Result<Value> {
        let target = self.safe(path)?;
        let mut file =
            crate::project_files::open_beneath(&self.root, &target).map_err(|error| {
                let syscall = if error.kind() == io::ErrorKind::NotFound {
                    "stat"
                } else {
                    "open"
                };
                file_error(error, syscall, &target)
            })?;
        let stat = file
            .metadata()
            .map_err(|error| file_error(error, "stat", &target))?;
        if !stat.is_file() {
            return Err(internal(format!("Not a file: {path}")));
        }
        let text = TEXT_EXTENSIONS.contains(
            &path
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_lowercase()
                .as_str(),
        );
        let mut result = json!({
            "projectKey": self.root, "path": self.relative(&target),
            "size": stat.len(), "encoding": if text { "utf8" } else { "base64" },
        });
        if stat.len() > MAX_READ_BYTES as u64 {
            result["binary"] = json!(!text);
            return Ok(result);
        }
        let bytes = crate::project_files::read_prefix(&mut file, MAX_READ_BYTES + 1)
            .map_err(|error| file_error(error, "read", &target))?;
        if bytes.len() > MAX_READ_BYTES {
            result["size"] = json!(
                file.metadata()
                    .map_err(|error| file_error(error, "stat", &target))?
                    .len()
            );
            result["binary"] = json!(!text);
        } else if text {
            result["content"] = json!(String::from_utf8_lossy(&bytes));
        } else {
            result["content"] = json!(STANDARD.encode(bytes));
            result["binary"] = json!(true);
        }
        Ok(result)
    }
    fn tree(&self, path: &str) -> Result<Value> {
        let target = self.safe(path)?;
        let file = crate::project_files::open_beneath(&self.root, &target).map_err(|error| {
            let syscall = if error.kind() == io::ErrorKind::NotFound {
                "stat"
            } else {
                "open"
            };
            file_error(error, syscall, &target)
        })?;
        if !file
            .metadata()
            .map_err(|error| file_error(error, "stat", &target))?
            .is_dir()
        {
            return Err(internal(format!("Not a directory: {path}")));
        }
        let mut entries = Vec::new();
        for entry in rustix::fs::Dir::read_from(&file)
            .map_err(|error| file_error(error.into(), "scandir", &target))?
        {
            let entry = entry.map_err(|error| file_error(error.into(), "scandir", &target))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let kind = match entry.file_type() {
                rustix::fs::FileType::Directory => "directory",
                rustix::fs::FileType::RegularFile => "file",
                rustix::fs::FileType::Symlink => "symlink",
                _ => "other",
            };
            let child = target.join(&name);
            let mut entry = json!({"name": name, "path": self.relative(&child), "type": kind});
            // ProjectFileService lists the link itself while stat follows its
            // target for size/mtime, including targets outside the workspace.
            // Content reads still use the checked descriptor boundary.
            if let Ok(stat) = crate::metadata::Files::stat(&NativeFiles, &child) {
                entry["size"] = json!(stat.size);
                entry["modifiedAt"] = json!(crate::runtime::context::prompt::iso_timestamp(
                    stat.mtime_ms
                ));
            }
            entries.push(entry);
        }
        entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
        let collator = crate::project_files::node_collator(&locale)
            .map_err(|(_, message)| internal(message))?;
        entries.sort_by(|left, right| {
            (right["type"] == "directory")
                .cmp(&(left["type"] == "directory"))
                .then_with(|| {
                    crate::project_files::node_compare_utf16(
                        &collator,
                        &JsString::from(left["name"].as_str().expect("entry name")),
                        &JsString::from(right["name"].as_str().expect("entry name")),
                    )
                })
        });
        Ok(json!({"projectKey": self.root, "entries": entries}))
    }
    fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        use rustix::fs::{AtFlags, Mode, OFlags, mkdirat, openat, renameat, unlinkat};
        let target = self.safe(path)?;
        let parent = target
            .parent()
            .ok_or_else(|| internal(format!("Not a file: {path}")))?;
        let mut directory = File::open("/").map_err(|error| file_error(error, "open", parent))?;
        let mut current = PathBuf::from("/");
        for component in parent.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            current.push(name);
            #[cfg(target_os = "linux")]
            let access = OFlags::PATH;
            #[cfg(target_vendor = "apple")]
            let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
            #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
            let access = OFlags::RDONLY;
            let flags = access | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let opened = match openat(&directory, name, flags, Mode::empty()) {
                Err(rustix::io::Errno::NOENT) => {
                    if !current.starts_with(&self.root) {
                        return Err(file_error(
                            io::Error::from_raw_os_error(libc::ENOENT),
                            "open",
                            &current,
                        ));
                    }
                    match mkdirat(&directory, name, Mode::from_bits_truncate(0o777)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(file_error(error.into(), "mkdir", &current)),
                    }
                    openat(&directory, name, flags, Mode::empty())
                }
                result => result,
            };
            directory = File::from(opened.map_err(|error| {
                if std::fs::symlink_metadata(&current)
                    .is_ok_and(|stat| stat.file_type().is_symlink())
                {
                    boundary(path)
                } else {
                    file_error(error.into(), "open", &current)
                }
            })?);
        }
        let name = target
            .file_name()
            .ok_or_else(|| internal(format!("Not a file: {path}")))?;
        let flags = OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let permissions = match openat(&directory, name, flags, Mode::empty()) {
            Ok(fd) => {
                let stat = File::from(fd)
                    .metadata()
                    .map_err(|error| file_error(error, "stat", &target))?;
                if !stat.is_file() {
                    return Err(internal(format!("Not a file: {path}")));
                }
                Some(stat.permissions())
            }
            Err(rustix::io::Errno::NOENT) => None,
            Err(rustix::io::Errno::LOOP) => return Err(boundary(path)),
            Err(error) => return Err(file_error(error.into(), "open", &target)),
        };
        let temporary = format!(".pdrs-write-{}", uuid::Uuid::new_v4());
        let mut file = File::from(
            openat(
                &directory,
                temporary.as_str(),
                flags | OFlags::CREATE | OFlags::EXCL,
                Mode::from_bits_truncate(0o666),
            )
            .map_err(|error| file_error(error.into(), "open", &target))?,
        );
        // Commit relative to the pinned parent; failed writes leave the original intact.
        let result = (|| {
            file.write_all(bytes)
                .map_err(|error| file_error(error, "write", &target))?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)
                    .map_err(|error| file_error(error, "chmod", &target))?;
            }
            file.sync_all()
                .map_err(|error| file_error(error, "fsync", &target))?;
            renameat(&directory, temporary.as_str(), &directory, name)
                .map_err(|error| file_error(error.into(), "rename", &target))
        })();
        if result.is_err() {
            let _ = unlinkat(&directory, temporary.as_str(), AtFlags::empty());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{header, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::{convert::Infallible, sync::Arc, time::Duration};

    fn project() -> (crate::scratch::Scratch, WebFiles) {
        let scratch = crate::scratch::Scratch::new("http-web");
        let root = scratch.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let files = WebFiles::new(scratch.path(), &root.to_string_lossy());
        (scratch, files)
    }
    fn url(path: &str) -> Url {
        Url::parse(&format!("http://localhost{path}")).unwrap()
    }
    fn project_url(root: &Path, suffix: &str) -> String {
        let key: String =
            url::form_urlencoded::byte_serialize(root.to_string_lossy().as_bytes()).collect();
        format!("/api/web/projects/{key}{suffix}")
    }
    fn state(home: Option<PathBuf>, cwd: PathBuf) -> Arc<super::super::HttpState> {
        Arc::new(super::super::HttpState::new("test-token".into(), home, cwd))
    }
    fn state_with_workspaces_root(cwd: PathBuf, root: PathBuf) -> Arc<super::super::HttpState> {
        let mut state = super::super::HttpState::new("test-token".into(), None, cwd);
        state.workspaces_root = Some(root);
        Arc::new(state)
    }
    async fn request(
        state: Arc<super::super::HttpState>,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> (StatusCode, Value) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (server_io, client_io) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(server_io),
                        service_fn(move |request| {
                            let state = state.clone();
                            async move { Ok::<_, Infallible>(handle(request, &state).await) }
                        }),
                    )
                    .await
                    .unwrap();
            });
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                    .await
                    .unwrap();
            let client = tokio::spawn(connection);
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "localhost");
            if let Some(token) = token {
                request = request.header(header::AUTHORIZATION, token);
            }
            let response = sender
                .send_request(
                    request
                        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let value = serde_json::from_slice(&bytes).unwrap();
            drop(sender);
            client.await.unwrap().unwrap();
            server.await.unwrap();
            (status, value)
        })
        .await
        .expect("web HTTP request timeout")
    }

    #[test]
    fn routes_decode_keys_query_once_and_preserve_method_rules() {
        assert!(matches!(
            route(&Method::GET, &url("/api/browse-filesystem")).unwrap(),
            Route::Browse
        ));
        let path = "/api/web/projects/%2Fproject%2Bone/files/read?path=one+two%2B.txt&path=ignored";
        match route(&Method::GET, &url(path)).unwrap() {
            Route::Read(key, path) => {
                assert_eq!(key, "/project+one");
                assert_eq!(path, "one two+.txt");
            }
            _ => panic!("file read route"),
        }
        assert!(matches!(
            route(&Method::GET, &url("/api/web/projects/p/")).unwrap(),
            Route::Project(_)
        ));
        assert!(
            matches!(route(&Method::GET, &url("/api/web/projects/p/files/tree")).unwrap(), Route::Tree(_, path) if path == ".")
        );
        assert!(
            matches!(route(&Method::GET, &url("/api/web/projects/p/files/tree?path=")).unwrap(), Route::Tree(_, path) if path.is_empty())
        );
        for (method, path) in [
            (Method::POST, "/api/web/projects"),
            (Method::GET, "/api/web/projects/"),
            (Method::GET, "/api/web/projects/p//"),
            (Method::POST, "/api/web/projects/p/files/read"),
        ] {
            assert!(matches!(
                route(&method, &url(path)).unwrap(),
                Route::NotFound
            ));
        }
        for path in [
            "/api/web/projects/p/files/read",
            "/api/web/projects/p/files/read?path=",
        ] {
            let error = route(&Method::GET, &url(path)).err().unwrap();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
            assert_eq!(error.1, "missing_path");
        }
        for key in ["%", "%GG", "%C0%AF", "%ED%A0%80"] {
            assert!(decode_project(key).is_err());
        }
        assert_eq!(decode_project("%252F+x").unwrap(), "%2F+x");
    }

    #[tokio::test]
    async fn browse_filesystem_consumes_workspaces_root_and_expands_tilde() {
        let scratch = crate::scratch::Scratch::new("http-browse");
        let root = scratch.path().join("configured-workspaces");
        for directory in [
            "Desktop",
            "dir10",
            "dir2",
            ".hidden",
            "nested",
            "nested/.inside",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(root.join("file.txt"), b"not a directory").unwrap();
        let state = state_with_workspaces_root(scratch.path().to_path_buf(), root.clone());

        let (status, body) = request(
            state.clone(),
            Method::GET,
            "/api/browse-filesystem",
            Some("Bearer test-token"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["path"], root.to_string_lossy().as_ref());
        let names: Vec<_> = body["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names[0], "Desktop");
        assert!(
            names.iter().position(|name| *name == "dir2").unwrap()
                < names.iter().position(|name| *name == "dir10").unwrap()
        );
        assert!(!names.contains(&".hidden"));
        assert!(!names.contains(&"file.txt"));

        let (status, body) = request(
            state.clone(),
            Method::GET,
            "/api/browse-filesystem?path=~%2Fnested&showHidden=true",
            Some("Bearer test-token"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["path"], root.join("nested").to_string_lossy().as_ref());
        assert_eq!(body["suggestions"][0]["name"], ".inside");

        for (path, expected, message) in [
            (
                "/api/browse-filesystem?path=~%2Ffile.txt",
                StatusCode::BAD_REQUEST,
                "Path is not a directory",
            ),
            (
                "/api/browse-filesystem?path=~%2Fmissing",
                StatusCode::NOT_FOUND,
                "Directory not accessible",
            ),
        ] {
            let (status, body) = request(
                state.clone(),
                Method::GET,
                path,
                Some("Bearer test-token"),
                "",
            )
            .await;
            assert_eq!(status, expected);
            assert_eq!(body["error"], message);
        }
    }

    #[test]
    fn write_validation_and_node_base64_decoding() {
        for body in [
            "",
            "{",
            "null",
            "[]",
            r#"{"path":"","content":"x"}"#,
            r#"{"path":"x","content":42}"#,
            r#"{"path":"x","content":"x","encoding":null}"#,
            r#"{"path":"x","content":"x","encoding":"hex"}"#,
        ] {
            let error = write_body(body.as_bytes()).unwrap_err();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
            assert_eq!(error.1, "invalid_body");
        }
        assert_eq!(
            write_body(br#"{"path":"x","content":"\ud800"}"#).unwrap().1,
            "�".as_bytes()
        );
        for (input, expected) in [
            ("aG Vs!!bG8=ignored", b"hello".as_slice()),
            ("-_8", &[251, 255]),
            ("a", &[]),
            ("Yf", b"a"),
        ] {
            let body =
                json!({"path": "binary", "content": input, "encoding": "base64"}).to_string();
            assert_eq!(write_body(body.as_bytes()).unwrap().1, expected);
        }
    }

    #[tokio::test]
    async fn http_write_accepts_json_larger_than_sixteen_mib_like_ts() {
        let (scratch, files) = project();
        let content = "x".repeat(16 * 1024 * 1024 + 1);
        let payload =
            serde_json::to_string(&json!({"path":"large.txt","content":content})).unwrap();
        let (status, body) = request(
            state(None, scratch.path().to_path_buf()),
            Method::POST,
            &project_url(&files.root, "/files/write"),
            Some("Bearer test-token"),
            &payload,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok":true}));
        assert_eq!(
            std::fs::read(files.root.join("large.txt")).unwrap(),
            content.as_bytes()
        );
        let metadata = files.read("large.txt").unwrap();
        assert_eq!(metadata["size"], content.len());
        assert!(
            metadata.get("content").is_none(),
            "TS retains the one MiB read cap"
        );
    }

    #[test]
    fn files_roundtrip_binary_tree_and_metadata_only_reads() {
        let (_scratch, files) = project();
        files.write("nested/readme.MD", "你好".as_bytes()).unwrap();
        let result = files.read("nested/readme.MD").unwrap();
        assert_eq!(result["path"], "nested/readme.MD");
        assert_eq!(result["content"], "你好");
        assert_eq!(result["encoding"], "utf8");
        assert!(result.get("binary").is_none());
        files.write("payload.bin", &[0, 255]).unwrap();
        assert_eq!(files.read("payload.bin").unwrap()["content"], "AP8=");
        files.write("nested/readme.MD", b"short").unwrap();
        assert_eq!(
            std::fs::read(files.root.join("nested/readme.MD")).unwrap(),
            b"short"
        );
        for name in ["large.txt", "large.bin"] {
            files.write(name, &vec![b'x'; MAX_READ_BYTES + 1]).unwrap();
            let result = files.read(name).unwrap();
            assert!(result.get("content").is_none());
            assert_eq!(result["binary"], name.ends_with("bin"));
        }
        let tree = files.tree(".").unwrap();
        assert_eq!(tree["projectKey"], files.root.to_string_lossy().as_ref());
        assert_eq!(tree["entries"][0]["type"], "directory");
        assert_eq!(tree["entries"][0]["name"], "nested");
        assert!(
            tree["entries"][0]["modifiedAt"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        assert!(std::fs::read_dir(&files.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pdrs-write-")
        }));
    }

    #[test]
    fn path_boundaries_reject_absolute_parent_and_prefix_paths() {
        let (_scratch, files) = project();
        for path in [
            "/etc/passwd",
            "../escape",
            "a/../../escape",
            "..notes",
            "..notes/x",
        ] {
            let error = files.safe(path).unwrap_err();
            assert_eq!(error.0, StatusCode::FORBIDDEN);
            assert_eq!(error.1, "workspace_boundary");
        }
        assert_eq!(files.safe("a/../ok").unwrap(), files.root.join("ok"));
        assert_eq!(
            files.read("missing.txt").unwrap_err().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        files.write("file.txt", b"original").unwrap();
        assert_eq!(
            files.tree("file.txt").unwrap_err().2,
            "Not a directory: file.txt"
        );
        assert_eq!(files.read(".").unwrap_err().2, "Not a file: .");
        assert!(files.write("file.txt/child", b"replacement").is_err());
        assert_eq!(
            std::fs::read(files.root.join("file.txt")).unwrap(),
            b"original"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tree_keeps_target_metadata_for_outside_links_and_omits_broken_targets() {
        use std::os::unix::fs::symlink;
        let (scratch, files) = project();
        let outside = scratch.path().join("outside.txt");
        std::fs::write(&outside, b"outside metadata").unwrap();
        symlink(&outside, files.root.join("outside-link")).unwrap();
        symlink(
            scratch.path().join("missing"),
            files.root.join("broken-link"),
        )
        .unwrap();
        let tree = files.tree(".").unwrap();
        let rows = tree["entries"].as_array().unwrap();
        let outside_row = rows
            .iter()
            .find(|row| row["name"] == "outside-link")
            .unwrap();
        let target = crate::metadata::Files::stat(&NativeFiles, &outside).unwrap();
        assert_eq!(outside_row["type"], "symlink");
        assert_eq!(outside_row["path"], "outside-link");
        assert_eq!(outside_row["size"], target.size);
        assert_eq!(
            outside_row["modifiedAt"],
            crate::runtime::context::prompt::iso_timestamp(target.mtime_ms)
        );
        let broken = rows
            .iter()
            .find(|row| row["name"] == "broken-link")
            .unwrap();
        assert_eq!(broken["type"], "symlink");
        assert!(broken.get("size").is_none());
        assert!(broken.get("modifiedAt").is_none());
        assert_eq!(
            files.read("outside-link").unwrap_err().1,
            "workspace_boundary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_read_write_boundaries_and_in_root_aliases() {
        use std::os::unix::fs::symlink;
        let (scratch, files) = project();
        let outside = scratch.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel.txt"), b"untouched").unwrap();
        symlink(&outside, files.root.join("escape")).unwrap();
        symlink(outside.join("sentinel.txt"), files.root.join("escape.txt")).unwrap();
        for path in ["escape/sentinel.txt", "escape.txt", "escape/new/deep.txt"] {
            assert_eq!(files.read(path).unwrap_err().0, StatusCode::FORBIDDEN);
            assert_eq!(
                files.write(path, b"changed").unwrap_err().0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            std::fs::read(outside.join("sentinel.txt")).unwrap(),
            b"untouched"
        );
        assert!(!outside.join("new").exists());
        files.write("real.txt", b"old").unwrap();
        symlink("real.txt", files.root.join("alias.txt")).unwrap();
        files.write("alias.txt", b"new").unwrap();
        assert_eq!(files.read("alias.txt").unwrap()["path"], "real.txt");
        assert_eq!(std::fs::read(files.root.join("real.txt")).unwrap(), b"new");
        assert!(
            std::fs::symlink_metadata(files.root.join("alias.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let tree = files.tree(".").unwrap();
        let escape = tree["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "escape.txt")
            .unwrap();
        assert_eq!(escape["type"], "symlink");
        assert_eq!(escape["size"], 9);
        assert!(escape["modifiedAt"].is_string());
    }

    #[tokio::test]
    async fn http_auth_errors_and_successful_file_requests() {
        let (_scratch, files) = project();
        let state = state(None, files.root.clone());
        let path = project_url(&files.root, "/files/read?path=readme.txt");
        for token in [None, Some("Bearer wrong")] {
            let (status, body) = request(state.clone(), Method::GET, &path, token, "").await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "unauthorized");
        }
        let path = project_url(&files.root, "/files/write");
        let (status, body) = request(
            state.clone(),
            Method::POST,
            &path,
            Some("Bearer test-token"),
            r#"{"path":"readme.txt","content":"from HTTP"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok": true}));
        let path = project_url(&files.root, "/files/read?path=readme.txt");
        let (status, body) = request(
            state.clone(),
            Method::GET,
            &path,
            Some("bEaReR test-token"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["content"], "from HTTP");
        for (method, suffix, payload, expected, code) in [
            (
                Method::GET,
                "/files/read",
                "",
                StatusCode::BAD_REQUEST,
                "missing_path",
            ),
            (
                Method::POST,
                "/files/write",
                "{}",
                StatusCode::BAD_REQUEST,
                "invalid_body",
            ),
            (
                Method::GET,
                "/files/read?path=..%2Fescape",
                "",
                StatusCode::FORBIDDEN,
                "workspace_boundary",
            ),
            (
                Method::GET,
                "/files/read?path=absent.txt",
                "",
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
            (
                Method::GET,
                "/unknown",
                "",
                StatusCode::NOT_FOUND,
                "not_found",
            ),
        ] {
            let (status, body) = request(
                state.clone(),
                method,
                &project_url(&files.root, suffix),
                Some("Bearer test-token"),
                payload,
            )
            .await;
            assert_eq!(status, expected);
            assert_eq!(body["error"]["code"], code);
        }
    }

    #[tokio::test]
    async fn http_projects_use_native_registered_metadata() {
        let (scratch, files) = project();
        let home = scratch.path().join("home");
        let registration = home
            .join("projects")
            .join(crate::metadata::project_id(&files.root));
        std::fs::create_dir_all(&registration).unwrap();
        std::fs::write(
            registration.join(".cwd"),
            files.root.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let state = state(Some(home), files.root.clone());
        let (status, body) = request(
            state.clone(),
            Method::GET,
            "/api/web/projects",
            Some("Bearer test-token"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["projects"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["projects"][0]["projectKey"],
            files.root.to_string_lossy().as_ref()
        );
        let (status, body) = request(
            state,
            Method::GET,
            &project_url(&files.root, "/"),
            Some("Bearer test-token"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["fullPath"], files.root.to_string_lossy().as_ref());
        assert_eq!(body["sessionCount"], 0);
    }

    #[test]
    fn malformed_project_key_and_missing_files_keep_http_errors() {
        let error = match route(&Method::GET, &url("/api/web/projects/%ZZ")) {
            Err(error) => error,
            _ => panic!("malformed URI accepted"),
        };
        assert_eq!(error.0, StatusCode::NOT_FOUND);
        assert_eq!(error.1, "project_not_found");
        let (_scratch, files) = project();
        for tree in [false, true] {
            let result = if tree {
                files.tree("missing")
            } else {
                files.read("missing")
            };
            let error = match result {
                Err(error) => error,
                _ => panic!("missing file accepted"),
            };
            assert!(error.2.contains("stat"), "{}", error.2);
            assert!(!error.2.contains("open '"), "{}", error.2);
        }
    }
}
