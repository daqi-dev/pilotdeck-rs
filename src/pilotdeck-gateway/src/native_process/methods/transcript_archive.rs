//! #2622: the `session_transcript_archive` pair — `export_session_transcript`
//! and `restore_session_transcript`, ported from the TS default CLI
//! (A7.5-TRANSCRIPT-ARCHIVE-DEFERRED-001, closed by
//! `A7.5-TRANSCRIPT-ARCHIVE-DEFERRED-001-R1`).
//!
//! TS baseline (`resources/PilotDeck`, cc7c4e6):
//!  - export delegate: `src/cli/createLocalGateway.ts:1162-1181`
//!  - restore delegate: `src/cli/createLocalGateway.ts:1315-1367`
//!  - portable-archive helpers: `src/cli/createLocalGateway.ts:1610-1612`
//!    (the three ceilings), `:1633-1648` (`toPortableSessionArchive`),
//!    `:1651-1668` (`portableTranscriptText`), `:1671-1676`
//!    (`stringifyPortableTranscriptValue`), `:1680-1703`
//!    (`validatePortableSessionArchive`)
//!  - tool-result rendering: `src/model/protocol/toolResultContent.ts:5-28`
//!
//! The conversation replay is the shared fold in `session_model` (#297 P2#3):
//! export consumes the same projected `messages` and the same merged
//! `title`/`aiTitle` that module's selection replay produces, so a port change
//! cannot drift between `session_model_get` and the archive.
//!
//! Registered Rust-side differences (`-R1` ledger entry): the restore set
//! add/remove runs inside the fenced blocking task (R12, cancellation), the
//! exists check runs under the same #1457 transcript fence so a serialized
//! second restore sees the conflict instead of last-rename-wins (R11), a
//! hydrating session's writer lease answers `SESSION_BUSY` where TS's earlier
//! `hasSession` read would have (R13), and the external-store refusal is
//! structurally unreachable because the JSONL store always replaces
//! atomically (R8).

use super::super::requests::Requests;
use crate::{
    js_value,
    metadata::object,
    runtime::{connection::RequestError, memory::js_trim_units, session::transcript_locks},
    storage::{
        AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorage,
        SessionStorageInput, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, quote_json_string};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// `MAX_PORTABLE_SESSION_ARCHIVE_MESSAGES` (createLocalGateway.ts:1610).
const MAX_PORTABLE_SESSION_ARCHIVE_MESSAGES: usize = 10_000;
/// `MAX_PORTABLE_SESSION_ARCHIVE_BYTES` (createLocalGateway.ts:1611).
const MAX_PORTABLE_SESSION_ARCHIVE_BYTES: usize = 2 * 1024 * 1024;
/// `MAX_PORTABLE_SESSION_ARCHIVE_TITLE_LENGTH` (createLocalGateway.ts:1612),
/// in UTF-16 units like every JS `.length` read here.
const MAX_PORTABLE_SESSION_ARCHIVE_TITLE_LENGTH: usize = 512;

static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn is(value: &JsValue, text: &str) -> bool {
    value
        .as_string()
        .is_some_and(|string| string.equals_str(text))
}
/// JS truthiness for the values a JSON transcript or wire archive can carry.
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(flag) => *flag,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.is_empty(),
        _ => true,
    }
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

/// `title.length` for the value shapes the export title gate reads it on:
/// strings carry UTF-16 units, arrays their element count, and an object
/// hands back its own `length` value when that is a finite number — every
/// other shape reads `undefined`, which fails the `<=` and drops the title.
fn title_length(value: &JsValue) -> f64 {
    match value {
        JsValue::String(text) => text.len_utf16() as f64,
        JsValue::Array(items) => items.len() as f64,
        JsValue::Object(object) => match object.get("length") {
            Some(JsValue::Number(number)) if number.is_finite() => *number,
            _ => f64::NAN,
        },
        _ => f64::NAN,
    }
}

fn gateway_error(code: &'static str, message: impl Into<String>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into().into(),
        details: None,
        validation: None,
    }
}

/// The replay fold's errors already carry their TS code and message; they only
/// need the RPC envelope, like `config_error` for the settings family.
fn replay_error(error: crate::config::Error) -> RequestError {
    RequestError {
        code: error.code,
        message: error.message,
        details: None,
        validation: None,
    }
}

/// `String(value)` at a template-literal slot. The TypeError/RangeError the
/// coercion can raise surfaces as the raw error text the RPC boundary wraps
/// (`gateway_request_failed`), exactly like the TS delegate's uncaught throw.
fn to_units(value: &JsValue) -> Result<Vec<u16>, RequestError> {
    js_value::to_string_units_checked(value).map_err(|error| RequestError::new(error.message()))
}

/// The property-read TypeError the template literals produce for a nullish
/// receiver (`data.length`, `inner.type`).
fn cannot_read(value: &JsValue, property: &str) -> RequestError {
    RequestError::new(format!(
        "Cannot read properties of {} (reading '{property}')",
        if matches!(value, JsValue::Null) {
            "null"
        } else {
            "undefined"
        }
    ))
}

/// `${value.length}` for the receivers the render path reads it on: arrays
/// and strings carry a length, an object hands back its own `length` value
/// (then `String()` coerces that), every other primitive reads `undefined`,
/// and a nullish receiver throws the raw TypeError.
fn length_units(value: &JsValue) -> Result<Vec<u16>, RequestError> {
    let length = match value {
        JsValue::Array(items) => js_value::number_string(items.len() as f64),
        JsValue::String(text) => js_value::number_string(text.len_utf16() as f64),
        JsValue::Object(object) => match object.get("length") {
            Some(value) => return to_units(value),
            None => "undefined".into(),
        },
        JsValue::Null | JsValue::Undefined => return Err(cannot_read(value, "length")),
        _ => "undefined".into(),
    };
    Ok(length.encode_utf16().collect())
}

/// `JSON.stringify(value, null, 2)` — the pretty shape
/// `stringifyPortableTranscriptValue` renders non-strings with. No pretty
/// printer exists in the codec (wire frames are compact), so the two-space
/// layout lives here: `{\n  "key": value,\n  …\n}`.
enum Pretty {
    Text(String),
    /// `JSON.stringify(undefined)` returns undefined; the surrounding template
    /// literal then coerces it to `"undefined"`.
    Undefined,
    /// Past the shared 2000-container bound (#1550 family) or past V8's
    /// string length. The string-length refusal is the same input where
    /// V8's stringify throws the catchable RangeError the TS helper turns
    /// into `String(value)`. The 2000-container bound is *not* V8's own
    /// throw point — that is shape-dependent (arrays render deeper,
    /// objects overflow earlier; registered in the A6-JSVALUE-DEPTH-001
    /// family), so the bands around it diverge from TS in both directions
    /// and are covered by that registered divergence, not by this code.
    /// The caller performs the same fallback for both halves.
    Fallback,
}

/// V8's `String::kMaxLength` (64-bit), counted in **UTF-16 code units** —
/// the unit V8 measures that constant in, so the stop line matches the TS
/// helper's catch exactly: past it the render falls back to `String(value)`,
/// the same route V8's stringify throw takes for the same input. fix-r4:
/// this was previously compared in UTF-8 bytes, stopping CJK renders up to
/// 1.5x early — a real mis-alignment, not the "slightly earlier" the old
/// comment claimed (DS r4 boundary P1).
const MAX_PRETTY_OUTPUT_UNITS: usize = (1 << 29) - 24;

/// The pretty render's running output: the UTF-8 buffer plus the UTF-16
/// code-unit count the budget above is measured in. Every push routes
/// through here so the counter stays exact — a non-BMP char is 4 UTF-8
/// bytes but 2 units, and estimating from `buf.len()` is what the fix-r4
/// P1 removed.
#[derive(Default)]
struct RenderOut {
    buf: String,
    units: usize,
}

impl RenderOut {
    fn push_str(&mut self, text: &str) {
        self.units += text.chars().map(char::len_utf16).sum::<usize>();
        self.buf.push_str(text);
    }

    fn push(&mut self, ch: char) {
        self.units += ch.len_utf16();
        self.buf.push(ch);
    }
}

/// One unit of remaining work in the explicit-stack pretty render. Deep
/// inputs (up to the shared 2000-container bound) must not spend a native
/// stack frame per level: the recursive form aborted the process
/// (`fatal runtime error: stack overflow`, exit 134 — measured on the cb8
/// host at depth 2000 × 50k flat elements, where V8's stringify also
/// refuses, that shape's pretty output exceeding `String::kMaxLength`).
/// Array frames borrow the item slice from the input tree; object frames
/// move their filtered entry `Vec` along (no per-step clone).
enum Step<'a> {
    ArrayItem {
        items: &'a [JsValue],
        index: usize,
        depth: usize,
    },
    ObjectEntry {
        entries: Vec<(&'a JsString, &'a JsValue)>,
        index: usize,
        depth: usize,
    },
    Close {
        depth: usize,
        bracket: char,
    },
}

