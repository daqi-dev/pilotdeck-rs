//! SESSION 生命周期七方法。装配端在 `routes.rs` 登记本模块，持有一个
//! `LifecycleRoutes`，将请求的 method/params 交给 `handle`，并把 config::Error
//! 的 code/message 映射为 RequestError。这里不产生 transport response envelope。
//!
//! 唯一 public(crate) API：
//! - `LifecycleRoutes`
//! - `LifecycleRoutes::new(owner: SessionOwner, metadata: Metadata,
//!   storage: Arc<AsyncTranscriptAdapter>, live: Arc<LiveBindings>, fallback_project: PathBuf,
//!   on_close: Arc<dyn Fn(&JsString, &JsString) -> Result<(), config::Error> + Send + Sync>) -> Self`
//! - `async handle(&self, method: &str, params: &JsValue)
//!   -> Option<Result<JsValue, config::Error>>`
//!
//! 注入 turn 使用的同一个 owner；adapter 的 chat-dir resolver 必须是
//! `crate::session_chat_dir(home, cwd, project)`，不能另开存储目录。close 回调
//! 必须清理同一 interactions 的 pending/binding、permission mode 和项目绑定；
//! delete 在关闭后以 session_closed 回调，成功删盘后以 session_deleted 回调；
//! 后者还须清理 SDK session state。回调失败会返回错误。若 SDK 用异步 Mutex，
//! 装配门面须在成功响应前 await 清理，不能后台派发后直接确认成功。
//! 本地 adapter 不承担外部 execution-provider 的存储删除；若启用该 provider，
//! 装配端须在删 transcript 前执行其 deleteSessionStorage，不能丢弃错误。
//!
//! TS 锚点：InProcessGateway.ts:2026-2056（new/resume 惰性；close/project-close），
//! GatewayWsConnection.ts:339-355（wire 结果），createLocalGateway.ts:1116-1163
//! （rename/tag 的 metadata 与 busy、delete 的关闭/存在检查/清理顺序）。
//! new/resume 不调用 factory、不恢复 transcript、不启动 sidecar；resume 原样返回。
//! rename/tag 通过真实 adapter 恢复 sequence 后追加 session_metadata，null 省略
//! title/tag。delete 先关闭真实 owner，再删除 transcript 与关联文件夹。
//! 验证结果由汇总方记录在 _tmp/20261027-session/integration-result.md。

