//! AgentEvent → GatewayEvent projection.
//!
//! Port of `gateway/client/GatewayAgentEventProjector.ts:37-652` at baseline
//! `cc7c4e65`. Events arrive as lossless [`JsValue`]s and frames are built as
//! `JsValue` objects so own-property insertion order matches the TypeScript
//! object literals; serde ordering is deliberately not used (#479 item 3).
//!
//! The projector only derives frames. Persisting advisory large tool-result
//! previews is delegated to [`GatewayToolResultArtifactStorePort`], mirroring
//! the TS `GatewayToolResultArtifactStorePort` injection point.

use crate::js_value::MAX_ARRAY_DEPTH;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

/// `MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS` (UTF-16 code units).
pub const MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS: usize = 20_000;
/// `MAX_GATEWAY_TOOL_DATA_STRING_CHARS` (UTF-16 code units).
pub const MAX_GATEWAY_TOOL_DATA_STRING_CHARS: usize = 4_000;
const DEFAULT_ARTIFACT_THRESHOLD_BYTES: usize = 4_096;

/// A projection failure that TS expresses as a thrown `TypeError`/`RangeError`.
///
/// `Display` matches `String(error)` in Node, e.g.
/// `TypeError: Cannot read properties of undefined (reading 'type')`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionError {
    text: String,
}

impl ProjectionError {
    fn type_error(message: impl Into<String>) -> Self {
        Self {
            text: format!("TypeError: {}", message.into()),
        }
    }

    fn range_error(message: impl Into<String>) -> Self {
        Self {
            text: format!("RangeError: {}", message.into()),
        }
    }

    /// The `TypeError: …` / `RangeError: …` text, as `String(error)` yields.
    pub fn message(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for ProjectionError {}

/// Projection result; `Err` mirrors a TS throw for structurally invalid events.
pub type ProjectionResult<T> = Result<T, ProjectionError>;

/// `GatewayToolResultArtifactInput`.
#[derive(Clone, Debug)]
pub struct GatewayToolResultArtifactInput {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub tool_call_id: JsString,
    pub text: JsString,
}

/// `GatewayToolResultArtifactStorePort`. Returns the artifact path when the
/// preview is over the threshold (TS returns it before the advisory write
/// finishes); `None` leaves `resultPath` undefined (below threshold, or no
/// advisory writer could be started).
pub trait GatewayToolResultArtifactStorePort: Send + Sync {
    fn persist(&self, input: &GatewayToolResultArtifactInput) -> Option<String>;
}

type CreateDirectoryHook = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;
type WriteTextHook = Arc<dyn Fn(&Path, &[u8]) -> io::Result<()> + Send + Sync>;
type ArtifactWriteJob = Box<dyn FnOnce() + Send + 'static>;

/// Background advisory writer.
///
/// TS's `void this.write(...)` (`GatewayToolResultArtifactStore.ts:48`) never
/// waits for disk and never drops a write. This port keeps that contract: jobs
/// go to one detached writer thread (bounded thread count, unbounded queue),
/// `enqueue` never blocks, and dropping the store does not join in-flight
/// writes. [`GatewayToolResultArtifactStore::wait_for_pending`] is the explicit
/// test/shutdown barrier.
#[derive(Default)]
struct ArtifactWrites {
    sender: Mutex<Option<mpsc::Sender<ArtifactWriteJob>>>,
}

impl ArtifactWrites {
    /// Returns whether the job was accepted. A rejected job must not be
    /// reported to the caller as a persisted path.
    fn enqueue(&self, job: impl FnOnce() + Send + 'static) -> bool {
        let mut sender = self.sender.lock().unwrap();
        if sender.is_none() {
            *sender = spawn_artifact_writer();
        }
        let failed = match sender.as_ref() {
            Some(tx) => tx.send(Box::new(job)).is_err(),
            None => return false,
        };
        if failed {
            // The writer died (a hook panicked); start over next time.
            *sender = None;
        }
        !failed
    }

    fn wait(&self) {
        let (done_tx, done_rx) = mpsc::channel();
        if self.enqueue(move || {
            let _ = done_tx.send(());
        }) {
            let _ = done_rx.recv();
        }
    }
}

fn spawn_artifact_writer() -> Option<mpsc::Sender<ArtifactWriteJob>> {
    let (sender, receiver) = mpsc::channel::<ArtifactWriteJob>();
    let spawned = std::thread::Builder::new()
        .name("gateway-tool-result-artifact".into())
        .spawn(move || {
            // Ends once the store drops its sender and the queue drains, so
            // queued writes still complete and Drop never waits.
            for job in receiver {
                job();
            }
        });
    // The handle is dropped on purpose: the writer is detached.
    spawned.ok().map(|_| sender)
}

/// Default `GatewayToolResultArtifactStore`: best-effort files under the OS
/// temp directory once the text exceeds the 4096-byte threshold.
///
/// Writer hooks mirror the TS `createDirectory`/`writeText` options so tests
/// can observe the fire-and-forget contract.
#[derive(Clone)]
pub struct GatewayToolResultArtifactStore {
    root_dir: PathBuf,
    threshold_bytes: usize,
    create_directory: CreateDirectoryHook,
    write_text: WriteTextHook,
    writes: Arc<ArtifactWrites>,
}

impl GatewayToolResultArtifactStore {
    pub fn new(root_dir: impl Into<PathBuf>, threshold_bytes: usize) -> Self {
        Self::with_io(
            root_dir,
            threshold_bytes,
            Arc::new(create_directory_private),
            Arc::new(write_private),
        )
    }

    /// TS `GatewayToolResultArtifactStoreOptions.createDirectory/writeText`.
    pub fn with_io(
        root_dir: impl Into<PathBuf>,
        threshold_bytes: usize,
        create_directory: CreateDirectoryHook,
        write_text: WriteTextHook,
    ) -> Self {
        let root_dir = root_dir.into();
        let root_dir = if root_dir.is_absolute() {
            root_dir
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(root_dir)
        };
        Self {
            root_dir,
            threshold_bytes,
            create_directory,
            write_text,
            writes: Arc::new(ArtifactWrites::default()),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Wait for advisory writes started so far (test/shutdown barrier).
    pub fn wait_for_pending(&self) {
        self.writes.wait();
    }
}

impl Default for GatewayToolResultArtifactStore {
    fn default() -> Self {
        Self::new(
            std::env::temp_dir().join("pilotdeck-tool-results"),
            DEFAULT_ARTIFACT_THRESHOLD_BYTES,
        )
    }
}

impl GatewayToolResultArtifactStorePort for GatewayToolResultArtifactStore {
    fn persist(&self, input: &GatewayToolResultArtifactInput) -> Option<String> {
        if utf8_len(input.text.units()) <= self.threshold_bytes {
            return None;
        }
        // TS `resolve(rootDir, safePathPart(sessionId), safePathPart(turnId))`
        // normalizes `.`/`..` segments (safePathPart can emit both).
        let directory = resolve_lexically(
            self.root_dir
                .join(safe_path_part(input.session_id.units()))
                .join(safe_path_part(input.turn_id.units())),
        );
        let path = resolve_lexically(directory.join(format!(
            "{}.txt",
            safe_path_part(input.tool_call_id.units())
        )));
        // `void this.write(...)`: return the path immediately; the advisory
        // write runs off-thread and failures are swallowed.
        let bytes = String::from_utf16_lossy(input.text.units()).into_bytes();
        let create_directory = Arc::clone(&self.create_directory);
        let write_text = Arc::clone(&self.write_text);
        let job_path = path.clone();
        let accepted = self.writes.enqueue(move || {
            if create_directory(&directory).is_err() {
                return;
            }
            let _ = write_text(&job_path, &bytes);
        });
        // Never hand out a path whose write was not even queued.
        accepted.then(|| path.to_string_lossy().into_owned())
    }
}

/// Lexical `path.resolve` normalization (no filesystem access).
fn resolve_lexically(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn create_directory_private(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)
}

/// `GatewayAgentEventProjectorOptions`.
#[derive(Default)]
pub struct GatewayAgentEventProjectorOptions {
    pub tool_result_artifacts: Option<Arc<dyn GatewayToolResultArtifactStorePort>>,
    /// `mapAgentEvent`'s `forwardSubagentText` facade option.
    pub forward_subagent_text: bool,
}

/// `GatewayAgentEventProjectionInput`.
pub struct GatewayAgentEventProjectionInput<'a> {
    pub event: &'a JsValue,
    pub run_id: &'a str,
    pub forward_subagent_text: bool,
}

pub struct GatewayAgentEventProjector {
    tool_result_artifacts: Arc<dyn GatewayToolResultArtifactStorePort>,
}

impl GatewayAgentEventProjector {
    pub fn new(options: GatewayAgentEventProjectorOptions) -> Self {
        Self {
            tool_result_artifacts: options
                .tool_result_artifacts
                .unwrap_or_else(|| Arc::new(GatewayToolResultArtifactStore::default())),
        }
    }

    pub fn project(
        &self,
        input: &GatewayAgentEventProjectionInput<'_>,
    ) -> ProjectionResult<Vec<JsValue>> {
        Ok(project_event(
            input.event,
            input.run_id,
            self.tool_result_artifacts.as_ref(),
            input.forward_subagent_text,
        )?
        .into_iter()
        .map(|frame| finish_frame(frame, input.event, input.run_id))
        .collect())
    }
}

impl Default for GatewayAgentEventProjector {
    fn default() -> Self {
        Self::new(GatewayAgentEventProjectorOptions::default())
    }
}

/// `mapAgentEvent` compatibility facade.
pub fn map_agent_event(
    event: &JsValue,
    run_id: &str,
    options: GatewayAgentEventProjectorOptions,
) -> ProjectionResult<Vec<JsValue>> {
    let forward_subagent_text = options.forward_subagent_text;
    let projector = GatewayAgentEventProjector::new(options);
    projector.project(&GatewayAgentEventProjectionInput {
        event,
        run_id,
        forward_subagent_text,
    })
}

// ---------------------------------------------------------------------------
// JS value helpers
// ---------------------------------------------------------------------------

static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}

fn is_null(value: &JsValue) -> bool {
    matches!(value, JsValue::Null)
}

/// V8's message for reading a property off `null`/`undefined`, which is what
/// `String(error)` yields for structurally invalid known events.
fn missing_property_error(receiver: &JsValue, name: &str) -> ProjectionError {
    let what = if is_null(receiver) {
        "null"
    } else {
        "undefined"
    };
    ProjectionError::type_error(format!(
        "Cannot read properties of {what} (reading '{name}')"
    ))
}

/// JS `base.name` where reading from a `null`/`undefined` `base` throws.
fn required<'a>(base: &'a JsValue, name: &str) -> ProjectionResult<&'a JsValue> {
    if is_null(base) || base.is_undefined() {
        return Err(missing_property_error(base, name));
    }
    Ok(field(base, name))
}