/// Emit `value` (a container's child at `depth`) into `out`, pushing
/// continuation frames for containers. Returns `false` past the shared
/// bounds — the whole render aborts and the caller falls back.
fn open_value<'a>(
    stack: &mut Vec<Step<'a>>,
    out: &mut RenderOut,
    value: &'a JsValue,
    depth: usize,
) -> bool {
    match value {
        // Only array elements reach here with undefined (object entries
        // filter it): JSON.stringify array elements coerce to null.
        JsValue::Undefined => out.push_str("null"),
        JsValue::Null => out.push_str("null"),
        JsValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        JsValue::Number(number) => out.push_str(&if number.is_finite() {
            js_value::number_string(*number)
        } else {
            // JSON.stringify maps NaN/Infinity to null.
            "null".to_string()
        }),
        JsValue::String(text) => out.push_str(&quote_json_string(text)),
        JsValue::Array(items) => {
            if depth >= js_value::MAX_ARRAY_DEPTH {
                return false;
            }
            if items.is_empty() {
                out.push_str("[]");
            } else {
                out.push_str("[\n");
                stack.push(Step::Close {
                    depth,
                    bracket: ']',
                });
                stack.push(Step::ArrayItem {
                    items,
                    index: 0,
                    depth,
                });
            }
        }
        JsValue::Object(object) => {
            if depth >= js_value::MAX_ARRAY_DEPTH {
                return false;
            }
            // JSON.stringify drops undefined values, keeps null ones.
            let entries = object
                .own_entries()
                .into_iter()
                .filter(|(_, item)| !item.is_undefined())
                .collect::<Vec<_>>();
            if entries.is_empty() {
                out.push_str("{}");
            } else {
                out.push_str("{\n");
                stack.push(Step::Close {
                    depth,
                    bracket: '}',
                });
                stack.push(Step::ObjectEntry {
                    entries,
                    index: 0,
                    depth,
                });
            }
        }
    }
    true
}

fn stringify_pretty(value: &JsValue) -> Pretty {
    // JSON.stringify(undefined) returns undefined; the surrounding template
    // literal then coerces it to "undefined".
    if matches!(value, JsValue::Undefined) {
        return Pretty::Undefined;
    }
    let mut out = RenderOut::default();
    let mut stack: Vec<Step<'_>> = Vec::new();
    if !open_value(&mut stack, &mut out, value, 0) {
        return Pretty::Fallback;
    }
    while let Some(step) = stack.pop() {
        match step {
            Step::ArrayItem {
                items,
                index,
                depth,
            } => {
                if index >= items.len() {
                    continue;
                }
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(&mut out, depth + 1);
                stack.push(Step::ArrayItem {
                    items,
                    index: index + 1,
                    depth,
                });
                if !open_value(&mut stack, &mut out, &items[index], depth + 1) {
                    return Pretty::Fallback;
                }
            }
            Step::ObjectEntry {
                entries,
                index,
                depth,
            } => {
                if index >= entries.len() {
                    continue;
                }
                let (key, item) = entries[index];
                stack.push(Step::ObjectEntry {
                    entries,
                    index: index + 1,
                    depth,
                });
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(&mut out, depth + 1);
                out.push_str(&quote_json_string(key));
                out.push_str(": ");
                if !open_value(&mut stack, &mut out, item, depth + 1) {
                    return Pretty::Fallback;
                }
            }
            Step::Close { depth, bracket } => {
                out.push('\n');
                indent(&mut out, depth);
                out.push(bracket);
            }
        }
        // A single scalar or container header can append a whole string in
        // one go; re-check the shared length bound every frame.
        if out.units > MAX_PRETTY_OUTPUT_UNITS {
            return Pretty::Fallback;
        }
    }
    if out.units > MAX_PRETTY_OUTPUT_UNITS {
        return Pretty::Fallback;
    }
    Pretty::Text(out.buf)
}

fn indent(out: &mut RenderOut, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// `stringifyPortableTranscriptValue` (createLocalGateway.ts:1671-1676): a
/// string passes through, anything else pretty-prints, and a stringify
/// failure falls back to `String(value)` for the whole input.
fn stringify_portable_value(value: &JsValue) -> Result<Vec<u16>, RequestError> {
    if let JsValue::String(text) = value {
        return Ok(text.units().to_vec());
    }
    match stringify_pretty(value) {
        Pretty::Text(text) => Ok(text.encode_utf16().collect()),
        Pretty::Undefined => Ok("undefined".encode_utf16().collect()),
        Pretty::Fallback => to_units(value),
    }
}

/// `toolResultContentBlockToText` over `block.content` joined with "\n"
/// (toolResultContent.ts:19-27): inner text pushes the raw value, images
/// and pdfs interpolate `data` or `data.length` (template interpolation
/// stringifies `undefined`), and an inner block whose `type` is none of the
/// three falls out of the switch as `undefined`. `Array.prototype.join`
/// then coerces `undefined`/`null` elements to the **empty string** —
/// unlike the interpolations above, a joined nullish element renders no
/// text at all. A nullish inner block throws reading `type`, like every
/// other raw property read here.
fn flatten_tool_result_text(block: &JsValue) -> Result<Vec<u16>, RequestError> {
    let mut parts: Vec<Vec<u16>> = Vec::new();
    // The replay's clone proved `content` is an array before the message
    // reached the fold output; any other shape renders no parts here.
    if let JsValue::Array(blocks) = field(block, "content") {
        for inner in blocks {
            if nullish(inner) {
                return Err(cannot_read(inner, "type"));
            }
            let kind = field(inner, "type");
            if is(kind, "text") {
                let text = field(inner, "text");
                // `block.text` returns as-is; a nullish or absent `text` is
                // then coerced to the empty string by the join below.
                parts.push(if nullish(text) {
                    Vec::new()
                } else {
                    to_units(text)?
                });
            } else if is(kind, "image") {
                let mut part = "[Image: ".encode_utf16().collect::<Vec<_>>();
                part.extend(to_units(field(inner, "mimeType"))?);
                part.extend(", ".encode_utf16());
                if is(field(inner, "source"), "url") {
                    part.extend(to_units(field(inner, "data"))?);
                    part.push(b']' as u16);
                } else {
                    part.extend(length_units(field(inner, "data"))?);
                    part.extend(" base64 characters]".encode_utf16());
                }
                parts.push(part);
            } else if is(kind, "pdf") {
                let mut part = "[PDF: ".encode_utf16().collect::<Vec<_>>();
                part.extend(to_units(field(inner, "mimeType"))?);
                part.extend(", ".encode_utf16());
                part.extend(length_units(field(inner, "data"))?);
                part.extend(" base64 characters".encode_utf16());
                let pages = field(inner, "pages");
                if truthy(pages) {
                    part.extend(", ".encode_utf16());
                    part.extend(to_units(pages)?);
                    part.extend(" pages".encode_utf16());
                }
                part.push(b']' as u16);
                parts.push(part);
            } else {
                // Unknown `type` falls out of the TS switch as `undefined`;
                // the join coerces that element to the empty string.
                parts.push(Vec::new());
            }
        }
    }
    let mut text: Vec<u16> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            text.push(b'\n' as u16);
        }
        text.extend(part);
    }
    Ok(text)
}

/// `portableTranscriptText` (createLocalGateway.ts:1651-1668): the five
/// content-block branches joined with "\n\n", JS-trimmed, dropped when the
/// result is empty. Parts carry raw values — the join does the `String()`
/// coercion, so a numeric text block renders "5" and a non-primitive one
/// throws at the join.
fn portable_transcript_text(message: &JsValue) -> Result<Option<JsString>, RequestError> {
    let mut parts: Vec<Vec<u16>> = Vec::new();
    // The replay's clone always leaves `content` an array; any other shape
    // renders no parts here.
    if let JsValue::Array(blocks) = field(message, "content") {
        for block in blocks {
            let kind = field(block, "type");
            if is(kind, "text") {
                if truthy(field(block, "text")) {
                    parts.push(to_units(field(block, "text"))?);
                }
            } else if is(kind, "tool_call") {
                let mut part = "[Tool call: ".encode_utf16().collect::<Vec<_>>();
                part.extend(to_units(field(block, "name"))?);
                part.push(b']' as u16);
                part.push(b'\n' as u16);
                part.extend(stringify_portable_value(field(block, "input"))?);
                parts.push(part);
            } else if is(kind, "tool_result") {
                let text = flatten_tool_result_text(block)?;
                parts.push(if text.is_empty() {
                    "[Tool result]".encode_utf16().collect()
                } else {
                    let mut part = "[Tool result]\n".encode_utf16().collect::<Vec<_>>();
                    part.extend(text);
                    part
                });
            } else if is(kind, "tool_result_reference") {
                let mut part = "[Tool result reference: "
                    .encode_utf16()
                    .collect::<Vec<_>>();
                part.extend(to_units(field(block, "toolCallId"))?);
                part.push(b']' as u16);
                parts.push(part);
            } else if is(kind, "image") {
                parts.push("[Image]".encode_utf16().collect());
            }
        }
    }
    let mut text: Vec<u16> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            text.extend("\n\n".encode_utf16());
        }
        text.extend(part);
    }
    let text = js_trim_units(&text);
    Ok((!text.is_empty()).then(|| JsString::from_utf16(text.to_vec())))
}

