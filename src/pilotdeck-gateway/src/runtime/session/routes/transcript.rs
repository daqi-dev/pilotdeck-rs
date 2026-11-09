//! Portable transcript controls. The host supplies the same owner, storage
//! layout and live checkpoints used by submit; no second session pool exists.
use crate::{
    config::Error,
    host::modules::tool_context::HostToolCheckpoint,
    runtime::session::SessionOwner,
    storage::{
        AdapterError, AsyncTranscriptAdapter, JsonlTranscriptStore, LedgerError, SessionStorage,
        SessionStorageInput,
        recorder::{Recorder, RecorderError},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::os::unix::fs::DirBuilderExt;
use std::{
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

#[path = "transcript/portable.rs"]
mod portable;
#[path = "transcript/seed.rs"]
mod seed;
#[cfg(test)]
#[path = "transcript/tests.rs"]
mod tests;

pub type TranscriptFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// A clone of the live session's checkpoint (its Arc-backed maps retain their
/// identity). The host must use it for the next turn's seedState and snapshot.
pub struct SeedReadTarget {
    pub cwd: JsString,
    /// Effective native read roots, including the configured im-attachments
    /// root when that native capability is enabled (never resolve user HOME).
    pub additional_working_directories: Vec<JsString>,
    pub bypass_permissions: bool,
    pub checkpoint: HostToolCheckpoint,
}

/// Mandatory native ports, not optional success fallbacks. Implementations must
/// serialize lifecycle changes with the same SessionOwner supplied to new().
pub trait TranscriptHost: Send + Sync {
    /// Resolve params.projectKey (or the configured fallback) via the registry.
    fn resolve_project(&self, params: &JsValue) -> Result<PathBuf, Error>;
    /// Must use session_chat_dir(pilot_home, cwd, project_root)/<sanitized>.jsonl,
    /// exactly as NativeSubmitHost::transcript_path. Never use a separate store.
    fn storage(&self, project_root: &Path, session_key: &JsString)
    -> Result<SessionStorage, Error>;
    /// Apply workspaceCwd and validated sdkSessionConfig, then get/create the
    /// real session while the caller holds owner maintenance admission. Use
    /// channelKey ?? "api_server"; do not attempt to acquire maintenance again.
    fn prepare_seed<'a>(&'a self, params: &'a JsValue) -> TranscriptFuture<'a, SeedReadTarget>;
    /// Append through the exact live writer and update its projections before
    /// resolving. Some(false) is a live writer's refusal; only None means cold.
    /// The host must fence live/cold selection against publication/eviction.
    fn record_live_status<'a>(&'a self, params: &'a JsValue) -> TranscriptFuture<'a, Option<bool>>;
}

pub struct TranscriptRoutes {
    owner: SessionOwner,
    host: Arc<dyn TranscriptHost>,
    writes: tokio::sync::Mutex<()>,
}

impl TranscriptRoutes {
    pub fn new(owner: SessionOwner, host: Arc<dyn TranscriptHost>) -> Self {
        Self {
            owner,
            host,
            writes: tokio::sync::Mutex::new(()),
        }
    }

    /// None only means this module does not own the method. All business and
    /// storage failures remain Some(Err), so the caller cannot fall back to TS.
    pub async fn handle(&self, method: &str, params: &JsValue) -> Option<Result<JsValue, Error>> {
        if !matches!(
            method,
            "export_session_transcript"
                | "restore_session_transcript"
                | "seed_read_state"
                | "record_agent_status_message"
        ) {
            return None;
        }
        let _write = self.writes.lock().await;
        Some(match method {
            "export_session_transcript" => self.export(params),
            "restore_session_transcript" => self.restore(params),
            "seed_read_state" => self.seed(params).await,
            "record_agent_status_message" => self.status(params).await,
            _ => unreachable!(),
        })
    }

    fn export(&self, params: &JsValue) -> Result<JsValue, Error> {
        let project = self.host.resolve_project(params)?;
        let session = required_string(
            params,
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        if self.owner.has_active_turn(session) {
            return Err(gateway(
                "SESSION_BUSY",
                "Cannot export a transcript while its session has an active turn.",
            ));
        }
        let storage = self.host.storage(&project, session)?;
        check_storage(&storage)?;
        let transcript = storage.read_transcript(None).map_err(adapter_error)?;
        if transcript
            .diagnostics
            .iter()
            .any(|d| d.code == "transcript_missing")
        {
            return Err(gateway(
                "SESSION_NOT_FOUND",
                format!("Session not found: {}", text(session)),
            ));
        }
        if transcript.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(gateway(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot export a transcript with invalid entries.",
            ));
        }
        let archive = portable::project(&transcript.entries)?;
        portable::validate(&archive)?;
        Ok(archive)
    }

    fn restore(&self, params: &JsValue) -> Result<JsValue, Error> {
        let project = self.host.resolve_project(params)?;
        let session = required_string(
            params,
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        if self.owner.has_active_turn(session) || self.owner.current_handle(session).is_some() {
            return Err(gateway(
                "SESSION_BUSY",
                "Cannot restore over an active Gateway session.",
            ));
        }
        let _maintenance = self
            .owner
            .begin_maintenance(session)
            .map_err(|e| plain("Error", e.message))?
            .ok_or_else(|| {
                gateway(
                    "SESSION_RESTORE_CONFLICT",
                    "A transcript restore is already in progress for this session.",
                )
            })?;
        let archive = field(params, "archive");
        let messages = portable::validate(archive)?;
        let storage = self.host.storage(&project, session)?;
        check_storage(&storage)?;
        match fs::symlink_metadata(&storage.transcript_path) {
            Ok(_) => return Err(restore_conflict()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e, "stat", &storage.transcript_path)),
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&storage.chat_dir)
            .map_err(|e| io_error(e, "mkdir", &storage.chat_dir))?;
        // Stage through the existing writer, so event envelopes/sequence/parent
        // links use the native protocol. Publish without replacing any winner.
        let directory = storage.chat_dir.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |_| directory.clone()),
        );
        let staged = adapter
            .create_session_storage(SessionStorageInput {
                project_root: project,
                pilot_home: storage.chat_dir.clone(),
                session_id: format!("{}.restore.tmp", uuid::Uuid::new_v4()).into(),
                now: Some(Arc::new(now)),
            })
            .map_err(adapter_error)?;
        let _temporary = Temporary(staged.transcript_path.clone());
        let turn: JsString = format!("sdk:restore:{}", uuid::Uuid::new_v4()).into();
        let title = field(archive, "title");
        if truthy(title) {
            staged
                .transcript
                .record_session_metadata(
                    session,
                    &turn,
                    object([
                        ("title", title.clone()),
                        ("updatedAt", JsValue::String(now().into())),
                    ]),
                )
                .map_err(adapter_error)?;
        }
        let canonical = messages
            .iter()
            .map(|message| {
                object([
                    ("role", field(message, "role").clone()),
                    (
                        "content",
                        JsValue::Array(vec![object([
                            ("type", "text".into()),
                            ("text", field(message, "text").clone()),
                        ])]),
                    ),
                ])
            })
            .collect();
        staged
            .transcript
            .record_accepted_input(
                session,
                &turn,
                canonical,
                Some(fields([
                    ("restoredFromGatewayArchive", JsValue::Bool(true)),
                    ("archiveSchemaVersion", JsValue::Number(1.0)),
                ])),
            )
            .map_err(adapter_error)?;
        staged.restore().map_err(adapter_error)?;
        fs::File::open(&staged.transcript_path)
            .and_then(|f| f.sync_all())
            .map_err(|e| io_error(e, "fsync", &staged.transcript_path))?;
        fs::hard_link(&staged.transcript_path, &storage.transcript_path).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                restore_conflict()
            } else {
                io_error(e, "link", &storage.transcript_path)
            }
        })?;
        Ok(object([
            ("sessionKey", JsValue::String(session.clone())),
            ("importedMessages", JsValue::Number(messages.len() as f64)),
        ]))
    }

    async fn seed(&self, params: &JsValue) -> Result<JsValue, Error> {
        let session = required_string(
            params,
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        let path = required_string(params, "path", "INVALID_FILE_PATH", "path is required.")?;
        let mtime = field(params, "mtime")
            .as_number()
            .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 0.0)
            .ok_or_else(|| {
                gateway(
                    "INVALID_FILE_MTIME",
                    "mtime must be a non-negative integer in milliseconds.",
                )
            })?;
        if self.owner.has_active_turn(session) {
            return Err(gateway(
                "SESSION_BUSY",
                "Cannot seed file read state while a turn is active.",
            ));
        }
        let _maintenance = self
            .owner
            .begin_maintenance(session)
            .map_err(|e| plain("Error", e.message))?
            .ok_or_else(|| {
                plain(
                    "Error",
                    "Cannot seed file read state while a turn or session control is active.",
                )
            })?;
        let target = self.host.prepare_seed(params).await?;
        if self.owner.has_active_turn(session) {
            return Err(plain(
                "Error",
                "Cannot seed file read state while a turn is active.",
            ));
        }
        let applied = seed::apply(&target, path, mtime)?;
        Ok(object([("applied", JsValue::Bool(applied))]))
    }

    async fn status(&self, params: &JsValue) -> Result<JsValue, Error> {
        if let Some(recorded) = self.host.record_live_status(params).await? {
            return Ok(object([("recorded", JsValue::Bool(recorded))]));
        }
        let project = self.host.resolve_project(params)?;
        // Native writer envelope validation supplies missing/non-string IDs.
        let session = field(params, "sessionKey")
            .as_string()
            .ok_or_else(|| event_id_error("sessionId"))?;
        let turn = field(params, "turnId")
            .as_string()
            .ok_or_else(|| event_id_error("turnId"))?;
        let _maintenance = self
            .owner
            .begin_maintenance(session)
            .map_err(|e| plain("Error", e.message))?
            .ok_or_else(|| {
                plain(
                    "Error",
                    "Cannot append cold status while a turn or session control is active.",
                )
            })?;
        if self.owner.current_handle(session).is_some() {
            return Err(plain(
                "Error",
                "Live status port returned a cold result for a published session.",
            ));
        }
        let storage = self.host.storage(&project, session)?;
        check_storage(&storage)?;
        let read = storage.read_transcript(None).map_err(adapter_error)?;
        // A corrupt log must not turn into an empty successful cold append.
        if read.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(gateway(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot append a status to a transcript with invalid entries.",
            ));
        }
        portable::replay(&read.entries)?;
        let recorder = Recorder::from_writer(Arc::new(storage.transcript));
        recorder.restore(&read.entries).map_err(recorder_error)?;
        let status = field(params, "status");
        property(status, "event")?;
        recorder
            .writer()
            .record_agent_status_message(session, turn, status)
            .map_err(adapter_error)?;
        Ok(object([("recorded", JsValue::Bool(true))]))
    }
}