/// `receiver.method(...)` where only a `null`/`undefined` *receiver* throws
/// (`reading 'method'`); the method itself is modeled by the caller.
fn required_receiver<'a>(receiver: &'a JsValue, name: &str) -> ProjectionResult<&'a JsValue> {
    if is_null(receiver) || receiver.is_undefined() {
        return Err(missing_property_error(receiver, name));
    }
    Ok(receiver)
}

/// JS `value.map(...)` / `value.flatMap(...)`; only arrays expose the method.
fn required_array<'a>(value: &'a JsValue, method: &str) -> ProjectionResult<&'a [JsValue]> {
    match value {
        JsValue::Array(items) => Ok(items),
        JsValue::Undefined | JsValue::Null => Err(missing_property_error(value, method)),
        // V8's message includes the source expression; no wire value reaches
        // this arm, so the terse form is enough to keep the throw observable.
        _ => Err(ProjectionError::type_error(format!(
            "{method} is not a function"
        ))),
    }
}

/// JS `for (const item of value)`: arrays iterate; strings iterate characters
/// (which carry no `.type`, so they produce no frames); other values throw.
fn required_iterable<'a>(value: &'a JsValue, expression: &str) -> ProjectionResult<&'a [JsValue]> {
    match value {
        JsValue::Array(items) => Ok(items),
        JsValue::String(_) => Ok(&[]),
        _ => Err(ProjectionError::type_error(format!(
            "{expression} is not iterable"
        ))),
    }
}

#[cfg(test)]
fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

fn object<'a>(entries: impl IntoIterator<Item = (&'a str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(JsString::from(key), value);
    }
    JsValue::Object(object)
}

fn insert(frame: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(object) = frame {
        object.insert(JsString::from(key), value);
    }
}

fn text(value: impl Into<JsString>) -> JsValue {
    JsValue::String(value.into())
}

fn number(value: f64) -> JsValue {
    JsValue::Number(value)
}

fn boolean(value: bool) -> JsValue {
    JsValue::Bool(value)
}

