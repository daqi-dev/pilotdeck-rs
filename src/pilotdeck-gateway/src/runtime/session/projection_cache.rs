//! Disposable checkpoint rows for the factory's durable session projections.
//! The transcript remains authoritative. Missing/unsupported rows replay it.
use super::*;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::io::Write;

const METADATA: &str = "agent-transcript.metadata";
const SUMMARY: &str = "agent-transcript.turn-summary";

pub trait ProjectionCheckpointStore: Send + Sync {
    fn load(&self) -> Option<JsValue>;
    fn save(&self, envelope: &JsValue) -> RuntimeResult;
}

pub struct FileProjectionCheckpointStore(pub PathBuf);
impl ProjectionCheckpointStore for FileProjectionCheckpointStore {
    fn load(&self) -> Option<JsValue> {
        // The checkpoint is a disposable cache, never authoritative. Past the
        // transcript read bound it is already unusable for a writable chain, so
        // treat it as missing and let restore replay the transcript instead of
        // allocating and parsing an unbounded file.
        let len = std::fs::metadata(&self.0).ok()?.len();
        if len > crate::storage::TRANSCRIPT_MAX_BYTES {
            return None;
        }
        parse_json(&std::fs::read_to_string(&self.0).ok()?).ok()
    }
    fn save(&self, envelope: &JsValue) -> RuntimeResult {
        let run = || -> Result<(), Box<dyn std::error::Error>> {
            let parent = self.0.parent().ok_or("checkpoint has no parent")?;
            let mut dirs = std::fs::DirBuilder::new();
            dirs.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                dirs.mode(0o700);
            }
            dirs.create(parent)?;
            let temporary = parent.join(format!(".projection-{}.tmp", uuid::Uuid::new_v4()));
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
            let _cleanup = Cleanup(temporary.clone());
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            let text = stringify_json(envelope)?.ok_or("checkpoint cannot be serialized")?;
            file.write_all(text.as_bytes())?;
            file.write_all(b"\n")?;
            std::fs::rename(&temporary, &self.0)?;
            Ok(())
        };
        run().map_err(|e| RuntimeError::new(e.to_string()))
    }
}

