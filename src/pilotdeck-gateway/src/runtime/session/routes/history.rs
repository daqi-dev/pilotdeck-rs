//! Durable history routes. TS: GatewaySessionCatalog.ts, readSessionMessages.ts,
//! forkSession.ts, replaceLastTurn.ts. The host shares both coordinators with submit.
#[path = "history/edit.rs"]
mod edit;
#[path = "history/projection.rs"]
mod projection;

use crate::{
    metadata::{object, resolve},
    runtime::{
        refresh::BoxFuture,
        replacement::{
            ReplacementCoordinator, ReplacementError, ReplacementResult, ReplacementStorage,
        },
        turn::{
            coordinator::TurnEventCoordinator,
            read::{self, SessionHistoryReader, TurnReadError},
        },
    },
    storage::{
        AsyncTranscriptAdapter, Clock, ProjectKey, SessionStorage, SessionStorageInput,
        TranscriptRead, sanitize_session_id,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs,
    future::Future,
    io,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type ForkEntryTransform = Arc<dyn Fn(JsValue) -> JsValue + Send + Sync>;
pub type ForkFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ReplacementError>> + Send + 'a>>;

#[derive(Clone)]
pub struct ProjectSessionForkRequest {
    pub project_root: PathBuf,
    pub pilot_home: PathBuf,
    pub source_session_id: JsString,
    pub target_session_id: JsString,
    pub entries: Vec<JsValue>,
    pub transform_auxiliary_entry: Option<ForkEntryTransform>,
}

pub trait ProjectSessionForkPort: Send + Sync {
    fn fork(&self, request: ProjectSessionForkRequest) -> ForkFuture<'_, ()>;
}

struct NativeProjectSessionForkPort {
    adapter: Arc<AsyncTranscriptAdapter>,
    now: Clock,
}

pub trait ExecutionStorageFork: Send + Sync {
    fn retarget_entry(
        &self,
        entry: JsValue,
        source_session_id: &JsString,
        target_session_id: &JsString,
    ) -> JsValue {
        let _ = (source_session_id, target_session_id);
        entry
    }
    fn commit(&self) -> ForkFuture<'_, ()>;
    fn rollback(&self) -> ForkFuture<'_, ()>;
}

pub trait ExecutionStorageForkPort: Send + Sync {
    fn begin(
        &self,
        source_session_id: JsString,
        target_session_id: JsString,
    ) -> ForkFuture<'_, Arc<dyn ExecutionStorageFork>>;
}

/// One host-owned adapter and clock; no parallel session cache or turn state.
pub struct HistoryStorage {
    adapter: Arc<AsyncTranscriptAdapter>,
    pilot_home: PathBuf,
    fallback_project_root: PathBuf,
    now: Clock,
    max_context_tokens: Option<f64>,
    max_output_tokens: Option<f64>,
    session_fork_port: Option<Arc<dyn ProjectSessionForkPort>>,
    execution_fork_port: Option<Arc<dyn ExecutionStorageForkPort>>,
    edits: Mutex<()>,
}

impl HistoryStorage {
    pub fn new(
        adapter: Arc<AsyncTranscriptAdapter>,
        pilot_home: PathBuf,
        fallback_project_root: PathBuf,
        now: Clock,
        max_context_tokens: Option<f64>,
        max_output_tokens: Option<f64>,
    ) -> Self {
        let session_fork_port = Arc::new(NativeProjectSessionForkPort {
            adapter: adapter.clone(),
            now: now.clone(),
        });
        Self {
            adapter,
            pilot_home,
            fallback_project_root,
            now,
            max_context_tokens,
            max_output_tokens,
            session_fork_port: Some(session_fork_port),
            execution_fork_port: None,
            edits: Mutex::new(()),
        }
    }

    pub fn with_fork_ports(
        mut self,
        session: Option<Arc<dyn ProjectSessionForkPort>>,
        execution: Option<Arc<dyn ExecutionStorageForkPort>>,
    ) -> Self {
        self.session_fork_port = session;
        self.execution_fork_port = execution;
        self
    }

    pub fn with_execution_fork_port(
        mut self,
        execution: Arc<dyn ExecutionStorageForkPort>,
    ) -> Self {
        self.execution_fork_port = Some(execution);
        self
    }

    pub fn with_session_fork_port(mut self, session: Arc<dyn ProjectSessionForkPort>) -> Self {
        self.session_fork_port = Some(session);
        self
    }

    /// Install these ports in the SAME ReplacementCoordinator used by submit.
    pub fn replacement_storage(self: &Arc<Self>) -> ReplacementStorage {
        let fork = self.clone();
        let prepare = self.clone();
        let finalize = self.clone();
        ReplacementStorage {
            fork_session: Some(Arc::new(move |input| {
                let fork = fork.clone();
                Box::pin(async move { fork.fork(&input).await })
            })),
            replace_last_turn: Some(Arc::new(move |input| {
                let result = prepare.prepare(&input);
                Box::pin(async move { result })
            })),
            finalize_last_turn_replacement: Some(Arc::new(move |input| {
                let result = finalize.finalize(&input);
                Box::pin(async move { result })
            })),
        }
    }

    fn project(&self, input: &JsValue) -> Result<ProjectKey, ReplacementError> {
        let root = match input.get("projectKey") {
            None | Some(JsValue::Undefined | JsValue::Null) => self.fallback_project_root.clone(),
            Some(JsValue::String(s)) => PathBuf::from(s.to_utf8().map_err(failure)?),
            _ => return Err(error("INVALID_INPUT", "projectKey must be a string.")),
        };
        if root.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(error("INVALID_INPUT", "projectKey contains a NUL byte."));
        }
        Ok(ProjectKey {
            project_root: crate::metadata::resolve_process_path(Path::new(""), &root),
            pilot_home: crate::metadata::resolve_process_path(Path::new(""), &self.pilot_home),
        })
    }

    fn storage(&self, input: &JsValue) -> Result<SessionStorage, ReplacementError> {
        let key = required(input, "sessionKey", "INVALID_INPUT")?;
        let safe = sanitize_session_id(&key);
        if safe == "." || safe == ".." || safe.contains('\0') {
            return Err(error("INVALID_INPUT", "Invalid sessionKey."));
        }
        let project = self.project(input)?;
        let storage = self.adapter.create_session_storage(SessionStorageInput {
            project_root: project.project_root,
            pilot_home: project.pilot_home,
            session_id: key,
            now: Some(self.now.clone()),
        });
        confined(&storage.chat_dir, &storage.transcript_path)?;
        Ok(storage)
    }

    fn read_entries(
        &self,
        storage: &SessionStorage,
        path: &Path,
    ) -> Result<TranscriptRead, ReplacementError> {
        confined(&storage.chat_dir, path)?;
        storage.read_transcript_at_path(path, None).map_err(failure)
    }

    pub fn list_sessions(&self, input: &JsValue) -> ReplacementResult {
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let project = self.project(input)?;
        let offset = cursor(input, false);
        let limit = limit(input)?;
        let sessions = if let Some(result) =
            self.adapter.list_sessions(&project, limit, Some(offset))
        {
            result.map_err(failure)?
        } else {
            let chat = self.adapter.project_chat_dir(&project);
            let mut sessions = Vec::new();
            let entries = match fs::read_dir(&chat) {
                Ok(entries) => Some(entries),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(failure(e)),
            };
            for entry in entries.into_iter().flatten() {
                let entry = entry.map_err(failure)?;
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "jsonl")
                    || !entry.file_type().map_err(failure)?.is_file()
                {
                    continue;
                }
                confined(&chat, &path)?;
                let id = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| error("INVALID_INPUT", "Invalid transcript filename."))?;
                let request = object([
                    ("sessionKey", id.into()),
                    (
                        "projectKey",
                        project.project_root.to_string_lossy().as_ref().into(),
                    ),
                ]);
                let storage = self.storage(&request)?;
                self.recover(&storage)?;
                let read = self.read_entries(&storage, &path)?;
                if read.entries.is_empty() || read.diagnostics.iter().any(|d| d.severity == "error")
                {
                    continue;
                }
                let info = projection::session_info(
                    &read.entries,
                    id,
                    &project.project_root,
                    Some(&entry.metadata().map_err(failure)?),
                );
                // Internal session lineages are not part of the user-facing catalog.
                if projection::internal(id)
                    || !projection::catalog_visible(&read.entries)
                    || field(&info, "summary")
                        .as_string()
                        .is_none_or(|s| s.is_empty())
                {
                    continue;
                }
                sessions.push(info);
            }
            sessions
                .sort_by(|a, b| number(b, "lastModified").total_cmp(&number(a, "lastModified")));
            page(&sessions, offset, limit)
        };
        let next = limit
            .filter(|n| *n != 0.0 && sessions.len() as f64 == *n)
            .map(|_| {
                js(pilotdeck_protocol::js_value::number_string(
                    offset + sessions.len() as f64,
                ))
            })
            .unwrap_or(JsValue::Undefined);
        Ok(object([
            ("sessions", JsValue::Array(sessions)),
            ("nextCursor", next),
        ]))
    }

    pub fn read_session_messages(&self, input: &JsValue) -> ReplacementResult {
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let storage = self.storage(input)?;
        self.recover(&storage)?;
        let path = transcript_path(input, &storage)?;
        let entries = self.read_entries(&storage, &path)?.entries;
        let key = required(input, "sessionKey", "INVALID_INPUT")?;
        let key = key.to_utf8().map_err(failure)?;
        let mut messages = projection::messages(
            &entries,
            &key,
            input.get("projectKey"),
            &(self.now)(),
            false,
            self.project(input)?.project_root
                != crate::metadata::resolve_process_path(Path::new(""), &self.pilot_home),
        );
        let total = messages.len();
        messages = page(&messages, cursor(input, true), limit(input)?);
        let offset = cursor(input, true) as usize;
        let next = limit(input)?
            .filter(|n| *n != 0.0 && offset.saturating_add(messages.len()) < total)
            .map(|_| js(offset.saturating_add(messages.len()).to_string()))
            .unwrap_or(JsValue::Undefined);
        let background = is(input, "sessionKind", "background_task")
            && !text(input, "relativeTranscriptPath").is_empty();
        let mut session = projection::session_info(
            &entries,
            &sanitize_session_id(&key.clone().into()),
            &self.project(input)?.project_root,
            fs::metadata(&path).ok().as_ref(),
        )
        .as_object()
        .cloned()
        .unwrap_or_default();
        if background {
            session = object([
                ("sessionId", key.as_str().into()),
                ("summary", key.as_str().into()),
                ("lastModified", JsValue::Number(0.0)),
            ])
            .as_object()
            .cloned()
            .unwrap();
        }
        session.insert("sessionKey".into(), key.as_str().into());
        for name in ["parentSessionId", "relativeTranscriptPath", "sessionKind"] {
            if let Some(value) = input.get(name) {
                session.insert(name.into(), value.clone());
            }
        }
        Ok(object([
            ("messages", JsValue::Array(messages)),
            ("nextCursor", next),
            ("total", JsValue::Number(total as f64)),
            (
                "tokenUsage",
                projection::token_usage(&entries, self.max_context_tokens, self.max_output_tokens),
            ),
            ("session", JsValue::Object(session)),
        ]))
    }

    pub fn read_subagent_messages(&self, input: &JsValue) -> ReplacementResult {
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let mut parent = input
            .as_object()
            .cloned()
            .ok_or_else(|| error("INVALID_INPUT", "Expected request object."))?;
        if let Some(id) = input
            .get("parentSessionId")
            .filter(|v| v.as_string().is_some())
        {
            parent.insert("sessionKey".into(), id.clone());
        }
        let storage = self.storage(&JsValue::Object(parent))?;
        self.recover(&storage)?;
        let path = transcript_path(input, &storage)?;
        let read = self.read_entries(&storage, &path)?;
        let subagent = required(input, "subagentId", "INVALID_INPUT")?;
        let reference = read.entries.iter().rev().find(|entry| {
            is(entry, "type", "subagent_started")
                && field(entry, "subagentId").as_string() == Some(&subagent)
        });
        let messages = if let Some(reference) = reference {
            let relative = text(reference, "transcriptRelativePath");
            if relative.is_empty() && field(reference, "subagentSessionId").as_string().is_none() {
                Vec::new()
            } else {
                let entries = if relative.is_empty() {
                    let mut child_input = input.as_object().cloned().unwrap_or_default();
                    child_input.insert(
                        "sessionKey".into(),
                        field(reference, "subagentSessionId").clone(),
                    );
                    let child_storage = self.storage(&JsValue::Object(child_input))?;
                    self.read_entries(&child_storage, &child_storage.transcript_path)?
                        .entries
                } else {
                    let child = relative_path(
                        &relative,
                        path.parent().unwrap_or(&storage.chat_dir),
                        &storage.chat_dir,
                    )?;
                    self.read_entries(&storage, &child)?.entries
                };
                let key = format!(
                    "{}::sub::{}",
                    text(input, "sessionKey"),
                    subagent.to_utf8().map_err(failure)?
                );
                projection::messages(
                    &entries,
                    &key,
                    input.get("projectKey"),
                    &(self.now)(),
                    true,
                    false,
                )
            }
        } else {
            Vec::new()
        };
        let total = messages.len();
        Ok(object([
            ("messages", JsValue::Array(messages)),
            ("total", JsValue::Number(total as f64)),
        ]))
    }
}

