//! `rewind_files` SDK operation (baseline `cli/ProjectRuntimeRegistry.ts:1125-1182`).
//!
//! The transcript is the only durable index: every `file_snapshot_recorded`
//! entry becomes one snapshot in the already-merged
//! [`FileHistoryStore`](crate::storage::file_history::FileHistoryStore), then the
//! same replay / conflict / diff / rewind sequence as TS runs. The store owns
//! the file effects; this module only extracts the snapshot entries and shapes
//! the result. Nothing here is wired into the WS dispatch (A8 owns that entry).
//!
//! TS reads the transcript through the persistent session storage
//! (`readAgentProjectSessionTranscript`), so the adapter is a parameter: the
//! project key -> root resolution and runtime lease are A6.3's job.
use crate::storage::{
    AdapterError, AsyncTranscriptAdapter, LedgerError, SessionStorageInput,
    file_history::{
        ExpectedFileState, FileHistoryClock, FileHistoryOptions, FileHistoryStore, RecordedBackup,
        SnapshotRecordedEntry,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::parse_node22_date;
use std::path::Path;

/// Returned when the transcript has no `file_snapshot_recorded` entry at all.
pub const NO_CHECKPOINT: &str = "No persisted file checkpoint is available for this session.";

pub struct RewindFilesInput<'a> {
    pub adapter: &'a AsyncTranscriptAdapter,
    /// The resolved `projectKey` runtime root (`this.resolve`).
    pub project_root: &'a Path,
    pub pilot_home: &'a Path,
    pub session_key: &'a JsString,
    pub user_message_id: &'a str,
    pub dry_run: bool,
    /// `this.options.now`, used only to build the store (replay never calls it).
    pub now: Option<FileHistoryClock>,
}

/// Runs one `rewind_files` request. Every error path is the TS `catch` branch:
/// `{ canRewind: false, error }` with the original `Error.message` text.
pub fn rewind_files(input: RewindFilesInput<'_>) -> JsValue {
    match run(input) {
        Ok(result) => JsValue::Object(result),
        Err(error) => JsValue::Object(failure(&error)),
    }
}

fn run(input: RewindFilesInput<'_>) -> Result<JsObject, String> {
    let storage = input.adapter.create_session_storage(SessionStorageInput {
        project_root: input.project_root.to_path_buf(),
        pilot_home: input.pilot_home.to_path_buf(),
        session_id: input.session_key.clone(),
        now: None,
    });
    let read = storage
        .read_transcript(None)
        .map_err(|error| storage_error(error, &storage.transcript_path))?;
    let snapshots = snapshot_entries(&read.entries)?;
    if snapshots.is_empty() {
        return Ok(failure(NO_CHECKPOINT));
    }
    let mut options = FileHistoryOptions::new(storage.file_history_dir.clone());
    options.now = input.now.clone();
    let history = FileHistoryStore::new(options);
    history.replay_from_transcript(&snapshots);

    let conflicts = history
        .get_conflict_paths(input.user_message_id)
        .map_err(|error| error.to_string())?;
    if !conflicts.is_empty() {
        return Ok(conflict(&conflicts));
    }
    let stats = history
        .get_diff_stats(input.user_message_id)
        .map_err(|error| error.to_string())?;
    if input.dry_run {
        let mut result = JsObject::new();
        result.insert("canRewind".into(), JsValue::Bool(true));
        result.insert("insertions".into(), number(stats.insertions));
        result.insert("deletions".into(), number(stats.deletions));
        return Ok(result);
    }
    let rewind = history
        .rewind(input.user_message_id)
        .map_err(|error| error.to_string())?;
    if !rewind.conflicts.is_empty() {
        return Ok(conflict(&rewind.conflicts));
    }
    let mut result = JsObject::new();
    result.insert("canRewind".into(), JsValue::Bool(true));
    result.insert("filesChanged".into(), strings(rewind.files_changed));
    result.insert("insertions".into(), number(stats.insertions));
    result.insert("deletions".into(), number(stats.deletions));
    if !rewind.missing.is_empty() {
        result.insert("missing".into(), strings(rewind.missing));
    }
    Ok(result)
}

fn failure(message: &str) -> JsObject {
    let mut result = JsObject::new();
    result.insert("canRewind".into(), JsValue::Bool(false));
    result.insert("error".into(), JsValue::String(message.into()));
    result
}

/// TS `readAgentProjectSessionTranscript` reads the JSONL with `fs.readFile`,
/// so a transcript I/O failure carries Node's `readFile` message. A Rust
/// Display (`...(os error N)`) must not become the SDK error text.
fn storage_error(error: AdapterError, transcript_path: &Path) -> String {
    match error {
        AdapterError::Store(LedgerError::Io(io_error)) => {
            match crate::node_io::read_file(&io_error, &transcript_path.to_string_lossy()) {
                Some((_, message)) => message,
                None => strip_os_error(&io_error),
            }
        }
        other => other.to_string(),
    }
}

fn strip_os_error(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(index) if text.ends_with(')') => text[..index].to_string(),
        _ => text,
    }
}