pub(super) struct ProjectionCache {
    pub(super) store: Arc<dyn ProjectionCheckpointStore>,
    transcript: Arc<dyn TranscriptStore>,
    key: TranscriptKey,
    projection: Arc<SessionProjection>,
    writes: Mutex<()>,
    pending: Mutex<PendingWrites>,
    done: std::sync::Condvar,
    pub(super) diagnostic: Mutex<Option<String>>,
    #[cfg(test)]
    pub(super) hydrated_rows: Mutex<usize>,
}
struct PendingWrites {
    active: bool,
    submitted: u64,
    completed: u64,
}
impl ProjectionCache {
    pub(super) fn new(
        store: Arc<dyn ProjectionCheckpointStore>,
        transcript: Arc<dyn TranscriptStore>,
        key: TranscriptKey,
        projection: Arc<SessionProjection>,
    ) -> Self {
        Self {
            #[cfg(test)]
            hydrated_rows: Mutex::new(0),
            store,
            transcript,
            key,
            projection,
            writes: Mutex::new(()),
            pending: Mutex::new(PendingWrites {
                active: true,
                submitted: 0,
                completed: 0,
            }),
            done: std::sync::Condvar::new(),
            diagnostic: Mutex::new(None),
        }
    }
    pub(super) fn restore(&self, mut entries: Vec<JsValue>) -> RuntimeResult {
        // Validate envelope and turn/step invariants without rewinding the
        // published recorder (whose revision may have advanced after read).
        Recorder::new(self.transcript.clone(), self.key.clone())
            .restore(&entries)
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        #[cfg(test)]
        {
            *lock(&self.hydrated_rows) = 0;
        }
        let checkpoint = self
            .load()
            .filter(|value| valid_envelope(value, &self.key.session_id, &entries));
        // Merge only durable commits published after this read. Keep the
        // projection lock until installation; append uses the same lock after
        // disk commit, so it either joins this suffix or publishes afterwards.
        let mut live = lock(&self.projection.entries);
        let read_watermark = last_sequence(&entries);
        entries.extend(
            live.iter()
                .filter(|entry| sequence(entry) > read_watermark)
                .cloned(),
        );
        let mut state = project(&entries)?;
        // Conversation contains replay context (including incomplete turns).
        // Keep that authoritative replay; cache only rows with standalone state.
        if let Some(checkpoint) = checkpoint {
            let rows = field(&checkpoint, "projections");
            if let Some((watermark, cached)) =
                row(rows, METADATA, 2.0).filter(|(_, value)| valid_metadata(value))
            {
                #[cfg(test)]
                {
                    *lock(&self.hydrated_rows) += 1;
                }
                let mut metadata = spread(cached);
                for entry in &entries {
                    if sequence(entry) > watermark {
                        let next = project(std::slice::from_ref(entry))?;
                        merge_metadata(&mut metadata, &next.metadata);
                    }
                }
                state.metadata = metadata;
            }
            if let Some((watermark, cached)) =
                row(rows, SUMMARY, 1.0).filter(|(_, value)| valid_summary(value))
            {
                #[cfg(test)]
                {
                    *lock(&self.hydrated_rows) += 1;
                }
                let mut usage = spread(field(cached, "usage"));
                let mut denials = array(field(cached, "permissionDenials"))?.to_vec();
                for entry in entries.iter().filter(|entry| {
                    sequence(entry) > watermark && is(field(entry, "type"), "turn_result")
                }) {
                    let result = field(entry, "result");
                    let mut merged = JsObject::new();
                    for key in [
                        "inputTokens",
                        "outputTokens",
                        "cacheReadTokens",
                        "cacheWriteTokens",
                        "totalTokens",
                    ] {
                        let prior = usage.get(key).cloned().unwrap_or(JsValue::Undefined);
                        let next = field(field(result, "usage"), key);
                        if !prior.is_undefined() || !next.is_undefined() {
                            merged.insert(key.into(), add_usage(&prior, next)?);
                        }
                    }
                    usage = merged;
                    denials.extend_from_slice(array(field(result, "permissionDenials"))?);
                }
                state.usage = usage;
                state.permission_denials = denials;
            }
        }
        *lock(&self.projection.completed_turns) = completed_turns(&entries);
        *live = entries;
        *lock(&self.projection.last_valid) = state;
        self.projection.valid.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn load(&self) -> Option<JsValue> {
        let target = lock(&self.pending).submitted;
        self.wait_for(target);
        let _write = lock(&self.writes);
        self.store.load()
    }
    fn wait_for(&self, target: u64) {
        let mut pending = lock(&self.pending);
        while pending.completed < target {
            pending = self.done.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }
    fn request(self: &Arc<Self>) -> Option<u64> {
        let mut pending = lock(&self.pending);
        if !pending.active {
            return None;
        }
        let start_worker = pending.completed == pending.submitted;
        pending.submitted += 1;
        let ticket = pending.submitted;
        if start_worker {
            let cache = self.clone();
            std::thread::spawn(move || {
                loop {
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cache.capture()));
                    if outcome.is_err() {
                        *lock(&cache.diagnostic) =
                            Some("Projection checkpoint store panicked.".into());
                    }
                    let mut pending = lock(&cache.pending);
                    pending.completed += 1;
                    cache.done.notify_all();
                    if pending.completed == pending.submitted {
                        break;
                    }
                }
            });
        }
        Some(ticket)
    }
    pub(super) fn schedule(self: &Arc<Self>) {
        let _ = self.request();
    }
    /// The flush ticket follows every previously admitted automatic save.
    pub(super) fn flush(self: &Arc<Self>) {
        if let Some(ticket) = self.request() {
            self.wait_for(ticket);
        }
    }
    fn capture(&self) {
        let _write = lock(&self.writes);
        let capture = || -> RuntimeResult {
            for _ in 0..3 {
                let read = self
                    .transcript
                    .read(&self.key, None)
                    .map_err(|e| RuntimeError::new(e.to_string()))?;
                read.ensure_writable()
                    .map_err(|e| RuntimeError::new(e.to_string()))?;
                let entries = lock(&self.projection.entries);
                if last_sequence(&entries) != last_sequence(&read.entries) {
                    continue;
                }
                let state = project(&entries)?;
                let envelope = envelope(&self.key.session_id, &entries, &state);
                drop(entries);
                self.store.save(&envelope)?;
                return Ok(());
            }
            Err(RuntimeError::new(format!(
                "Could not capture a consistent projection checkpoint for {}.",
                String::from_utf16_lossy(self.key.session_id.units())
            )))
        };
        if let Err(error) = capture() {
            *lock(&self.diagnostic) = Some(error.message);
        }
    }
    pub(super) fn dispose(&self) {
        let target = {
            let mut pending = lock(&self.pending);
            pending.active = false;
            pending.submitted
        };
        self.wait_for(target);
    }
}
fn sequence(entry: &JsValue) -> f64 {
    field(entry, "sequence").as_number().unwrap_or(-1.0)
}
fn last_sequence(entries: &[JsValue]) -> f64 {
    entries.iter().map(sequence).fold(-1.0, f64::max)
}
fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut value = JsObject::new();
    for (key, item) in fields {
        value.insert((*key).into(), item.clone());
    }
    JsValue::Object(value)
}
fn anchor(entry: &JsValue) -> JsValue {
    object(
        &[
            "sequence",
            "entryId",
            "parentEntryId",
            "turnId",
            "type",
            "createdAt",
        ]
        .map(|name| {
            let value = field(entry, name);
            (
                name,
                if value.is_undefined() {
                    JsValue::Null
                } else {
                    value.clone()
                },
            )
        }),
    )
}
fn safe_sequence(value: &JsValue) -> bool {
    value.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && (-1.0..=9_007_199_254_740_991.0).contains(&n)
    })
}
fn valid_envelope(value: &JsValue, session: &JsString, entries: &[JsValue]) -> bool {
    let watermark = field(value, "asOfSequence");
    if !is(field(value, "format"), "pilotdeck.session-projections")
        || field(value, "version").as_number() != Some(1.0)
        || field(value, "sessionId").as_string() != Some(session)
        || !safe_sequence(watermark)
    {
        return false;
    }
    let Some(rows) = field(value, "projections").as_object() else {
        return false;
    };
    for (name, row) in rows.entries() {
        if String::from_utf16_lossy(name.units()).trim().is_empty()
            || row.as_object().is_none()
            || !field(row, "version")
                .as_number()
                .is_some_and(|n| (1.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0)
            || !safe_sequence(field(row, "asOfSequence"))
            || field(row, "asOfSequence").as_number() > watermark.as_number()
            || field(row, "state").is_undefined()
        {
            return false;
        }
    }
    if watermark.as_number() == Some(-1.0) {
        return field(value, "anchor") == &JsValue::Null;
    }
    entries
        .iter()
        .find(|e| field(e, "sequence") == watermark)
        .is_some_and(|entry| {
            let expected = anchor(entry);
            [
                "sequence",
                "entryId",
                "parentEntryId",
                "turnId",
                "type",
                "createdAt",
            ]
            .iter()
            .all(|key| field(&expected, key) == field(field(value, "anchor"), key))
        })
}
fn row<'a>(rows: &'a JsValue, name: &str, version: f64) -> Option<(f64, &'a JsValue)> {
    let row = field(rows, name);
    (field(row, "version").as_number() == Some(version)).then(|| {
        (
            field(row, "asOfSequence").as_number().unwrap_or(-1.0),
            field(row, "state"),
        )
    })
}
fn valid_summary(value: &JsValue) -> bool {
    let Some(usage) = field(value, "usage").as_object() else {
        return false;
    };
    for key in [
        "inputTokens",
        "outputTokens",
        "totalTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "nativeCost",
    ] {
        if let Some(n) = usage.get(key)
            && !n.as_number().is_some_and(|n| n.is_finite() && n >= 0.0)
        {
            return false;
        }
    }
    array(field(value, "permissionDenials")).is_ok_and(|values| {
        values.iter().all(|v| {
            field(v, "toolName").as_string().is_some()
                && field(v, "toolCallId").as_string().is_some()
                && (field(v, "errorCode").is_undefined()
                    || field(v, "errorCode").as_string().is_some())
        })
    })
}
fn valid_metadata(value: &JsValue) -> bool {
    if value.as_object().is_none() {
        return false;
    }
    for name in [
        "title",
        "aiTitle",
        "tag",
        "firstPrompt",
        "lastPrompt",
        "gitBranch",
        "parentSessionId",
        "forkedFromTurnId",
        "titleProviderId",
        "titleSourceTurnId",
        "updatedAt",
    ] {
        let v = field(value, name);
        if !v.is_undefined() && v.as_string().is_none() {
            return false;
        }
    }
    let v = field(value, "titleSource");
    if !v.is_undefined()
        && !["user", "provider", "fallback"]
            .iter()
            .any(|name| is(v, name))
    {
        return false;
    }
    let v = field(value, "titleMessageSequences");
    if !v.is_undefined()
        && !array(v).is_ok_and(|items| {
            items
                .iter()
                .all(|n| safe_sequence(n) && n.as_number() > Some(0.0))
        })
    {
        return false;
    }
    let v = field(value, "titleModel");
    if !v.is_undefined()
        && (field(v, "provider").as_string().is_none() || field(v, "model").as_string().is_none())
    {
        return false;
    }
    let v = field(value, "isSnapshot");
    if !v.is_undefined() && v != &JsValue::Bool(true) {
        return false;
    }
    let v = field(value, "mode");
    if !v.is_undefined() && !is(v, "normal") && !is(v, "coordinator") {
        return false;
    }
    let v = field(value, "modelSelection");
    if !matches!(v, JsValue::Undefined | JsValue::Null)
        && !(is(field(v, "mode"), "auto")
            || (is(field(v, "mode"), "model")
                && field(v, "provider").as_string().is_some()
                && field(v, "model").as_string().is_some()))
    {
        return false;
    }
    let v = field(value, "linkedPullRequest");
    if !v.is_undefined()
        && !(field(v, "number").as_number().is_some_and(|n| {
            n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0
        }) && field(v, "url").as_string().is_some()
            && field(v, "repository").as_string().is_some())
    {
        return false;
    }
    true
}
fn envelope(session: &JsString, entries: &[JsValue], state: &DurableSessionState) -> JsValue {
    let watermark = last_sequence(entries);
    let row = |version, state| {
        object(&[
            ("version", JsValue::Number(version)),
            ("asOfSequence", JsValue::Number(watermark)),
            ("state", state),
        ])
    };
    object(&[
        (
            "format",
            JsValue::String("pilotdeck.session-projections".into()),
        ),
        ("version", JsValue::Number(1.0)),
        ("sessionId", JsValue::String(session.clone())),
        ("asOfSequence", JsValue::Number(watermark)),
        (
            "anchor",
            entries
                .iter()
                .find(|e| sequence(e) == watermark)
                .map(anchor)
                .unwrap_or(JsValue::Null),
        ),
        (
            "projections",
            object(&[
                (METADATA, row(2.0, JsValue::Object(state.metadata.clone()))),
                (
                    SUMMARY,
                    row(
                        1.0,
                        object(&[
                            ("usage", JsValue::Object(state.usage.clone())),
                            (
                                "permissionDenials",
                                JsValue::Array(state.permission_denials.clone()),
                            ),
                        ]),
                    ),
                ),
            ]),
        ),
    ])
}

/// FIFO admission for storage operations. Dropping a waiting future does not
/// cancel its admitted task; shutting down its executor still drops the job
/// and releases the existing writer guards.
#[derive(Default)]
pub(super) struct StorageQueue {
    tail: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl StorageQueue {
    pub(super) fn enqueue<T: Send + 'static>(
        self: &Arc<Self>,
        future: impl std::future::Future<Output = Result<T, RuntimeError>> + Send + 'static,
    ) -> BoxFuture<Result<T, RuntimeError>> {
        let (finish, next) = tokio::sync::oneshot::channel();
        let previous = lock(&self.tail).replace(next);
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            // The sender's drop releases the next ticket on success, panic,
            // or executor cancellation. Caller cancellation owns neither.
            let _finish = finish;
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let result = tokio::spawn(future).await;
            let _ = send.send(result);
        });
        Box::pin(async move {
            match receive.await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Ok(Err(error)) => Err(RuntimeError::new(error.to_string())),
                Err(_) => Err(RuntimeError::new("Session storage executor stopped.")),
            }
        })
    }
}