fn is_type(value: &JsValue, expected: &str) -> bool {
    value
        .as_string()
        .is_some_and(|value| value.equals_str(expected))
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

/// ECMAScript `String(value)` as UTF-16 units, via the shared
/// [`crate::js_value::to_string_units`] (same `Array.prototype.join` semantics
/// and container-depth bound as the rest of the crate). Template-literal
/// interpolation and `String()` must not degrade deep arrays to
/// `[object Object]`: TS joins them, and throws `RangeError` past the bound.
fn checked_string_units(value: &JsValue) -> ProjectionResult<Vec<u16>> {
    crate::js_value::to_string_units(value)
        .map_err(|_| ProjectionError::range_error(crate::js_value::StackOverflow::MESSAGE))
}

/// `String(value)` as used inside a `catch` block.
fn checked_js_text(value: &JsValue) -> ProjectionResult<JsValue> {
    Ok(JsValue::String(JsString::from_utf16(checked_string_units(
        value,
    )?)))
}

/// `value.length` (UTF-16 units): reading it off null/undefined throws.
fn js_length(value: &JsValue) -> ProjectionResult<usize> {
    if is_null(value) || value.is_undefined() {
        return Err(missing_property_error(value, "length"));
    }
    Ok(checked_string_units(value)?.len())
}

/// `JSON.stringify` with the repository's shared container-depth bound.
fn json_stringify(value: &JsValue) -> Result<Option<String>, crate::js_value::StackOverflow> {
    crate::js_value::json_stringify(value)
}

fn utf8_len(units: &[u16]) -> usize {
    let mut bytes = 0usize;
    let mut index = 0usize;
    while index < units.len() {
        let unit = units[index];
        bytes += match unit {
            0x0000..=0x007f => 1,
            0x0080..=0x07ff => 2,
            0xd800..=0xdbff => {
                if index + 1 < units.len() && (0xdc00..=0xdfff).contains(&units[index + 1]) {
                    index += 1;
                    4
                } else {
                    3
                }
            }
            0xdc00..=0xdfff => 3,
            _ => 3,
        };
        index += 1;
    }
    bytes
}

fn is_js_space(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000a | 0x000b | 0x000c | 0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

fn js_trim(units: &[u16]) -> &[u16] {
    let mut start = 0usize;
    let mut end = units.len();
    while start < end && is_js_space(units[start]) {
        start += 1;
    }
    while end > start && is_js_space(units[end - 1]) {
        end -= 1;
    }
    &units[start..end]
}

/// `safeGatewayPathPart` (`GatewayAgentEventProjector.ts:579`, identical to
/// `GatewayToolResultArtifactStore.ts:62`): trim, collapse disallowed runs to
/// `-`, trim edge dashes, then fall back to `"value"`.
///
/// `runtime/context/tool_budget.rs` ports a *different* TS helper,
/// `safePathPart` in `context/budget/ToolResultBudget.ts:477`, which has no
/// `|| "value"` fallback (`scopedToolResultKey` adds `|| "tool-call"` at its
/// call site, `:472`). The two Rust ports therefore intentionally differ; they
/// are not the same helper and must not be unified.
fn safe_path_part(units: &[u16]) -> String {
    let mut out = String::new();
    let mut pending_separator = false;
    for unit in js_trim(units) {
        let allowed = matches!(
            unit,
            0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x2e | 0x5f | 0x2d
        );
        if allowed {
            out.push(*unit as u8 as char);
            pending_separator = false;
        } else if !pending_separator {
            out.push('-');
            pending_separator = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "value".into()
    } else {
        trimmed.into()
    }
}

/// `safeGatewayPathPart(value)`: `value.trim()` throws on null/undefined, and
/// the template-literal call site evaluates it before the extension lookup.
fn safe_path_part_js(value: &JsValue) -> ProjectionResult<String> {
    let value = required_receiver(value, "trim")?;
    Ok(safe_path_part(&checked_string_units(value)?))
}

fn last_path_segment(units: &[u16]) -> JsString {
    let mut start = 0usize;
    for (index, unit) in units.iter().enumerate() {
        if *unit == u16::from(b'/') || *unit == u16::from(b'\\') {
            start = index + 1;
        }
    }
    JsString::from_utf16(units[start..].to_vec())
}

/// `path.split(/[\\/]/).pop()`: `split` throws on null/undefined.
fn last_path_segment_js(value: &JsValue) -> ProjectionResult<JsString> {
    let value = required_receiver(value, "split")?;
    Ok(last_path_segment(&checked_string_units(value)?))
}

/// TS uses Unicode `toLowerCase()`; ASCII lowering is equivalent for the four
/// known mime types because no non-ASCII case fold lands on one of them.
fn extension_for_mime(mime_type: &JsValue) -> ProjectionResult<&'static str> {
    let mime_type = required_receiver(mime_type, "toLowerCase")?;
    let lowered: Vec<u16> = checked_string_units(mime_type)?
        .into_iter()
        .map(|unit| match unit {
            0x41..=0x5a => unit + 0x20,
            _ => unit,
        })
        .collect();
    let lowered = String::from_utf16_lossy(&lowered);
    Ok(match lowered.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    })
}

fn slice_utf16(units: &[u16], start: usize, end: usize) -> Vec<u16> {
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    units[start..end].to_vec()
}

/// `text.slice(-count)`; JS treats `-0` as `0` and returns the whole string.
fn slice_from_end(units: &[u16], count: usize) -> Vec<u16> {
    if count == 0 {
        units.to_vec()
    } else {
        units[units.len().saturating_sub(count)..].to_vec()
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

fn project_event(
    event: &JsValue,
    run_id: &str,
    store: &dyn GatewayToolResultArtifactStorePort,
    forward_subagent_text: bool,
) -> ProjectionResult<Vec<JsValue>> {
    let kind = required(event, "type")?;
    let kind = match kind.as_string() {
        Some(kind) => kind,
        None => return Ok(Vec::new()),
    };
    let kind = String::from_utf16_lossy(kind.units());
    Ok(match kind.as_str() {
        "turn_started" => vec![object([
            ("type", text("turn_started")),
            ("runId", text(run_id)),
        ])],
        "input_accepted" => vec![object([
            ("type", text("input_accepted")),
            ("runId", text(run_id)),
        ])],
        "steer_applied" => {
            let message = field(event, "message");
            let timeline_source = required(message, "content")?;
            let mut frame = object([
                ("type", text("steer_applied")),
                ("itemId", field(event, "itemId").clone()),
                ("message", message.clone()),
            ]);
            if let Some(timeline) = first_block_timeline(timeline_source)? {
                insert(&mut frame, "timeline", timeline);
            }
            vec![frame]
        }
        "steer_unapplied" => vec![object([
            ("type", text("steer_unapplied")),
            ("itemId", field(event, "itemId").clone()),
            ("reason", field(event, "reason").clone()),
        ])],
        "model_request_started" => vec![object([
            ("type", text("model_request_started")),
            ("model", field(event, "model").clone()),
            ("provider", field(event, "provider").clone()),
        ])],
        "model_event" => {
            let mut frames = map_model_event(field(event, "event"), run_id)?;
            let block_id = field(event, "blockId");
            if truthy(block_id) {
                for frame in &mut frames {
                    insert(frame, "blockId", block_id.clone());
                }
            }
            frames
        }
        "assistant_message" => {
            let message = field(event, "message");
            assistant_message_frames(message)?
        }
        "prompt_suggestion" => vec![object([
            ("type", text("prompt_suggestion")),
            ("suggestion", field(event, "suggestion").clone()),
        ])],
        "tool_calls_detected" => required_array(field(event, "calls"), "map")?
            .iter()
            .map(|call| {
                let mut frame = JsObject::new();
                if truthy(field(call, "timeline")) {
                    frame.insert(JsString::from("timeline"), field(call, "timeline").clone());
                }
                frame.insert(JsString::from("type"), text("tool_call_started"));
                frame.insert(JsString::from("toolCallId"), field(call, "id").clone());
                frame.insert(JsString::from("name"), field(call, "name").clone());
                frame.insert(
                    JsString::from("argsPreview"),
                    preview_unknown(field(call, "input"))?,
                );
                Ok(JsValue::Object(frame))
            })
            .collect::<ProjectionResult<Vec<_>>>()?,
        "tool_progress" => {
            let mut frame = JsObject::new();
            frame.insert(JsString::from("type"), text("tool_progress"));
            frame.insert(
                JsString::from("toolCallId"),
                field(event, "toolCallId").clone(),
            );
            frame.insert(JsString::from("toolName"), field(event, "toolName").clone());
            frame.insert(JsString::from("message"), field(event, "message").clone());
            if truthy(field(event, "metadata")) {
                frame.insert(JsString::from("metadata"), field(event, "metadata").clone());
            }
            frame.insert(
                JsString::from("createdAt"),
                field(event, "createdAt").clone(),
            );
            vec![JsValue::Object(frame)]
        }
        "tool_result" => tool_result_frames(event, store)?,
        "file_artifacts" => vec![object([
            ("type", text("file_artifacts")),
            ("artifacts", field(event, "artifacts").clone()),
        ])],
        "mode_change_requested" => vec![object([
            ("type", text("plan_mode_changed")),
            ("mode", field(event, "mode").clone()),
        ])],
        "turn_completed" => turn_completed_frames(field(event, "result"))?,
        "turn_failed" => {
            let error = field(event, "error");
            let code = required(error, "code")?;
            vec![object([
                ("type", text("error")),
                ("code", code.clone()),
                ("message", field(error, "message").clone()),
                ("recoverable", boolean(false)),
                ("userHint", field(error, "userHint").clone()),
                ("providerError", provider_error_from_agent_error(error)?),
            ])]
        }
        "token_cap_adjusted" => vec![object([
            ("type", text("agent_status")),
            ("event", text("token_cap_adjusted")),
            (
                "detail",
                object([
                    ("provider", field(event, "provider").clone()),
                    ("model", field(event, "model").clone()),
                    ("cap", field(event, "cap").clone()),
                    ("previous", field(event, "previous").clone()),
                    ("next", field(event, "next").clone()),
                    ("reason", field(event, "reason").clone()),
                ]),
            ),
        ])],
        "empty_output_recovery" => vec![object([
            ("type", text("agent_status")),
            ("event", text("empty_output_recovery")),
            (
                "detail",
                object([
                    ("provider", field(event, "provider").clone()),
                    ("model", field(event, "model").clone()),
                    ("finishReason", field(event, "finishReason").clone()),
                    (
                        "previousMaxOutputTokens",
                        field(event, "previousMaxOutputTokens").clone(),
                    ),
                    (
                        "nextMaxOutputTokens",
                        field(event, "nextMaxOutputTokens").clone(),
                    ),
                ]),
            ),
        ])],
        "model_recovery_failed" => {
            let error = field(event, "error");
            let code = required(error, "code")?;
            vec![object([
                ("type", text("agent_status")),
                ("event", text("model_recovery_failed")),
                (
                    "detail",
                    object([
                        ("provider", field(event, "provider").clone()),
                        ("model", field(event, "model").clone()),
                        ("code", code.clone()),
                        ("message", field(error, "message").clone()),
                        ("providerError", provider_error_from_model_error(error)?),
                    ]),
                ),
            ])]
        }
        "session_aborted" => {
            let reason = field(event, "reason");
            let message = if nullish(reason) {
                text("Session aborted.")
            } else {
                reason.clone()
            };
            vec![object([
                ("type", text("error")),
                ("code", text("agent_aborted")),
                ("message", message),
                ("recoverable", boolean(true)),
            ])]
        }
        "tool_results_projected" => tool_results_projected_frames(field(event, "message"))?,
        "compact_started" => vec![object([
            ("type", text("agent_status")),
            ("event", text("compact_started")),
            (
                "detail",
                object([
                    ("compactionId", field(event, "compactionId").clone()),
                    ("trigger", field(event, "trigger").clone()),
                    ("preTokens", field(event, "preTokens").clone()),
                ]),
            ),
        ])],
        "compact_completed" => vec![object([
            ("type", text("agent_status")),
            ("event", text("compact_completed")),
            (
                "detail",
                object([
                    ("compactionId", field(event, "compactionId").clone()),
                    ("trigger", field(event, "trigger").clone()),
                    ("status", field(event, "status").clone()),
                    ("preTokens", field(event, "preTokens").clone()),
                    ("postTokens", field(event, "postTokens").clone()),
                    (
                        "messagesSummarized",
                        field(event, "messagesSummarized").clone(),
                    ),
                ]),
            ),
        ])],
        "context_budget" => context_budget_frames(field(event, "snapshot"))?,
        "warning" => vec![object([
            ("type", text("agent_status")),
            ("event", text("warning")),
            (
                "detail",
                object([
                    ("code", field(event, "code").clone()),
                    ("message", field(event, "message").clone()),
                    ("metadata", field(event, "metadata").clone()),
                ]),
            ),
        ])],
        "agent_status" => vec![object([
            ("type", text("agent_status")),
            ("event", field(event, "event").clone()),
            ("detail", field(event, "detail").clone()),
        ])],
        "turn_continued" => vec![object([
            ("type", text("agent_status")),
            ("event", text("turn_continued")),
            (
                "detail",
                object([("reason", field(event, "reason").clone())]),
            ),
        ])],
        "subagent_started" => vec![object([
            ("type", text("agent_status")),
            ("event", text("subagent_started")),
            (
                "detail",
                object([
                    ("subagentId", field(event, "subagentId").clone()),
                    ("subagentType", field(event, "subagentType").clone()),
                    ("toolCallId", field(event, "toolCallId").clone()),
                ]),
            ),
        ])],
        "subagent_completed" => {
            let mut detail = JsObject::new();
            detail.insert(
                JsString::from("subagentId"),
                field(event, "subagentId").clone(),
            );
            detail.insert(
                JsString::from("subagentType"),
                field(event, "subagentType").clone(),
            );
            detail.insert(JsString::from("success"), field(event, "success").clone());
            if truthy(field(event, "aborted")) {
                detail.insert(JsString::from("aborted"), boolean(true));
            }
            detail.insert(
                JsString::from("durationMs"),
                field(event, "durationMs").clone(),
            );
            vec![object([
                ("type", text("agent_status")),
                ("event", text("subagent_completed")),
                ("detail", JsValue::Object(detail)),
            ])]
        }
        "subagent_model_event" => map_subagent_model_event(event, forward_subagent_text)?,
        "subagent_tool_calls_detected" => required_array(field(event, "calls"), "map")?
            .iter()
            .map(|call| {
                object([
                    ("type", text("agent_status")),
                    ("event", text("subagent_tool_call_started")),
                    (
                        "detail",
                        object([
                            ("subagentId", field(event, "subagentId").clone()),
                            ("subagentType", field(event, "subagentType").clone()),
                            ("toolCallId", field(call, "id").clone()),
                            ("toolName", field(call, "name").clone()),
                            ("input", field(call, "input").clone()),
                        ]),
                    ),
                ])
            })
            .collect(),
        "subagent_tool_result" => subagent_tool_result_frames(event)?,
        "subagent_status" => vec![object([
            ("type", text("agent_status")),
            ("event", text("subagent_status")),
            (
                "detail",
                object([
                    ("subagentId", field(event, "subagentId").clone()),
                    ("subagentType", field(event, "subagentType").clone()),
                    ("status", field(event, "status").clone()),
                    ("toolCallId", field(event, "toolCallId").clone()),
                    ("toolName", field(event, "toolName").clone()),
                    ("success", field(event, "success").clone()),
                    ("durationMs", field(event, "durationMs").clone()),
                ]),
            ),
        ])],
        "retry_progress" => {
            let detail = field(event, "detail");
            let attempt = required(detail, "attempt")?;
            vec![object([
                ("type", text("agent_status")),
                ("event", text("retry_progress")),
                (
                    "detail",
                    object([
                        ("attempt", attempt.clone()),
                        ("maxAttempts", field(detail, "maxAttempts").clone()),
                        ("delayMs", field(detail, "delayMs").clone()),
                        ("reason", field(detail, "reason").clone()),
                        ("provider", field(detail, "provider").clone()),
                        ("model", field(detail, "model").clone()),
                    ]),
                ),
            ])]
        }
        "session_ended"
        | "user_prompt_submitted"
        | "setup_completed"
        | "instructions_loaded"
        | "stop_requested"
        | "stop_failure"
        | "elicitation_resolved"
        | "pre_tool_execute"
        | "post_tool_execute"
        | "permission_requested"
        | "elicitation_requested" => Vec::new(),
        "permission_denied" => vec![object([
            ("type", text("permission_denied")),
            ("toolName", field(event, "toolName").clone()),
            ("reason", field(event, "reason").clone()),
        ])],
        _ => Vec::new(),
    })
}

/// `withGatewayRunId` and the `timeline`/`streamBoundary` spread in `project`.
fn finish_frame(mut frame: JsValue, event: &JsValue, run_id: &str) -> JsValue {
    let attachment = frame
        .get("type")
        .is_some_and(|kind| is_type(kind, "assistant_attachment"));
    if !attachment && truthy(field(event, "timeline")) {
        insert(&mut frame, "timeline", field(event, "timeline").clone());
    }
    if truthy(field(event, "streamBoundary")) {
        insert(
            &mut frame,
            "streamBoundary",
            field(event, "streamBoundary").clone(),
        );
    }
    let has_run_id = frame.get("runId").is_some_and(|value| {
        value
            .as_string()
            .is_some_and(|value| !js_trim(value.units()).is_empty())
    });
    if !has_run_id {
        insert(&mut frame, "runId", text(run_id));
    }
    frame
}

/// `event.message.content.find(block => block.timeline)?.timeline`.
fn first_block_timeline(content: &JsValue) -> ProjectionResult<Option<JsValue>> {
    Ok(required_array(content, "find")?
        .iter()
        .find(|block| truthy(field(block, "timeline")))
        .map(|block| field(block, "timeline").clone()))
}

fn assistant_message_frames(message: &JsValue) -> ProjectionResult<Vec<JsValue>> {
    let model = field(field(message, "metadata"), "model");
    let content = required_array(required(message, "content")?, "flatMap")?;
    let mut frames = Vec::new();
    for block in content {
        let block_type = field(block, "type");
        let block_id = field(block, "blockId");
        let timeline = field(block, "timeline");
        let is_closed_block = (is_type(block_type, "text") || is_type(block_type, "thinking"))
            && truthy(block_id)
            && truthy(timeline);
        if is_closed_block {
            frames.push(object([
                ("type", text("assistant_block")),
                ("kind", block_type.clone()),
                ("blockId", block_id.clone()),
                ("text", field(block, "text").clone()),
                ("timeline", timeline.clone()),
                ("streamState", text("closed")),
                ("model", model.clone()),
            ]));
        } else if is_type(block_type, "text") {
            let mut frame = JsObject::new();
            frame.insert(JsString::from("type"), text("assistant_text_delta"));
            frame.insert(JsString::from("text"), field(block, "text").clone());
            if truthy(model) {
                frame.insert(JsString::from("model"), model.clone());
            }
            frames.push(JsValue::Object(frame));
        } else if is_type(block_type, "thinking") {
            frames.push(object([
                ("type", text("assistant_thinking_delta")),
                ("text", field(block, "text").clone()),
            ]));
        }
    }
    Ok(frames)
}

fn map_model_event(event: &JsValue, run_id: &str) -> ProjectionResult<Vec<JsValue>> {
    let kind = required(event, "type")?;
    if is_type(kind, "text_delta") {
        Ok(vec![object([
            ("type", text("assistant_text_delta")),
            ("text", field(event, "text").clone()),
            ("runId", text(run_id)),
        ])])
    } else if is_type(kind, "thinking_delta") {
        Ok(vec![object([
            ("type", text("assistant_thinking_delta")),
            ("text", field(event, "text").clone()),
            ("runId", text(run_id)),
        ])])
    } else {
        Ok(Vec::new())
    }
}

fn preview_unknown(value: &JsValue) -> ProjectionResult<JsValue> {
    if value.is_undefined() {
        return Ok(JsValue::Undefined);
    }
    match json_stringify(value) {
        Ok(Some(text)) => Ok(JsValue::from(text.as_str())),
        // `catch { return String(value) }`; a deep array throws there too.
        Ok(None) | Err(_) => checked_js_text(value),
    }
}

// ---------------------------------------------------------------------------
// Tool results
// ---------------------------------------------------------------------------

fn tool_result_frames(
    event: &JsValue,
    store: &dyn GatewayToolResultArtifactStorePort,
) -> ProjectionResult<Vec<JsValue>> {
    let result = field(event, "result");
    let content = required_array(required(result, "content")?, "map")?;
    let full_text = join_units(
        &content
            .iter()
            .map(tool_result_content_text)
            .collect::<ProjectionResult<Vec<_>>>()?,
        u16::from(b'\n'),
    );
    let result_preview = limit_tool_result_preview(&full_text);
    let result_line_count = line_count(&full_text);
    let result_bytes = utf8_len(&full_text);
    let result_path = store.persist(&GatewayToolResultArtifactInput {
        session_id: string_as_js(field(event, "sessionId"))?,
        turn_id: string_as_js(field(event, "turnId"))?,
        tool_call_id: string_as_js(field(result, "toolCallId"))?,
        text: JsString::from_utf16(full_text.clone()),
    });
    let tool_name = field(result, "toolName");

    let mut images = Vec::new();
    let mut attachments = Vec::new();
    for item in content {
        if is_type(field(item, "type"), "image") {
            let mut image = JsObject::new();
            image.insert(JsString::from("mimeType"), field(item, "mimeType").clone());
            image.insert(JsString::from("data"), field(item, "data").clone());
            if !field(item, "bytes").is_undefined() {
                image.insert(JsString::from("bytes"), field(item, "bytes").clone());
            }
            if truthy(field(item, "detail")) {
                image.insert(JsString::from("detail"), field(item, "detail").clone());
            }
            images.push(JsValue::Object(image));
            if !is_type(tool_name, "read_file") {
                attachments.push(object([
                    ("type", text("assistant_attachment")),
                    (
                        "attachment",
                        object([
                            ("type", text("image")),
                            ("mimeType", field(item, "mimeType").clone()),
                            ("content", field(item, "data").clone()),
                            ("bytes", field(item, "bytes").clone()),
                            (
                                "name",
                                text(format!(
                                    "{}-{}.{}",
                                    safe_path_part_js(tool_name)?,
                                    safe_path_part_js(field(result, "toolCallId"))?,
                                    extension_for_mime(field(item, "mimeType"))?,
                                )),
                            ),
                            ("source", text("tool_result")),
                            (
                                "metadata",
                                object([
                                    ("toolCallId", field(result, "toolCallId").clone()),
                                    ("toolName", tool_name.clone()),
                                ]),
                            ),
                        ]),
                    ),
                ]));
            }
        } else if is_type(field(item, "type"), "file") {
            attachments.push(object([
                ("type", text("assistant_attachment")),
                (
                    "attachment",
                    object([
                        ("type", text("file")),
                        ("path", field(item, "path").clone()),
                        ("mimeType", field(item, "mimeType").clone()),
                        (
                            "name",
                            JsValue::String(last_path_segment_js(field(item, "path"))?),
                        ),
                        ("source", text("tool_result")),
                        (
                            "metadata",
                            object([
                                ("toolCallId", field(result, "toolCallId").clone()),
                                ("toolName", tool_name.clone()),
                                ("description", field(item, "description").clone()),
                            ]),
                        ),
                    ]),
                ),
            ]));
        }
    }

    let mut finished = JsObject::new();
    finished.insert(JsString::from("type"), text("tool_call_finished"));
    finished.insert(
        JsString::from("toolCallId"),
        field(result, "toolCallId").clone(),
    );
    finished.insert(
        JsString::from("ok"),
        boolean(is_type(field(result, "type"), "success")),
    );
    finished.insert(
        JsString::from("resultPreview"),
        JsValue::String(result_preview),
    );
    finished.insert(
        JsString::from("resultLineCount"),
        number(result_line_count as f64),
    );
    finished.insert(JsString::from("resultBytes"), number(result_bytes as f64));
    finished.insert(JsString::from("toolName"), tool_name.clone());
    finished.insert(
        JsString::from("resultPath"),
        result_path.map_or(JsValue::Undefined, |path| JsValue::from(path.as_str())),
    );
    if !images.is_empty() {
        finished.insert(JsString::from("images"), JsValue::Array(images));
    }
    if is_type(field(result, "type"), "error") {
        finished.insert(
            JsString::from("errorCode"),
            required(field(result, "error"), "code")?.clone(),
        );
    }
    if is_type(field(result, "type"), "success") && truthy(field(result, "data")) {
        finished.insert(
            JsString::from("data"),
            sanitize_tool_data(field(result, "data"))?,
        );
    }
    let mut frames = vec![JsValue::Object(finished)];
    frames.extend(attachments);
    Ok(frames)
}

fn string_as_js(value: &JsValue) -> ProjectionResult<JsString> {
    match value {
        JsValue::String(value) => Ok(value.clone()),
        _ => Ok(JsString::from_utf16(checked_string_units(value)?)),
    }
}

fn join_units(parts: &[Vec<u16>], separator: u16) -> Vec<u16> {
    let mut out = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(separator);
        }
        out.extend_from_slice(part);
    }
    out
}

fn line_count(units: &[u16]) -> usize {
    units
        .iter()
        .filter(|unit| **unit == u16::from(b'\n'))
        .count()
        + 1
}

fn first_lines(units: &[u16], count: usize) -> Vec<u16> {
    let mut seen = 0usize;
    for (index, unit) in units.iter().enumerate() {
        if *unit == u16::from(b'\n') {
            seen += 1;
            if seen == count {
                return units[..index].to_vec();
            }
        }
    }
    units.to_vec()
}

fn limit_tool_result_preview(units: &[u16]) -> JsString {
    if units.len() <= MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS {
        return JsString::from_utf16(units.to_vec());
    }
    let marker = format!(
        "\n\n... [Gateway preview truncated: {} characters omitted; full result remains available through persisted tool-result references when shown to the model.] ...\n\n",
        units.len() - MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS
    );
    let available =
        MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS.saturating_sub(marker.encode_utf16().count());
    let mut out = slice_utf16(units, 0, available.div_ceil(2));
    out.extend(marker.encode_utf16());
    out.extend(slice_from_end(units, available / 2));
    JsString::from_utf16(out)
}

fn head_tail_string(units: &[u16], max_chars: usize, label: &str) -> JsString {
    if units.len() <= max_chars {
        return JsString::from_utf16(units.to_vec());
    }
    let marker = format!(
        "\n\n... [{label}: {} characters omitted] ...\n\n",
        units.len() - max_chars
    );
    let available = max_chars.saturating_sub(marker.encode_utf16().count());
    let mut out = slice_utf16(units, 0, available.div_ceil(2));
    out.extend(marker.encode_utf16());
    out.extend(slice_from_end(units, available / 2));
    JsString::from_utf16(out)
}

fn push_str(out: &mut Vec<u16>, value: &str) {
    out.extend(value.encode_utf16());
}

fn push_units(out: &mut Vec<u16>, value: &JsValue) -> ProjectionResult<()> {
    out.extend(checked_string_units(value)?);
    Ok(())
}

/// `contentToText` (PilotDeckToolResultContent). Built from UTF-16 units so
/// lone surrogates in string fields survive template interpolation.
fn tool_result_content_text(item: &JsValue) -> ProjectionResult<Vec<u16>> {
    let kind = required(item, "type")?;
    let mut out = Vec::new();
    if is_type(kind, "text") {
        out.extend(checked_string_units(field(item, "text"))?);
    } else if is_type(kind, "json") {
        // `JSON.stringify(undefined)` is undefined and `Array.prototype.join`
        // renders that element as the empty string; a deep value throws.
        match json_stringify(field(item, "value")) {
            Ok(Some(text)) => out.extend(text.encode_utf16()),
            Ok(None) => {}
            Err(_) => {
                return Err(ProjectionError::range_error(
                    "Maximum call stack size exceeded",
                ));
            }
        }
    } else if is_type(kind, "image") {
        push_str(&mut out, "[Image: ");
        push_units(&mut out, field(item, "mimeType"))?;
        push_str(&mut out, ", ");
        // TS reads `content.data.length`; that throws when the field is absent.
        push_str(&mut out, &js_length(field(item, "data"))?.to_string());
        push_str(&mut out, " base64 characters]");
    } else if is_type(kind, "pdf") {
        push_str(&mut out, "[PDF: ");
        push_units(&mut out, field(item, "mimeType"))?;
        push_str(&mut out, ", ");
        push_str(&mut out, &js_length(field(item, "data"))?.to_string());
        push_str(&mut out, " base64 characters");
        if truthy(field(item, "pages")) {
            push_str(&mut out, ", ");
            push_units(&mut out, field(item, "pages"))?;
            push_str(&mut out, " pages");
        }
        push_str(&mut out, "]");
    } else if is_type(kind, "file") {
        push_str(&mut out, "[File: ");
        push_units(&mut out, field(item, "path"))?;
        if truthy(field(item, "mimeType")) {
            push_str(&mut out, ", ");
            push_units(&mut out, field(item, "mimeType"))?;
        }
        if truthy(field(item, "description")) {
            push_str(&mut out, ", ");
            push_units(&mut out, field(item, "description"))?;
        }
        push_str(&mut out, "]");
    }
    Ok(out)
}

/// Canonical `toolResultContentBlockToText`.
fn canonical_content_text(block: &JsValue) -> ProjectionResult<Vec<u16>> {
    let kind = required(block, "type")?;
    let mut out = Vec::new();
    if is_type(kind, "text") {
        out.extend(checked_string_units(field(block, "text"))?);
    } else if is_type(kind, "image") {
        push_str(&mut out, "[Image: ");
        push_units(&mut out, field(block, "mimeType"))?;
        push_str(&mut out, ", ");
        if is_type(field(block, "source"), "url") {
            push_units(&mut out, field(block, "data"))?;
        } else {
            push_str(&mut out, &js_length(field(block, "data"))?.to_string());
            push_str(&mut out, " base64 characters");
        }
        push_str(&mut out, "]");
    } else if is_type(kind, "pdf") {
        push_str(&mut out, "[PDF: ");
        push_units(&mut out, field(block, "mimeType"))?;
        push_str(&mut out, ", ");
        push_str(&mut out, &js_length(field(block, "data"))?.to_string());
        push_str(&mut out, " base64 characters");
        if truthy(field(block, "pages")) {
            push_str(&mut out, ", ");
            push_units(&mut out, field(block, "pages"))?;
            push_str(&mut out, " pages");
        }
        push_str(&mut out, "]");
    }
    Ok(out)
}

fn flatten_tool_result_block_text(block: &JsValue) -> ProjectionResult<Vec<u16>> {
    let content = required_array(required(block, "content")?, "map")?;
    Ok(join_units(
        &content
            .iter()
            .map(canonical_content_text)
            .collect::<ProjectionResult<Vec<_>>>()?,
        u16::from(b'\n'),
    ))
}

fn sanitize_tool_data(value: &JsValue) -> ProjectionResult<JsValue> {
    let sanitized = sanitize_tool_data_value(value)?;
    if sanitized.as_object().is_some() {
        Ok(sanitized)
    } else {
        Ok(object([("value", sanitized)]))
    }
}

/// `sanitizeGatewayToolDataValue` with an explicit stack. V8 recurses and
/// throws `RangeError` past the shared container-depth bound; Rust must not
/// exhaust the native stack (the debug test thread dies well below 2,000
/// recursive frames), so the tree is rebuilt iteratively.
///
/// The bound is the repository's conservative `MAX_ARRAY_DEPTH`: V8's real
/// limit depends on the current stack, so depths just past the bound throw
/// here but can still succeed in TS (measured divergence recorded in RESULT).
fn sanitize_tool_data_value(value: &JsValue) -> ProjectionResult<JsValue> {
    enum Frame<'a> {
        Array {
            items: &'a [JsValue],
            next: usize,
            output: Vec<JsValue>,
            depth: usize,
        },
        Object {
            entries: &'a [(JsString, JsValue)],
            next: usize,
            output: JsObject,
            depth: usize,
        },
    }

    let mut frames: Vec<Frame<'_>> = Vec::new();
    let mut current: Option<(&JsValue, usize)> = Some((value, 0usize));
    let mut completed: Option<JsValue> = None;
    loop {
        if let Some((value, depth)) = current.take() {
            if depth > MAX_ARRAY_DEPTH {
                // Same observable failure as V8's recursive helper.
                return Err(ProjectionError::range_error(
                    "Maximum call stack size exceeded",
                ));
            }
            match value {
                JsValue::String(value) => completed = Some(limit_tool_data_string(value)),
                JsValue::Array(items) => {
                    if items.is_empty() {
                        completed = Some(JsValue::Array(Vec::new()));
                    } else {
                        frames.push(Frame::Array {
                            items,
                            next: 1,
                            output: Vec::new(),
                            depth,
                        });
                        current = Some((&items[0], depth + 1));
                        continue;
                    }
                }
                JsValue::Object(object) => {
                    let entries = object.entries();
                    // `output[key] = ...` with key `__proto__` hits the setter
                    // and never becomes an own enumerable property, so TS
                    // omits it (top level and nested alike).
                    let mut next = 0usize;
                    while next < entries.len() && entries[next].0.equals_str("__proto__") {
                        next += 1;
                    }
                    if next == entries.len() {
                        completed = Some(JsValue::Object(JsObject::new()));
                    } else {
                        let child = &entries[next].1;
                        frames.push(Frame::Object {
                            entries,
                            next: next + 1,
                            output: JsObject::new(),
                            depth,
                        });
                        current = Some((child, depth + 1));
                        continue;
                    }
                }
                _ => completed = Some(value.clone()),
            }
        }

        let frame_value = completed.take().expect("sanitize frame value");
        let Some(frame) = frames.last_mut() else {
            return Ok(frame_value);
        };
        let done = match frame {
            Frame::Array {
                items,
                next,
                output,
                depth,
            } => {
                output.push(frame_value);
                if *next < items.len() {
                    let child = &items[*next];
                    *next += 1;
                    current = Some((child, *depth + 1));
                    false
                } else {
                    true
                }
            }
            Frame::Object {
                entries,
                next,
                output,
                depth,
            } => {
                let key = entries[*next - 1].0.clone();
                output.insert(key, frame_value);
                let mut next_wanted = *next;
                while next_wanted < entries.len() && entries[next_wanted].0.equals_str("__proto__")
                {
                    next_wanted += 1;
                }
                if next_wanted < entries.len() {
                    let child = &entries[next_wanted].1;
                    *next = next_wanted + 1;
                    current = Some((child, *depth + 1));
                    false
                } else {
                    true
                }
            }
        };
        if done {
            let frame = frames.pop().expect("sanitize frame");
            completed = Some(match frame {
                Frame::Array { output, .. } => JsValue::Array(output),
                Frame::Object { output, .. } => JsValue::Object(output),
            });
        }
    }
}

