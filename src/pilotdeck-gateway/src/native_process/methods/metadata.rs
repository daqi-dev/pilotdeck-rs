use super::super::requests::Requests;
use crate::{
    metadata::{Metadata, NativeFiles, object},
    runtime::connection::{Call, RequestError},
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = crate::dispatch::LOCAL_METHODS;
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::metadata_request);

fn field(value: &JsValue, name: &str) -> JsValue {
    value.get(name).cloned().unwrap_or(JsValue::Undefined)
}
impl Requests {
    pub(in crate::native_process) fn metadata_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            if method == "session_model_get" {
                return session_model_get(self.options.home.clone(), self.env.clone(), params)
                    .await;
            }
            if [
                "list_projects",
                "describe_project",
                "project_files_list",
                "project_file_read",
            ]
            .contains(&method.as_str())
            {
                return project_metadata(
                    self.options.home.clone(),
                    self.env.clone(),
                    method,
                    params,
                )
                .await;
            }
            if method == "model_catalog_list" {
                return model_catalog(self.options.home.clone(), self.env.clone(), params).await;
            }
            let metadata = Metadata {
                home: self.options.home.clone(),
                cwd: std::path::PathBuf::new(),
                files: NativeFiles,
            };
            let request = object([
                ("type", "request".into()),
                ("id", "native".into()),
                ("method", JsValue::String(method.clone().into())),
                ("params", params),
            ]);
            match crate::dispatch::route_with_environment(
                &request,
                Some(&metadata),
                Some(&self.env),
            ) {
                crate::dispatch::Route::LocalResponse(bytes) => {
                    let response = crate::dispatch::parse(&bytes)
                        .ok_or_else(|| RequestError::new("Native method returned invalid JSON."))?;
                    if response.get("ok") == Some(&JsValue::Bool(true)) {
                        Ok(field(&response, "result"))
                    } else {
                        let error = field(&response, "error");
                        Err(RequestError {
                            code: error
                                .get("code")
                                .and_then(JsValue::as_string)
                                .and_then(|s| s.to_utf8().ok())
                                .unwrap_or_else(|| "gateway_request_failed".into()),
                            message: error
                                .get("message")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_else(|| "Native request failed.".into()),
                            details: error.get("details").cloned().map(Box::new),
                            validation: None,
                        })
                    }
                }
                crate::dispatch::Route::Forward => unavailable(method),
            }
        })
    }
}

pub(super) fn unavailable(method: String) -> Result<JsValue, RequestError> {
    Err(RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: format!("Native gateway method {method} is not configured.").into(),
        details: None,
        validation: None,
    })
}

/// The legacy dispatcher uses synchronous native files. The native gateway's
/// session-model read is public and must not open a FIFO or unbounded file on
/// the single-threaded host; preserve the original dispatcher for all other
/// methods and use the same model projector with a separately bounded config.
async fn session_model_get(
    home: std::path::PathBuf,
    env: crate::config::Env,
    params: JsValue,
) -> Result<JsValue, RequestError> {
    use std::sync::{Arc, LazyLock};
    static SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
        LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    let permit = SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RequestError::new("Session model reader is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let metadata = Metadata {
            home: home.clone(),
            cwd: std::path::PathBuf::new(),
            files: crate::runtime::history::HistoryMarkerFiles::for_model_selection(),
        };
        let result =
            crate::session_model::execute_with_config(&metadata, &params, &env, |project| {
                crate::config::load_project_bounded_lazy(&home, &metadata.cwd, &env, project)
            });
        // Refusals from marker discovery are often otherwise interpreted as
        // an absent registration. Never return a successful wrong selection.
        if metadata.files.was_refused() {
            return Err(RequestError::new(
                "Unsafe project marker or session transcript.",
            ));
        }
        result.map_err(|error| {
            if error.requires_host {
                return RequestError {
                    code: "CAPABILITY_UNAVAILABLE".into(),
                    message: "Native gateway method session_model_get is not configured.".into(),
                    details: None,
                    validation: None,
                };
            }
            RequestError {
                code: error.code,
                message: error.message,
                details: None,
                validation: None,
            }
        })
    })
    .await
    .map_err(|_| RequestError::new("Session model reader stopped unexpectedly."))?
}