use crate::{
    config,
    metadata::{Metadata, js_trim, object, resolve},
    runtime::session::SessionOwner,
    storage::{
        AdapterError, AsyncTranscriptAdapter, LedgerError, SessionStorage, SessionStorageInput,
        sanitize_session_id,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

type Result<T> = std::result::Result<T, config::Error>;
type Close = Arc<dyn Fn(&JsString, &JsString) -> Result<()> + Send + Sync>;

pub(crate) struct LifecycleRoutes {
    owner: SessionOwner,
    metadata: Metadata,
    storage: Arc<AsyncTranscriptAdapter>,
    live: Arc<super::live::LiveBindings>,
    fallback_project: PathBuf,
    on_close: Close,
}

impl LifecycleRoutes {
    pub(crate) fn new(
        owner: SessionOwner,
        metadata: Metadata,
        storage: Arc<AsyncTranscriptAdapter>,
        live: Arc<super::live::LiveBindings>,
        fallback_project: PathBuf,
        on_close: Close,
    ) -> Self {
        Self {
            owner,
            metadata,
            storage,
            live,
            fallback_project,
            on_close,
        }
    }

    pub(crate) async fn handle(&self, method: &str, params: &JsValue) -> Option<Result<JsValue>> {
        Some(match method {
            "new_session" => new_session(params),
            "resume_session" => Ok(params.clone()),
            "close_session" => self.close_session(params).await,
            "close_project_sessions" => self.close_project_sessions(params).await,
            "delete_session" => self.delete_session(params).await,
            "rename_session" | "tag_session" => self.metadata_update(method, params).await,
            _ => return None,
        })
    }

    async fn close_session(&self, params: &JsValue) -> Result<JsValue> {
        // TS accepts a missing key as an idempotent close of a nonexistent entry.
        let key = property(params, "sessionKey")?;
        if !matches!(key, JsValue::Undefined | JsValue::Null) {
            let key = key
                .as_string()
                .cloned()
                .ok_or_else(|| error("gateway_request_failed", "sessionKey must be a string."))?;
            self.owner.close(&key).await.map_err(failed)?;
            let reason = property(params, "reason")?;
            let reason = if matches!(reason, JsValue::Null | JsValue::Undefined) {
                "session_closed".into()
            } else {
                string(reason)?
            };
            (self.on_close)(&key, &reason)?;
        }
        Ok(object([("ok", JsValue::Bool(true))]))
    }

    async fn close_project_sessions(&self, params: &JsValue) -> Result<JsValue> {
        let project = property(params, "projectKey")?;
        let project = match project {
            JsValue::Null | JsValue::Undefined => {
                return Err(error("gateway_request_failed", "projectKey is required."));
            }
            JsValue::String(value)
                if js_trim(&String::from_utf16_lossy(value.units())).is_empty() =>
            {
                return Err(error("gateway_request_failed", "projectKey is required."));
            }
            JsValue::String(value) => path_string(&self.metadata.cwd, value)?,
            _ => {
                return Err(error(
                    "gateway_request_failed",
                    "input.projectKey?.trim is not a function",
                ));
            }
        };
        let keys = if truthy(property(params, "resume")?) {
            self.owner.resume_project(&project);
            vec![]
        } else {
            let keys = self.owner.close_project(&project).await.map_err(failed)?;
            for key in &keys {
                (self.on_close)(key, &"project_closed".into())?;
            }
            keys
        };
        Ok(object([(
            "sessionKeys",
            JsValue::Array(keys.into_iter().map(JsValue::String).collect()),
        )]))
    }

    fn session_storage(&self, params: &JsValue) -> Result<(JsString, SessionStorage)> {
        // TS resolves/validates the project before checking sessionKey.
        let supplied = property(params, "projectKey")?;
        let project = match supplied {
            JsValue::Null | JsValue::Undefined => self.fallback_project.clone(),
            JsValue::String(value) => PathBuf::from(
                path_string(&self.metadata.cwd, value)?
                    .to_utf8()
                    .map_err(failed)?,
            ),
            _ => {
                return Err(error(
                    "gateway_request_failed",
                    "projectKey must be a string.",
                ));
            }
        };
        let project = resolve(&self.metadata.cwd, &project.to_string_lossy());
        if project != self.metadata.home
            && !self
                .metadata
                .has_registered_project(&project)
                .map_err(failed)?
        {
            let supplied = match supplied.as_string() {
                Some(value) => value.clone(),
                None => self.fallback_project.to_string_lossy().as_ref().into(),
            };
            return Err(error(
                "PROJECT_NOT_FOUND",
                concat(&[&"Unknown projectKey: ".into(), &supplied]),
            ));
        }
        let key = property(params, "sessionKey")?;
        let key = match key {
            JsValue::String(value)
                if !js_trim(&String::from_utf16_lossy(value.units())).is_empty() =>
            {
                value.clone()
            }
            JsValue::String(_) | JsValue::Null | JsValue::Undefined => {
                return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));
            }
            _ => {
                return Err(error(
                    "gateway_request_failed",
                    "input.sessionKey?.trim is not a function",
                ));
            }
        };
        let storage = self
            .storage
            .create_session_storage(SessionStorageInput {
                project_root: project,
                pilot_home: self.metadata.home.clone(),
                session_id: key.clone(),
                now: None,
            })
            .map_err(failed)?;
        Ok((key, storage))
    }

    async fn metadata_update(&self, method: &str, params: &JsValue) -> Result<JsValue> {
        let (key, storage) = self.session_storage(params)?;
        let rename = method == "rename_session";
        let message = if rename {
            "Cannot rename an active session."
        } else {
            "Cannot tag an active session."
        };
        // The owner's admission guard also prevents a concurrent submit from
        // starting between the busy check and the append.
        let _admission = self
            .owner
            .begin_maintenance(&key)
            .map_err(failed)?
            .ok_or_else(|| error("SESSION_BUSY", message))?;
        let handle = self.owner.current_handle(&key);
        let _activity = handle
            .as_ref()
            .map(|handle| handle.begin_maintenance())
            .transpose()
            .map_err(|_| error("SESSION_BUSY", message))?;
        let value = property(params, "value")?;
        let mut metadata = JsObject::new();
        if !matches!(value, JsValue::Null | JsValue::Undefined) {
            metadata.insert(
                if rename { "title" } else { "tag" }.into(),
                JsValue::String(string(value)?),
            );
        }
        metadata.insert(
            "updatedAt".into(),
            crate::runtime::context::prompt::iso_timestamp_now()
                .as_str()
                .into(),
        );
        let _publication = self.live.publication.lock().await;
        let live = self
            .live
            .current_for(&self.owner, &key)
            .map_err(failed)?
            .filter(|state| state.recorder.writer().path() == storage.transcript_path);
        let writer = if let Some(state) = &live {
            state.check_error().map_err(failed)?;
            state.recorder.writer().as_ref()
        } else {
            storage
                .restore()
                .map_err(|cause| storage_failed(cause, "read", &storage.transcript_path))?;
            &storage.transcript
        };
        writer
            .record_session_metadata(
                &key,
                &if rename { "sdk-rename" } else { "sdk-tag" }.into(),
                JsValue::Object(metadata),
            )
            .map_err(|cause| storage_failed(cause, "open", &storage.transcript_path))?;
        Ok(object([("updated", JsValue::Bool(true))]))
    }

    async fn delete_session(&self, params: &JsValue) -> Result<JsValue> {
        let (key, storage) = self.session_storage(params)?;
        self.owner.close(&key).await.map_err(failed)?;
        let _admission = self
            .owner
            .begin_maintenance(&key)
            .map_err(failed)?
            .ok_or_else(|| error("SESSION_BUSY", "Cannot delete an active session."))?;
        (self.on_close)(&key, &"session_closed".into())?;
        let exists = match storage.transcript_exists() {
            Some(result) => {
                result.map_err(|cause| storage_failed(cause, "stat", &storage.transcript_path))?
            }
            None => match fs::metadata(&storage.transcript_path) {
                Ok(info) => info.is_file(),
                Err(cause) if cause.kind() == io::ErrorKind::NotFound => false,
                Err(cause) => return Err(io_failed(cause, "stat", &storage.transcript_path)),
            },
        };
        if !exists {
            return Err(error(
                "SESSION_NOT_FOUND",
                concat(&[&"Session not found: ".into(), &key]),
            ));
        }
        let project = match property(params, "projectKey")? {
            JsValue::String(value) => PathBuf::from(
                path_string(&self.metadata.cwd, value)?
                    .to_utf8()
                    .map_err(failed)?,
            ),
            _ => self.fallback_project.clone(),
        };
        let project = resolve(&self.metadata.cwd, &project.to_string_lossy());
        let session_dir = storage.chat_dir.join(sanitize_session_id(&key));
        if storage.file_history_dir != session_dir.join("file-history")
            || storage.subagents_dir != session_dir.join("subagents")
            || storage.tool_results_dir
                != project
                    .join(".pilotdeck/tool-results")
                    .join(sanitize_session_id(&key))
        {
            return Err(error(
                "path_not_allowed",
                "Session artifact path is outside its owning storage root.",
            ));
        }
        // Validate every path before the first unlink: an intermediate symlink
        // must never turn a selected session's deletion into another's.
        preflight_directory(&storage.chat_dir, &self.metadata.home)?;
        for (path, root) in [
            (&storage.file_history_dir, &storage.chat_dir),
            (&storage.tool_results_dir, &project),
            (&storage.subagents_dir, &storage.chat_dir),
        ] {
            preflight_directory(path, root)?;
        }
        if let Some(result) = storage.delete_session_transcripts() {
            result.map_err(|cause| storage_failed(cause, "unlink", &storage.transcript_path))?;
        } else if let Some(result) = storage.delete_transcript() {
            result.map_err(|cause| storage_failed(cause, "unlink", &storage.transcript_path))?;
        } else {
            fs::remove_file(&storage.transcript_path)
                .map_err(|cause| io_failed(cause, "unlink", &storage.transcript_path))?;
        }
        remove_directory(&storage.file_history_dir)?;
        remove_directory(&storage.tool_results_dir)?;
        remove_directory(&storage.subagents_dir)?;
        (self.on_close)(&key, &"session_deleted".into())?;
        Ok(object([("ok", JsValue::Bool(true))]))
    }
}