fn limit_tool_data_string(value: &JsString) -> JsValue {
    if value.len_utf16() <= MAX_GATEWAY_TOOL_DATA_STRING_CHARS {
        return JsValue::String(value.clone());
    }
    let units = value.units();
    object([
        (
            "preview",
            JsValue::String(head_tail_string(
                units,
                MAX_GATEWAY_TOOL_DATA_STRING_CHARS,
                "Gateway data string truncated",
            )),
        ),
        ("originalChars", number(units.len() as f64)),
        ("originalBytes", number(utf8_len(units) as f64)),
        ("truncated", boolean(true)),
    ])
}

// ---------------------------------------------------------------------------
// Tool result projections
// ---------------------------------------------------------------------------

fn tool_results_projected_frames(message: &JsValue) -> ProjectionResult<Vec<JsValue>> {
    let mut frames = Vec::new();
    let content = required_iterable(required(message, "content")?, "event.message.content")?;
    for block in content {
        let kind = required(block, "type")?;
        if is_type(kind, "tool_result_reference") {
            frames.push(object([
                ("type", text("tool_result_detail_available")),
                ("toolCallId", field(block, "toolCallId").clone()),
                ("resultPath", field(block, "path").clone()),
            ]));
        } else if is_type(kind, "media_reference") && truthy(field(block, "toolCallId")) {
            frames.push(object([
                ("type", text("tool_result_detail_available")),
                ("toolCallId", field(block, "toolCallId").clone()),
                ("resultPath", field(block, "path").clone()),
            ]));
            if is_type(field(block, "reason"), "media_result_too_large") {
                continue;
            }
            frames.push(object([
                ("type", text("assistant_attachment")),
                (
                    "attachment",
                    object([
                        (
                            "type",
                            if is_type(field(block, "mediaType"), "image") {
                                text("image")
                            } else {
                                text("file")
                            },
                        ),
                        ("path", field(block, "path").clone()),
                        ("mimeType", field(block, "mimeType").clone()),
                        ("bytes", field(block, "originalBytes").clone()),
                        (
                            "name",
                            JsValue::String(last_path_segment_js(field(block, "path"))?),
                        ),
                        ("source", text("media_reference")),
                        (
                            "metadata",
                            object([
                                ("toolCallId", field(block, "toolCallId").clone()),
                                ("reason", field(block, "reason").clone()),
                            ]),
                        ),
                    ]),
                ),
            ]));
        } else if is_type(kind, "tool_result") {
            frames.push(object([
                ("type", text("tool_result_detail_available")),
                ("toolCallId", field(block, "toolCallId").clone()),
                (
                    "fullText",
                    JsValue::String(JsString::from_utf16(flatten_tool_result_block_text(block)?)),
                ),
            ]));
        }
    }
    Ok(frames)
}