/// Project summaries read untrusted per-project marker and transcript bytes.
/// Count directory entries and bytes across the whole request; callers whose
/// legacy algorithms swallow IO errors must still observe resource refusal.
struct BoundedMetadataFiles {
    inner: crate::runtime::history::HistoryMarkerFiles,
    entries: std::cell::Cell<usize>,
    directories:
        std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, Vec<(String, bool)>>>,
    summary_bytes: std::cell::Cell<usize>,
    bytes: std::cell::Cell<usize>,
    refused: std::cell::Cell<bool>,
}
impl BoundedMetadataFiles {
    const ENTRY_LIMIT: usize = 16_384;
    const BYTE_LIMIT: usize = 16 * 1024 * 1024;
    fn new() -> Self {
        Self {
            inner: crate::runtime::history::HistoryMarkerFiles::for_model_selection(),
            entries: std::cell::Cell::new(0),
            directories: std::cell::RefCell::new(std::collections::HashMap::new()),
            summary_bytes: std::cell::Cell::new(0),
            bytes: std::cell::Cell::new(0),
            refused: std::cell::Cell::new(false),
        }
    }
    fn was_refused(&self) -> bool {
        self.refused.get() || self.inner.was_refused()
    }
    fn limit(&self) -> std::io::Error {
        self.refused.set(true);
        std::io::Error::other("Native project metadata resource limit exceeded.")
    }
}
impl crate::metadata::Files for BoundedMetadataFiles {
    fn entries(&self, path: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
        if self.was_refused() {
            return Err(self.limit());
        }
        if let Some(entries) = self.directories.borrow().get(path) {
            return Ok(entries.clone());
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let count = self.entries.get().saturating_add(1);
            if count > Self::ENTRY_LIMIT {
                return Err(self.limit());
            }
            self.entries.set(count);
            entries.push((entry.file_name(), entry.file_type()?.is_dir()));
        }
        entries.sort_by(|a, b| a.0.as_encoded_bytes().cmp(b.0.as_encoded_bytes()));
        let entries: Vec<_> = entries
            .into_iter()
            .map(|(name, dir)| (name.to_string_lossy().into_owned(), dir))
            .collect();
        self.directories
            .borrow_mut()
            .insert(path.to_path_buf(), entries.clone());
        Ok(entries)
    }
    fn stat(&self, path: &std::path::Path) -> std::io::Result<crate::metadata::Stat> {
        crate::metadata::Files::stat(&crate::metadata::NativeFiles, path)
    }
    fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
        if self.was_refused() {
            return Err(self.limit());
        }
        let bytes = crate::metadata::Files::read(&self.inner, path)?;
        let used = self.bytes.get().saturating_add(bytes.len());
        if used > Self::BYTE_LIMIT {
            return Err(self.limit());
        }
        self.bytes.set(used);
        Ok(bytes)
    }
    #[cfg(unix)]
    fn session_summary(&self, path: &std::path::Path, _size: u64) -> Result<bool, ()> {
        use std::os::unix::fs::OpenOptionsExt;
        if self.was_refused() {
            return Ok(false);
        }
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(_) => return Ok(false),
        };
        let meta = match file.metadata() {
            Ok(meta) => meta,
            Err(_) => return Ok(false),
        };
        if !meta.is_file() {
            let _ = self.limit();
            return Ok(false);
        }
        let mut bounded = SummaryReader { file, owner: self };
        crate::metadata::summary_from_file(&mut bounded, meta.len())
    }
    fn canonical(&self, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        crate::metadata::Files::canonical(&NativeFiles, path)
    }
    fn project_entries(
        &self,
        root: &std::path::Path,
        path: &std::path::Path,
    ) -> std::io::Result<Vec<(String, bool)>> {
        crate::metadata::Files::project_entries(&NativeFiles, root, path)
    }
    fn project_stat(
        &self,
        root: &std::path::Path,
        path: &std::path::Path,
    ) -> std::io::Result<(crate::metadata::Stat, bool)> {
        crate::metadata::Files::project_stat(&NativeFiles, root, path)
    }
    fn project_read(
        &self,
        root: &std::path::Path,
        path: &std::path::Path,
        limit: usize,
    ) -> std::io::Result<Vec<u8>> {
        crate::metadata::Files::project_read(&NativeFiles, root, path, limit)
    }
}

