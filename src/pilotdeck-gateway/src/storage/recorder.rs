//! Durable model/tool transcript recording, ported from TS
//! `AgentSessionEventRecorder.recordModelRequest/recordModelEvent` and
//! `recordToolCalls/recordToolResults` (B/src/agent/session/
//! AgentSessionEventRecorder.ts:187-315), including the private step
//! lifecycle those methods use (`startTurn`/`openNextStep`/`completeStep`).
//!
//! The durable ports that call these methods are thin wrappers in TS
//! (`durableModelInvokerPort.ts:4-22`, `durableToolPort.ts`): a model request is
//! appended before the stream starts and every stream event is appended before
//! it is yielded; tool calls are appended before execution and results after it
//! (an aborted turn skips the result record). Those wrappers never own
//! transcript state, so this module ports the recorder surface they call.
//!
//! Entries are appended through the A6.2 [`TranscriptStore`] with the exact TS
//! JSONL shape: one `JSON.stringify(entry) + "\n"` per record, sequence starts
//! at 1, `entryId` is a v4 UUID, `parentEntryId` is the previous entry id, and
//! `createdAt` is the injected clock's ISO text. The A6.2 `TranscriptWriter`
//! deliberately refuses turn-scoped events (`NodeHostRequired`) until domain
//! validation is ported, so the recorder appends to the same store directly;
//! it does not change or bypass any existing adapter behaviour. `raw` fields are recursively
//! stripped from stream events, tool calls and tool results (never from a model
//! request), matching `stripRawFields`.
//!
//! Not ported (unreachable through the two durable ports, and they stay closed
//! rather than guessed): compaction/question/permission/inbox lifecycle,
//! instruction-layer snapshots, context materialization and `completeTurn`
//! (`turn_result`). `restore` only re-seeds the writer sequence/parent chain
//! and the domain-validation state; a restored turn is not an active recorder
//! turn, exactly like constructing a fresh TS recorder from restored entries.
//!
//! The port is synchronous: calls serialize on one recorder mutex. Sequential
//! calls match TS; no interleaving order is claimed for unsynchronized
//! concurrent callers (both durable ports invoke the recorder sequentially).
use super::{
    ledger::field,
    transcript::uuid,
    transcript_adapter::{AdapterError, TranscriptKey, TranscriptStore, validate_event_log},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

/// ISO text for `createdAt`; an error aborts the append.
pub type RecorderClock = Arc<dyn Fn() -> RecorderResult<String> + Send + Sync>;
/// Entry id source; production uses [`uuid`], tests may inject a counter.
pub type RecorderUuid = Arc<dyn Fn() -> RecorderResult<String> + Send + Sync>;
pub type RecorderResult<T> = std::result::Result<T, RecorderError>;

#[derive(Debug)]
pub enum RecorderError {
    /// TS `Error` from recorder state (`Durable turn ...`).
    State(String),
    /// TS `SessionDomainValidationError`; code and wording match TS.
    Domain { code: &'static str, message: String },
    /// TS `SessionEventValidationError` from restoring a log.
    Envelope { code: &'static str, message: String },
    /// TS `TypeError` from a malformed value at a recorder call site.
    Type(String),
    /// Host store failure, propagated unchanged.
    Store(super::LedgerError),
}
impl fmt::Display for RecorderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(message)
            | Self::Type(message)
            | Self::Domain { message, .. }
            | Self::Envelope { message, .. } => f.write_str(message),
            Self::Store(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for RecorderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}
fn state(message: impl Into<String>) -> RecorderError {
    RecorderError::State(message.into())
}
fn domain(code: &'static str, message: impl Into<String>) -> RecorderError {
    RecorderError::Domain {
        code,
        message: message.into(),
    }
}

/// One turn's TS `TurnRecordingState` plus the domain-validation projection
/// (SessionDomainValidation.ts StepState/TurnState) that a TS transcript writer
/// would keep while committing these entries.
#[derive(Clone, Debug)]
struct Turn {
    /// The current recorder instance started this turn (`turns` in TS). A
    /// turn rebuilt by [`Recorder::restore`] is domain state only.
    recording: bool,
    next_step: u64,
    last_step: u64,
    open_step: Option<u64>,
    model_requested: bool,
    open_tool_calls: Vec<Option<JsString>>,
}
impl Turn {
    fn started() -> Self {
        Self {
            recording: false,
            next_step: 1,
            last_step: 0,
            open_step: None,
            model_requested: false,
            open_tool_calls: Vec::new(),
        }
    }
}

struct Inner {
    sequence: u64,
    last_entry_id: Option<JsString>,
    turns: HashMap<JsString, Turn>,
}

/// Durable recorder over a host-owned transcript store.
pub struct Recorder {
    store: Arc<dyn TranscriptStore>,
    key: TranscriptKey,
    clock: RecorderClock,
    uuid: RecorderUuid,
    inner: Mutex<Inner>,
}

impl Recorder {
    /// Production clock (`Date#toISOString` equivalent) and v4 UUID source.
    pub fn new(store: Arc<dyn TranscriptStore>, key: TranscriptKey) -> Self {
        Self::with_sources(
            store,
            key,
            Arc::new(|| super::transcript::timestamp().map_err(RecorderError::Store)),
            Arc::new(|| uuid().map_err(|e| RecorderError::Store(e.into()))),
        )
    }
    /// Fixed clock with the production UUID source.
    pub fn with_clock(
        store: Arc<dyn TranscriptStore>,
        key: TranscriptKey,
        clock: RecorderClock,
    ) -> Self {
        Self::with_sources(
            store,
            key,
            clock,
            Arc::new(|| uuid().map_err(|e| RecorderError::Store(e.into()))),
        )
    }
    pub fn with_sources(
        store: Arc<dyn TranscriptStore>,
        key: TranscriptKey,
        clock: RecorderClock,
        uuid: RecorderUuid,
    ) -> Self {
        Self {
            store,
            key,
            clock,
            uuid,
            inner: Mutex::new(Inner {
                sequence: 0,
                last_entry_id: None,
                turns: HashMap::new(),
            }),
        }
    }
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// TS `startTurn`: `turn_started`, then `{nextStep: 1}`.
    pub fn start_turn(&self, session_id: &JsString, turn_id: &JsString) -> RecorderResult<()> {
        let mut inner = self.lock();
        if inner.turns.contains_key(turn_id) {
            return Err(domain(
                "duplicate_turn_start",
                format!("Turn {} has already started.", lossy(turn_id)),
            ));
        }
        let draft = draft("turn_started");
        self.append(&mut inner, session_id, turn_id, draft)?;
        let mut turn = Turn::started();
        turn.recording = true;
        inner.turns.insert(turn_id.clone(), turn);
        Ok(())
    }
    /// TS `recordModelRequest`: open a step if needed, append the request, and
    /// return the step number.
    pub fn record_model_request(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        request: JsValue,
    ) -> RecorderResult<u64> {
        let mut inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let needs_step = match inner.turns.get(turn_id) {
            Some(turn) => turn.open_step.is_none() || turn.model_requested,
            None => unreachable!("require_recording"),
        };
        if needs_step {
            self.open_next_step(&mut inner, session_id, turn_id)?;
        }
        let step = self.require_open_step(&inner, turn_id)?;
        let mut draft = draft("model_request");
        draft.insert("step".into(), JsValue::Number(step as f64));
        draft.insert("request".into(), request);
        self.append(&mut inner, session_id, turn_id, draft)?;
        if let Some(turn) = inner.turns.get_mut(turn_id) {
            turn.model_requested = true;
        }
        Ok(step)
    }
    /// TS `recordModelEvent`: one `model_stream_event` per canonical event.
    pub fn record_model_event(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        event: &JsValue,
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let step = self.require_open_step(&inner, turn_id)?;
        if !inner.turns[turn_id].model_requested {
            return Err(domain(
                "model_request_missing",
                format!("Step {} emitted a model event before its request.", step),
            ));
        }
        let mut draft = draft("model_stream_event");
        draft.insert("step".into(), JsValue::Number(step as f64));
        draft.insert("event".into(), strip_raw_fields(event));
        self.append(&mut inner, session_id, turn_id, draft)
    }
    /// TS `recordToolCalls`: one `tool_call` per call, all at the open step.
    pub fn record_tool_calls(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        calls: &[JsValue],
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let step = self.require_open_step(&inner, turn_id)?;
        for call in calls {
            let call = strip_raw_fields(call);
            let id = call_id(&call, "id")?;
            let requested = inner.turns[turn_id].model_requested;
            if !requested {
                return Err(domain(
                    "model_request_missing",
                    format!(
                        "Step {} emitted tool call {} before its request.",
                        step,
                        show_id(id.as_ref())
                    ),
                ));
            }
            if inner.turns[turn_id].open_tool_calls.contains(&id) {
                return Err(domain(
                    "duplicate_tool_call",
                    format!("Tool call {} is already open.", show_id(id.as_ref())),
                ));
            }
            let mut draft = draft("tool_call");
            draft.insert("step".into(), JsValue::Number(step as f64));
            draft.insert("call".into(), call);
            self.append(&mut inner, session_id, turn_id, draft)?;
            if let Some(turn) = inner.turns.get_mut(turn_id) {
                turn.open_tool_calls.push(id);
            }
        }
        Ok(())
    }
    /// TS `recordToolResults`: one `tool_result` per result, then close the
    /// step as `completed`.
    pub fn record_tool_results(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        results: &[JsValue],
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let step = self.require_open_step(&inner, turn_id)?;
        for result in results {
            let result = strip_raw_fields(result);
            let id = call_id(&result, "toolCallId")?;
            let position = inner.turns[turn_id]
                .open_tool_calls
                .iter()
                .position(|open| *open == id);
            if position.is_none() {
                return Err(domain(
                    "tool_result_without_call",
                    format!(
                        "Tool result {} has no open call in step {}.",
                        show_id(id.as_ref()),
                        step
                    ),
                ));
            }
            let mut draft = draft("tool_result");
            draft.insert("step".into(), JsValue::Number(step as f64));
            draft.insert("result".into(), result);
            self.append(&mut inner, session_id, turn_id, draft)?;
            if let Some(position) = position
                && let Some(turn) = inner.turns.get_mut(turn_id)
            {
                turn.open_tool_calls.remove(position);
            }
        }
        self.complete_step(&mut inner, session_id, turn_id, "completed")
    }
    /// Re-seed sequence, parent chain and domain state from a persisted log.
    /// The recorder's active turn set stays empty, like a fresh TS recorder.
    /// Unlike bare `writer.restoreState`, this also replays the turn/step
    /// domain projection (what a resumed `SessionRuntime` keeps), so a reused
    /// turn id still reports `duplicate_turn_start`.
    pub fn restore(&self, entries: &[JsValue]) -> RecorderResult<()> {
        let (writer, _ids) = validate_event_log(entries).map_err(|error| match error {
            AdapterError::Validation { code, message } => RecorderError::Envelope { code, message },
            other => state(other.to_string()),
        })?;
        let mut inner = self.lock();
        inner.sequence = writer.sequence;
        inner.last_entry_id = writer.last_entry_id;
        inner.turns.clear();
        for entry in entries {
            replay_domain(&mut inner.turns, entry);
        }
        Ok(())
    }
    /// TS `snapshotState` equivalent, for resume callers.
    pub fn snapshot_state(&self) -> (u64, Option<JsString>) {
        let inner = self.lock();
        (inner.sequence, inner.last_entry_id.clone())
    }

    fn require_recording(&self, inner: &Inner, turn_id: &JsString) -> RecorderResult<()> {
        match inner.turns.get(turn_id) {
            Some(turn) if turn.recording => Ok(()),
            _ => Err(state(format!(
                "Durable turn {} has not started.",
                lossy(turn_id)
            ))),
        }
    }
    fn require_open_step(&self, inner: &Inner, turn_id: &JsString) -> RecorderResult<u64> {
        inner
            .turns
            .get(turn_id)
            .and_then(|turn| turn.open_step)
            .ok_or_else(|| state(format!("Durable turn {} has no open step.", lossy(turn_id))))
    }
    fn open_next_step(
        &self,
        inner: &mut Inner,
        session_id: &JsString,
        turn_id: &JsString,
    ) -> RecorderResult<u64> {
        if inner
            .turns
            .get(turn_id)
            .and_then(|turn| turn.open_step)
            .is_some()
        {
            self.complete_step(inner, session_id, turn_id, "completed")?;
        }
        let step = inner.turns[turn_id].next_step;
        if step <= inner.turns[turn_id].last_step {
            return Err(domain(
                "invalid_step",
                format!(
                    "Step {} must increase within turn {}.",
                    step,
                    lossy(turn_id)
                ),
            ));
        }
        let mut draft = draft("step_started");
        draft.insert("step".into(), JsValue::Number(step as f64));
        self.append(inner, session_id, turn_id, draft)?;
        if let Some(turn) = inner.turns.get_mut(turn_id) {
            turn.open_step = Some(step);
            turn.model_requested = false;
            turn.last_step = step;
            turn.next_step = step + 1;
        }
        Ok(step)
    }
    fn complete_step(
        &self,
        inner: &mut Inner,
        session_id: &JsString,
        turn_id: &JsString,
        outcome: &str,
    ) -> RecorderResult<()> {
        let Some(step) = inner.turns.get(turn_id).and_then(|turn| turn.open_step) else {
            return Ok(());
        };
        if outcome == "completed" && !inner.turns[turn_id].open_tool_calls.is_empty() {
            return Err(domain(
                "step_has_open_tool_calls",
                format!("Step {} completed with unresolved tool calls.", step),
            ));
        }
        let mut draft = draft("step_completed");
        draft.insert("step".into(), JsValue::Number(step as f64));
        draft.insert("outcome".into(), outcome.into());
        self.append(inner, session_id, turn_id, draft)?;
        if let Some(turn) = inner.turns.get_mut(turn_id) {
            turn.open_step = None;
            turn.model_requested = false;
        }
        Ok(())
    }
    /// SessionRuntime.commit: materialize the draft, add the envelope, append
    /// once, then publish sequence/parent. A failed append publishes nothing.
    fn append(
        &self,
        inner: &mut Inner,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
    ) -> RecorderResult<()> {
        let mut stable = materialize(&JsValue::Object(draft))?;
        let JsValue::Object(fields) = &mut stable else {
            return Err(RecorderError::Type(
                "Session event must be an object.".into(),
            ));
        };
        let created_at = (self.clock)()?;
        let entry_id = (self.uuid)()?;
        let sequence = inner.sequence + 1;
        fields.insert("sessionId".into(), JsValue::String(session_id.clone()));
        fields.insert("turnId".into(), JsValue::String(turn_id.clone()));
        fields.insert("sequence".into(), JsValue::Number(sequence as f64));
        fields.insert("createdAt".into(), created_at.as_str().into());
        fields.insert("entryId".into(), entry_id.as_str().into());
        fields.insert(
            "parentEntryId".into(),
            inner
                .last_entry_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        let entry = materialize(&stable)?;
        self.store
            .append(&self.key, &entry)
            .map_err(RecorderError::Store)?;
        inner.sequence = sequence;
        inner.last_entry_id = Some(entry_id.as_str().into());
        Ok(())
    }
}

fn draft(kind: &str) -> JsObject {
    let mut draft = JsObject::new();
    draft.insert("type".into(), kind.into());
    draft
}
fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn show_id(id: Option<&JsString>) -> String {
    id.map_or_else(|| "undefined".into(), lossy)
}
/// `entry.call.id` / `entry.result.toolCallId` in TS: a nullish owner throws a
/// TypeError before the property lookup; a missing property is `undefined`.
fn call_id(value: &JsValue, key: &str) -> RecorderResult<Option<JsString>> {
    match value {
        JsValue::Undefined | JsValue::Null => Err(RecorderError::Type(format!(
            "Cannot read properties of {} (reading '{}')",
            if matches!(value, JsValue::Null) {
                "null"
            } else {
                "undefined"
            },
            key
        ))),
        _ => Ok(field(value, key).as_string().cloned()),
    }
}
/// SessionRuntime.materializeJsonValue: JSON round trip.
fn materialize(value: &JsValue) -> RecorderResult<JsValue> {
    let text = stringify_json(value)
        .map_err(|e| RecorderError::Type(e.to_string()))?
        .ok_or_else(|| RecorderError::Type("Session event must be JSON serializable.".into()))?;
    parse_json(&text).map_err(|e| RecorderError::Type(e.to_string()))
}
/// `stripRawFields` (AgentSessionEventRecorder.ts:568-577).
fn strip_raw_fields(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(strip_raw_fields).collect()),
        JsValue::Object(object) => {
            let mut sanitized = JsObject::new();
            for (key, child) in object.entries() {
                if !key.equals_str("raw") {
                    sanitized.insert(key.clone(), strip_raw_fields(child));
                }
            }
            JsValue::Object(sanitized)
        }
        other => other.clone(),
    }
}

/// SessionDomainValidation.ts for the subset these recorder methods emit.
fn replay_domain(turns: &mut HashMap<JsString, Turn>, entry: &JsValue) {
    let Some(turn_id) = field(entry, "turnId").as_string() else {
        return;
    };
    let kind = field(entry, "type").as_string();
    if kind.is_some_and(|k| k.equals_str("turn_started")) {
        turns.insert(turn_id.clone(), Turn::started());
        return;
    }
    let Some(turn) = turns.get_mut(turn_id) else {
        return;
    };
    let step = field(entry, "step").as_number().map(|n| n as u64);
    match kind.map(lossy).as_deref() {
        Some("step_started") => {
            if let Some(step) = step {
                turn.last_step = step;
                turn.open_step = Some(step);
            }
            turn.model_requested = false;
            turn.open_tool_calls.clear();
        }
        Some("model_request") => turn.model_requested = true,
        Some("tool_call") => {
            turn.open_tool_calls
                .push(field(field(entry, "call"), "id").as_string().cloned());
        }
        Some("tool_result") => {
            let id = field(field(entry, "result"), "toolCallId")
                .as_string()
                .cloned();
            if let Some(position) = turn.open_tool_calls.iter().position(|open| *open == id) {
                turn.open_tool_calls.remove(position);
            }
        }
        Some("step_completed") => {
            turn.open_step = None;
            turn.model_requested = false;
            turn.open_tool_calls.clear();
        }
        _ => {}
    }
}