// ---------------------------------------------------------------------------
// Turn / budget / subagent projections
// ---------------------------------------------------------------------------

fn turn_completed_frames(result: &JsValue) -> ProjectionResult<Vec<JsValue>> {
    let mut frames = Vec::new();
    let structured_output = required(result, "structuredOutput")?;
    if !structured_output.is_undefined() {
        frames.push(object([
            ("type", text("structured_output")),
            ("payload", structured_output.clone()),
        ]));
    }
    frames.push(object([
        ("type", text("turn_completed")),
        ("usage", field(result, "usage").clone()),
        ("finishReason", field(result, "stopReason").clone()),
    ]));
    Ok(frames)
}

fn provider_error_from_agent_error(error: &JsValue) -> ProjectionResult<JsValue> {
    let details = field(error, "details");
    if details.as_object().is_none() {
        return Ok(JsValue::Undefined);
    }
    provider_error_from_record(details)
}

fn provider_error_from_model_error(error: &JsValue) -> ProjectionResult<JsValue> {
    Ok(object([
        ("provider", field(error, "provider").clone()),
        ("protocol", field(error, "protocol").clone()),
        ("status", field(error, "status").clone()),
        ("code", field(error, "code").clone()),
        ("message", field(error, "message").clone()),
        ("raw", stringify_provider_raw(field(error, "raw"))?),
    ]))
}