fn preflight_directory(path: &Path, root: &Path) -> Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        error(
            "path_not_allowed",
            "Session artifact path is outside its owning storage root.",
        )
    })?;
    if !root.is_absolute()
        || relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(error(
            "path_not_allowed",
            "Session artifact path is outside its owning storage root.",
        ));
    }
    // The configured root may contain platform aliases (e.g. macOS /var);
    // every component below that trusted root must be a real directory.
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(error(
                    "path_not_allowed",
                    "Session artifact path contains a symbolic link.",
                ));
            }
            Ok(info) if !info.is_dir() => {
                return Err(io_failed(
                    io::Error::from_raw_os_error(libc::ENOTDIR),
                    "scandir",
                    &current,
                ));
            }
            Ok(_) => {}
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => break,
            Err(cause) => return Err(io_failed(cause, "lstat", &current)),
        }
    }
    Ok(())
}

fn remove_directory(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(cause) => Err(io_failed(cause, "rmdir", path)),
    }
}

fn new_session(params: &JsValue) -> Result<JsValue> {
    let project = property(params, "projectKey")?;
    let project = if truthy(project) {
        concat(&[&"project=".into(), &string(project)?, &":".into()])
    } else {
        "".into()
    };
    let key = concat(&[
        &string(property(params, "channelKey")?)?,
        &":".into(),
        &project,
        &"s_".into(),
        &uuid::Uuid::new_v4().to_string().as_str().into(),
    ]);
    Ok(object([("sessionKey", JsValue::String(key))]))
}

fn property<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue> {
    static UNDEFINED: JsValue = JsValue::Undefined;
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        return Err(error(
            "gateway_request_failed",
            format!(
                "Cannot read properties of {} (reading '{key}')",
                if value.is_undefined() {
                    "undefined"
                } else {
                    "null"
                }
            ),
        ));
    }
    Ok(value.get(key).unwrap_or(&UNDEFINED))
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}

fn string(value: &JsValue) -> Result<JsString> {
    Ok(match value {
        JsValue::String(value) => value.clone(),
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(value) => value.to_string().as_str().into(),
        JsValue::Number(value) if value.is_nan() => "NaN".into(),
        JsValue::Number(value) if *value == f64::INFINITY => "Infinity".into(),
        JsValue::Number(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
        JsValue::Number(_) => stringify_json(value)
            .map_err(failed)?
            .unwrap_or_default()
            .as_str()
            .into(),
        JsValue::Array(items) => {
            let mut units = vec![];
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    units.push(b',' as u16);
                }
                if !matches!(item, JsValue::Null | JsValue::Undefined) {
                    units.extend_from_slice(string(item)?.units());
                }
            }
            JsString::from_utf16(units)
        }
        JsValue::Object(value) => {
            if value.get_property("toString").is_some() {
                return Err(error(
                    "gateway_request_failed",
                    "Cannot convert object to primitive value",
                ));
            }
            format!("[object {}]", value.native_tag().unwrap_or("Object"))
                .as_str()
                .into()
        }
    })
}

fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|part| part.units().iter().copied())
            .collect(),
    )
}

fn path_string(cwd: &Path, value: &JsString) -> Result<JsString> {
    crate::metadata::resolved_identity(cwd, value)
        .map(|value| value.as_str().into())
        .ok_or_else(|| {
            error(
                "PROJECT_NOT_FOUND",
                concat(&[&"Unknown projectKey: ".into(), value]),
            )
        })
}

fn error(code: &str, message: impl Into<JsString>) -> config::Error {
    config::Error {
        code: code.into(),
        message: message.into(),
        diagnostics: vec![],
        name: "Error",
        requires_host: false,
    }
}

fn failed(cause: impl std::fmt::Display) -> config::Error {
    error("gateway_request_failed", cause.to_string())
}

fn io_failed(cause: io::Error, syscall: &str, path: &Path) -> config::Error {
    let diagnostic = if syscall == "read" {
        crate::node_io::read_file(&cause, &path.to_string_lossy())
    } else {
        crate::node_io::diagnostic(&cause, syscall, Some(&path.to_string_lossy()))
    };
    match diagnostic {
        Some((code, message)) => error(code, message),
        None => failed(cause),
    }
}