impl SessionHistoryReader for HistoryStorage {
    fn read(&self, input: &JsValue) -> BoxFuture<Result<JsValue, TurnReadError>> {
        let result = self
            .read_session_messages(input)
            .map_err(|e| TurnReadError {
                code: e.code.map(str::to_owned),
                name: "Error".into(),
                message: e.message.into(),
            });
        Box::pin(async move { result })
    }
}

pub struct HistoryRoutes {
    storage: Arc<HistoryStorage>,
    turns: Arc<TurnEventCoordinator>,
    replacements: ReplacementCoordinator,
}
impl HistoryRoutes {
    pub fn new(
        storage: Arc<HistoryStorage>,
        turns: Arc<TurnEventCoordinator>,
        replacements: ReplacementCoordinator,
    ) -> Self {
        Self {
            storage,
            turns,
            replacements,
        }
    }
    pub async fn handle(&self, method: &str, params: &JsValue) -> Option<ReplacementResult> {
        Some(match method {
            "list_sessions" => self.storage.list_sessions(params),
            "read_session_messages" => {
                read::read_session_messages(&self.turns, Some(self.storage.as_ref()), params)
                    .await
                    .map_err(|e| ReplacementError {
                        code: match e.code.as_deref() {
                            Some("INVALID_INPUT") => Some("INVALID_INPUT"),
                            _ => None,
                        },
                        message: e.to_string(),
                        details: None,
                    })
            }
            "read_subagent_messages" => self.storage.read_subagent_messages(params),
            "fork_session" => self.replacements.fork_session(params.clone()).await,
            "replace_last_turn" => self.replacements.replace_last_turn(params.clone()).await,
            "finalize_last_turn_replacement" => {
                self.replacements
                    .finalize_last_turn_replacement(params.clone())
                    .await
            }
            _ => return None,
        })
    }
}