fn provider_error_from_record(details: &JsValue) -> ProjectionResult<JsValue> {
    let provider = string_or_undefined(field(details, "provider"));
    let protocol = string_or_undefined(field(details, "protocol"));
    let status = number_or_undefined(field(details, "status"));
    let code = string_or_undefined(field(details, "code"));
    let message = string_or_undefined(field(details, "message"));
    let raw = stringify_provider_raw(field(details, "raw"))?;
    if !truthy(&provider)
        && !truthy(&protocol)
        && status.is_undefined()
        && !truthy(&code)
        && !truthy(&message)
        && !truthy(&raw)
    {
        return Ok(JsValue::Undefined);
    }
    Ok(object([
        ("provider", provider),
        ("protocol", protocol),
        ("status", status),
        ("code", code),
        ("message", message),
        ("raw", raw),
    ]))
}

fn string_or_undefined(value: &JsValue) -> JsValue {
    if value.as_string().is_some_and(|value| !value.is_empty()) {
        value.clone()
    } else {
        JsValue::Undefined
    }
}

fn number_or_undefined(value: &JsValue) -> JsValue {
    if value.as_number().is_some_and(f64::is_finite) {
        value.clone()
    } else {
        JsValue::Undefined
    }
}

fn stringify_provider_raw(raw: &JsValue) -> ProjectionResult<JsValue> {
    if nullish(raw) {
        return Ok(JsValue::Undefined);
    }
    let units = if raw.as_string().is_some() {
        checked_string_units(raw)?
    } else {
        match json_stringify(raw) {
            Ok(Some(text)) => text.encode_utf16().collect::<Vec<_>>(),
            // `safeJsonStringify`'s `catch { return String(value) }`.
            Ok(None) | Err(_) => checked_string_units(raw)?,
        }
    };
    if units.is_empty() {
        return Ok(JsValue::Undefined);
    }
    if units.len() > 1_200 {
        let mut truncated = slice_utf16(&units, 0, 1_200);
        truncated.push(0x2026);
        return Ok(JsValue::String(JsString::from_utf16(truncated)));
    }
    Ok(JsValue::String(JsString::from_utf16(units)))
}