/// `toPortableSessionArchive` (createLocalGateway.ts:1633-1648) over the
/// shared replay fold: synthetic messages are dropped, each remaining message
/// renders to portable text or is dropped, and the title chain
/// (`title ?? aiTitle` over the merged values) is included only when truthy
/// and at most 512 UTF-16 units — anything else is omitted, never truncated.
fn to_portable_archive(entries: &[JsValue]) -> Result<JsValue, RequestError> {
    let replay = crate::session_model::transcript_replay(entries).map_err(replay_error)?;
    let mut messages = Vec::new();
    for message in &replay.messages {
        // `message.metadata?.synthetic === true`: the optional chain makes a
        // nullish metadata simply not synthetic.
        if field(field(message, "metadata"), "synthetic") == &JsValue::Bool(true) {
            continue;
        }
        let Some(text) = portable_transcript_text(message)? else {
            continue;
        };
        messages.push(object([
            ("role", field(message, "role").clone()),
            ("text", JsValue::String(text)),
        ]));
    }
    let title = replay
        .title
        .filter(|value| !nullish(value))
        .or_else(|| replay.ai_title.filter(|value| !nullish(value)));
    let mut archive = JsObject::new();
    archive.insert("schemaVersion".into(), JsValue::Number(1.0));
    archive.insert("format".into(), "portable_text_messages".into());
    archive.insert("messages".into(), JsValue::Array(messages));
    if let Some(value) = &title {
        // `title && title.length <= MAX` (createLocalGateway.ts:1643): a
        // truthiness check plus the `.length` property — **no typeof check**.
        // An empty string is falsy and drops; a non-empty string counts its
        // UTF-16 units; a truthy array-like title (e.g. hand-written
        // `title: ["x"]`, `.length` 1) passes in TS and must pass here. A
        // value with no numeric `.length` (undefined) fails `<=` and drops
        // (DS r4 boundary P2 — ported literally rather than string-only).
        if truthy(value) && title_length(value) <= MAX_PORTABLE_SESSION_ARCHIVE_TITLE_LENGTH as f64
        {
            archive.insert("title".into(), value.clone());
        }
    }
    Ok(JsValue::Object(archive))
}

/// `validatePortableSessionArchive` (createLocalGateway.ts:1680-1703), checks
/// in the recorded order: shape, count, title, per-message, serialized size.
fn validate_portable_archive(value: &JsValue) -> Result<(), RequestError> {
    const SHAPE: &str = "Unsupported or malformed session transcript archive.";
    const INVALID: &str = "INVALID_SESSION_ARCHIVE";
    let invalid = |message: &'static str| gateway_error(INVALID, message);
    if !truthy(value)
        || field(value, "schemaVersion").as_number() != Some(1.0)
        || !is(field(value, "format"), "portable_text_messages")
    {
        return Err(invalid(SHAPE));
    }
    let JsValue::Array(messages) = field(value, "messages") else {
        return Err(invalid(SHAPE));
    };
    if messages.is_empty() || messages.len() > MAX_PORTABLE_SESSION_ARCHIVE_MESSAGES {
        return Err(invalid(
            "Session transcript archive has an invalid message count.",
        ));
    }
    // `value.title !== undefined && (typeof !== "string" || length > 512)` —
    // a present null title is invalid too.
    if let Some(title) = value.get("title").filter(|title| !title.is_undefined()) {
        let valid = matches!(title, JsValue::String(text)
            if text.len_utf16() <= MAX_PORTABLE_SESSION_ARCHIVE_TITLE_LENGTH);
        if !valid {
            return Err(invalid("Session transcript archive title is invalid."));
        }
    }
    for message in messages {
        if !truthy(message)
            || !(is(field(message, "role"), "user") || is(field(message, "role"), "assistant"))
            || field(message, "text").as_string().is_none()
        {
            return Err(invalid(
                "Session transcript archive contains an invalid message.",
            ));
        }
    }
    // `Buffer.byteLength(JSON.stringify(value), "utf8")` — compact JSON through
    // the shared V8-semantics wrapper (`js_value::json_stringify`), so a
    // too-deep archive reports V8's RangeError exactly like the TS delegate
    // instead of the unbounded codec walk. The value cannot be `undefined`
    // here (messages is an array above).
    let serialized = js_value::json_stringify(value)
        .map_err(|_| RequestError::new(js_value::StackOverflow::MESSAGE))?
        .unwrap_or_default();
    if serialized.len() > MAX_PORTABLE_SESSION_ARCHIVE_BYTES {
        return Err(invalid("Session transcript archive is too large."));
    }
    Ok(())
}

/// `!input.sessionKey?.trim()` — the family-shared #1604 guard, so both
/// archive methods answer blank, missing and non-string keys exactly like the
/// session-directory family.
fn require_session_key(params: &JsValue) -> Result<JsString, RequestError> {
    super::require_text(
        params,
        "sessionKey",
        "INVALID_SESSION_KEY",
        "sessionKey is required.",
    )
}

/// The persistent session storage the delegates read and replace through
/// (`registry.createPersistentSessionStorage(projectKey, sessionKey, now)`).
fn transcript_storage(
    home: &Path,
    project_root: &Path,
    key: &JsString,
) -> Result<SessionStorage, RequestError> {
    let adapter_home = home.to_path_buf();
    let adapter = AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(move |project: &ProjectKey| {
            crate::session_chat_dir(
                &adapter_home,
                &project.project_root,
                &project.project_root.to_string_lossy(),
            )
        }),
    );
    adapter
        .create_session_storage(SessionStorageInput {
            project_root: project_root.to_path_buf(),
            pilot_home: home.to_path_buf(),
            session_id: key.clone(),
            now: None,
        })
        .map_err(|error| RequestError::new(error.to_string()))
}

/// The TS restore writes through an `InMemoryTranscriptWriter` when the
/// storage has an external store (createLocalGateway.ts:1350-1355): entries
/// are built with full envelopes in memory and handed to the atomic
/// `replaceTranscript`. This is that store — a capture buffer the
/// [`crate::storage::TranscriptWriter`] appends into.
#[derive(Default)]
struct RestoreMemoryStore(Mutex<Vec<JsValue>>);
impl RestoreMemoryStore {
    fn take(&self) -> Vec<JsValue> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|error| error.into_inner()))
    }
}
impl TranscriptStore for RestoreMemoryStore {
    fn append(&self, _key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(entry.clone());
        Ok(())
    }
    fn read(&self, _key: &TranscriptKey, _max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        Ok(TranscriptRead {
            entries: self
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            diagnostics: vec![],
        })
    }
}

