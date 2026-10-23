//! Turn read surface (InProcessGateway.ts:2479-2481,2525-2553,2841-2861).
//!
//! Three read faces, none of which admits turns or writes history:
//! `active_turn_snapshot` is a bare delegation to the coordinator's replay
//! snapshot; `read_session_messages` merges durable history with a live replay
//! snapshot; `run_get` / `run_events` / `run_reattach` project the durable run
//! registry. The WS methods that forward to these live in the connection
//! layer (A7.2-pr5); nothing here allocates a WS or registry sequence.
//!
//! A turn snapshot is an epoch (`sameTurnEpoch`, InProcessGateway.ts:3659-
//! 3666): if it settles or is replaced while durable history is being read,
//! the history is reread so a completed turn cannot fall between the durable
//! result and the live replay snapshot. Exactly three attempts, no backoff —
//! TS has none either — and the third failure surfaces the baseline's exact
//! plain-`Error` message. A history reader rejection crosses unchanged: it
//! propagates on the attempt that threw, even when the epoch also moved.
//!
//! Snapshot consistency under concurrency: no lock is held across the history
//! await (the coordinator snapshots are self-contained), so a concurrent
//! publish can only change what the next attempt observes, never deadlock a
//! reader. The registry reads wait behind the same FIFO worker the publish
//! face uses, so a `run_events` result never observes events that a pending
//! durable append has not yet confirmed.
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::coordinator::{TurnEventCoordinator, same_turn_epoch};
use super::publish::{FileRegistry, RegistryFuture};
use super::replay::{ActiveTurnSnapshot, ActiveTurnSnapshotInput};
use crate::js_value;
use crate::storage::{RunRecord, RunRef, RunRegistryError};

/// `Number.MAX_SAFE_INTEGER`: the ceiling of `Number.isSafeInteger`, which the
/// cursor and limit validation at InProcessGateway.ts:2533-2538 rely on.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The error shapes the read face can surface. `DialogGatewayError` carries a
/// wire code; the two `readSessionMessages` throws are plain `Error`s; a
/// registry rejection crosses with its own `RunRegistryError` name and code,
/// or with the builtin JS name (`SyntaxError`, `TypeError`) of a corrupt
/// snapshot or a stored `lastSeq` ToPrimitive cannot convert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnReadError {
    /// `DialogGatewayError.code`, or the TS registry code (`conflict`,
    /// `not_found`, `terminal`) when the error crossed from the registry.
    /// Absent for plain `Error` throws.
    pub code: Option<String>,
    /// The JS error name: "DialogGatewayError", "Error", "RunRegistryError",
    /// or a builtin name like "SyntaxError" / "TypeError".
    pub name: String,
    /// The baseline's exact message text as UTF-16 units: a run id may carry
    /// an unpaired surrogate, which no Rust `String` can hold.
    pub message: JsString,
}
impl TurnReadError {
    /// A `DialogGatewayError` (InProcessGateway.ts:172) with its stable code.
    fn dialog(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            code: Some(code.to_string()),
            name: "DialogGatewayError".to_string(),
            message: message.into(),
        }
    }
    /// A plain `Error` throw, exactly as the TS `readSessionMessages` guards
    /// raise them (InProcessGateway.ts:2842-2846,2860): no code.
    fn plain(message: impl Into<JsString>) -> Self {
        Self {
            code: None,
            name: "Error".to_string(),
            message: message.into(),
        }
    }
}
impl fmt::Display for TurnReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Rust-side rendering only: an unpaired surrogate becomes U+FFFD
        // here, while the wire message keeps the exact units.
        f.write_str(&String::from_utf16_lossy(self.message.units()))
    }
}
impl std::error::Error for TurnReadError {}