fn storage_failed(cause: AdapterError, syscall: &str, path: &Path) -> config::Error {
    match cause {
        AdapterError::Validation { code, message } => {
            let mut result = error(code, message);
            result.name = "SessionEventValidationError";
            result
        }
        AdapterError::Type(message) => {
            let mut result = error("gateway_request_failed", message);
            result.name = "TypeError";
            result
        }
        AdapterError::Range(message) => {
            let mut result = error("gateway_request_failed", message);
            result.name = "RangeError";
            result
        }
        AdapterError::Store(LedgerError::Io(cause)) => io_failed(cause, syscall, path),
        other => failed(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::Limits,
        metadata::{NativeFiles, project_id},
        runtime::{
            project::RuntimeError,
            refresh::BoxFuture,
            session::{
                HandleState, LiveSessionHandle, SessionContext, SessionFactory,
                SessionOwnerOptions,
                sidecar_session::{SidecarSession, SidecarSessionConfig, SidecarSpawner},
            },
        },
        storage::{JsonlTranscriptStore, ProjectKey},
    };
    use pilotdeck_protocol::parse_json;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct NeverSpawn;
    impl SidecarSpawner for NeverSpawn {
        fn requires_subprocess(&self) -> bool {
            true
        }

        fn command(&self) -> io::Result<tokio::process::Command> {
            panic!("Lifecycle routes must never start a sidecar")
        }
    }

    struct Factory {
        creates: Arc<AtomicUsize>,
        adapter: Arc<AsyncTranscriptAdapter>,
        live: Arc<super::super::live::LiveBindings>,
        home: PathBuf,
    }
    impl SessionFactory for Factory {
        fn create(
            &self,
            context: SessionContext,
            _previous: Option<LiveSessionHandle>,
        ) -> BoxFuture<std::result::Result<LiveSessionHandle, RuntimeError>> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            let cwd = context.project_key.clone().unwrap_or_else(|| "/tmp".into());
            let storage = self.adapter.create_session_storage(SessionStorageInput {
                project_root: PathBuf::from(cwd.to_utf8().unwrap()),
                pilot_home: self.home.clone(),
                session_id: context.session_key.clone(),
                now: None,
            });
            let live = self.live.clone();
            Box::pin(async move {
                let storage = storage.map_err(|cause| RuntimeError::new(cause.to_string()))?;
                let _publication = live.publication.lock().await;
                super::super::host::bind(
                    live.clone(),
                    context.clone(),
                    cwd,
                    Arc::new(SidecarSession::new(
                        SidecarSessionConfig {
                            limits: Limits::default(),
                            session_key: context.session_key,
                            cwd: context.project_key.clone().unwrap_or_else(|| "/tmp".into()),
                            project_key: context.project_key,
                            snapshot: JsValue::Null,
                            services: None,
                        },
                        Arc::new(NeverSpawn),
                    )),
                    storage,
                    None,
                    None,
                )
            })
        }
    }

    struct Fixture {
        root: PathBuf,
        project: PathBuf,
        other: PathBuf,
        routes: LifecycleRoutes,
        creates: Arc<AtomicUsize>,
        closed: Arc<Mutex<Vec<(JsString, JsString)>>>,
        fail_cleanup: Arc<AtomicBool>,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("pdrs-lifecycle-{}", uuid::Uuid::new_v4()));
            let project = root.join("project");
            let other = root.join("other");
            let home = root.join("pilot-home");
            for path in [&project, &other] {
                fs::create_dir_all(path).unwrap();
                let registered = home.join("projects").join(project_id(path));
                fs::create_dir_all(&registered).unwrap();
                fs::write(registered.join(".cwd"), path.to_string_lossy().as_bytes()).unwrap();
            }
            let creates = Arc::new(AtomicUsize::new(0));
            let storage = Arc::new(AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(|key: &ProjectKey| {
                    key.pilot_home
                        .join("projects")
                        .join(project_id(&key.project_root))
                        .join("chats")
                }),
            ));
            let live = Arc::new(super::super::live::LiveBindings::default());
            let mut options = SessionOwnerOptions::new(Arc::new(Factory {
                creates: creates.clone(),
                adapter: storage.clone(),
                live: live.clone(),
                home: home.clone(),
            }));
            options.idle_sweep_interval_ms = 0;
            let owner = SessionOwner::new(options);
            let closed = Arc::new(Mutex::new(vec![]));
            let closed_events = closed.clone();
            let fail_cleanup = Arc::new(AtomicBool::new(false));
            let fail = fail_cleanup.clone();
            let routes = LifecycleRoutes::new(
                owner,
                Metadata {
                    home,
                    cwd: root.clone(),
                    files: NativeFiles,
                },
                storage,
                live,
                project.clone(),
                Arc::new(move |key, reason| {
                    closed_events
                        .lock()
                        .unwrap()
                        .push((key.clone(), reason.clone()));
                    if fail.load(Ordering::SeqCst) {
                        Err(error("gateway_request_failed", "cleanup failed"))
                    } else {
                        Ok(())
                    }
                }),
            );
            Self {
                root,
                project,
                other,
                routes,
                creates,
                closed,
                fail_cleanup,
            }
        }
        async fn call(&self, method: &str, params: JsValue) -> Result<JsValue> {
            self.routes
                .handle(method, &params)
                .await
                .expect("known method")
        }
        fn params(&self, project: &Path, key: &str, value: JsValue) -> JsValue {
            object([
                ("projectKey", project.to_string_lossy().as_ref().into()),
                ("sessionKey", key.into()),
                ("value", value),
            ])
        }
        fn storage(&self, project: &Path, key: &str) -> SessionStorage {
            self.routes
                .session_storage(&self.params(project, key, JsValue::Null))
                .unwrap()
                .1
        }
        async fn live(&self, project: &Path, key: &str) -> LiveSessionHandle {
            self.routes
                .owner
                .get_or_create(SessionContext {
                    session_key: key.into(),
                    project_key: Some(project.to_string_lossy().as_ref().into()),
                    channel_key: "test".into(),
                    allowed_tools: None,
                    disallowed_tools: None,
                })
                .await
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn js(text: &str) -> JsValue {
        parse_json(text).unwrap()
    }
    fn text(value: &JsValue) -> String {
        value.as_string().unwrap().to_utf8().unwrap()
    }

    #[tokio::test]
    async fn new_and_resume_are_lazy_and_preserve_ts_shapes() {
        let f = Fixture::new();
        let a = f
            .call(
                "new_session",
                js(r#"{"channelKey":"web","projectKey":"../literal:项目","hint":"ignored"}"#),
            )
            .await
            .unwrap();
        let key = text(a.get("sessionKey").unwrap());
        let suffix = key.strip_prefix("web:project=../literal:项目:s_").unwrap();
        assert!(uuid::Uuid::parse_str(suffix).is_ok());
        assert_eq!(a.as_object().unwrap().entries().len(), 1);
        let b = f
            .call("new_session", js(r#"{"channelKey":"web","projectKey":""}"#))
            .await
            .unwrap();
        assert!(text(b.get("sessionKey").unwrap()).starts_with("web:s_"));
        assert_ne!(a, b);
        let unknown = js(r#"{"sessionKey":"does-not-exist","extra":42}"#);
        assert_eq!(
            f.call("resume_session", unknown.clone()).await.unwrap(),
            unknown
        );
        assert_eq!(
            f.call("resume_session", JsValue::Null).await.unwrap(),
            JsValue::Null
        );
        assert!(
            f.routes
                .owner
                .current_handle(&key.as_str().into())
                .is_none()
        );
        assert_eq!(f.creates.load(Ordering::SeqCst), 0);
        assert!(
            !f.storage(&f.project, "does-not-exist")
                .transcript_path
                .exists()
        );
        let default = f.call("new_session", js("{}")).await.unwrap();
        assert!(text(default.get("sessionKey").unwrap()).starts_with("undefined:s_"));
        let unicode = f
            .call(
                "new_session",
                js(r#"{"channelKey":"\ud800","projectKey":12}"#),
            )
            .await
            .unwrap();
        assert_eq!(
            unicode
                .get("sessionKey")
                .unwrap()
                .as_string()
                .unwrap()
                .units()[0],
            0xd800
        );
    }

    #[tokio::test]
    async fn close_drains_the_shared_owner_and_preserves_recoverable_data() {
        let f = Fixture::new();
        let handle = f.live(&f.project, "s").await;
        let same = f.live(&f.project, "s").await;
        assert!(handle.same_identity(&same));
        f.call("rename_session", f.params(&f.project, "s", "saved".into()))
            .await
            .unwrap();
        let before = fs::read(f.storage(&f.project, "s").transcript_path).unwrap();
        assert_eq!(
            f.call(
                "close_session",
                js(r#"{"sessionKey":"s","reason":"custom"}"#)
            )
            .await
            .unwrap(),
            js(r#"{"ok":true}"#)
        );
        assert_eq!(handle.state(), HandleState::Disposed);
        assert!(f.routes.owner.current_handle(&"s".into()).is_none());
        assert_eq!(
            f.closed.lock().unwrap().as_slice(),
            &[("s".into(), "custom".into())]
        );
        let input = js(r#"{"sessionKey":"s"}"#);
        assert_eq!(
            f.call("resume_session", input.clone()).await.unwrap(),
            input
        );
        assert_eq!(f.creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read(f.storage(&f.project, "s").transcript_path).unwrap(),
            before
        );
        let restored = f.storage(&f.project, "s").restore().unwrap();
        assert_eq!(
            restored.entries[0].get("metadata").unwrap().get("title"),
            Some(&JsValue::from("saved"))
        );
        let fresh = f.live(&f.project, "s").await;
        assert!(!handle.same_identity(&fresh));
        f.call("close_session", input).await.unwrap();
        f.call("close_session", js(r#"{"sessionKey":"missing"}"#))
            .await
            .unwrap();
        f.call("close_session", js("{}")).await.unwrap();
    }

    #[tokio::test]
    async fn bound_session_publishes_started_then_accepted_with_mode_metadata() {
        let f = Fixture::new();
        struct Exit;
        impl SidecarSpawner for Exit {
            fn requires_subprocess(&self) -> bool {
                true
            }

            fn command(&self) -> io::Result<tokio::process::Command> {
                Ok(tokio::process::Command::new("false"))
            }
        }
        let handle = super::super::host::bind(
            Arc::new(super::super::live::LiveBindings::default()),
            SessionContext {
                session_key: "s".into(),
                project_key: Some(f.project.to_string_lossy().as_ref().into()),
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            },
            f.project.to_string_lossy().as_ref().into(),
            Arc::new(SidecarSession::new(
                SidecarSessionConfig {
                    limits: Limits::default(),
                    session_key: "s".into(),
                    project_key: None,
                    cwd: f.project.to_string_lossy().as_ref().into(),
                    snapshot: JsValue::Null,
                    services: None,
                },
                Arc::new(Exit),
            )),
            f.storage(&f.project, "s"),
            None,
            None,
        )
        .unwrap();
        let mut turn = handle.submit(
            js(r#"{"agent":{},"runMode":"plan","allowPlanModeTools":false,"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#),
            js(r#"{"turnId":"t","operationId":"op"}"#),
        ).unwrap();
        for kind in ["turn_started", "input_accepted"] {
            let event = turn.next().await.unwrap().unwrap();
            assert_eq!(event.get("type"), Some(&JsValue::from(kind)));
        }
        let entries = f
            .storage(&f.project, "s")
            .read_transcript(None)
            .unwrap()
            .entries;
        let accepted = entries
            .iter()
            .find(|e| e.get("type") == Some(&"accepted_input".into()))
            .unwrap();
        assert_eq!(
            accepted.get("metadata").and_then(|m| m.get("runMode")),
            Some(&"plan".into())
        );
        assert_eq!(
            accepted
                .get("metadata")
                .and_then(|m| m.get("allowPlanModeTools")),
            Some(&JsValue::Bool(false))
        );
        drop(turn);
        handle.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn close_project_is_isolated_and_resume_releases_its_pause() {
        let f = Fixture::new();
        let a = f.live(&f.project, "a").await;
        let b = f.live(&f.other, "b").await;
        let params = object([("projectKey", f.project.to_string_lossy().as_ref().into())]);
        let result = f.call("close_project_sessions", params).await.unwrap();
        assert_eq!(result, js(r#"{"sessionKeys":["a"]}"#));
        assert_eq!(a.state(), HandleState::Disposed);
        assert!(
            f.routes
                .owner
                .current_handle(&"b".into())
                .unwrap()
                .same_identity(&b)
        );
        let context = SessionContext {
            session_key: "c".into(),
            project_key: Some(f.project.to_string_lossy().as_ref().into()),
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        };
        assert!(f.routes.owner.get_or_create(context.clone()).await.is_err());
        let params = object([
            ("projectKey", f.project.to_string_lossy().as_ref().into()),
            ("resume", JsValue::Bool(true)),
        ]);
        assert_eq!(
            f.call("close_project_sessions", params).await.unwrap(),
            js(r#"{"sessionKeys":[]}"#)
        );
        let c = f.routes.owner.get_or_create(context).await.unwrap();
        assert_eq!(c.state(), HandleState::Active);
        assert_eq!(
            f.closed.lock().unwrap().as_slice(),
            &[("a".into(), "project_closed".into())]
        );
        f.routes.owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rename_and_tag_append_durable_metadata_without_replacing_the_owner() {
        let f = Fixture::new();
        let handle = f.live(&f.project, "s").await;
        for (method, value) in [
            ("rename_session", "title".into()),
            ("tag_session", "tag".into()),
            ("rename_session", JsValue::Null),
            ("tag_session", JsValue::Null),
        ] {
            assert_eq!(
                f.call(method, f.params(&f.project, "s", value))
                    .await
                    .unwrap(),
                js(r#"{"updated":true}"#)
            );
        }
        let storage = f.storage(&f.project, "s");
        let read = storage.restore().unwrap();
        assert_eq!(read.entries.len(), 4);
        assert!(!read.diagnostics.iter().any(|d| d.severity == "error"));
        for (i, entry) in read.entries.iter().enumerate() {
            assert_eq!(
                entry.get("sequence").and_then(JsValue::as_number),
                Some((i + 1) as f64)
            );
            assert_eq!(text(entry.get("type").unwrap()), "session_metadata");
            assert_eq!(
                text(entry.get("turnId").unwrap()),
                if i % 2 == 0 { "sdk-rename" } else { "sdk-tag" }
            );
            assert!(entry.get("metadata").unwrap().get("updatedAt").is_some());
        }
        assert_eq!(
            read.entries[0].get("metadata").unwrap().get("title"),
            Some(&JsValue::from("title"))
        );
        assert_eq!(
            read.entries[1].get("metadata").unwrap().get("tag"),
            Some(&JsValue::from("tag"))
        );
        assert!(
            read.entries[2]
                .get("metadata")
                .unwrap()
                .get("title")
                .is_none()
        );
        assert!(
            read.entries[3]
                .get("metadata")
                .unwrap()
                .get("tag")
                .is_none()
        );
        assert!(
            f.routes
                .owner
                .current_handle(&"s".into())
                .unwrap()
                .same_identity(&handle)
        );
        assert_eq!(f.creates.load(Ordering::SeqCst), 1);
        f.routes
            .live
            .current(&"s".into())
            .unwrap()
            .record_status(
                &"t".into(),
                &js(r#"{"event":"after-metadata","text":"stored"}"#),
            )
            .unwrap();
        let read = f.storage(&f.project, "s").restore().unwrap();
        assert!(!read.diagnostics.iter().any(|d| d.severity == "error"));
        assert_eq!(
            read.entries.last().unwrap().get("sequence"),
            Some(&JsValue::Number(5.0))
        );
        f.routes.owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn metadata_rejects_active_turns_and_handle_activity_without_writing() {
        let f = Fixture::new();
        let handle = f.live(&f.project, "s").await;
        let reservation = f
            .routes
            .owner
            .begin_turn(&"s".into(), &"run".into())
            .unwrap();
        for method in ["rename_session", "tag_session"] {
            assert_eq!(
                f.call(method, f.params(&f.project, "s", "value".into()))
                    .await
                    .unwrap_err()
                    .code,
                "SESSION_BUSY"
            );
        }
        assert!(!f.storage(&f.project, "s").transcript_path.exists());
        f.routes
            .owner
            .end_turn(&"s".into(), Some(&"run".into()), Some(&reservation));
        let activity = handle.begin_activity().unwrap();
        assert_eq!(
            f.call("tag_session", f.params(&f.project, "s", "value".into()))
                .await
                .unwrap_err()
                .code,
            "SESSION_BUSY"
        );
        drop(activity);
        f.call("tag_session", f.params(&f.project, "s", "value".into()))
            .await
            .unwrap();
        assert_eq!(
            f.storage(&f.project, "s").restore().unwrap().entries.len(),
            1
        );
        f.routes.owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_closes_the_actual_owner_and_removes_only_selected_project_data() {
        let f = Fixture::new();
        let handle = f.live(&f.project, "s").await;
        f.call("rename_session", f.params(&f.project, "s", "a".into()))
            .await
            .unwrap();
        f.call("tag_session", f.params(&f.other, "s", "b".into()))
            .await
            .unwrap();
        let storage = f.storage(&f.project, "s");
        for dir in [
            &storage.file_history_dir,
            &storage.tool_results_dir,
            &storage.subagents_dir,
        ] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("artifact"), "kept until deletion").unwrap();
        }
        let other = fs::read(f.storage(&f.other, "s").transcript_path).unwrap();
        assert_eq!(
            f.call("delete_session", f.params(&f.project, "s", JsValue::Null))
                .await
                .unwrap(),
            js(r#"{"ok":true}"#)
        );
        assert_eq!(handle.state(), HandleState::Disposed);
        assert!(f.routes.owner.current_handle(&"s".into()).is_none());
        assert!(!storage.transcript_path.exists());
        for dir in [
            &storage.file_history_dir,
            &storage.tool_results_dir,
            &storage.subagents_dir,
        ] {
            assert!(!dir.exists());
        }
        assert_eq!(
            fs::read(f.storage(&f.other, "s").transcript_path).unwrap(),
            other
        );
        assert_eq!(
            f.closed.lock().unwrap().as_slice(),
            &[
                ("s".into(), "session_closed".into()),
                ("s".into(), "session_deleted".into())
            ]
        );
        let missing = f
            .call("delete_session", f.params(&f.project, "s", JsValue::Null))
            .await
            .unwrap_err();
        assert_eq!(missing.code, "SESSION_NOT_FOUND");
        assert!(missing.message.equals_str("Session not found: s"));
        assert_eq!(f.creates.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalid_inputs_and_unknown_projects_preserve_ts_error_boundaries() {
        let f = Fixture::new();
        assert!(
            f.routes
                .handle("list_sessions", &JsValue::Null)
                .await
                .is_none()
        );
        assert_eq!(
            f.call("new_session", JsValue::Null).await.unwrap_err().code,
            "gateway_request_failed"
        );
        for method in ["rename_session", "tag_session", "delete_session"] {
            assert_eq!(
                f.call(method, js("{}")).await.unwrap_err().code,
                "INVALID_SESSION_KEY"
            );
            assert_eq!(
                f.call(method, js(r#"{"sessionKey":"  \ufeff"}"#))
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_SESSION_KEY"
            );
            assert_eq!(
                f.call(method, js(r#"{"sessionKey":123}"#))
                    .await
                    .unwrap_err()
                    .code,
                "gateway_request_failed"
            );
            assert_eq!(
                f.call(
                    method,
                    js(r#"{"projectKey":"/unknown-project","sessionKey":"s"}"#)
                )
                .await
                .unwrap_err()
                .code,
                "PROJECT_NOT_FOUND"
            );
        }
        for params in [
            js("{}"),
            js(r#"{"projectKey":" "}"#),
            js(r#"{"projectKey":1}"#),
        ] {
            assert_eq!(
                f.call("close_project_sessions", params)
                    .await
                    .unwrap_err()
                    .code,
                "gateway_request_failed"
            );
        }
        assert_eq!(f.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn metadata_for_missing_session_persists_and_io_or_cleanup_errors_are_not_success() {
        let f = Fixture::new();
        f.call("rename_session", js(r#"{"sessionKey":"new","value":42}"#))
            .await
            .unwrap();
        let read = f.storage(&f.project, "new").restore().unwrap();
        assert_eq!(
            read.entries[0].get("metadata").unwrap().get("title"),
            Some(&JsValue::from("42"))
        );
        let bad = f.storage(&f.project, "bad");
        fs::create_dir_all(&bad.transcript_path).unwrap();
        assert!(
            f.call("tag_session", f.params(&f.project, "bad", "x".into()))
                .await
                .is_err()
        );
        let path = f.storage(&f.project, "new").transcript_path;
        let before = fs::read(&path).unwrap();
        f.fail_cleanup.store(true, Ordering::SeqCst);
        assert!(
            f.call("delete_session", js(r#"{"sessionKey":"new"}"#))
                .await
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(
            f.call("close_session", js(r#"{"sessionKey":"missing"}"#))
                .await
                .is_err()
        );
        assert_eq!(f.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn delete_rejects_intermediate_symlinks_before_removing_any_project_data() {
        for linked in ["tool-results", "file-history", "subagents"] {
            let f = Fixture::new();
            for project in [&f.project, &f.other] {
                f.call("rename_session", f.params(project, "s", "keep".into()))
                    .await
                    .unwrap();
            }
            let a = f.storage(&f.project, "s");
            let b = f.storage(&f.other, "s");
            let (link, target) = if linked == "tool-results" {
                (
                    a.tool_results_dir.parent().unwrap().to_path_buf(),
                    b.tool_results_dir.parent().unwrap().to_path_buf(),
                )
            } else if linked == "file-history" {
                (a.file_history_dir.clone(), b.file_history_dir.clone())
            } else {
                (a.subagents_dir.clone(), b.subagents_dir.clone())
            };
            let b_paths = [&b.file_history_dir, &b.tool_results_dir, &b.subagents_dir];
            for path in b_paths {
                fs::create_dir_all(path).unwrap();
                fs::write(path.join("artifact"), b"project B untouched").unwrap();
            }
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let legal_a = if linked == "tool-results" {
                &a.file_history_dir
            } else {
                &a.tool_results_dir
            };
            fs::create_dir_all(legal_a).unwrap();
            fs::write(legal_a.join("artifact"), b"project A untouched").unwrap();
            let a_bytes = fs::read(&a.transcript_path).unwrap();
            let b_bytes = fs::read(&b.transcript_path).unwrap();
            let result = f
                .call("delete_session", f.params(&f.project, "s", JsValue::Null))
                .await
                .unwrap_err();
            assert_eq!(result.code, "path_not_allowed", "{linked}");
            assert_eq!(fs::read(&a.transcript_path).unwrap(), a_bytes);
            assert_eq!(fs::read(&b.transcript_path).unwrap(), b_bytes);
            assert_eq!(
                fs::read(legal_a.join("artifact")).unwrap(),
                b"project A untouched"
            );
            for path in b_paths {
                assert_eq!(
                    fs::read(path.join("artifact")).unwrap(),
                    b"project B untouched"
                );
            }
            assert_eq!(fs::read_link(link).unwrap(), target);
        }
    }

    #[tokio::test]
    async fn delete_busy_after_owner_close_preserves_new_turn_cleanup_state_and_transcript() {
        use crate::runtime::session::{EvictionReason, TurnReservation};
        let mut f = Fixture::new();
        f.call("rename_session", f.params(&f.project, "s", "keep".into()))
            .await
            .unwrap();
        let before = fs::read(f.storage(&f.project, "s").transcript_path).unwrap();
        let owner_slot = Arc::new(Mutex::new(None::<SessionOwner>));
        let reservation = Arc::new(Mutex::new(None::<TurnReservation>));
        let pending = Arc::new(AtomicBool::new(false));
        let mut options = SessionOwnerOptions::new(Arc::new(Factory {
            creates: f.creates.clone(),
            adapter: f.routes.storage.clone(),
            live: f.routes.live.clone(),
            home: f.routes.metadata.home.clone(),
        }));
        options.idle_sweep_interval_ms = 0;
        let callback_owner = owner_slot.clone();
        let callback_reservation = reservation.clone();
        let new_pending = pending.clone();
        // on_evict is a real owner callback after disposal and before close
        // resolves. It deterministically occupies the close/maintenance window.
        options.on_evict = Some(Arc::new(move |key, reason| {
            if reason == EvictionReason::Closed {
                let owner = callback_owner.lock().unwrap().as_ref().unwrap().clone();
                *callback_reservation.lock().unwrap() =
                    Some(owner.begin_turn(key, &"new-run".into()).unwrap());
                new_pending.store(true, Ordering::SeqCst);
            }
        }));
        f.routes.owner = SessionOwner::new(options);
        *owner_slot.lock().unwrap() = Some(f.routes.owner.clone());
        let closed = f.closed.clone();
        let cleanup_pending = pending.clone();
        f.routes.on_close = Arc::new(move |key, reason| {
            closed.lock().unwrap().push((key.clone(), reason.clone()));
            cleanup_pending.store(false, Ordering::SeqCst);
            Ok(())
        });
        let old = f.live(&f.project, "s").await;
        let result = f
            .call("delete_session", f.params(&f.project, "s", JsValue::Null))
            .await;
        // Break the callback's owner reference even if an assertion below fails.
        owner_slot.lock().unwrap().take();
        assert_eq!(old.state(), HandleState::Disposed);
        assert_eq!(result.unwrap_err().code, "SESSION_BUSY");
        assert!(pending.load(Ordering::SeqCst));
        assert!(f.closed.lock().unwrap().is_empty());
        assert_eq!(
            f.routes.owner.active_turn_run_id(&"s".into()),
            Some("new-run".into())
        );
        assert_eq!(
            fs::read(f.storage(&f.project, "s").transcript_path).unwrap(),
            before
        );
        let turn = reservation.lock().unwrap().take().unwrap();
        f.routes
            .owner
            .end_turn(&"s".into(), Some(&"new-run".into()), Some(&turn));
    }

    #[tokio::test]
    async fn rename_and_tag_preserve_node_io_and_validation_error_codes() {
        let f = Fixture::new();
        let directory = f.storage(&f.project, "directory").transcript_path;
        fs::create_dir_all(&directory).unwrap();
        for method in ["rename_session", "tag_session"] {
            let cause = f
                .call(method, f.params(&f.project, "directory", "value".into()))
                .await
                .unwrap_err();
            assert_eq!(cause.code, "EISDIR");
            assert!(
                cause
                    .message
                    .equals_str("EISDIR: illegal operation on a directory, read")
            );
            assert!(directory.is_dir());
        }
        let invalid = f.storage(&f.project, "invalid-sequence").transcript_path;
        let bytes = b"{\"type\":\"session_metadata\",\"sessionId\":\"invalid-sequence\",\"turnId\":\"t\",\"sequence\":0,\"createdAt\":\"2026-10-27T00:00:00Z\",\"metadata\":{}}\n";
        fs::write(&invalid, bytes).unwrap();
        for method in ["rename_session", "tag_session"] {
            let cause = f
                .call(
                    method,
                    f.params(&f.project, "invalid-sequence", "value".into()),
                )
                .await
                .unwrap_err();
            assert_eq!(cause.code, "invalid_sequence");
            assert_eq!(cause.name, "SessionEventValidationError");
            assert!(
                cause
                    .message
                    .equals_str("Session event sequence must be a positive safe integer: 0.")
            );
            assert_eq!(fs::read(&invalid).unwrap(), bytes);
        }
        let direct = io_failed(
            io::Error::from_raw_os_error(libc::ENOENT),
            "unlink",
            &invalid,
        );
        assert_eq!(direct.code, "ENOENT");
        assert!(direct.message.equals_str(&format!(
            "ENOENT: no such file or directory, unlink '{}'",
            invalid.display()
        )));
    }
}