fn restore_conflict() -> Error {
    gateway(
        "SESSION_RESTORE_CONFLICT",
        "A persistent transcript already exists for this session.",
    )
}
fn event_id_error(key: &str) -> Error {
    named(
        "SessionEventValidationError",
        &format!(
            "invalid_{}",
            if key == "sessionId" {
                "session_id"
            } else {
                "turn_id"
            }
        ),
        format!("Session event {key} must be a non-empty string."),
    )
}
fn check_storage(storage: &SessionStorage) -> Result<(), Error> {
    if !storage.chat_dir.is_absolute()
        || storage.transcript_path.parent() != Some(storage.chat_dir.as_path())
    {
        return Err(gateway(
            "path_not_allowed",
            "Transcript path is outside the configured session storage.",
        ));
    }
    for path in [&storage.chat_dir, &storage.transcript_path] {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(gateway(
                    "path_not_allowed",
                    "Transcript storage must not follow symbolic links.",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e, "stat", path)),
        }
    }
    Ok(())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn now() -> String {
    let epoch_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(error) => -(error.duration().as_millis() as i64),
    };
    pilotdeck_transport::system_clock::format_epoch_iso(epoch_ms)
}
fn named(name: &'static str, code: &str, message: impl Into<JsString>) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
        diagnostics: vec![],
        name,
        requires_host: false,
    }
}
fn gateway(code: &str, message: impl Into<JsString>) -> Error {
    named("DialogGatewayError", code, message)
}
fn plain(name: &'static str, message: impl Into<JsString>) -> Error {
    named(name, "gateway_request_failed", message)
}
fn io_error(error: io::Error, operation: &str, path: &Path) -> Error {
    let (code, reason) = match error.kind() {
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        _ => {
            return plain(
                "Error",
                format!("{operation} '{}': {error}", path.display()),
            );
        }
    };
    named(
        "Error",
        code,
        format!("{code}: {reason}, {operation} '{}'", path.display()),
    )
}
fn ledger_error(error: LedgerError) -> Error {
    match error {
        LedgerError::Io(e) => {
            let mut result = plain("Error", e.to_string());
            result.code = match e.kind() {
                io::ErrorKind::NotFound => "ENOENT",
                io::ErrorKind::PermissionDenied => "EACCES",
                io::ErrorKind::NotADirectory => "ENOTDIR",
                io::ErrorKind::IsADirectory => "EISDIR",
                _ => "gateway_request_failed",
            }
            .into();
            result
        }
        other => plain("Error", other.to_string()),
    }
}
fn adapter_error(error: AdapterError) -> Error {
    match error {
        AdapterError::Validation { code, message } => {
            named("SessionEventValidationError", code, message)
        }
        AdapterError::Type(message) => plain("TypeError", message),
        AdapterError::Range(message) => plain("RangeError", message),
        AdapterError::Store(error) => ledger_error(error),
        AdapterError::Domain { code, message } => {
            named("SessionDomainValidationError", code, message)
        }
        AdapterError::ProjectionUnavailable(message) | AdapterError::Retired(message) => {
            plain("Error", message)
        }
    }
}
fn recorder_error(error: RecorderError) -> Error {
    match error {
        RecorderError::Domain { code, message } => {
            named("SessionDomainValidationError", code, message)
        }
        RecorderError::Envelope { code, message } => {
            named("SessionEventValidationError", code, message)
        }
        RecorderError::Type(message) => plain("TypeError", message),
        RecorderError::Range(message) => plain("RangeError", message),
        RecorderError::Store(error) => ledger_error(error),
        other => plain("Error", other.to_string()),
    }
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn property<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue, Error> {
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        return Err(plain(
            "TypeError",
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
    Ok(field(value, key))
}
fn required_string<'a>(
    value: &'a JsValue,
    key: &str,
    code: &str,
    message: &str,
) -> Result<&'a JsString, Error> {
    let value = property(value, key)?;
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        return Err(gateway(code, message.to_owned()));
    }
    let string = value
        .as_string()
        .ok_or_else(|| plain("TypeError", format!("input.{key}?.trim is not a function")))?;
    if trim(string).is_empty() {
        return Err(gateway(code, message.to_owned()));
    }
    Ok(string)
}
fn trim(value: &JsString) -> JsString {
    let whitespace = |u: &u16| matches!(*u, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
    let start = value
        .units()
        .iter()
        .position(|u| !whitespace(u))
        .unwrap_or(value.len_utf16());
    let end = value
        .units()
        .iter()
        .rposition(|u| !whitespace(u))
        .map_or(start, |i| i + 1);
    JsString::from_utf16(value.units()[start..end].to_vec())
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn is(value: &JsValue, expected: &str) -> bool {
    value.as_string().is_some_and(|v| v.equals_str(expected))
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
fn fields<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    out
}
fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(fields(entries))
}