/// A registry rejection crosses the read face unchanged: TS propagates the
/// thrown error object (InProcessGateway.ts:2525-2528,2551-2552) with its
/// `name` and `code` — `RunRegistryError` for business codes, the builtin JS
/// name (`SyntaxError` / `TypeError`) for a corrupt snapshot, plain `Error`
/// for storage failures without a code.
fn registry_error(error: RunRegistryError) -> TurnReadError {
    TurnReadError {
        code: error.code().map(str::to_string),
        name: error.name().to_string(),
        message: error.to_string().into(),
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}

/// `Number.isSafeInteger` for a wire value: numbers only, integral, within
/// ±(2^53 - 1). NaN and Infinity fail the integral test like in JS.
fn is_safe_integer(value: &JsValue) -> bool {
    value
        .as_number()
        .is_some_and(|n| n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER)
}

/// The full cursor predicate of InProcessGateway.ts:2533 — `afterSeq` must be
/// a non-negative safe integer. `-0` passes, exactly like `-0 < 0` in JS.
fn is_non_negative_safe_integer(value: &JsValue) -> bool {
    is_safe_integer(value) && value.as_number().is_some_and(|n| n >= 0.0)
}

/// The page-size predicate of InProcessGateway.ts:2536 — `limit` must be an
/// integer between 1 and 500 inclusive.
fn is_valid_limit(value: &JsValue) -> bool {
    is_safe_integer(value)
        && value
            .as_number()
            .is_some_and(|n| (1.0..=500.0).contains(&n))
}

/// The ToPrimitive failure the gap check's relational `>` can raise:
/// `TypeError: Cannot convert object to primitive value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotPrimitive;

/// Whether an object's ToPrimitive still has a primitive source. JSON data
/// cannot hold callables, so an own `toString` property — whatever its value
/// — shadows the inherited `Object.prototype.toString` with data, and the
/// inherited `valueOf` only ever returns the object itself; V8 then throws
/// (node probe: `6 > {toString: null}` and `6 > {valueOf: 5, toString: 5}`
/// both raise the TypeError, while `6 > {valueOf: 5}` stays false because the
/// inherited `toString` still answers `[object Object]`).
fn own_to_string(object: &JsObject) -> Result<(), NotPrimitive> {
    if object.get_property("toString").is_some() {
        Err(NotPrimitive)
    } else {
        Ok(())
    }
}

/// `ToNumber` for the stored `lastSeq`, with ToPrimitive failures kept: the
/// JS relational `>` (InProcessGateway.ts:2541-2542) applies ToNumber to the
/// stored value before comparing, so an object — or an array element — with
/// an own `toString` throws V8's TypeError instead of quietly comparing
/// against NaN.
///
/// Unlike the shared [`js_value::to_number`], the array walk below carries
/// no depth bound: V8's join recursion converts arrays thousands of layers
/// deep inside `runEvents` (measured, node 22: 2530 layers convert, 2540
/// throw; caller-stack dependent), and the shared 2000-layer gate
/// (A6-JSVALUE-DEPTH-001, unapproved) must not reach this comparison.
fn last_seq_to_number(value: &JsValue) -> Result<f64, NotPrimitive> {
    match value {
        // Both ToPrimitive hints consult the same two methods, and for JSON
        // data only the inherited `toString` can answer (see
        // [`own_to_string`]); its `[object Object]` is never a number.
        JsValue::Object(object) => own_to_string(object).map(|()| f64::NAN),
        JsValue::Array(_) => Ok(js_value::string_to_number(&join_string(value)?)),
        // Only the array walk fails in the shared converter, and arrays are
        // matched above; a scalar is always `Ok` (NaN for `undefined` and
        // unparseable strings, exactly like ToNumber).
        scalar => Ok(js_value::to_number(scalar).unwrap_or(f64::NAN)),
    }
}

/// `Array.prototype.join` for a JSON-representable wire array, iterative and
/// without a depth bound (see [`last_seq_to_number`]). Object elements keep
/// ToPrimitive's failure — an own `toString` throws instead of rendering.
fn join_string(value: &JsValue) -> Result<String, NotPrimitive> {
    let mut out = Vec::new();
    // One frame per array being joined: its remaining items and whether a
    // separator is due before the next one.
    let mut frames: Vec<(std::slice::Iter<'_, JsValue>, bool)> = Vec::new();
    let mut next = Some(value);
    loop {
        match next.take() {
            Some(JsValue::Array(items)) => frames.push((items.iter(), false)),
            Some(JsValue::Object(object)) => {
                own_to_string(object)?;
                out.extend("[object Object]".encode_utf16());
            }
            Some(scalar) => join_scalar_units(scalar, &mut out),
            None => {}
        }
        let Some((items, separate)) = frames.last_mut() else {
            return Ok(String::from_utf16_lossy(&out));
        };
        match items.next() {
            Some(item) => {
                if *separate {
                    out.push(u16::from(b','));
                }
                *separate = true;
                if !matches!(item, JsValue::Undefined | JsValue::Null) {
                    next = Some(item);
                }
            }
            None => {
                frames.pop();
            }
        }
    }
}

/// `ToString` of a joined element: `null` / `undefined` join as empty
/// strings; a lone surrogate survives the units and never parses as a
/// number, so the lossy fold at the end of [`join_string`] is equivalent.
fn join_scalar_units(value: &JsValue, out: &mut Vec<u16>) {
    match value {
        JsValue::String(s) => out.extend_from_slice(s.units()),
        JsValue::Bool(b) => out.extend(b.to_string().encode_utf16()),
        JsValue::Number(n) => out.extend(js_value::number_string(*n).encode_utf16()),
        JsValue::Undefined | JsValue::Null => {}
        JsValue::Object(_) | JsValue::Array(_) => unreachable!("containers are joined above"),
    }
}

/// The `options.readSessionMessages` port (InProcessGateway.ts:279): the
/// host-owned durable history reader. Implementations return the history
/// result object; the read face attaches the replay snapshot to it.
pub trait SessionHistoryReader: Send + Sync {
    fn read(
        &self,
        input: &JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, TurnReadError>> + Send>>;
}

/// The registry read face the gateway read methods use (`RunRegistryPort.get`
/// and `.events`, RunRegistry.ts:266-283). The publish face is
/// [`super::publish::TurnRunRegistry`]; both are satisfied by one
/// [`FileRegistry`], sharing its FIFO worker and durable store.
pub trait TurnRunReader: Send + Sync {
    fn get(&self, run: &RunRef) -> RegistryFuture<Option<RunRecord>>;
    /// Stored `{ seq, event }` entries with `seq > after_seq` (default 0),
    /// then `slice(0, limit)` (default 500). Cursor and limit validation
    /// belongs to the gateway method, not storage.
    fn events(
        &self,
        run: &RunRef,
        after_seq: Option<f64>,
        limit: Option<f64>,
    ) -> RegistryFuture<Vec<JsValue>>;
}

impl TurnRunReader for FileRegistry {
    fn get(&self, run: &RunRef) -> RegistryFuture<Option<RunRecord>> {
        let run = run.clone();
        self.call(move |registry| registry.get(&run))
    }
    fn events(
        &self,
        run: &RunRef,
        after_seq: Option<f64>,
        limit: Option<f64>,
    ) -> RegistryFuture<Vec<JsValue>> {
        let run = run.clone();
        self.call(move |registry| registry.events(&run, after_seq, limit))
    }
}

/// The `run_get` / `run_events` / `run_reattach` input cursor and page size.
/// The raw wire values reach validation un-coerced: `afterSeq: "3"` or `1.5`
/// must fail `Number.isSafeInteger` exactly like TS, so they are kept as
/// [`JsValue`]s here and only converted once validated.
#[derive(Clone, Debug, PartialEq)]
pub struct RunEventsInput {
    pub run: RunRef,
    pub after_seq: Option<JsValue>,
    pub limit: Option<JsValue>,
}

/// InProcessGateway.ts:2479-2481: the gateway method is a bare delegation to
/// the coordinator's replay snapshot.
pub fn active_turn_snapshot(
    coordinator: &TurnEventCoordinator,
    input: &ActiveTurnSnapshotInput,
) -> ActiveTurnSnapshot {
    coordinator.snapshot(input)
}

/// InProcessGateway.ts:2841-2861. `history` is the host-owned durable reader;
/// `None` is the unconfigured gateway. `input` is the caller's
/// `read_session_messages` request, passed through unchanged. Three attempts,
/// no backoff — TS has none either.
pub const HISTORY_EPOCH_ATTEMPTS: usize = 3;

pub async fn read_session_messages(
    coordinator: &TurnEventCoordinator,
    history: Option<&dyn SessionHistoryReader>,
    input: &JsValue,
) -> Result<JsValue, TurnReadError> {
    let Some(history) = history else {
        return Err(TurnReadError::plain(
            "read_session_messages is not configured. Wire `readSessionMessages` via createLocalGateway.",
        ));
    };
    let session_key = input
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    for _ in 0..HISTORY_EPOCH_ATTEMPTS {
        let before = coordinator
            .snapshot(&ActiveTurnSnapshotInput::new(session_key.clone()).without_events());
        let history = history.read(input).await?;
        let stream = coordinator.snapshot(&ActiveTurnSnapshotInput::new(session_key.clone()));
        if !same_turn_epoch(&before, &stream) {
            continue;
        }
        return Ok(merge_history_and_stream(history, &stream));
    }
    Err(TurnReadError::plain(
        "Session changed during transcript synchronization; retry the snapshot.",
    ))
}

/// InProcessGateway.ts:2858: `{ ...history, stream }`. Key order follows the
/// history object; a `stream` key the host already returned keeps its position
/// and gets the fresh snapshot's value.
fn merge_history_and_stream(history: JsValue, stream: &ActiveTurnSnapshot) -> JsValue {
    let mut merged = history.as_object().cloned().unwrap_or_default();
    merged.insert("stream".into(), stream.to_json());
    JsValue::Object(merged)
}

/// InProcessGateway.ts:2525-2529.
pub async fn run_get(
    registry: Option<&dyn TurnRunReader>,
    run: &RunRef,
) -> Result<Option<JsValue>, TurnReadError> {
    let Some(registry) = registry else {
        return Err(TurnReadError::dialog(
            "CAPABILITY_UNAVAILABLE",
            "Persistent run registry is unavailable.",
        ));
    };
    let record = registry.get(run).await.map_err(registry_error)?;
    Ok(record.map(|record| to_gateway_run_record(&record)))
}

/// `` `Run ${runId} is not registered.` `` (InProcessGateway.ts:2540) as
/// UTF-16 units: a run id may carry an unpaired surrogate, and the TS template
/// literal keeps it in the message verbatim.
fn run_not_found_message(run_id: &JsString) -> JsString {
    let mut units = "Run ".encode_utf16().collect::<Vec<u16>>();
    units.extend_from_slice(run_id.units());
    units.extend(" is not registered.".encode_utf16());
    JsString::from_utf16(units)
}

/// InProcessGateway.ts:2531-2549.
pub async fn run_events(
    registry: Option<&dyn TurnRunReader>,
    input: &RunEventsInput,
) -> Result<JsValue, TurnReadError> {
    let Some(registry) = registry else {
        return Err(TurnReadError::dialog(
            "CAPABILITY_UNAVAILABLE",
            "Persistent run registry is unavailable.",
        ));
    };
    let after_seq = input
        .after_seq
        .as_ref()
        .filter(|value| !value.is_undefined());
    if let Some(value) = after_seq
        && !is_non_negative_safe_integer(value)
    {
        return Err(TurnReadError::dialog(
            "INVALID_RUN_CURSOR",
            "afterSeq must be a non-negative safe integer.",
        ));
    }
    let limit = input.limit.as_ref().filter(|value| !value.is_undefined());
    if let Some(value) = limit
        && !is_valid_limit(value)
    {
        return Err(TurnReadError::dialog(
            "INVALID_RUN_LIMIT",
            "limit must be an integer between 1 and 500.",
        ));
    }
    let Some(record) = registry.get(&input.run).await.map_err(registry_error)? else {
        return Err(TurnReadError::dialog(
            "RUN_NOT_FOUND",
            run_not_found_message(&input.run.run_id),
        ));
    };
    if let Some(after) = after_seq {
        // TS compares against the stored record's `lastSeq` with the JS
        // relational `>` (InProcessGateway.ts:2541-2542): the stored value may
        // be a string / null / boolean / array, and ToNumber applies before
        // the comparison (NaN → false). A ToPrimitive failure throws with
        // V8's exact shape — and the conversion must be the ungated one, so
        // a deep stored array converts like V8's join does here instead of
        // hitting the shared converter's conservative depth bound.
        let last = field(record.as_json(), "lastSeq");
        let gap = match last_seq_to_number(last) {
            Ok(last) => after.as_number().is_some_and(|a| a > last),
            Err(NotPrimitive) => {
                return Err(TurnReadError {
                    code: None,
                    name: "TypeError".to_string(),
                    message: js_value::ToStringError::NOT_PRIMITIVE.into(),
                });
            }
        };
        if gap {
            let mut gap = JsObject::new();
            gap.insert("events".into(), JsValue::Array(vec![]));
            gap.insert("nextSeq".into(), field(record.as_json(), "lastSeq").clone());
            gap.insert("gap".into(), JsValue::Bool(true));
            return Ok(JsValue::Object(gap));
        }
    }
    let events = registry
        .events(
            &input.run,
            after_seq.and_then(JsValue::as_number),
            limit.and_then(JsValue::as_number),
        )
        .await
        .map_err(registry_error)?;
    let mut result = JsObject::new();
    result.insert("events".into(), JsValue::Array(events.clone()));
    if let Some(last) = events.last() {
        result.insert("nextSeq".into(), field(last, "seq").clone());
    }
    Ok(JsValue::Object(result))
}

/// InProcessGateway.ts:2551-2553: reattach is the same durable lookup.
pub async fn run_reattach(
    registry: Option<&dyn TurnRunReader>,
    run: &RunRef,
) -> Result<Option<JsValue>, TurnReadError> {
    run_get(registry, run).await
}

/// InProcessGateway.ts:225-237: the gateway projection of a registry record.
/// Key order is fixed by the TS object literal; `requestMaterial` and
/// `ownerPid` stay private to the registry, and `projectKey` / `result` are
/// only present when the stored record defines them (a `null` project key
/// survives, matching `record.projectKey === undefined ? {} : ...`).
fn to_gateway_run_record(record: &RunRecord) -> JsValue {
    let record = record.as_json();
    let mut mapped = JsObject::new();
    mapped.insert("sessionKey".into(), field(record, "sessionKey").clone());
    if let Some(project) = record
        .get("projectKey")
        .filter(|value| !value.is_undefined())
    {
        mapped.insert("projectKey".into(), project.clone());
    }
    for key in [
        "runId",
        "state",
        "revision",
        "lastSeq",
        "acceptedAt",
        "updatedAt",
    ] {
        mapped.insert(key.into(), field(record, key).clone());
    }
    if let Some(result) = record.get("result").filter(|value| !value.is_undefined()) {
        mapped.insert("result".into(), result.clone());
    }
    JsValue::Object(mapped)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
