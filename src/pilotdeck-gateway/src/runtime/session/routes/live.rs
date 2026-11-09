//! Real session-to-module-host injection boundary.
//!
//! Integration: register this module in the session routes module. A production
//! wrapper must own this state and call `module_host::with_live_state` around
//! the synchronous `inner.submit`; the module router retains its Arc-backed
//! ports after that scope ends. This is not a session registry or a factory.
//!
//! Recorder::complete_turn closes the real recording domain. Factory/wrapper
//! and TranscriptHost assembly belong to the integration owner; all producers
//! must share this state's writer, never construct a second writer for its path.
//!
//! A future factory must bind its wrapper to the exact handle published by the
//! existing owner, use the injected adapter's native transcript path, and retain
//! only weak identity-checked lookup entries. Cold status append must remain
//! inside a publication/eviction fence through commit; current_handle followed
//! by is_current alone does not establish that fence.

use crate::{
    host::modules::{event::EventSink, tool_context::HostToolCheckpoint},
    runtime::project::RuntimeError,
    storage::{SessionStorage, recorder::Recorder},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

/// Actual shared host ports. EventSink persists and projects before the
/// original EventBuffer acknowledges either event entry path.
pub struct LiveSessionState {
    pub session_key: JsString,
    pub cwd: JsString,
    /// Include this token in the wrapper snapshot; compare it with the owner's
    /// current handle before selecting a weak registry entry.
    pub identity: JsString,
    pub checkpoint: HostToolCheckpoint,
    pub recorder: Arc<Recorder>,
    pub file_history: Arc<crate::storage::file_history::FileHistoryStore>,
    projection: Mutex<Projection>,
    disposed: AtomicBool,
    failure: Mutex<Option<RuntimeError>>,
}

#[derive(Default)]
struct Projection {
    messages: Vec<JsValue>,
    statuses: Vec<JsValue>,
    metadata: JsObject,
    metadata_tail_turn: Option<JsString>,
}

impl LiveSessionState {
    pub(crate) fn for_subagent(
        session_key: JsString,
        cwd: JsString,
        writer: Arc<crate::storage::TranscriptWriter>,
        checkpoint: HostToolCheckpoint,
        file_history: Arc<crate::storage::file_history::FileHistoryStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_key,
            cwd,
            identity: uuid::Uuid::new_v4().to_string().into(),
            checkpoint,
            recorder: Arc::new(Recorder::from_writer(writer)),
            file_history,
            projection: Mutex::default(),
            disposed: AtomicBool::new(false),
            failure: Mutex::new(None),
        })
    }

    pub fn new(
        session_key: JsString,
        cwd: JsString,
        storage: SessionStorage,
    ) -> Result<Arc<Self>, RuntimeError> {
        let read = storage.read_transcript(None).map_err(failed)?;
        if read.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(RuntimeError::new("Session transcript is invalid."));
        }
        let backup_dir = storage.file_history_dir.clone();
        let restored_snapshots =
            crate::runtime::rewind::snapshot_entries(&read.entries).map_err(failed)?;
        let recorder = Arc::new(Recorder::from_writer(Arc::new(storage.transcript)));
        recorder.restore(&read.entries).map_err(failed)?;
        let file_history = crate::storage::file_history::create_recorded_file_history(
            crate::storage::file_history::FileHistoryOptions::new(backup_dir),
            recorder.clone(),
            session_key.clone(),
            &restored_snapshots,
        );
        let mut projection = Projection::default();
        for entry in &read.entries {
            project_entry(&mut projection, entry);
        }
        Ok(Arc::new(Self {
            session_key,
            cwd,
            identity: uuid::Uuid::new_v4().to_string().into(),
            checkpoint: HostToolCheckpoint::new(None, &[]).map_err(failed)?,
            recorder,
            file_history,
            projection: Mutex::new(projection),
            disposed: AtomicBool::new(false),
            failure: Mutex::new(None),
        }))
    }

    pub fn record_status(&self, turn: &JsString, status: &JsValue) -> Result<bool, RuntimeError> {
        self.check_error()?;
        let mut projection = self.projection.lock().map_err(failed)?;
        if self.disposed.load(Ordering::SeqCst) {
            return Ok(false);
        }
        self.recorder
            .writer()
            .record_agent_status_message(&self.session_key, turn, status)
            .map_err(failed)?;
        projection.statuses.push(status.clone());
        Ok(true)
    }

    /// Record before forwarding an event to the sidecar/gateway consumer.
    /// Model/tool durability is owned by module_host ports, not duplicated here.
    pub fn record_event(&self, event: &JsValue) -> Result<(), RuntimeError> {
        self.check_error()?;
        let turn = event.get("turnId").and_then(JsValue::as_string);
        if event
            .get("sessionId")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s != &self.session_key)
        {
            return Err(RuntimeError::new("Session event identity mismatch."));
        }
        let kind = event
            .get("type")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_default();
        if kind == "context_budget" {
            use crate::runtime::turn::projector::map_agent_event;
            let turn =
                turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?;
            let run = turn.to_utf8().map_err(failed)?;
            // The facade reuses the process-level default projector, matching
            // the TS `mapAgentEvent` owner; a per-call projector would fork it.
            let frames = map_agent_event(
                event,
                &run,
                crate::runtime::turn::projector::GatewayAgentEventProjectorOptions::default(),
            )
            .map_err(failed)?;
            for detail in frames {
                let status = crate::metadata::object([
                    ("event", "context_budget".into()),
                    ("kind", "status".into()),
                    ("text", "context_budget".into()),
                    ("detail", detail),
                ]);
                if !self.record_status(turn, &status)? {
                    return Err(RuntimeError::new("Session writer is disposed."));
                }
            }
            return Ok(());
        }
        if kind == "agent_status" {
            return self
                .record_status(
                    turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?,
                    event,
                )
                .and_then(|recorded| {
                    if recorded {
                        Ok(())
                    } else {
                        Err(RuntimeError::new("Session writer is disposed."))
                    }
                });
        }
        let mut projection = self.projection.lock().map_err(failed)?;
        if self.disposed.load(Ordering::SeqCst) {
            return Err(RuntimeError::new("Session writer is disposed."));
        }
        match kind.as_str() {
            "turn_started" => self
                .recorder
                .start_turn(
                    &self.session_key,
                    turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?,
                )
                .map_err(failed)?,
            "input_accepted" => {
                let turn =
                    turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?;
                let Some(JsValue::Array(messages)) = event.get("messages") else {
                    return Err(RuntimeError::new(
                        "Accepted input messages must be an array.",
                    ));
                };
                self.recorder
                    .writer()
                    .record_accepted_input(
                        &self.session_key,
                        turn,
                        messages.clone(),
                        event.get("metadata").and_then(JsValue::as_object).cloned(),
                    )
                    .map_err(failed)?;
                projection.messages.extend(messages.iter().cloned());
                let turn = turn.clone();
                let prompt = listing_prompt(messages);
                let selection = event.get("metadata").and_then(|m| m.get("modelSelection"));
                if !prompt.is_empty() || selection.is_some() {
                    let mut metadata = JsObject::new();
                    if !prompt.is_empty() {
                        if !projection
                            .metadata
                            .get("firstPrompt")
                            .and_then(JsValue::as_string)
                            .is_some_and(|v| !v.is_empty())
                        {
                            metadata.insert("firstPrompt".into(), JsValue::String(prompt.clone()));
                        }
                        metadata.insert("lastPrompt".into(), JsValue::String(prompt));
                    }
                    if let Some(selection) = selection {
                        metadata.insert("modelSelection".into(), selection.clone());
                    }
                    metadata.insert("updatedAt".into(), metadata_timestamp());
                    self.recorder
                        .writer()
                        .record_session_metadata(
                            &self.session_key,
                            &turn,
                            JsValue::Object(metadata.clone()),
                        )
                        .map_err(failed)?;
                    for (key, value) in metadata.entries() {
                        projection.metadata.insert(key.clone(), value.clone());
                    }
                }
            }
            "assistant_message" | "tool_results_projected" => {
                let turn =
                    turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?;
                let message = event
                    .get("message")
                    .ok_or_else(|| RuntimeError::new("Session message is required."))?;
                let mut draft = JsObject::new();
                draft.insert(
                    "type".into(),
                    if kind == "assistant_message" {
                        "assistant_message"
                    } else {
                        "tool_result_message"
                    }
                    .into(),
                );
                draft.insert("message".into(), message.clone());
                self.recorder
                    .writer()
                    .record_session_event(&self.session_key, turn, draft)
                    .map_err(failed)?;
                projection.messages.push(message.clone());
            }
            "turn_completed" => {
                let result = event
                    .get("result")
                    .ok_or_else(|| RuntimeError::new("Turn result is required."))?;
                if result.get("sessionId").and_then(JsValue::as_string) != Some(&self.session_key)
                    || result.get("turnId").and_then(JsValue::as_string) != turn
                {
                    return Err(RuntimeError::new("Turn result identity mismatch."));
                }
                // This path already holds the projection lock; call the recorder
                // directly (identity was checked above) instead of re-entering
                // `complete_turn`, which would deadlock on the same mutex.
                let turn = turn.ok_or_else(|| {
                    RuntimeError::new("Session event turnId is required.")
                })?;
                self.recorder
                    .complete_turn(&self.session_key, turn, result.clone())
                    .map_err(failed)?;
                self.reappend_metadata(&mut projection, turn)?;
            }
            "turn_failed" => {
                let turn =
                    turn.ok_or_else(|| RuntimeError::new("Session event turnId is required."))?;
                let failure = event
                    .get("error")
                    .ok_or_else(|| RuntimeError::new("Turn failure error is required."))?;
                let mut status = JsObject::new();
                status.insert("event".into(), "turn_failed".into());
                status.insert("kind".into(), "error".into());
                status.insert(
                    "text".into(),
                    failure
                        .get("message")
                        .cloned()
                        .unwrap_or_else(|| failure.clone()),
                );
                let mut detail = JsObject::new();
                detail.insert("visible".into(), JsValue::Bool(true));
                detail.insert("source".into(), "agent".into());
                detail.insert("scope".into(), "turn".into());
                if let Some(code) = failure.get("code") {
                    detail.insert("code".into(), code.clone());
                }
                status.insert("detail".into(), JsValue::Object(detail));
                let status = JsValue::Object(status);
                self.recorder
                    .writer()
                    .record_agent_status_message(&self.session_key, turn, &status)
                    .map_err(failed)?;
                projection.statuses.push(status);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<JsValue, RuntimeError> {
        self.check_error()?;
        let projection = self.projection.lock().map_err(failed)?;
        let mut value = JsObject::new();
        value.insert(
            "liveIdentity".into(),
            JsValue::String(self.identity.clone()),
        );
        value.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        value.insert(
            "messages".into(),
            JsValue::Array(projection.messages.clone()),
        );
        value.insert(
            "statuses".into(),
            JsValue::Array(projection.statuses.clone()),
        );
        value.insert(
            "seedState".into(),
            JsValue::Object(crate::host::modules::tool_context::serialize_seed_state(
                &self.checkpoint.snapshot().map_err(failed)?,
            )),
        );
        value.insert(
            "sequence".into(),
            JsValue::Number(self.recorder.snapshot_state().0 as f64),
        );
        Ok(JsValue::Object(value))
    }

    /// Merge into the wrapper's input before calling inner.submit; the host
    /// builder reads these exact shared maps under the synchronous scope.
    pub fn seed_state(&self) -> Result<JsValue, RuntimeError> {
        self.check_error()?;
        Ok(JsValue::Object(
            crate::host::modules::tool_context::serialize_seed_state(
                &self.checkpoint.snapshot().map_err(failed)?,
            ),
        ))
    }

    /// The real sidecar terminal may be returned instead of an emitted
    /// turn_completed event. Never synthesize a success/failure result here.
    pub fn complete_turn(&self, result: &JsValue) -> Result<(), RuntimeError> {
        self.check_error()?;
        let mut projection = self.projection.lock().map_err(failed)?;
        if self.disposed.load(Ordering::SeqCst) {
            return Err(RuntimeError::new("Session writer is disposed."));
        }
        if result.get("sessionId").and_then(JsValue::as_string) != Some(&self.session_key) {
            return Err(RuntimeError::new("Turn result identity mismatch."));
        }
        let turn = result
            .get("turnId")
            .and_then(JsValue::as_string)
            .ok_or_else(|| RuntimeError::new("Turn result identity mismatch."))?;
        self.recorder
            .complete_turn(&self.session_key, turn, result.clone())
            .map_err(failed)?;
        self.reappend_metadata(&mut projection, turn)
    }

    fn reappend_metadata(
        &self,
        projection: &mut Projection,
        turn: &JsString,
    ) -> Result<(), RuntimeError> {
        if projection.metadata.entries().is_empty()
            || projection.metadata_tail_turn.as_ref() == Some(turn)
        {
            return Ok(());
        }
        let mut metadata = projection.metadata.clone();
        metadata.insert("isSnapshot".into(), JsValue::Bool(true));
        metadata.insert("updatedAt".into(), metadata_timestamp());
        self.recorder
            .writer()
            .record_session_metadata(&self.session_key, turn, JsValue::Object(metadata))
            .map_err(failed)?;
        projection.metadata_tail_turn = Some(turn.clone());
        Ok(())
    }

    /// SidecarSession's direct HostHandler::event currently discards sink
    /// errors. Its outer LiveTurn must check this before yielding/ending.
    pub fn check_error(&self) -> Result<(), RuntimeError> {
        match self.failure.lock().map_err(failed)?.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn dispose(&self) -> Result<(), RuntimeError> {
        let _projection = self.projection.lock().map_err(failed)?;
        self.disposed.store(true, Ordering::SeqCst);
        // The wrapper must drain the real inner session before invoking this.
        self.recorder.writer().close();
        Ok(())
    }
}

impl EventSink for LiveSessionState {
    fn emit(&self, event: JsObject) -> Result<(), crate::host::HostError> {
        self.record_event(&JsValue::Object(event)).map_err(|error| {
            if let Ok(mut failure) = self.failure.lock()
                && failure.is_none()
            {
                *failure = Some(error.clone());
            }
            crate::host::ModuleError::new(error.to_string()).into()
        })
    }
}

/// Weak lookup only: the actual owner/factory retains all live sessions.
/// Factory publication and cold writes must hold `publication` through commit.
#[derive(Default)]
pub struct LiveBindings {
    states: Mutex<HashMap<JsString, Weak<LiveSessionState>>>,
    cwds: Mutex<HashMap<JsString, JsString>>,
    pub publication: tokio::sync::Mutex<()>,
}
impl LiveBindings {
    pub fn clear(&self, session: &JsString) {
        self.states.lock().unwrap().remove(session);
        self.cwds.lock().unwrap().remove(session);
    }
    pub fn current(&self, session: &JsString) -> Option<Arc<LiveSessionState>> {
        self.states
            .lock()
            .unwrap()
            .get(session)?
            .upgrade()
            .filter(|s| !s.disposed.load(Ordering::SeqCst))
    }
    pub fn bind(&self, state: &Arc<LiveSessionState>) {
        self.states
            .lock()
            .unwrap()
            .insert(state.session_key.clone(), Arc::downgrade(state));
    }
    pub fn set_cwd(&self, session: &JsString, cwd: JsString) {
        self.cwds.lock().unwrap().insert(session.clone(), cwd);
    }
    pub fn cwd(&self, session: &JsString) -> Option<JsString> {
        self.cwds.lock().unwrap().get(session).cloned()
    }
    /// An identity lookup, not a publication fence. The caller must additionally
    /// hold publication through the entire live/cold decision and disk commit.
    pub fn current_for(
        &self,
        owner: &crate::runtime::session::SessionOwner,
        session: &JsString,
    ) -> Result<Option<Arc<LiveSessionState>>, RuntimeError> {
        let state = self.current(session);
        match (owner.current_handle(session), state) {
            (None, None) => Ok(None),
            (Some(handle), Some(state))
                if handle
                    .snapshot()
                    .get("liveIdentity")
                    .and_then(JsValue::as_string)
                    == Some(&state.identity)
                    && owner.is_current(session, &handle) =>
            {
                Ok(Some(state))
            }
            _ => Err(RuntimeError::new(
                "Session writer is publishing, draining, or has changed.",
            )),
        }
    }

    /// Factory creation/binding and wrapper disposal use this same mutex.
    /// The cold callback commits before it is released; callers must not
    /// return None then append later outside this fence.
    pub async fn record_status(
        &self,
        owner: &crate::runtime::session::SessionOwner,
        session: &JsString,
        turn: &JsString,
        status: &JsValue,
        cold: impl FnOnce() -> Result<(), RuntimeError>,
    ) -> Result<bool, RuntimeError> {
        let _publication = self.publication.lock().await;
        match self.current_for(owner, session)? {
            Some(state) => state.record_status(turn, status),
            None => {
                cold()?;
                Ok(true)
            }
        }
    }
}

fn metadata_timestamp() -> JsValue {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    JsValue::String(pilotdeck_transport::system_clock::format_epoch_iso(ms).into())
}

pub(crate) fn listing_prompt(messages: &[JsValue]) -> JsString {
    let parts: Vec<String> = messages
        .iter()
        .filter(|m| {
            m.get("role") == Some(&JsValue::from("user"))
                && !m
                    .get("metadata")
                    .and_then(|v| v.get("synthetic"))
                    .is_some_and(|v| match v {
                        JsValue::Null | JsValue::Undefined => false,
                        JsValue::Bool(v) => *v,
                        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
                        JsValue::String(v) => !v.is_empty(),
                        _ => true,
                    })
        })
        .filter_map(|message| {
            let JsValue::Array(blocks) = message.get("content")? else {
                return None;
            };
            let text = blocks
                .iter()
                .filter(|b| b.get("type") == Some(&JsValue::from("text")))
                .filter_map(|b| b.get("text").and_then(JsValue::as_string))
                .map(|s| String::from_utf16_lossy(s.units()))
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim_matches(crate::metadata::js_space);
            (!text.is_empty()).then(|| text.to_owned())
        })
        .collect();
    JsString::from_utf16(parts.join("\n").encode_utf16().take(1200).collect())
}

fn project_entry(projection: &mut Projection, entry: &JsValue) {
    let kind = entry.get("type").and_then(JsValue::as_string);
    if kind.is_some_and(|k| k.equals_str("session_metadata")) {
        if let Some(metadata) = entry.get("metadata").and_then(JsValue::as_object) {
            for (key, value) in metadata.entries() {
                if key.equals_str("isSnapshot") {
                    continue;
                }
                if matches!(value, JsValue::Null | JsValue::Undefined)
                    && (key.equals_str("title") || key.equals_str("linkedPullRequest"))
                {
                    continue;
                }
                projection.metadata.insert(key.clone(), value.clone());
            }
        }
    } else if kind.is_some_and(|k| k.equals_str("accepted_input")) {
        if let Some(JsValue::Array(messages)) = entry.get("messages") {
            projection.messages.extend(messages.iter().cloned());
        }
    } else if kind.is_some_and(|k| {
        k.equals_str("assistant_message")
            || k.equals_str("tool_result_message")
            || k.equals_str("durable_message")
    }) {
        if let Some(message) = entry.get("message") {
            projection.messages.push(message.clone());
        }
    } else if kind.is_some_and(|k| k.equals_str("control_boundary")) {
        if let Some(boundary) = entry.get("boundary") {
            let messages = boundary
                .get("replacementMessages")
                .or_else(|| boundary.get("snapshot").and_then(|s| s.get("messages")));
            if let Some(JsValue::Array(messages)) = messages {
                projection.messages = messages.clone();
            }
        }
    } else if kind.is_some_and(|k| k.equals_str("agent_status_message")) {
        projection.statuses.push(entry.clone());
    }
}
fn failed(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(error.to_string())
}