fn conflict(paths: &[String]) -> JsObject {
    let mut result = JsObject::new();
    result.insert("canRewind".into(), JsValue::Bool(false));
    result.insert("conflicts".into(), strings(paths.to_vec()));
    result.insert(
        "error".into(),
        JsValue::String(
            format!(
                "Files changed outside PilotDeck since checkpoint: {}",
                paths.join(", ")
            )
            .into(),
        ),
    );
    result
}

fn number(value: usize) -> JsValue {
    JsValue::Number(value as f64)
}

fn strings(values: Vec<String>) -> JsValue {
    JsValue::Array(
        values
            .into_iter()
            .map(|value| JsValue::String(value.into()))
            .collect(),
    )
}

/// TS `snapshots.filter(...).map(...)`; property order comes from
/// `Object.entries`, i.e. the transcript JSON object's own key order.
fn snapshot_entries(entries: &[JsValue]) -> Result<Vec<SnapshotRecordedEntry>, String> {
    let mut snapshots = Vec::new();
    for entry in entries {
        if entry
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| text(kind) == "file_snapshot_recorded")
        {
            snapshots.push(snapshot_entry(entry)?);
        }
    }
    Ok(snapshots)
}

fn snapshot_entry(entry: &JsValue) -> Result<SnapshotRecordedEntry, String> {
    // TS `.map(entry => ({messageId: entry.messageId, ...}))` keeps
    // `undefined`; strict comparison in `findSnapshot` then never matches a
    // real id. `None` is that undefined, not an error.
    let message_id = string_field(entry, "messageId").map(text);
    // TS `replayFromTranscript` runs `Object.entries(entry.trackedFileBackups)`
    // unconditionally, so an absent (or null) object throws V8's TypeError and
    // the outer catch returns it as `error`. A present non-object value keeps
    // the TS behaviour of enumerating no own keys here.
    let backups = match entry.get("trackedFileBackups") {
        Some(JsValue::Null | JsValue::Undefined) | None => {
            return Err("Cannot convert undefined or null to object".to_string());
        }
        Some(backups) => backups.as_object(),
    };
    let mut tracked_file_backups = Vec::new();
    if let Some(backups) = backups {
        for (path, backup) in backups.own_entries() {
            tracked_file_backups.push((text(path), recorded_backup(backup)));
        }
    }
    let mut expected_file_states = Vec::new();
    if let Some(expected) = entry.get("expectedFileStates").and_then(JsValue::as_object) {
        for (path, state) in expected.own_entries() {
            expected_file_states.push((text(path), expected_state(state)));
        }
    }
    Ok(SnapshotRecordedEntry {
        message_id,
        tracked_file_backups,
        expected_file_states,
        // `rewindFilesForSdk` copies `entry.snapshotTimestamp` (not the
        // event-store `timestamp`), and the legacy field is usually absent, so
        // replay sees `new Date(0)` exactly like TS.
        timestamp_ms: entry
            .get("snapshotTimestamp")
            .and_then(JsValue::as_string)
            .and_then(epoch_ms),
    })
}

fn recorded_backup(backup: &JsValue) -> RecordedBackup {
    RecordedBackup {
        backup_file_name: match backup.get("backupFileName") {
            Some(JsValue::Null) | None => None,
            Some(value) => value.as_string().map(text),
        },
        version: backup
            .get("version")
            .and_then(JsValue::as_number)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map_or(0, |value| value as u64),
        backup_time_ms: backup
            .get("backupTime")
            .and_then(JsValue::as_string)
            .and_then(epoch_ms)
            .unwrap_or(0),
        mode: backup
            .get("mode")
            .and_then(JsValue::as_number)
            .map(|value| value as u32),
    }
}

fn expected_state(state: &JsValue) -> ExpectedFileState {
    if !state
        .get("exists")
        .and_then(JsValue::as_bool)
        .unwrap_or(false)
    {
        return ExpectedFileState::Missing;
    }
    ExpectedFileState::Present {
        sha256: string_field(state, "sha256").map(text).unwrap_or_default(),
        mode: state
            .get("mode")
            .and_then(JsValue::as_number)
            .map(|value| value as u32),
    }
}

fn string_field<'a>(value: &'a JsValue, name: &str) -> Option<&'a JsString> {
    value.get(name).and_then(JsValue::as_string)
}

/// UTF-16 to text. Property names and message ids are valid UTF-8 in every
/// transcript the TS writer can produce; replacement decoding keeps a corrupt
/// entry from aborting the request.
fn text(value: &JsString) -> String {
    value
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(value.units()))
}

/// `new Date(input).getTime()` through the ported Node 22 parser.
fn epoch_ms(value: &JsString) -> Option<i64> {
    match parse_node22_date(value) {
        Ok(Some(ms)) if ms.is_finite() => Some(ms as i64),
        _ => None,
    }
}
