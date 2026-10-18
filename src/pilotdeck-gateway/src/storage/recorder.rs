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
//! Entries are appended through the A6.2 [`TranscriptWriter`] with the exact TS
//! JSONL shape: one `JSON.stringify(entry) + "\n"` per record, sequence starts
//! at 1, `entryId` is a v4 UUID, `parentEntryId` is the previous entry id, and
//! `createdAt` is the injected clock's ISO text. A recorder takes the same writer
//! as the rest of its session, sharing its commit lock, sequence, parent chain,
//! entry ids and domain validation state. `raw` and `__proto__` properties are
//! recursively stripped from stream events, tool calls and tool results (never
//! from a model request), matching `stripRawFields`.
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
    transcript_adapter::{
        AdapterError, TranscriptKey, TranscriptStore, TranscriptWriter, domain_supported,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
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
    Domain {
        code: &'static str,
        message: String,
    },
    /// TS `SessionEventValidationError` from an append or restored log.
    Envelope {
        code: &'static str,
        message: String,
    },
    /// TS `TypeError` from a malformed value at a recorder call site.
    Type(String),
    /// Unsupported lifecycle requires the Node host; it is never silently replayed.
    NodeHostRequired(String),
    Range(String),
    /// Host store failure, propagated unchanged.
    Store(super::LedgerError),
}
impl fmt::Display for RecorderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(message)
            | Self::Type(message)
            | Self::NodeHostRequired(message)
            | Self::Range(message)
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

impl From<AdapterError> for RecorderError {
    fn from(error: AdapterError) -> Self {
        match error {
            AdapterError::Validation { code, message } => Self::Envelope { code, message },
            AdapterError::Type(message) => Self::Type(message),
            AdapterError::Range(message) => Self::Range(message),
            AdapterError::NodeHostRequired(message) => Self::NodeHostRequired(message),
            AdapterError::Store(error) => Self::Store(error),
        }
    }
}

/// Local TurnRecordingState; validation state belongs to the shared writer.
struct Turn {
    next_step: u64,
    open_step: Option<u64>,
    model_requested: bool,
}
impl Turn {
    fn started() -> Self {
        Self {
            next_step: 1,
            open_step: None,
            model_requested: false,
        }
    }
}
struct Inner {
    turns: HashMap<JsString, Turn>,
}

/// Durable recorder over a session's shared transcript writer.
pub struct Recorder {
    writer: Arc<TranscriptWriter>,
    sources: Option<(RecorderClock, RecorderUuid)>,
    inner: Mutex<Inner>,
}