fn nullish_or<'a>(value: &'a JsValue, fallback: &'a JsValue) -> &'a JsValue {
    if nullish(value) { fallback } else { value }
}

/// JS `+` for the numeric snapshot fields (string operands concatenate).
fn js_add(left: &JsValue, right: &JsValue) -> ProjectionResult<JsValue> {
    if left.as_string().is_some() || right.as_string().is_some() {
        let mut units = checked_string_units(left)?;
        units.extend(checked_string_units(right)?);
        return Ok(JsValue::String(JsString::from_utf16(units)));
    }
    Ok(JsValue::Number(js_number(left) + js_number(right)))
}

fn js_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Number(value) => *value,
        JsValue::Null => 0.0,
        JsValue::Bool(value) => f64::from(u8::from(*value)),
        JsValue::Undefined => f64::NAN,
        _ => f64::NAN,
    }
}

fn context_budget_frames(snapshot: &JsValue) -> ProjectionResult<Vec<JsValue>> {
    let reserved = nullish_or(
        required(snapshot, "reservedOutputTokens")?,
        field(snapshot, "maxOutputTokens"),
    );
    let reserved = if nullish(reserved) {
        JsValue::Number(0.0)
    } else {
        reserved.clone()
    };
    let explicit_total = field(snapshot, "totalContextTokens");
    // `!== undefined` (not nullish) selects the effective-context branch.
    let total = if !field(snapshot, "effectiveContextTokens").is_undefined() {
        if nullish(explicit_total) {
            js_add(field(snapshot, "effectiveContextTokens"), &reserved)?
        } else {
            explicit_total.clone()
        }
    } else if nullish(explicit_total) {
        js_add(field(snapshot, "maxContextTokens"), &reserved)?
    } else {
        explicit_total.clone()
    };

    let mut frame = JsObject::new();
    frame.insert(JsString::from("type"), text("context_budget"));
    frame.insert(JsString::from("used"), field(snapshot, "tokens").clone());
    frame.insert(
        JsString::from("displayUsed"),
        field(snapshot, "tokens").clone(),
    );
    for name in [
        "localEstimateTokens",
        "displayTokens",
        "estimateSource",
        "usageTokens",
        "calibrationActualInputTokens",
        "calibrationEstimatedInputTokens",
    ] {
        let value = field(snapshot, name);
        if !value.is_undefined() {
            frame.insert(JsString::from(name), value.clone());
        }
    }
    frame.insert(JsString::from("total"), total);
    if !explicit_total.is_undefined() {
        frame.insert(JsString::from("totalContextTokens"), explicit_total.clone());
    }
    frame.insert(
        JsString::from("maxContextTokens"),
        field(snapshot, "maxContextTokens").clone(),
    );
    let effective = field(snapshot, "effectiveContextTokens");
    frame.insert(
        JsString::from("effectiveTotal"),
        if nullish(effective) {
            field(snapshot, "maxContextTokens").clone()
        } else {
            effective.clone()
        },
    );
    if !effective.is_undefined() {
        frame.insert(JsString::from("effectiveContextTokens"), effective.clone());
    }
    if !field(snapshot, "maxOutputTokens").is_undefined() {
        frame.insert(
            JsString::from("maxOutputTokens"),
            field(snapshot, "maxOutputTokens").clone(),
        );
    }
    frame.insert(JsString::from("reservedOutputTokens"), reserved.clone());
    for name in ["warningRatio", "blockingRatio", "ratio", "state"] {
        frame.insert(JsString::from(name), field(snapshot, name).clone());
    }
    for name in ["source", "exact", "estimatorError", "breakdown"] {
        let value = field(snapshot, name);
        if !value.is_undefined() {
            frame.insert(JsString::from(name), value.clone());
        }
    }
    Ok(vec![JsValue::Object(frame)])
}

