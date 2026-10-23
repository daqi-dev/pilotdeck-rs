//! Durable model/tool and compaction transcript recording, ported from TS
//! `AgentSessionEventRecorder` (`recordModelRequest`/`recordModelEvent`/
//! `recordToolCalls`/`recordToolResults` at B/src/agent/session/
//! AgentSessionEventRecorder.ts:187-315, plus `recordCompactionStarted/
//! Completed/Failed` at :316-355) and the writer's atomic control-boundary
//! methods (`JsonlTranscriptWriter.ts:166-191`).
//!
//! The durable ports that call the model/tool methods are thin wrappers in TS
//! (`durableModelInvokerPort.ts:4-22`, `durableToolPort.ts`): a model request is
//! appended before the stream starts and every stream event is appended before
//! it is yielded; tool calls are appended before execution and results after it
//! (an aborted turn skips the result record). Those wrappers never own
//! transcript state, so this module ports the recorder surface they call. The
//! compaction bracket is driven by `durableContextRuntime.ts` (started, then a
//! deferred or immediate terminal) and `TurnRunner.ts:338-374` (atomic
//! replacement, commit, or failure).
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
//! High-level question/permission/inbox and completeTurn recorder methods are
//! outside this durable-port surface. Their transcript entries are validated by
//! the complete shared session-domain state machine when written via the writer.
//! `restore` only re-seeds the writer sequence/parent chain and the
//! domain-validation state; a restored turn is not an active recorder turn and
//! its pending compaction brackets are empty, exactly like constructing a fresh
//! TS recorder from restored entries.
//!
//! The port is synchronous: calls serialize on one recorder mutex. Sequential
//! calls match TS; no interleaving order is claimed for unsynchronized
//! concurrent callers (both durable ports invoke the recorder sequentially).
use super::{
    ledger::field,
    transcript::uuid,
    transcript_adapter::{AdapterError, TranscriptKey, TranscriptStore, TranscriptWriter},
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
    /// Projection outside the currently ported read surface.
    ProjectionUnavailable(String),
    Range(String),
    /// Host store failure, propagated unchanged.
    Store(super::LedgerError),
}
impl fmt::Display for RecorderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(message)
            | Self::Type(message)
            | Self::ProjectionUnavailable(message)
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
pub(super) fn domain(code: &'static str, message: impl Into<String>) -> RecorderError {
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
            AdapterError::ProjectionUnavailable(message) => Self::ProjectionUnavailable(message),
            AdapterError::Domain { code, message } => Self::Domain { code, message },
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
/// One `compaction_started` bracket. The operation id is kept as the raw JS
/// value: TS compares it with `===` (and stores it in a `Set` while replaying
/// the log), so two structurally equal objects must not match.
struct PendingCompaction {
    operation_id: JsValue,
    completion: Option<JsValue>,
}
struct Inner {
    turns: HashMap<JsString, Turn>,
    compactions: HashMap<JsString, Vec<PendingCompaction>>,
}

/// Durable recorder over a session's shared transcript writer.
pub struct Recorder {
    writer: Arc<TranscriptWriter>,
    sources: Option<(RecorderClock, RecorderUuid)>,
    /// TS `AgentSessionEventRecorderOptions.uuid`; separate from the writer's
    /// entry-id source because a recorder can be rebuilt over a live writer.
    operation_ids: Option<RecorderUuid>,
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
            operation_ids: None,
            inner: Mutex::new(Inner {
                turns: HashMap::new(),
                compactions: HashMap::new(),
            }),
        }
    }
    /// Inject the recorder's own compaction operation-id source
    /// (`AgentSessionEventRecorderOptions.uuid`). Entry ids stay on the writer's
    /// source, like TS.
    pub fn with_operation_ids(mut self, operation_ids: RecorderUuid) -> Self {
        self.operation_ids = Some(operation_ids);
        self
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
    /// TS `nextCompactionOperationId`: the recorder's UUID source, not the
    /// writer's. It consumes one id even when nothing is recorded afterwards.
    /// When no source is injected, `AgentSessionEventRecorderOptions.uuid`
    /// defaults to `node:crypto.randomUUID`, which is independent of the
    /// writer's entry-id source.
    pub fn next_compaction_operation_id(&self) -> RecorderResult<JsString> {
        let id = match &self.operation_ids {
            Some(operation_ids) => operation_ids()?,
            None => uuid().map_err(|error| RecorderError::Store(error.into()))?,
        };
        Ok(JsString::from(id))
    }
    /// TS `recordCompactionStarted` (AgentSessionEventRecorder:316-331): append
    /// the bracket first, then register the pending operation. A failed append
    /// leaves no pending state, exactly like TS.
    pub fn record_compaction_started(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.append(session_id, turn_id, spread("compaction_started", draft))?;
        inner
            .compactions
            .entry(compaction_key(session_id, turn_id))
            .or_default()
            .push(PendingCompaction {
                operation_id: operation_id(draft),
                completion: None,
            });
        Ok(())
    }
    /// TS `recordCompactionCompleted` (AgentSessionEventRecorder:333-344).
    pub fn record_compaction_completed(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.record_compaction_completed_locked(&mut inner, session_id, turn_id, draft)
    }
    fn record_compaction_completed_locked(
        &self,
        inner: &mut Inner,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        self.append(session_id, turn_id, spread("compaction_completed", draft))?;
        remove_pending(inner, session_id, turn_id, &operation_id(draft));
        Ok(())
    }
    /// TS `recordCompactionFailed` (AgentSessionEventRecorder:346-355).
    pub fn record_compaction_failed(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        self.record_compaction_failed_locked(&mut inner, session_id, turn_id, draft)
    }
    fn record_compaction_failed_locked(
        &self,
        inner: &mut Inner,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        self.append(session_id, turn_id, spread("compaction_failed", draft))?;
        remove_pending(inner, session_id, turn_id, &operation_id(draft));
        Ok(())
    }
    /// TS `deferCompactionCompletion` (AgentSessionEventRecorder:358-367):
    /// hold the terminal fact until the replacement surface is durable.
    pub fn defer_compaction_completion(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: &JsValue,
    ) -> RecorderResult<()> {
        let operation_id = operation_id(draft);
        let mut inner = self.lock();
        let pending = inner
            .compactions
            .get_mut(&compaction_key(session_id, turn_id))
            .and_then(|pending| {
                pending
                    .iter_mut()
                    .find(|candidate| same_id(&candidate.operation_id, &operation_id))
            });
        let Some(pending) = pending else {
            return Err(state(format!(
                "Compaction {} has no pending durable lifecycle.",
                display(&operation_id)?
            )));
        };
        pending.completion = Some(draft.clone());
        Ok(())
    }
    /// TS `commitDeferredCompaction` (AgentSessionEventRecorder:369-373): write
    /// the first deferred completion and close its bracket. No deferred entry
    /// is a no-op.
    pub fn commit_deferred_compaction(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
    ) -> RecorderResult<()> {
        // Select and commit under one lock: a concurrent commit/fail on the
        // same turn must not observe the bracket after it was closed.
        let mut inner = self.lock();
        let completion = inner
            .compactions
            .get(&compaction_key(session_id, turn_id))
            .and_then(|pending| {
                pending
                    .iter()
                    .find_map(|candidate| candidate.completion.clone())
            });
        if let Some(completion) = completion {
            self.record_compaction_completed_locked(&mut inner, session_id, turn_id, &completion)?;
        }
        Ok(())
    }
    /// TS `failDeferredCompaction` (AgentSessionEventRecorder:375-385). The
    /// caller has already resolved `Error#message` vs `String(error)`.
    pub fn fail_deferred_compaction(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        error: &str,
    ) -> RecorderResult<()> {
        let mut inner = self.lock();
        let operation_id = inner
            .compactions
            .get(&compaction_key(session_id, turn_id))
            .and_then(|pending| {
                pending
                    .iter()
                    .find(|candidate| candidate.completion.is_some())
                    .map(|candidate| candidate.operation_id.clone())
            });
        let Some(operation_id) = operation_id else {
            return Ok(());
        };
        let mut draft = JsObject::new();
        draft.insert("operationId".into(), operation_id);
        draft.insert("error".into(), JsValue::String(error.into()));
        self.record_compaction_failed_locked(
            &mut inner,
            session_id,
            turn_id,
            &JsValue::Object(draft),
        )
    }
    /// TS `JsonlTranscriptWriter.recordControlBoundary`
    /// (JsonlTranscriptWriter.ts:166-175): one raw control boundary.
    pub fn record_control_boundary(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        boundary: &JsValue,
    ) -> RecorderResult<()> {
        let mut draft = draft("control_boundary");
        draft.insert("boundary".into(), boundary.clone());
        self.append(session_id, turn_id, draft)
    }
    /// TS `JsonlTranscriptWriter.recordCompactionReplacement`
    /// (JsonlTranscriptWriter.ts:177-191): commit the complete replacement
    /// context in the same durable record.
    pub fn record_compaction_replacement(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        boundary_value: &JsValue,
        messages: &[JsValue],
    ) -> RecorderResult<()> {
        let mut boundary = JsObject::new();
        spread_into(&mut boundary, boundary_value);
        let mut snapshot = JsObject::new();
        snapshot.insert("version".into(), JsValue::Number(1.0));
        snapshot.insert("messages".into(), JsValue::Array(messages.to_vec()));
        boundary.insert("snapshot".into(), JsValue::Object(snapshot));
        let mut draft = draft("control_boundary");
        draft.insert("boundary".into(), JsValue::Object(boundary));
        self.append(session_id, turn_id, draft)
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
        inner.compactions.clear();
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
/// `{type, ...draft}`. Typed drafts are objects; arrays spread by index and a
/// string by code point, mirroring JS own-property enumeration. Other values
/// spread nothing.
fn spread(kind: &str, value: &JsValue) -> JsObject {
    let mut out = draft(kind);
    spread_into(&mut out, value);
    out
}
fn spread_into(out: &mut JsObject, value: &JsValue) {
    match value {
        JsValue::Object(object) => {
            for (key, child) in object.entries() {
                out.insert(key.clone(), child.clone());
            }
        }
        JsValue::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                out.insert(index.to_string().into(), child.clone());
            }
        }
        JsValue::String(string) => {
            let units = string.units();
            let mut index = 0usize;
            let mut at = 0usize;
            while at < units.len() {
                let len = if (0xd800..0xdc00).contains(&units[at])
                    && at + 1 < units.len()
                    && (0xdc00..0xe000).contains(&units[at + 1])
                {
                    2
                } else {
                    1
                };
                out.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(units[at..at + len].to_vec())),
                );
                index += 1;
                at += len;
            }
        }
        JsValue::Undefined | JsValue::Null | JsValue::Bool(_) | JsValue::Number(_) => {}
    }
}
/// `draft.operationId`; missing is `undefined`, like the JS property read.
fn operation_id(draft: &JsValue) -> JsValue {
    draft
        .get("operationId")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}