impl Recorder {
    /// Production clock (`Date#toISOString` equivalent) and v4 UUID source.
    /// Creates a standalone writer. A session with an existing writer must use
    /// [`Self::from_writer`] so every producer shares the same commit state.
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
        let writer = Arc::new(TranscriptWriter::for_store(store, key));
        let mut recorder = Self::from_writer(writer);
        recorder.sources = Some((clock, uuid));
        recorder
    }
    /// Inject the same writer used for accepted input and session metadata.
    pub fn from_writer(writer: Arc<TranscriptWriter>) -> Self {
        Self {
            writer,
            sources: None,
            inner: Mutex::new(Inner {
                turns: HashMap::new(),
            }),
        }
    }
    /// Access the authoritative writer, for other session transcript producers.
    pub fn writer(&self) -> &Arc<TranscriptWriter> {
        &self.writer
    }
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// TS `startTurn`: `turn_started`, then `{nextStep: 1}`.
    pub fn start_turn(&self, session_id: &JsString, turn_id: &JsString) -> RecorderResult<()> {
        let mut inner = self.lock();
        let draft = draft("turn_started");
        self.append(session_id, turn_id, draft)?;
        inner.turns.insert(turn_id.clone(), Turn::started());
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
        self.append(session_id, turn_id, draft)?;
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
        let inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let step = self.require_open_step(&inner, turn_id)?;
        let mut draft = draft("model_stream_event");
        draft.insert("step".into(), JsValue::Number(step as f64));
        draft.insert("event".into(), strip_raw_fields(event));
        self.append(session_id, turn_id, draft)
    }
    /// TS `recordToolCalls`: one `tool_call` per call, all at the open step.
    pub fn record_tool_calls(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        calls: &[JsValue],
    ) -> RecorderResult<()> {
        let inner = self.lock();
        self.require_recording(&inner, turn_id)?;
        let step = self.require_open_step(&inner, turn_id)?;
        for call in calls {
            let call = strip_raw_fields(call);
            let mut draft = draft("tool_call");
            draft.insert("step".into(), JsValue::Number(step as f64));
            draft.insert("call".into(), call);
            self.append(session_id, turn_id, draft)?;
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
            let mut draft = draft("tool_result");
            draft.insert("step".into(), JsValue::Number(step as f64));
            draft.insert("result".into(), result);
            self.append(session_id, turn_id, draft)?;
        }
        self.complete_step(&mut inner, session_id, turn_id, "completed")
    }
    /// Re-seed sequence, parent chain and domain state from a persisted log.
    /// The recorder's active turn set stays empty, like a fresh TS recorder.
    /// Unlike bare `writer.restoreState`, this also replays the turn/step
    /// domain projection (what a resumed `SessionRuntime` keeps), so a reused
    /// turn id still reports `duplicate_turn_start`.
    pub fn restore(&self, entries: &[JsValue]) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.writer.restore_recorder_entries(entries)?;
        inner.turns.clear();
        Ok(())
    }
    /// TS snapshotState, shared with every caller of the injected writer.
    pub fn snapshot_state(&self) -> (u64, Option<JsString>) {
        let state = self.writer.snapshot_state();
        (state.sequence, state.last_entry_id)
    }

    fn require_recording(&self, inner: &Inner, turn_id: &JsString) -> RecorderResult<()> {
        match inner.turns.get(turn_id) {
            Some(_) => Ok(()),
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
        let mut draft = draft("step_started");
        draft.insert("step".into(), JsValue::Number(step as f64));
        self.append(session_id, turn_id, draft)?;
        if let Some(turn) = inner.turns.get_mut(turn_id) {
            turn.open_step = Some(step);
            turn.model_requested = false;
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
        let mut draft = draft("step_completed");
        draft.insert("step".into(), JsValue::Number(step as f64));
        draft.insert("outcome".into(), outcome.into());
        self.append(session_id, turn_id, draft)?;
        if let Some(turn) = inner.turns.get_mut(turn_id) {
            turn.open_step = None;
            turn.model_requested = false;
        }
        Ok(())
    }
    fn append(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
    ) -> RecorderResult<()> {
        self.writer
            .append_recorder_event(session_id, turn_id, draft, self.sources.as_ref())
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
/// Property access happens only after the entry's JSON round trips.
fn call_id<'a>(value: &'a JsValue, key: &str) -> RecorderResult<&'a JsValue> {
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
        _ => Ok(field(value, key)),
    }
}
fn display(value: &JsValue) -> RecorderResult<String> {
    // JSON cannot contain callable own properties. Shadowing Object#toString
    // therefore makes String(id) fail; arrays coerce each non-null element.
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            JsValue::Object(object) if object.get("toString").is_some() => {
                return Err(RecorderError::Type(
                    "Cannot convert object to primitive value".into(),
                ));
            }
            JsValue::Array(items) => pending.extend(items.iter().rev()),
            _ => {}
        }
    }
    crate::js_value::display(value)
        .map_err(|_| RecorderError::Range(crate::js_value::StackOverflow::MESSAGE.into()))
}
/// JS Set uses SameValueZero, not deep equality. Each materialized entry owns
/// fresh objects/arrays, so a new entry can never match a prior reference id.
fn same_id(left: &JsValue, right: &JsValue) -> bool {
    !matches!(left, JsValue::Object(_) | JsValue::Array(_)) && left == right
}
/// `stripRawFields` (AgentSessionEventRecorder.ts:568-577).
fn strip_raw_fields(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(strip_raw_fields).collect()),
        JsValue::Object(object) => {
            let mut sanitized = JsObject::new();
            for (key, child) in object.entries() {
                if !key.equals_str("raw") && !key.equals_str("__proto__") {
                    sanitized.insert(key.clone(), strip_raw_fields(child));
                }
            }
            JsValue::Object(sanitized)
        }
        other => other.clone(),
    }
}

/// SessionDomainValidation subset used by the durable model/tool ports.
/// Owned by TranscriptWriter so two recorders also share validation state.
#[derive(Clone, Default)]
pub(super) struct DomainState {
    turns: HashMap<JsString, DomainTurn>,
}
#[derive(Clone, Default)]
struct DomainTurn {
    last_step: u64,
    open_step: Option<u64>,
    model_requested: bool,
    open_tool_calls: Vec<JsValue>,
}
impl DomainState {
    /// The old adapter's domain-free entries remain available. Once a durable
    /// turn exists, messages claiming inbox work and turn closure need the
    /// unported lifecycle; accepting them would invalidate this projection.
    pub(super) fn validate_adapter_entry(&self, entry: &JsValue) -> Result<(), AdapterError> {
        domain_supported(entry)?;
        let active = field(entry, "turnId")
            .as_string()
            .is_some_and(|id| self.turns.contains_key(id));
        let kind = field(entry, "type")
            .as_string()
            .map(lossy)
            .unwrap_or_default();
        let queued_message = matches!(
            kind.as_str(),
            "durable_message" | "assistant_message" | "tool_result_message"
        ) && field(field(field(entry, "message"), "metadata"), "queueItemId")
            .as_string()
            .is_some_and(|id| !id.is_empty());
        if active && (kind == "turn_result" || queued_message) {
            return Err(AdapterError::NodeHostRequired(format!(
                "Session domain validation for {kind} in an active turn requires the Node host."
            )));
        }
        Ok(())
    }