fn map_subagent_model_event(
    event: &JsValue,
    forward_subagent_text: bool,
) -> ProjectionResult<Vec<JsValue>> {
    let model_event = field(event, "event");
    let kind = required(model_event, "type")?;
    let subagent_id = field(event, "subagentId").clone();
    let subagent_type = field(event, "subagentType").clone();
    if is_type(kind, "text_delta") {
        if forward_subagent_text {
            Ok(vec![object([
                ("type", text("subagent_text_delta")),
                ("subagentId", subagent_id),
                ("subagentType", subagent_type),
                ("text", field(model_event, "text").clone()),
            ])])
        } else {
            Ok(vec![object([
                ("type", text("agent_status")),
                ("event", text("subagent_text_delta")),
                (
                    "detail",
                    object([
                        ("subagentId", subagent_id),
                        ("subagentType", subagent_type),
                        ("text", field(model_event, "text").clone()),
                    ]),
                ),
            ])])
        }
    } else if is_type(kind, "thinking_delta") {
        Ok(vec![object([
            ("type", text("agent_status")),
            ("event", text("subagent_thinking_delta")),
            (
                "detail",
                object([
                    ("subagentId", subagent_id),
                    ("subagentType", subagent_type),
                    ("text", field(model_event, "text").clone()),
                ]),
            ),
        ])])
    } else if is_type(kind, "error") {
        let error = field(model_event, "error");
        let code = required(error, "code")?;
        Ok(vec![object([
            ("type", text("agent_status")),
            ("event", text("subagent_model_error")),
            (
                "detail",
                object([
                    ("subagentId", subagent_id),
                    ("subagentType", subagent_type),
                    ("code", code.clone()),
                    ("message", field(error, "message").clone()),
                ]),
            ),
        ])])
    } else {
        Ok(Vec::new())
    }
}

fn subagent_tool_result_frames(event: &JsValue) -> ProjectionResult<Vec<JsValue>> {
    let result = field(event, "result");
    let content = required_array(required(result, "content")?, "map")?;
    let full_text = join_units(
        &content
            .iter()
            .map(tool_result_content_text)
            .collect::<ProjectionResult<Vec<_>>>()?,
        u16::from(b'\n'),
    );
    let result_preview = limit_tool_result_preview(&full_text);
    let preview = limit_tool_result_preview(&first_lines(&full_text, 3));
    let mut detail = JsObject::new();
    detail.insert(
        JsString::from("subagentId"),
        field(event, "subagentId").clone(),
    );
    detail.insert(
        JsString::from("subagentType"),
        field(event, "subagentType").clone(),
    );
    detail.insert(
        JsString::from("toolCallId"),
        field(result, "toolCallId").clone(),
    );
    detail.insert(
        JsString::from("toolName"),
        field(result, "toolName").clone(),
    );
    detail.insert(
        JsString::from("ok"),
        boolean(is_type(field(result, "type"), "success")),
    );
    detail.insert(JsString::from("content"), JsValue::String(result_preview));
    detail.insert(JsString::from("preview"), JsValue::String(preview));
    detail.insert(
        JsString::from("resultLineCount"),
        number(line_count(&full_text) as f64),
    );
    detail.insert(
        JsString::from("resultBytes"),
        number(utf8_len(&full_text) as f64),
    );
    if is_type(field(result, "type"), "error") {
        detail.insert(
            JsString::from("errorCode"),
            required(field(result, "error"), "code")?.clone(),
        );
    }
    Ok(vec![object([
        ("type", text("agent_status")),
        ("event", text("subagent_tool_result")),
        ("detail", JsValue::Object(detail)),
    ])])
}

#[cfg(test)]
#[path = "projector_tests.rs"]
mod tests;