/// The fenced write half of the restore delegate
/// (createLocalGateway.ts:1342-1367): the memory writer builds the
/// envelope-complete entries, then the atomic replace commits them.
fn write_restored_transcript(
    home: &Path,
    project_root: &Path,
    storage: &SessionStorage,
    key: &JsString,
    archive: &JsValue,
    turn_id: &JsString,
) -> Result<(), RequestError> {
    // R13: a session claimed the transcript after the async-side `hasSession`
    // read — the factory's restore→publish window. TS would have observed
    // `hasSession` and answered SESSION_BUSY, so the lease refusal answers
    // the same domain error instead of the rename family's writer-busy one.
    // Err propagates the real fs error (e.g. EACCES), matching main's
    // cold_write_lease split of busy vs. I/O failure.
    let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path)? else {
        return Err(gateway_error(
            "SESSION_BUSY",
            "Cannot restore over an active Gateway session.",
        ));
    };
    let memory = Arc::new(RestoreMemoryStore::default());
    let memory_home = home.to_path_buf();
    let memory_adapter = AsyncTranscriptAdapter::new(
        memory.clone(),
        Arc::new(move |project: &ProjectKey| {
            crate::session_chat_dir(
                &memory_home,
                &project.project_root,
                &project.project_root.to_string_lossy(),
            )
        }),
    );
    let writer = memory_adapter
        .create_session_storage(SessionStorageInput {
            project_root: project_root.to_path_buf(),
            pilot_home: home.to_path_buf(),
            session_id: key.clone(),
            now: None,
        })
        .map_err(|error| RequestError::new(error.to_string()))?;
    // `if (archive.title)` — truthy; validate has already proven the value
    // a string of at most 512 units. `updatedAt` is the same clock shape the
    // rename family writes.
    let title = field(archive, "title");
    if truthy(title) {
        let updated_at = pilotdeck_transport::system_clock::format_epoch_iso(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        );
        writer
            .transcript
            .record_session_metadata(
                key,
                turn_id,
                object([
                    ("title", title.clone()),
                    ("updatedAt", JsValue::String(updated_at.into())),
                ]),
            )
            .map_err(|error| RequestError::new(error.to_string()))?;
    }
    let messages = match field(archive, "messages") {
        JsValue::Array(messages) => messages
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
            .collect::<Vec<_>>(),
        // Unreachable: validate refused a non-array messages before the
        // write half runs.
        _ => Vec::new(),
    };
    let mut metadata = JsObject::new();
    metadata.insert("restoredFromGatewayArchive".into(), JsValue::Bool(true));
    metadata.insert("archiveSchemaVersion".into(), JsValue::Number(1.0));
    writer
        .transcript
        .record_accepted_input(key, turn_id, messages, Some(metadata))
        .map_err(|error| RequestError::new(error.to_string()))?;
    let entries = memory.take();
    // `storage.externalTranscriptStore && !storage.replaceTranscript` refuses
    // with SESSION_RESTORE_UNSUPPORTED; the JSONL store always replaces, so
    // this arm is structurally unreachable here (R8, registered in -R1).
    match storage.replace_transcript(&entries) {
        Some(result) => result.map_err(|error| RequestError::new(error.to_string())),
        None => Err(gateway_error(
            "SESSION_RESTORE_UNSUPPORTED",
            "The configured external transcript store does not support atomic restore.",
        )),
    }
}