fn js(value: impl Into<JsString>) -> JsValue {
    JsValue::String(value.into())
}
fn error(code: &'static str, message: impl Into<String>) -> ReplacementError {
    ReplacementError {
        code: Some(code),
        message: message.into(),
        details: None,
    }
}
fn failure(e: impl std::fmt::Display) -> ReplacementError {
    ReplacementError::new(e.to_string())
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(name).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue, name: &str) -> String {
    field(value, name)
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default()
}
fn is(value: &JsValue, name: &str, expected: &str) -> bool {
    field(value, name)
        .as_string()
        .is_some_and(|s| s.equals_str(expected))
}
fn number(value: &JsValue, name: &str) -> f64 {
    field(value, name).as_number().unwrap_or(0.0)
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(a) => a,
        _ => &[],
    }
}
fn required(value: &JsValue, name: &str, code: &'static str) -> Result<JsString, ReplacementError> {
    field(value, name)
        .as_string()
        .filter(|s| {
            s.units()
                .iter()
                .any(|u| !char::from_u32(*u as u32).is_some_and(crate::metadata::js_space))
        })
        .cloned()
        .ok_or_else(|| error(code, format!("{name} is required.")))
}
fn cursor(input: &JsValue, nonnegative: bool) -> f64 {
    let value = text(input, "cursor");
    let value = value.trim_start_matches(crate::metadata::js_space);
    let digits = value
        .strip_prefix(['+', '-'])
        .unwrap_or(value)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let parsed =
        digits.parse::<f64>().unwrap_or(0.0) * if value.starts_with('-') { -1.0 } else { 1.0 };
    if !parsed.is_finite() || nonnegative && parsed < 0.0 {
        0.0
    } else {
        parsed
    }
}
fn limit(input: &JsValue) -> Result<Option<f64>, ReplacementError> {
    match input.get("limit") {
        None | Some(JsValue::Undefined | JsValue::Null) => Ok(None),
        Some(JsValue::Number(n)) if n.is_finite() => Ok(Some(*n)),
        _ => Err(error("INVALID_INPUT", "limit must be a finite number.")),
    }
}
fn page(items: &[JsValue], offset: f64, limit: Option<f64>) -> Vec<JsValue> {
    let offset = offset.max(0.0);
    let index = |n: f64| {
        let n = n.trunc();
        if n < 0.0 {
            (items.len() as f64 + n).max(0.0) as usize
        } else {
            n.min(items.len() as f64) as usize
        }
    };
    let start = index(offset);
    let end = limit
        .filter(|n| *n != 0.0)
        .map_or(items.len(), |n| index(offset + n));
    items[start..end.max(start)].to_vec()
}