    pub(super) fn next(&self, entry: &JsValue) -> RecorderResult<Self> {
        let kind = field(entry, "type")
            .as_string()
            .map(lossy)
            .unwrap_or_default();
        if !matches!(
            kind.as_str(),
            "turn_started"
                | "step_started"
                | "step_completed"
                | "model_request"
                | "model_stream_event"
                | "tool_call"
                | "tool_result"
        ) {
            // Other domain lifecycles are not part of this recorder port.
            self.validate_adapter_entry(entry)?;
            if kind == "turn_result" {
                return Err(RecorderError::NodeHostRequired(
                    "Session domain validation for turn_result entries requires the Node host."
                        .into(),
                ));
            }
            return Ok(self.clone());
        }
        let turn_id = field(entry, "turnId").as_string().ok_or_else(|| {
            RecorderError::NodeHostRequired("Non-string turn ids require the Node host.".into())
        })?;
        let mut next = self.clone();
        if kind == "turn_started" {
            if next.turns.contains_key(turn_id) {
                return Err(domain(
                    "duplicate_turn_start",
                    format!("Turn {} has already started.", lossy(turn_id)),
                ));
            }
            if entry.get("inboxItemId").is_some() {
                return Err(RecorderError::NodeHostRequired(
                    "Queued turn starts require the Node host.".into(),
                ));
            }
            next.turns.insert(turn_id.clone(), DomainTurn::default());
            return Ok(next);
        }
        let turn = next.turns.get_mut(turn_id).ok_or_else(|| {
            domain(
                "event_outside_turn",
                format!("{kind} requires an active turn {}.", lossy(turn_id)),
            )
        })?;
        let step_value = field(entry, "step");
        let step = step_value.as_number();
        if kind == "step_started" {
            if !step.is_some_and(|n| {
                n.is_finite()
                    && n.fract() == 0.0
                    && n <= 9_007_199_254_740_991.0
                    && n > turn.last_step as f64
            }) {
                return Err(domain(
                    "invalid_step",
                    format!(
                        "Step {} must increase within turn {}.",
                        display(step_value)?,
                        lossy(turn_id)
                    ),
                ));
            }
            if let Some(open) = turn.open_step {
                return Err(domain(
                    "step_already_open",
                    format!("Turn {} already has open step {open}.", lossy(turn_id)),
                ));
            }
            let step = step.unwrap() as u64;
            turn.last_step = step;
            turn.open_step = Some(step);
            turn.model_requested = false;
            turn.open_tool_calls.clear();
            return Ok(next);
        }
        if turn.open_step.is_none() || step != turn.open_step.map(|n| n as f64) {
            return Err(domain(
                "step_not_open",
                format!(
                    "{kind} requires open step {} in turn {}.",
                    display(step_value)?,
                    lossy(turn_id)
                ),
            ));
        }
        let step = turn.open_step.unwrap();
        match kind.as_str() {
            "model_request" => {
                if turn.model_requested {
                    return Err(domain(
                        "duplicate_model_request",
                        format!("Step {step} already has a model request."),
                    ));
                }
                turn.model_requested = true;
            }
            "model_stream_event" if !turn.model_requested => {
                return Err(domain(
                    "model_request_missing",
                    format!("Step {step} emitted a model event before its request."),
                ));
            }
            "tool_call" => {
                let id = call_id(field(entry, "call"), "id")?;
                if !turn.model_requested {
                    return Err(domain(
                        "model_request_missing",
                        format!(
                            "Step {step} emitted tool call {} before its request.",
                            display(id)?
                        ),
                    ));
                }
                if turn.open_tool_calls.iter().any(|open| same_id(open, id)) {
                    return Err(domain(
                        "duplicate_tool_call",
                        format!("Tool call {} is already open.", display(id)?),
                    ));
                }
                turn.open_tool_calls.push(id.clone());
            }
            "tool_result" => {
                let id = call_id(field(entry, "result"), "toolCallId")?;
                let position = turn
                    .open_tool_calls
                    .iter()
                    .position(|open| same_id(open, id));
                let Some(position) = position else {
                    return Err(domain(
                        "tool_result_without_call",
                        format!(
                            "Tool result {} has no open call in step {step}.",
                            display(id)?
                        ),
                    ));
                };
                turn.open_tool_calls.remove(position);
            }
            "step_completed" => {
                if field(entry, "outcome")
                    .as_string()
                    .is_some_and(|s| s.equals_str("completed"))
                    && !turn.open_tool_calls.is_empty()
                {
                    return Err(domain(
                        "step_has_open_tool_calls",
                        format!("Step {step} completed with unresolved tool calls."),
                    ));
                }
                turn.open_step = None;
                turn.model_requested = false;
                turn.open_tool_calls.clear();
            }
            _ => {}
        }
        Ok(next)
    }
}