// A separate streamed-summary budget permits one legal 50 MiB fallback scan
// plus head/tail previews without charging transcript size as marker bytes.
#[cfg(unix)]
struct SummaryReader<'a> {
    file: std::fs::File,
    owner: &'a BoundedMetadataFiles,
}
#[cfg(unix)]
impl std::io::Read for SummaryReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        const LIMIT: usize = 50 * 1024 * 1024 + 2 * 65536;
        if self.owner.was_refused() {
            return Err(self.owner.limit());
        }
        let remaining = LIMIT.saturating_sub(self.owner.summary_bytes.get());
        if remaining == 0 {
            let mut byte = [0u8; 1];
            if std::io::Read::read(&mut self.file, &mut byte)? == 0 {
                return Ok(0);
            }
            return Err(self.owner.limit());
        }
        let take = buf.len().min(remaining);
        let count = std::io::Read::read(&mut self.file, &mut buf[..take])?;
        self.owner
            .summary_bytes
            .set(self.owner.summary_bytes.get() + count);
        Ok(count)
    }
}
#[cfg(unix)]
impl std::io::Seek for SummaryReader<'_> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.file, pos)
    }
}
fn metadata_route_result(
    route: crate::dispatch::Route,
    method: String,
) -> Result<JsValue, RequestError> {
    match route {
        crate::dispatch::Route::LocalResponse(bytes) => {
            let response = crate::dispatch::parse(&bytes)
                .ok_or_else(|| RequestError::new("Native method returned invalid JSON."))?;
            if response.get("ok") == Some(&JsValue::Bool(true)) {
                return Ok(field(&response, "result"));
            }
            let error = field(&response, "error");
            Err(RequestError {
                code: error
                    .get("code")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_else(|| "gateway_request_failed".into()),
                message: error
                    .get("message")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_else(|| "Native request failed.".into()),
                details: error.get("details").cloned().map(Box::new),
                validation: None,
            })
        }
        crate::dispatch::Route::Forward => unavailable(method),
    }
}
async fn project_metadata(
    home: std::path::PathBuf,
    env: crate::config::Env,
    method: String,
    params: JsValue,
) -> Result<JsValue, RequestError> {
    use std::sync::{Arc, LazyLock};
    static SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
        LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    let permit = SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RequestError::new("Native project reader is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let metadata = Metadata {
            home,
            cwd: std::path::PathBuf::new(),
            files: BoundedMetadataFiles::new(),
        };
        let request = object([
            ("type", "request".into()),
            ("id", "native".into()),
            ("method", JsValue::String(method.clone().into())),
            ("params", params),
        ]);
        let route = crate::dispatch::route_with_environment(&request, Some(&metadata), Some(&env));
        if metadata.files.was_refused() {
            return Err(RequestError::new(
                "Unsafe or oversized native project metadata.",
            ));
        }
        metadata_route_result(route, method)
    })
    .await
    .map_err(|_| RequestError::new("Native project reader stopped unexpectedly."))?
}
async fn model_catalog(
    home: std::path::PathBuf,
    env: crate::config::Env,
    params: JsValue,
) -> Result<JsValue, RequestError> {
    use std::sync::{Arc, LazyLock};
    static SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
        LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    let permit = SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RequestError::new("Native model catalog is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result =
            crate::config::load_project_bounded_lazy(&home, &std::path::PathBuf::new(), &env, None)
                .and_then(|config| crate::model_catalog::execute_loaded(config, &params, &env));
        result.map_err(|error| {
            if error.requires_host {
                return RequestError {
                    code: "CAPABILITY_UNAVAILABLE".into(),
                    message: "Native gateway method model_catalog_list is not configured.".into(),
                    details: None,
                    validation: None,
                };
            }
            RequestError {
                code: error.code,
                message: error.message,
                details: None,
                validation: None,
            }
        })
    })
    .await
    .map_err(|_| RequestError::new("Native model catalog stopped unexpectedly."))?
}

#[cfg(test)]
#[path = "metadata_session_model_tests.rs"]
mod session_model_tests;

#[cfg(all(test, unix))]
#[path = "metadata_project_bounded_tests.rs"]
mod project_bounded_tests;