fn transcript_path(input: &JsValue, storage: &SessionStorage) -> Result<PathBuf, ReplacementError> {
    if is(input, "sessionKind", "background_task")
        && !text(input, "relativeTranscriptPath").is_empty()
    {
        relative_path(
            &text(input, "relativeTranscriptPath"),
            &storage.chat_dir,
            &storage.chat_dir,
        )
    } else {
        Ok(storage.transcript_path.clone())
    }
}
fn relative_path(relative: &str, base: &Path, root: &Path) -> Result<PathBuf, ReplacementError> {
    if relative.is_empty() || Path::new(relative).is_absolute() || relative.contains('\0') {
        return Err(failure("relativeTranscriptPath must be a relative path."));
    }
    let candidate = resolve(base, relative);
    if candidate.extension().is_none_or(|e| e != "jsonl") {
        return Err(failure(
            "relativeTranscriptPath points outside the project transcript directory.",
        ));
    }
    confined(root, &candidate)?;
    Ok(candidate)
}
// Check each existing component as well as the lexical path: symlinks cannot escape.
fn confined(root: &Path, candidate: &Path) -> Result<(), ReplacementError> {
    let root = crate::metadata::resolve_process_path(Path::new(""), root);
    let candidate = crate::metadata::resolve_process_path(Path::new(""), candidate);
    let relative = candidate.strip_prefix(&root).map_err(|_| {
        failure("relativeTranscriptPath points outside the project transcript directory.")
    })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(failure("Invalid transcript path."));
    }
    if fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(failure("Transcript directory must not be a symbolic link."));
    }
    let mut path = root;
    for component in relative.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(failure(
                    "Transcript paths must not traverse symbolic links.",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failure(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "history/tests.rs"]
mod tests;