impl Requests {
    /// The export delegate (createLocalGateway.ts:1162-1181).
    pub(super) async fn export_session_transcript(
        &self,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        // `resolveProjectKey(input.projectKey ?? fallback)` reads the
        // property before anything else, so a nullish params names
        // projectKey in the optional-chain TypeError.
        super::input_property(params, "projectKey")?;
        let project_root: PathBuf = super::session_dir::resolve_project(self, params)?.into();
        let key = require_session_key(params)?;
        if self.owner.has_active_turn(&key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot export a transcript while its session has an active turn.",
            ));
        }
        let home = self.options.home.clone();
        let key_for_read = key.clone();
        let read = tokio::task::spawn_blocking(move || {
            transcript_storage(&home, &project_root, &key_for_read)?
                .read_transcript(None)
                .map_err(|error| RequestError::new(error.to_string()))
        })
        .await
        .map_err(|error| RequestError::new(error.to_string()))??;
        if read
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "transcript_missing")
        {
            return Err(gateway_error(
                "SESSION_NOT_FOUND",
                format!(
                    "Session not found: {}",
                    String::from_utf16_lossy(key.units())
                ),
            ));
        }
        if read
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            return Err(gateway_error(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot export a transcript with invalid entries.",
            ));
        }
        let archive = to_portable_archive(&read.entries)?;
        validate_portable_archive(&archive)?;
        // The delegate's own empty check is unreachable through this order —
        // validate's message-count arm answers zero messages first (the
        // `export.empty-text` oracle row) — kept for shape fidelity.
        if matches!(field(&archive, "messages"), JsValue::Array(messages) if messages.is_empty()) {
            return Err(gateway_error(
                "SESSION_TRANSCRIPT_EMPTY",
                "Session has no portable text messages to export.",
            ));
        }
        Ok(archive)
    }

    /// The restore delegate (createLocalGateway.ts:1315-1367).
    pub(super) async fn restore_session_transcript(
        &self,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        super::input_property(params, "projectKey")?;
        let project_root: PathBuf = super::session_dir::resolve_project(self, params)?.into();
        let key = require_session_key(params)?;
        // `router.hasActiveTurn(key) || router.hasSession(key)`: export checks
        // only the active half; a loaded but idle session still owns the
        // transcript and refuses the restore.
        if self.owner.has_active_turn(&key) || self.owner.current_handle(&key).is_some() {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot restore over an active Gateway session.",
            ));
        }
        // `restoringSessionKeys.has(key)`, read before any storage work
        // exactly where TS reads it. The add/remove pair itself rides inside
        // the fenced blocking task below (R12), so a cancelled request cannot
        // strand the key in the set.
        if self
            .restoring_session_keys
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(&key)
        {
            return Err(gateway_error(
                "SESSION_RESTORE_CONFLICT",
                "A transcript restore is already in progress for this session.",
            ));
        }
        let archive = field(params, "archive").clone();
        validate_portable_archive(&archive)?;
        let imported_messages = match field(&archive, "messages") {
            JsValue::Array(messages) => messages.len(),
            // Unreachable: validate refused a non-array messages above.
            _ => 0,
        };
        // #1457 family: one fence per transcript file, so the exists check and
        // the replace below are one read-modify-write the hydrating session
        // factory also fences — that serialization is what makes a concurrent
        // second restore see the exists conflict instead of racing the
        // rename (R11).
        let fence_path =
            transcript_locks::session_transcript_path(&self.options.home, &project_root, &key);
        let write_fence = transcript_locks::acquire(&fence_path).await?;
        let home = self.options.home.clone();
        let restoring = self.restoring_session_keys.clone();
        let key_for_write = key.clone();
        let turn_id = JsString::from(format!("sdk:restore:{}", uuid::Uuid::new_v4()));
        tokio::task::spawn_blocking(move || {
            // Held inside the closure: a cancelled request drops the future
            // but not the blocking write, so the fence and the set membership
            // travel with it (R12) — the write runs to completion either way.
            let _write = write_fence;
            let storage = transcript_storage(&home, &project_root, &key_for_write)?;
            let exists = storage
                .transcript_exists()
                .transpose()
                .map_err(|error| RequestError::new(error.to_string()))?
                .unwrap_or(false);
            if exists {
                return Err(gateway_error(
                    "SESSION_RESTORE_CONFLICT",
                    "A persistent transcript already exists for this session.",
                ));
            }
            restoring
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(key_for_write.clone());
            let outcome = write_restored_transcript(
                &home,
                &project_root,
                &storage,
                &key_for_write,
                &archive,
                &turn_id,
            );
            restoring
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&key_for_write);
            outcome
        })
        .await
        .map_err(|error| RequestError::new(error.to_string()))??;
        Ok(object([
            ("sessionKey", JsValue::String(key)),
            (
                "importedMessages",
                JsValue::Number(imported_messages as f64),
            ),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        native_process::{management, management_tests},
        negative_assert, negative_assert_eq,
        runtime::{session::SessionContext, turn::gateway::GatewayRequests},
    };
    use serde_json::{Value, json};
    use std::sync::LazyLock;

    const ORACLE: &str =
        include_str!("../../../tests/fixtures/transcript-archive-2622/oracle.json");
    /// The fixed clock the TS generator ran under. The Rust writer stamps
    /// real time, so the wire mask below folds both sides to `<clock>`.
    const SEEDED_AT: &str = "2026-01-01T00:00:00.000Z";

    fn json_value(value: &JsValue) -> Value {
        serde_json::from_str(
            &pilotdeck_protocol::stringify_json(value)
                .expect("#2622 stringify")
                .expect("#2622 serializable value"),
        )
        .expect("#2622 JSON output")
    }

    fn params(value: &Value) -> JsValue {
        pilotdeck_protocol::parse_json(&value.to_string()).expect("#2622 wire params")
    }

    async fn call(requests: &Requests, method: &str, params: &JsValue) -> Value {
        let result = GatewayRequests::request(requests, method.into(), params.clone()).await;
        json_value(&management::outcome(result))
    }

    fn row<'a>(oracle: &'a Value, id: &str) -> &'a Value {
        oracle["rows"]
            .as_array()
            .expect("#2622 rows")
            .iter()
            .find(|row| row["id"] == json!(id))
            .unwrap_or_else(|| panic!("#2622 oracle row {id}"))
    }

    fn chat_dir(requests: &Requests) -> PathBuf {
        crate::session_chat_dir(
            &requests.options.home,
            &requests.options.project,
            &requests.options.project.to_string_lossy(),
        )
    }

    async fn teardown(requests: &Requests) {
        requests
            .owner
            .shutdown()
            .await
            .expect("#2622 fixture owner shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#2622 fixture project shutdown");
    }

    /// generate.mts `writeSession`: one deterministic envelope per entry — the
    /// fixed `createdAt`, the `entry-000N` id chain, and the null first parent.
    fn transcript(entries: &[(&str, &str, Value)]) -> String {
        let mut out = String::new();
        for (index, (kind, turn, fields)) in entries.iter().enumerate() {
            let sequence = index + 1;
            let mut entry = json!({
                "type": kind,
                "sessionId": "s",
                "turnId": turn,
                "sequence": sequence,
                "createdAt": SEEDED_AT,
                "entryId": format!("entry-{sequence:04}"),
                "parentEntryId": if sequence == 1 {
                    Value::Null
                } else {
                    json!(format!("entry-{:04}", sequence - 1))
                },
            });
            let object = entry.as_object_mut().expect("#2622 entry object");
            if let Value::Object(fields) = fields {
                for (key, value) in fields {
                    object.insert(key.clone(), value.clone());
                }
            }
            out.push_str(&serde_json::to_string(&entry).expect("#2622 entry JSON"));
            out.push('\n');
        }
        out
    }

    fn write_session(requests: &Requests, key: &str, text: &str) {
        std::fs::write(chat_dir(requests).join(format!("{key}.jsonl")), text)
            .expect("#2622 seeded transcript");
    }

    /// The generator's seeded sessions, byte for byte in shape (the same
    /// entries, the same envelope values; JSON key order is the reader's, not
    /// the bytes', concern).
    fn seed_transcripts(requests: &Requests) {
        // rich: every portable-text branch, the synthetic filter, the
        // incomplete-turn durable skip, a surviving title.
        write_session(
            requests,
            "rich",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "Export me"}]},
                    ]}),
                ),
                (
                    "assistant_message",
                    "t1",
                    json!({"message": {"role": "assistant", "content": [
                        {"type": "text", "text": "Working on it"},
                        {"type": "tool_call", "id": "c1", "name": "read_file", "input": {"path": "a.txt"}},
                        {"type": "tool_call", "id": "c2", "name": "search", "input": "plain string input"},
                        {"type": "tool_call", "id": "c3", "name": "no_input"},
                        {"type": "tool_result_reference", "toolCallId": "c1"},
                        {"type": "image", "mimeType": "image/png"},
                    ]}}),
                ),
                (
                    "tool_result_message",
                    "t1",
                    json!({"message": {"role": "user", "content": [
                        {"type": "tool_result", "toolCallId": "c1", "content": [
                            {"type": "text", "text": "file bytes here"},
                            {"type": "image", "source": "base64", "mimeType": "image/png", "data": "AAAA"},
                            {"type": "image", "source": "url", "mimeType": "image/png", "data": "https://example.invalid/a.png"},
                            {"type": "pdf", "mimeType": "application/pdf", "data": "BBBB", "pages": 2},
                        ]},
                    ]}}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
                (
                    "session_metadata",
                    "t1",
                    json!({"metadata": {"title": "Rich session"}}),
                ),
                (
                    "accepted_input",
                    "t2",
                    json!({"messages": [
                        {"role": "user", "metadata": {"synthetic": true},
                         "content": [{"type": "text", "text": "hidden by the synthetic filter"}]},
                    ]}),
                ),
                (
                    "durable_message",
                    "t-never",
                    json!({"message": {"role": "assistant", "content": [
                        {"type": "text", "text": "incomplete turn"},
                    ]}}),
                ),
                (
                    "turn_result",
                    "t2",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
                (
                    "assistant_message",
                    "t2",
                    json!({"message": {"role": "assistant", "content": [
                        {"type": "text", "text": "Done"},
                    ]}}),
                ),
            ]),
        );
        // plain: no title anywhere, the archive omits the key entirely.
        write_session(
            requests,
            "plain",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "No title"}]},
                    ]}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        // Titles past the 512-unit ceiling are dropped, not truncated.
        write_session(
            requests,
            "long-title",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "Titled"}]},
                    ]}),
                ),
                (
                    "session_metadata",
                    "t1",
                    json!({"metadata": {"title": "x".repeat(513)}}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        write_session(
            requests,
            "boundary-title",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "Titled"}]},
                    ]}),
                ),
                (
                    "session_metadata",
                    "t1",
                    json!({"metadata": {"title": "y".repeat(512)}}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        // Every message renders to empty text: the count check answers before
        // the delegate's own empty check can.
        write_session(
            requests,
            "empty-text",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": ""}]},
                    ]}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        // coerce: a numeric text block renders "5".
        write_session(
            requests,
            "coerce",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "x"}]},
                    ]}),
                ),
                (
                    "assistant_message",
                    "t1",
                    json!({"message": {"role": "assistant", "content": [
                        {"type": "text", "text": 5},
                    ]}}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        // bad-block: a tool_result image block without data throws reading
        // data.length.
        write_session(
            requests,
            "bad-block",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "x"}]},
                    ]}),
                ),
                (
                    "assistant_message",
                    "t1",
                    json!({"message": {"role": "assistant", "content": [
                        {"type": "tool_result", "toolCallId": "c1", "content": [
                            {"type": "image", "mimeType": "image/png"},
                        ]},
                    ]}}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
        // invalid: one valid entry then a torn line.
        let mut invalid = transcript(&[("accepted_input", "t1", json!({"messages": []}))]);
        invalid.push_str("not json\n");
        write_session(requests, "invalid", &invalid);
        // busy-1: the transcript exists before the turn starts, so the
        // in-flight rows pin the guard ORDER (busy before restore conflict).
        write_session(
            requests,
            "busy-1",
            &transcript(&[
                (
                    "accepted_input",
                    "t1",
                    json!({"messages": [
                        {"role": "user", "content": [{"type": "text", "text": "warm"}]},
                    ]}),
                ),
                (
                    "turn_result",
                    "t1",
                    json!({"result": {"usage": {}, "permissionDenials": []}}),
                ),
            ]),
        );
    }

    /// The wire mask over a restore-written transcript: the random uuid
    /// shapes (entryId, `sdk:restore:<uuid>`) the generator masked to
    /// `<uuid>`, plus the clock — fixed in the generator, real here — which
    /// both sides fold to `<clock>`.
    fn mask_wire(text: &str) -> String {
        static UUID: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .expect("#2622 uuid mask")
        });
        static CLOCK: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new("[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{3}Z")
                .expect("#2622 clock mask")
        });
        CLOCK
            .replace_all(&UUID.replace_all(text, "<uuid>"), "<clock>")
            .into_owned()
    }

    /// The generator's standard restore payload (rows `restore.fresh`,
    /// `restore.exists`, the busy rows).
    fn restore_archive() -> Value {
        json!({
            "schemaVersion": 1,
            "format": "portable_text_messages",
            "title": "Restored title",
            "messages": [
                {"role": "user", "text": "Restore me"},
                {"role": "assistant", "text": "Acknowledged"},
            ],
        })
    }

    /// Every `export.*` oracle row, replayed in the recorded order against
    /// the seeded transcripts.
    #[tokio::test]
    async fn export_rows_match_ts() {
        crate::test_baseline::oracle_bounded(ORACLE);
        let oracle: Value = serde_json::from_str(ORACLE).expect("#2622 oracle JSON");
        let (_scratch, requests) = management_tests::setup();
        seed_transcripts(&requests);
        for id in [
            "export.rich",
            "export.plain",
            "export.long-title",
            "export.boundary-title",
            "export.empty-text",
            "export.coerce",
            "export.bad-block",
            "export.missing",
            "export.invalid",
            "export.blank-key",
            "export.no-key",
            "export.nonstring-key",
            "export.nullish-params",
        ] {
            let row = row(&oracle, id);
            negative_assert_eq!(
                call(
                    &requests,
                    "export_session_transcript",
                    &params(&row["params"])
                )
                .await,
                row["result"],
                "#2622 {id}"
            );
        }
        teardown(&requests).await;
    }

    /// Every `restore.*` oracle row: the two successful writes with their
    /// byte-pinned transcripts, the re-export round trip, and all thirteen
    /// refusal branches.
    #[tokio::test]
    async fn restore_rows_match_ts() {
        crate::test_baseline::oracle_bounded(ORACLE);
        let oracle: Value = serde_json::from_str(ORACLE).expect("#2622 oracle JSON");
        let (_scratch, requests) = management_tests::setup();
        seed_transcripts(&requests);

        let fresh = row(&oracle, "restore.fresh");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&fresh["params"])
            )
            .await,
            fresh["result"],
            "#2622 restore.fresh"
        );
        let written = std::fs::read_to_string(chat_dir(&requests).join("restored.jsonl"))
            .expect("#2622 restored transcript");
        negative_assert_eq!(
            mask_wire(&written),
            mask_wire(
                row(&oracle, "restore.written-bytes")["text"]
                    .as_str()
                    .expect("#2622 bytes")
            ),
            "#2622 restore.written-bytes"
        );
        negative_assert_eq!(
            call(
                &requests,
                "export_session_transcript",
                &params(&json!({"sessionKey": "restored"}))
            )
            .await,
            row(&oracle, "restore.reexport")["result"],
            "#2622 restore.reexport"
        );
        let no_title = row(&oracle, "restore.no-title");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&no_title["params"])
            )
            .await,
            no_title["result"],
            "#2622 restore.no-title"
        );
        let written = std::fs::read_to_string(chat_dir(&requests).join("restored-plain.jsonl"))
            .expect("#2622 restored-plain transcript");
        negative_assert_eq!(
            mask_wire(&written),
            mask_wire(
                row(&oracle, "restore.written-bytes.no-title")["text"]
                    .as_str()
                    .expect("#2622 bytes")
            ),
            "#2622 restore.written-bytes.no-title"
        );
        let exists = row(&oracle, "restore.exists");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&exists["params"])
            )
            .await,
            exists["result"],
            "#2622 restore.exists"
        );
        for id in [
            "restore.blank-key",
            "restore.no-key",
            "restore.nonstring-key",
            "restore.nullish-params",
        ] {
            let row = row(&oracle, id);
            negative_assert_eq!(
                call(
                    &requests,
                    "restore_session_transcript",
                    &params(&row["params"])
                )
                .await,
                row["result"],
                "#2622 {id}"
            );
        }

        // The five INVALID_SESSION_ARCHIVE branches. The oracle's params
        // carry the generator's `'<archive>'` summaries, so the payloads are
        // rebuilt exactly as the generator built them.
        let one = json!([{"role": "user", "text": "x"}]);
        let invalid: Vec<(&str, Option<Value>)> = vec![
            (
                "unsupported-shape",
                Some(
                    json!({"schemaVersion": 2, "format": "portable_text_messages", "messages": one}),
                ),
            ),
            ("missing-archive", None),
            ("null-archive", Some(Value::Null)),
            (
                "empty-messages",
                Some(
                    json!({"schemaVersion": 1, "format": "portable_text_messages", "messages": []}),
                ),
            ),
            (
                "null-title",
                Some(
                    json!({"schemaVersion": 1, "format": "portable_text_messages", "messages": one, "title": null}),
                ),
            ),
            (
                "bad-role",
                Some(
                    json!({"schemaVersion": 1, "format": "portable_text_messages", "messages": [{"role": "system", "text": "x"}]}),
                ),
            ),
            (
                "bad-text",
                Some(
                    json!({"schemaVersion": 1, "format": "portable_text_messages", "messages": [{"role": "user", "text": 5}]}),
                ),
            ),
            (
                "bad-format",
                Some(json!({"schemaVersion": 1, "format": "other", "messages": one})),
            ),
            (
                "messages-not-array",
                Some(
                    json!({"schemaVersion": 1, "format": "portable_text_messages", "messages": {}}),
                ),
            ),
        ];
        for (name, archive) in invalid {
            let id = format!("restore.invalid.{name}");
            let mut wire = json!({"sessionKey": format!("bad-{name}")});
            if let Some(archive) = archive {
                wire["archive"] = archive;
            }
            negative_assert_eq!(
                call(&requests, "restore_session_transcript", &params(&wire)).await,
                row(&oracle, &id)["result"],
                "#2622 {id}"
            );
        }
        // The count and size ceilings (the oracle rows summarize the params).
        let too_many = json!({
            "sessionKey": "bad-too-many",
            "archive": {
                "schemaVersion": 1,
                "format": "portable_text_messages",
                "messages": vec![json!({"role": "user", "text": "x"}); 10001],
            },
        });
        negative_assert_eq!(
            call(&requests, "restore_session_transcript", &params(&too_many)).await,
            row(&oracle, "restore.invalid.too-many")["result"],
            "#2622 restore.invalid.too-many"
        );
        let too_large = json!({
            "sessionKey": "bad-too-large",
            "archive": {
                "schemaVersion": 1,
                "format": "portable_text_messages",
                "messages": [{"role": "user", "text": "y".repeat(2 * 1024 * 1024)}],
            },
        });
        negative_assert_eq!(
            call(&requests, "restore_session_transcript", &params(&too_large)).await,
            row(&oracle, "restore.invalid.too-large")["result"],
            "#2622 restore.invalid.too-large"
        );
        // No invalid archive reached storage: none of the restore-invalid
        // keys has a transcript (`bad-block` is a seeded export session,
        // not one of them).
        let mut seeded: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(chat_dir(&requests)).expect("#2622 chat directory") {
            seeded.push(
                entry
                    .expect("#2622 chat entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        negative_assert!(
            !seeded
                .iter()
                .any(|name| name.starts_with("bad-") && name != "bad-block.jsonl"),
            "#2622 refused archives wrote nothing: {seeded:?}"
        );
        teardown(&requests).await;
    }

    /// The busy rows: a reserved turn makes both methods refuse; after it
    /// ends, a loaded session still owns the transcript — restore refuses
    /// (`hasSession`), export still works (it only checks `hasActiveTurn`).
    #[tokio::test]
    async fn busy_and_loaded_session_rows_match_ts() {
        crate::test_baseline::oracle_bounded(ORACLE);
        let oracle: Value = serde_json::from_str(ORACLE).expect("#2622 oracle JSON");
        let (_scratch, requests) = management_tests::setup();
        seed_transcripts(&requests);
        let key: JsString = "busy-1".into();
        let run: JsString = "probe-hold-run".into();
        let reservation = requests
            .owner
            .begin_turn(&key, &run)
            .expect("#2622 reserve the active turn");
        let busy_params = params(&json!({"sessionKey": "busy-1"}));
        negative_assert_eq!(
            call(&requests, "export_session_transcript", &busy_params).await,
            row(&oracle, "export.busy")["result"],
            "#2622 export.busy"
        );
        let busy = row(&oracle, "restore.busy");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&busy["params"])
            )
            .await,
            busy["result"],
            "#2622 restore.busy"
        );
        requests
            .owner
            .end_turn(&key, Some(&run), Some(&reservation));
        negative_assert!(
            !requests.owner.has_active_turn(&key),
            "#2622 the fixture turn ended"
        );
        // The generator aborts its held turn; the session stays loaded, which
        // is the state `hasSession` observes.
        requests
            .owner
            .get_or_create(SessionContext {
                session_key: key.clone(),
                project_key: Some(
                    requests
                        .options
                        .project
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                ),
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            })
            .await
            .expect("#2622 loaded fixture session");
        negative_assert!(
            requests.owner.current_handle(&key).is_some(),
            "#2622 the fixture session stays loaded"
        );
        let loaded = row(&oracle, "restore.loaded-session");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&loaded["params"])
            )
            .await,
            loaded["result"],
            "#2622 restore.loaded-session"
        );
        negative_assert_eq!(
            call(&requests, "export_session_transcript", &busy_params).await,
            row(&oracle, "export.loaded-session")["result"],
            "#2622 export.loaded-session"
        );
        teardown(&requests).await;
    }

    /// The `chat.files` row: restores rename their temporary staging files
    /// away, so only final transcripts (plus the fence's own `.lock`
    /// siblings, A6.2-TRANSCRIPT-LOCK-001, and the shared fixture's seed)
    /// remain.
    #[tokio::test]
    async fn chat_tree_holds_only_final_transcripts() {
        crate::test_baseline::oracle_bounded(ORACLE);
        let oracle: Value = serde_json::from_str(ORACLE).expect("#2622 oracle JSON");
        let (_scratch, requests) = management_tests::setup();
        seed_transcripts(&requests);
        for id in ["restore.fresh", "restore.no-title"] {
            let row = row(&oracle, id);
            call(
                &requests,
                "restore_session_transcript",
                &params(&row["params"]),
            )
            .await;
        }
        let mut files: Vec<String> = std::fs::read_dir(chat_dir(&requests))
            .expect("#2622 chat directory")
            .map(|entry| {
                entry
                    .expect("#2622 chat entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        files.sort();
        negative_assert!(
            files
                .iter()
                .all(|name| name.ends_with(".jsonl") || name.ends_with(".jsonl.lock")),
            "#2622 no restore staging files remain: {files:?}"
        );
        let mut transcripts: Vec<String> = files
            .iter()
            .filter(|name| name.ends_with(".jsonl"))
            .cloned()
            .collect();
        transcripts.sort();
        let mut expected: Vec<String> = row(&oracle, "chat.files")["files"]
            .as_array()
            .expect("#2622 chat.files row")
            .iter()
            .map(|name| name.as_str().expect("#2622 file name").to_string())
            .collect();
        // The shared fixture seeds its own transcript before this family's
        // sessions exist.
        expected.push("a8-session.jsonl".into());
        expected.sort();
        negative_assert_eq!(
            transcripts,
            expected,
            "#2622 chat.files holds exactly the final transcripts"
        );
        teardown(&requests).await;
    }

    /// The oracle's `capabilities` row: the archive name is advertised at
    /// the TS position, between `delete_session` and
    /// `native_transcript_archive`.
    #[tokio::test]
    async fn session_transcript_archive_sits_between_its_ts_neighbors() {
        crate::test_baseline::oracle_bounded(ORACLE);
        let (_scratch, requests) = management_tests::setup();
        let described = GatewayRequests::describe_server(&requests)
            .await
            .expect("#2622 describe");
        let capabilities = match described.get("capabilities") {
            Some(JsValue::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_string()
                        .expect("#2622 capability name")
                        .to_utf8()
                        .expect("#2622 capability UTF8")
                })
                .collect::<Vec<_>>(),
            _ => panic!("#2622 capability array"),
        };
        let position = capabilities
            .iter()
            .position(|name| name == "session_transcript_archive")
            .expect("#2622 the archive name is advertised");
        negative_assert_eq!(
            capabilities
                .get(position.saturating_sub(1))
                .map(String::as_str),
            Some("delete_session"),
            "#2622 the archive name follows delete_session"
        );
        negative_assert_eq!(
            capabilities.get(position + 1).map(String::as_str),
            Some("native_transcript_archive"),
            "#2622 the archive name precedes native_transcript_archive"
        );
        teardown(&requests).await;
    }

    /// R5: `restoringSessionKeys` membership is read where TS reads it —
    /// before any storage work — so an in-flight key refuses without
    /// touching the transcript.
    #[tokio::test]
    async fn restore_refuses_while_the_key_is_already_restoring() {
        let (_scratch, requests) = management_tests::setup();
        let dup: JsString = "dup".into();
        {
            let mut restoring = requests
                .restoring_session_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            restoring.insert(dup.clone());
        }
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&json!({"sessionKey": "dup", "archive": restore_archive()}))
            )
            .await,
            json!({"error": {
                "code": "SESSION_RESTORE_CONFLICT",
                "message": "A transcript restore is already in progress for this session.",
            }}),
            "#2622 R5 an in-flight restore refuses with the conflict, not the busy error"
        );
        negative_assert!(
            !chat_dir(&requests).join("dup.jsonl").exists(),
            "#2622 the in-flight refusal wrote no transcript"
        );
        {
            let mut restoring = requests
                .restoring_session_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            negative_assert!(
                restoring.contains(&dup),
                "#2622 the pre-existing membership survives the refusal"
            );
            restoring.remove(&dup);
            negative_assert!(restoring.is_empty(), "#2622 the test cleaned its key");
        }
        teardown(&requests).await;
    }

    /// R13: a session holding the writer lease (hydrating or closing) makes
    /// the cold write answer the busy domain error, the same shape TS's
    /// earlier `hasSession` read produces — not the rename family's
    /// writer-busy code.
    #[tokio::test]
    async fn restore_refuses_while_a_session_holds_the_writer_lease() {
        let (_scratch, requests) = management_tests::setup();
        let key: JsString = "leased".into();
        let path = transcript_locks::session_transcript_path(
            &requests.options.home,
            &requests.options.project,
            &key,
        );
        let lease = transcript_locks::cold_write_lease(&path)
            .expect("#2622 the cold lease probe itself must not fail")
            .expect("#2622 the cold lease is free before the restore");
        negative_assert_eq!(
            call(
                &requests,
                "restore_session_transcript",
                &params(&json!({"sessionKey": "leased", "archive": restore_archive()}))
            )
            .await,
            json!({"error": {
                "code": "SESSION_BUSY",
                "message": "Cannot restore over an active Gateway session.",
            }}),
            "#2622 R13 the lease refusal answers the busy domain error"
        );
        negative_assert!(
            !path.exists(),
            "#2622 the leased refusal wrote no transcript"
        );
        negative_assert!(
            requests
                .restoring_session_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty(),
            "#2622 the failed restore removed its key"
        );
        drop(lease);
        teardown(&requests).await;
    }

    /// R10 + R12: a read-only chat directory refuses the restore without
    /// partial state — no transcript, no stranded `restoringSessionKeys`
    /// entry — and the retry succeeds once the directory is writable again.
    ///
    /// The TS delegate surfaces the raw EACCES from `replaceTranscript`;
    /// here the cold-write lease cannot create its lock file first and the
    /// fs error propagates as `gateway_request_failed` ("Could not open
    /// session ledger lock …: Permission denied" — the fence inode is
    /// Rust-only, so its text is deliberately not Node's uv shape,
    /// transcript.rs `LedgerLock::acquire`) instead of the busy domain
    /// error (#1873's busy/Err split; narrowed from the `-R1` divergence,
    /// whose base flattened the fs failure into SESSION_BUSY).
    #[cfg(unix)]
    #[tokio::test]
    async fn restore_refuses_on_an_unwritable_chat_directory_and_retries_clean() {
        // #1554-family convention: chmod 0o500 is not a write error for root
        // or CAP_DAC_OVERRIDE hosts — probe the behaviour, do not assume.
        if rustix::process::geteuid().as_raw() == 0 {
            assert!(
                !crate::test_permission_faults::require_permission_faults(),
                "#2622 PDRS_REQUIRE_PERMISSION_FAULTS is set but chmod 500 did not make the \
                 chat directory unwritable (root or CAP_DAC_OVERRIDE); this test needs an \
                 unprivileged host (需要非特权宿主)"
            );
            crate::test_skip_line::stderr_skip_line(
                "#2622 skip: restore_refuses_on_an_unwritable_chat_directory: \
                 running as root (chmod 500 is not a write error for root)\n",
            );
            return;
        }
        let (_scratch, requests) = management_tests::setup();
        let chat = chat_dir(&requests);
        use std::os::unix::fs::PermissionsExt;
        // Restore 0o755 even when an assert panics, so the scratch teardown
        // can always remove the tree.
        struct WritableAgain(std::path::PathBuf);
        impl Drop for WritableAgain {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let mut permissions = std::fs::metadata(&chat)
            .expect("#2622 chat metadata")
            .permissions();
        permissions.set_mode(0o500);
        std::fs::set_permissions(&chat, permissions).expect("#2622 read-only chat directory");
        let _writable_again = WritableAgain(chat.clone());
        let params = params(&json!({"sessionKey": "fenced-write", "archive": restore_archive()}));
        let refused = call(&requests, "restore_session_transcript", &params).await;
        let error = &refused["error"];
        negative_assert!(
            error["code"] == json!("gateway_request_failed"),
            "#2622 R10 an unwritable chat directory fails with the raw fs error, not a busy domain error: {refused}"
        );
        negative_assert!(
            error["message"]
                .as_str()
                .is_some_and(|text| text.contains("Permission denied")),
            "#2622 R10 the refusal carries the raw EACCES io text: {refused}"
        );
        negative_assert!(
            !chat.join("fenced-write.jsonl").exists(),
            "#2622 the refused restore wrote no transcript"
        );
        negative_assert!(
            requests
                .restoring_session_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty(),
            "#2622 R12 the failed write removed its key"
        );
        let mut permissions = std::fs::metadata(&chat)
            .expect("#2622 chat metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&chat, permissions).expect("#2622 writable chat directory");
        negative_assert_eq!(
            call(&requests, "restore_session_transcript", &params).await,
            json!({"ok": {"sessionKey": "fenced-write", "importedMessages": 2}}),
            "#2622 the retry succeeds after the directory is writable"
        );
        teardown(&requests).await;
    }

    /// R11: the exists check runs under the transcript fence. Holding the
    /// fence while a restore is queued, then writing the transcript the way
    /// the concurrent first restore's rename would, makes the queued restore
    /// observe the conflict instead of overwriting it.
    #[tokio::test]
    async fn restore_exists_check_waits_for_the_transcript_fence() {
        let (_scratch, requests) = management_tests::setup();
        let key: JsString = "fenced-exists".into();
        let path = transcript_locks::session_transcript_path(
            &requests.options.home,
            &requests.options.project,
            &key,
        );
        let fence = transcript_locks::acquire(&path)
            .await
            .expect("#2622 hold the transcript fence");
        let queued = requests.clone();
        let handle = tokio::spawn(async move {
            call(
                &queued,
                "restore_session_transcript",
                &params(&json!({"sessionKey": "fenced-exists", "archive": restore_archive()})),
            )
            .await
        });
        // Everything ahead of the fence is in-memory; a short wait lets the
        // queued restore reach the fence await before the file lands.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::fs::write(&path, b"{}\n").expect("#2622 concurrent transcript");
        drop(fence);
        negative_assert_eq!(
            handle.await.expect("#2622 the queued restore finished"),
            json!({"error": {
                "code": "SESSION_RESTORE_CONFLICT",
                "message": "A persistent transcript already exists for this session.",
            }}),
            "#2622 R11 the exists check ran under the fence"
        );
        negative_assert!(
            requests
                .restoring_session_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty(),
            "#2622 the conflicted restore removed its key"
        );
        teardown(&requests).await;
    }

    /// #2622 fix-r1 (boundary P1, arbitration probe on cb8): a 2000-container
    /// chain sits exactly at the shared bound and pretty-renders without
    /// spending a native frame per level — the recursive form aborted the
    /// process (`fatal runtime error: stack overflow`, exit 134). The bound
    /// is the shared family budget, not V8's shape-dependent throw point
    /// (see the `Fallback` doc and the registered A6-JSVALUE-DEPTH-001
    /// divergence).
    #[test]
    fn deep_tool_input_renders_within_the_shared_container_bound() {
        let mut value = JsValue::Array(vec![JsValue::Number(1.0)]);
        for _ in 0..1_999 {
            value = JsValue::Array(vec![value]);
        }
        let units = stringify_portable_value(&value)
            .expect("#2622 the 2000-deep chain is inside the shared bound");
        // Two-space indentation at 2000 levels renders ~8 MiB of layout.
        assert!(
            units.len() > 100_000,
            "pretty layout rendered {} units",
            units.len()
        );
        assert_eq!(units.last(), Some(&u16::from(b']')));
    }

    /// #2622 fix-r3 (DS r3b boundary P2): `Array.prototype.join` coerces
    /// `undefined`/`null` elements to the empty string, unlike the template
    /// interpolations around them which stringify `undefined`
    /// (toolResultContent.ts:22). An inner block whose `type` is none of
    /// text/image/pdf falls out of the TS switch as `undefined`, and a text
    /// block whose `text` is absent or null renders the same empty element —
    /// neither may leak a literal `undefined` into the export. A nullish
    /// inner block still throws reading `type`, like every other raw
    /// property read.
    #[test]
    fn flatten_joins_nullish_and_unknown_inner_blocks_as_empty_strings() {
        let block = pilotdeck_protocol::parse_json(
            r#"{"content": [
                {"type": "audio"},
                {"type": "text", "text": "kept"},
                {"type": "text"},
                {"type": "text", "text": null}
            ]}"#,
        )
        .expect("#2622 the joined block parses");
        let units =
            flatten_tool_result_text(&block).expect("#2622 the nullish-free joins never throw");
        let text = String::from_utf16(&units).expect("#2622 joined units");
        negative_assert_eq!(
            text,
            "\nkept\n\n".to_string(),
            "#2622 join semantics: unknown types and missing text coerce to the empty string"
        );
        let nullish = pilotdeck_protocol::parse_json(
            r#"{"content": [{"type": "text", "text": "first"}, null]}"#,
        )
        .expect("#2622 the nullish block parses");
        let error = flatten_tool_result_text(&nullish)
            .expect_err("#2622 a nullish inner block throws reading type");
        negative_assert!(
            error
                .message
                .to_utf8()
                .map(|text| text.contains("Cannot read properties of null (reading 'type')"))
                .unwrap_or(false),
            "#2622 the raw property read surfaces (got {})",
            error.message.to_utf8().unwrap_or_default()
        );
    }

    /// #2622 fix-r4 (DS r4 boundary P1): the pretty budget counts UTF-16
    /// code units — the unit V8's `String::kMaxLength` is measured in — not
    /// UTF-8 bytes. 190M CJK chars render to ~570M UTF-8 bytes (past the
    /// old byte bound, which fell back) but only ~190M units, well under
    /// the limit: the render must complete as pretty JSON. `String(value)`
    /// on a one-element array joins the bare element, so it starts with
    /// the character itself, not `[`.
    #[test]
    fn cjk_pretty_render_is_budgeted_in_utf16_units() {
        let text: Vec<u16> = std::iter::repeat(0x6C49).take(190_000_000).collect();
        let value = JsValue::Array(vec![JsValue::String(JsString::from_utf16(text))]);
        let rendered =
            stringify_portable_value(&value).expect("#2622 the unit budget keeps this renderable");
        negative_assert_eq!(
            rendered.first(),
            Some(&u16::from(b'[')),
            "#2622 the CJK render stays pretty past the old byte budget"
        );
    }

    /// #2622 fix-r4 (DS r4 boundary P2): the export title gate is
    /// `title && title.length <= MAX` (createLocalGateway.ts:1643) — a
    /// truthiness check plus the `.length` property, **no typeof check**.
    /// A truthy array-like title with a small `.length` passes in TS and
    /// must pass here; a title without a numeric `.length` fails `<=` and
    /// drops, and so does a 513-element array.
    #[tokio::test]
    async fn export_title_gate_reads_the_length_property_like_ts() {
        let (_scratch, requests) = management_tests::setup();
        for (key, metadata, keeps) in [
            ("array-title", json!({"title": ["x"]}), true),
            ("array-over", json!({"title": ["x"; 513]}), false),
            ("number-title", json!({"title": 42}), false),
            ("empty-title", json!({"title": ""}), false),
        ] {
            write_session(
                &requests,
                key,
                &transcript(&[
                    (
                        "accepted_input",
                        "t1",
                        json!({"messages": [
                            {"role": "user",
                             "content": [{"type": "text", "text": "Titled"}]},
                        ]}),
                    ),
                    ("session_metadata", "t1", json!({"metadata": metadata})),
                    (
                        "turn_result",
                        "t1",
                        json!({"result": {"usage": {}, "permissionDenials": []}}),
                    ),
                ]),
            );
            let archive = call(
                &requests,
                "export_session_transcript",
                &params(&json!({"sessionKey": key})),
            )
            .await;
            if keeps {
                negative_assert_eq!(
                    archive["title"],
                    json!(["x"]),
                    "#2622 a truthy array-like title with a small .length passes"
                );
            } else {
                negative_assert!(
                    archive.get("title").is_none(),
                    "#2622 {key}: the title gate drops this shape"
                );
            }
        }
        teardown(&requests).await;
    }

    /// #2622 fix-r2 (sibling P2 pairing the depth half of the fallback): one
    /// container past the shared bound the pretty render refuses and the
    /// `String(value)` fallback runs — which itself reports the family's
    /// RangeError at this depth. Without the depth guard the explicit stack
    /// would happily render 2001 levels, so this test is the kill anchor for
    /// the `export-pretty-depth-omitted` mutant.
    #[test]
    fn deep_tool_input_falls_back_past_the_shared_container_bound() {
        let mut value = JsValue::Array(vec![JsValue::Number(1.0)]);
        for _ in 0..2_000 {
            value = JsValue::Array(vec![value]);
        }
        let error = stringify_portable_value(&value)
            .expect_err("#2622 the pretty render refuses one container past the shared bound");
        assert!(
            error
                .message
                .to_utf8()
                .map(|text| text.contains("Maximum call stack size exceeded"))
                .unwrap_or(false),
            "#2622 the depth fallback joins through the shared String bound (got {})",
            error.message.to_utf8().unwrap_or_default()
        );
    }

    /// #2622 fix-r1: a wide payload at that depth pushes the pretty render
    /// past V8's string length — the render stops at
    /// `MAX_PRETTY_OUTPUT_BYTES` and falls back to `String(value)`. This
    /// string-length boundary is the aligned one: V8's stringify throws the
    /// catchable RangeError here and the TS helper's catch takes the same
    /// fallback.
    #[test]
    fn wide_deep_tool_input_falls_back_past_the_pretty_output_bound() {
        let mut value = JsValue::Array(vec![JsValue::Number(1.0); 140_000]);
        for _ in 0..1_999 {
            value = JsValue::Array(vec![value]);
        }
        let units = stringify_portable_value(&value)
            .expect("#2622 the fallback keeps this inside the shared bound");
        // String(value) joins the flat elements with commas; a pretty render
        // would be ≥ 140_000 × 4_000 units.
        assert!(
            units.len() < 1_000_000,
            "#2622 the fallback keeps this inside the shared bound (joined {} units)",
            units.len()
        );
    }
}