/// TS `compactionKey`: `${sessionId}\0${turnId}`.
fn compaction_key(session_id: &JsString, turn_id: &JsString) -> JsString {
    let mut units = session_id.units().to_vec();
    units.push(0);
    units.extend_from_slice(turn_id.units());
    JsString::from_utf16(units)
}
fn remove_pending(
    inner: &mut Inner,
    session_id: &JsString,
    turn_id: &JsString,
    operation_id: &JsValue,
) {
    let key = compaction_key(session_id, turn_id);
    let Some(pending) = inner.compactions.get_mut(&key) else {
        return;
    };
    pending.retain(|candidate| !same_id(&candidate.operation_id, operation_id));
    if pending.is_empty() {
        inner.compactions.remove(&key);
    }
}
fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
/// Property access happens only after the entry's JSON round trips.
pub(super) fn call_id<'a>(value: &'a JsValue, key: &str) -> RecorderResult<&'a JsValue> {
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
pub(super) fn display(value: &JsValue) -> RecorderResult<String> {
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
pub(super) fn same_id(left: &JsValue, right: &JsValue) -> bool {
    !matches!(left, JsValue::Object(_) | JsValue::Array(_)) && left == right
}
/// JS truthiness for the raw `operationId`; the draft is typed but the domain
/// log is validated exactly as TS reads it.
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
pub(super) fn safe_integer(value: Option<f64>) -> bool {
    value.is_some_and(|value| {
        value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
    })
}
/// `String(entry.operationId || "(unknown)")` from the TS error templates.
pub(super) fn compaction_subject(operation: &JsValue) -> RecorderResult<String> {
    if truthy(operation) {
        display(operation)
    } else {
        Ok("(unknown)".into())
    }
}
/// SessionDomainValidation.ts:359-367.
pub(super) fn validate_compaction_started(entry: &JsValue) -> RecorderResult<()> {
    let operation = field(entry, "operationId");
    let count = field(entry, "messageCount").as_number();
    if !truthy(operation) || !safe_integer(count) || count.is_some_and(|value| value < 0.0) {
        return Err(domain(
            "invalid_compaction",
            format!(
                "Compaction {} has invalid start metadata.",
                compaction_subject(operation)?
            ),
        ));
    }
    let max = field(entry, "maxContextTokens");
    if !max.is_undefined()
        && !max
            .as_number()
            .is_some_and(|value| safe_integer(Some(value)) && value > 0.0)
    {
        return Err(domain(
            "invalid_compaction",
            format!(
                "Compaction {} has invalid maxContextTokens.",
                display(operation)?
            ),
        ));
    }
    Ok(())
}
/// SessionDomainValidation.ts:371-377.
pub(super) fn validate_compaction_completed(entry: &JsValue) -> RecorderResult<()> {
    let operation = field(entry, "operationId");
    let count = field(entry, "messageCount").as_number();
    if !truthy(operation) || !safe_integer(count) || count.is_some_and(|value| value < 0.0) {
        return Err(domain(
            "invalid_compaction",
            format!(
                "Compaction {} has invalid completion metadata.",
                compaction_subject(operation)?
            ),
        ));
    }
    Ok(())
}
/// SessionDomainValidation.ts:379-384. `entry.error.length` throws for
/// null/undefined and reads as zero for an empty string or array, or for any
/// other value whose own `length` property is the number 0 (`{length: 0}`).
pub(super) fn validate_compaction_failed(entry: &JsValue) -> RecorderResult<()> {
    let operation = field(entry, "operationId");
    if !truthy(operation) {
        return Err(domain(
            "invalid_compaction",
            format!(
                "Compaction {} has invalid failure metadata.",
                compaction_subject(operation)?
            ),
        ));
    }
    let error = field(entry, "error");
    let empty = match error {
        JsValue::Undefined => {
            return Err(RecorderError::Type(
                "Cannot read properties of undefined (reading 'length')".into(),
            ));
        }
        JsValue::Null => {
            return Err(RecorderError::Type(
                "Cannot read properties of null (reading 'length')".into(),
            ));
        }
        JsValue::String(value) => value.is_empty(),
        JsValue::Array(values) => values.is_empty(),
        JsValue::Object(object) => object
            .get("length")
            .and_then(JsValue::as_number)
            .is_some_and(|length| length == 0.0),
        JsValue::Bool(_) | JsValue::Number(_) => false,
    };
    if empty {
        return Err(domain(
            "invalid_compaction",
            format!(
                "Compaction {} has invalid failure metadata.",
                display(operation)?
            ),
        ));
    }
    Ok(())
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
