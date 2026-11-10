//! Native application consumer for durable fork and last-turn replacement.
//! Disk names and journal fields follow the fixed TS Node replacement provider.
use crate::{
    metadata::object,
    runtime::{
        replacement::{ReplacementError, ReplacementResult, ReplacementStorage, StorageOperation},
        session::SessionOwner,
    },
    storage::{self, replacement_artifacts::valid_uuid, sanitize_session_id},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rustix::fs::{AtFlags, Mode, OFlags, linkat, mkdirat, openat, renameat, unlinkat};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
const MAX_BYTES: u64 = 50 * 1024 * 1024;
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn text(v: &JsValue, k: &str) -> String {
    field(v, k)
        .as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn string(s: impl Into<JsString>) -> JsValue {
    JsValue::String(s.into())
}
fn error(code: &'static str, message: impl Into<JsString>) -> ReplacementError {
    ReplacementError {
        code: Some(code.into()),
        message: message.into(),
        details: None,
    }
}
fn io(e: impl std::fmt::Display) -> ReplacementError {
    ReplacementError::new(e.to_string())
}
fn request_error(e: crate::runtime::connection::RequestError) -> ReplacementError {
    ReplacementError {
        code: Some(e.code),
        message: e.message,
        details: e.details,
    }
}
fn strict_id(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::String(a), JsValue::String(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Null, JsValue::Null) | (JsValue::Undefined, JsValue::Undefined) => true,
        _ => false,
    }
}
fn prefixed_value(prefix: &str, value: &JsValue) -> Result<JsString, ReplacementError> {
    let suffix =
        crate::js_value::property_key(value).map_err(|e| ReplacementError::new(e.message()))?;
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(suffix.units());
    Ok(JsString::from_utf16(units))
}
fn set(v: &mut JsValue, k: &str, value: JsValue) {
    if let JsValue::Object(o) = v {
        o.insert(k.into(), value);
    }
}
fn omit(v: &JsValue, keys: &[&str]) -> JsValue {
    let mut o = JsObject::new();
    if let JsValue::Object(src) = v {
        for (k, value) in src.entries() {
            if !keys.iter().any(|key| k.equals_str(key)) {
                o.insert(k.clone(), value.clone());
            }
        }
    }
    JsValue::Object(o)
}
fn merge(base: &JsValue, patch: &JsValue) -> JsValue {
    let mut out = base.clone();
    if !matches!(out, JsValue::Object(_)) {
        out = JsValue::Object(JsObject::new());
    }
    if let JsValue::Object(patch) = patch {
        for (k, v) in patch.entries() {
            if let JsValue::Object(o) = &mut out {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    out
}
fn body(entries: &[JsValue]) -> Result<Vec<u8>, ReplacementError> {
    let mut out = Vec::new();
    for e in entries {
        let line = stringify_json(e)
            .map_err(io)?
            .ok_or_else(|| io("Transcript entry is not serializable."))?;
        if out.len() + line.len() + 1 > MAX_BYTES as usize {
            return Err(io("Native transcript exceeds 50 MiB."));
        }
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Ok(out)
}
fn now() -> Result<String, ReplacementError> {
    storage::transcript_timestamp_for_history().map_err(io)
}
fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn parent(path: &Path) -> Result<(File, &std::ffi::OsStr), ReplacementError> {
    let directory = path.parent().ok_or_else(|| io("Artifact has no parent."))?;
    let fd = crate::project_files::open_directory_beneath(Path::new("/"), directory).map_err(io)?;
    Ok((
        fd,
        path.file_name()
            .ok_or_else(|| io("Artifact has no name."))?,
    ))
}
fn dirs(path: &Path) -> Result<(), ReplacementError> {
    let root = File::open("/").map_err(io)?;
    let relative = path.strip_prefix("/").map_err(io)?;
    crate::project_files::create_directories_at(&root, relative, Mode::from_raw_mode(0o700))
        .map_err(io)
}
fn new_dir(path: &Path) -> Result<(), ReplacementError> {
    let (fd, name) = parent(path)?;
    mkdirat(&fd, name, Mode::from_raw_mode(0o700)).map_err(io)
}
fn read_bytes(path: &Path) -> Result<Vec<u8>, ReplacementError> {
    read_open_artifact(crate::project_files::open_beneath(Path::new("/"), path).map_err(io)?)
}
fn require_current_transcript(path: &Path) -> Result<(), ReplacementError> {
    let file = crate::project_files::open_beneath(Path::new("/"), path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(io("Transcript artifact must be a regular file."));
    }
    Ok(())
}
fn read_open_artifact(f: File) -> Result<Vec<u8>, ReplacementError> {
    if !f.metadata().map_err(io)?.is_file() {
        return Err(io("Transcript artifact must be a regular file."));
    }
    let mut b = Vec::new();
    f.take(MAX_BYTES + 1).read_to_end(&mut b).map_err(io)?;
    if b.len() > MAX_BYTES as usize {
        return Err(io("Native transcript artifact exceeds 50 MiB."));
    }
    Ok(b)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ReplacementError> {
    write_new_mode(path, bytes, 0o600)
}
fn write_new_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), ReplacementError> {
    let (fd, name) = parent(path)?;
    let mut f = File::from(
        openat(
            &fd,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?,
    );
    let operation = (|| {
        f.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(io)?;
        f.write_all(bytes).map_err(io)?;
        f.sync_all().map_err(io)
    })();
    if operation.is_err() {
        let _ = unlinkat(&fd, name, AtFlags::empty());
    }
    operation
}
fn remove(path: &Path) -> Result<(), ReplacementError> {
    let directory = path.parent().ok_or_else(|| io("Artifact has no parent."))?;
    let fd = match crate::project_files::open_directory_beneath(Path::new("/"), directory) {
        Ok(fd) => fd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io("Artifact has no name."))?;
    match unlinkat(&fd, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(io(e)),
    }
}

fn remove_tree(path: &Path) -> Result<(), ReplacementError> {
    let (fd, name) = parent(path)?;
    crate::project_files::remove_tree_at(&fd, Path::new(name), path).map_err(io)
}
fn publish(path: &Path, bytes: &[u8], suffix: &str) -> Result<(), ReplacementError> {
    let (fd, name) = parent(path)?;
    let temp = format!(
        ".{}.{}.{}.tmp",
        path.file_stem().unwrap_or_default().to_string_lossy(),
        uuid(),
        suffix
    );
    let mut file = File::from(
        openat(
            &fd,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?,
    );
    let operation = (|| {
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        renameat(&fd, temp.as_str(), &fd, name).map_err(io)
    })();
    let _ = unlinkat(&fd, temp.as_str(), AtFlags::empty());
    operation
}

fn entries(path: &Path, reject_invalid: bool) -> Result<Vec<JsValue>, ReplacementError> {
    let read = storage::read_transcript_beneath(Path::new("/"), path)
        .map_err(io)?
        .read;
    if reject_invalid && read.diagnostics.iter().any(|d| d.severity == "error") {
        return Err(error(
            "replace_invalid_transcript",
            "The conversation transcript could not be safely rewritten.",
        ));
    }
    Ok(read.entries)
}

struct State {
    home: PathBuf,
    fallback: PathBuf,
    owner: SessionOwner,
    instance: String,
}
/// The composition owner resolves projectKey through its catalog before calling
/// these storage operations; the fallback is the application-selected project.
pub(super) fn storage(home: PathBuf, fallback: PathBuf, owner: SessionOwner) -> ReplacementStorage {
    let home = home.canonicalize().unwrap_or(home);
    let state = Arc::new(State {
        home,
        fallback,
        owner,
        instance: uuid(),
    });
    fn operation(state: &Arc<State>, kind: u8) -> StorageOperation {
        let state = state.clone();
        Arc::new(move |input| {
            let state = state.clone();
            Box::pin(async move {
                let home = state.home.clone();
                let requested = if matches!(field(&input, "projectKey"), JsValue::Undefined) {
                    string(state.fallback.to_string_lossy().as_ref())
                } else {
                    field(&input, "projectKey").clone()
                };
                let permit = super::methods::session_dir::session_io_permit()
                    .await
                    .map_err(request_error)?;
                let (project, chat, permit) = tokio::task::spawn_blocking(move || {
                    let project = super::project_scope::registered(&home, &requested)?;
                    let chat = super::methods::session_dir::bounded_chat_dir(&home, &project)?;
                    Ok::<_, crate::runtime::connection::RequestError>((project, chat, permit))
                })
                .await
                .map_err(io)?
                .map_err(request_error)?;
                let key = field(&input, "sessionKey")
                    .as_string()
                    .cloned()
                    .unwrap_or_default();
                let project_js = JsString::from(project.to_string_lossy().as_ref());
                // ReplacementCoordinator has already closed/drained before invoking
                // replace. Its reservation is separate from the owner's IO admission.
                // Accepted-input commit only unlinks its own sidecars. It must
                // coexist with the active writer and still join owner drain.
                let committing = kind == 2 && text(&input, "action") == "commit";
                let background = if committing {
                    Some(
                        state
                            .owner
                            .begin_background_io(&key, &project_js)
                            .map_err(io)?,
                    )
                } else {
                    None
                };
                let guard = if kind == 0 || committing {
                    None
                } else {
                    Some(
                        state
                            .owner
                            .try_reserve_archive_with_project(&key, &project_js, false)
                            .map_err(io)?
                            .ok_or_else(|| error("SESSION_BUSY", "Session is busy."))?,
                    )
                };
                let path = chat.join(format!("{}.jsonl", sanitize_session_id(&key)));
                // Raw session keys can alias one stored filename (a/b == a-b).
                // Reuse the canonical transcript fence, also shared by metadata
                // and hydration, rather than a process-global storage mutex.
                // The writer lease precedes the fence, as in the session factory.
                let writer = if kind == 1 || kind == 2 && !committing {
                    let lease_path = path.clone();
                    Some(tokio::task::spawn_blocking(move || {
                        crate::runtime::session::transcript_locks::cold_write_lease(&lease_path)
                            .map_err(request_error)?
                            .ok_or_else(|| request_error(crate::runtime::session::transcript_locks::writer_busy_error()))
                    }).await.map_err(io)??)
                } else {
                    None
                };
                let fence = crate::runtime::session::transcript_locks::acquire(&path)
                    .await
                    .map_err(request_error)?;
                tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    let _background = background;
                    let _permit = permit;
                    let _writer = writer;
                    let _fence = fence;
                    match kind {
                        0 => fork(&state, &input, &project, &chat, &path),
                        1 => replace(&state, &input, &chat, &path),
                        _ => finalize(&input, &chat, &path),
                    }
                })
                .await
                .map_err(io)?
            })
        })
    }
    ReplacementStorage {
        fork_session: Some(operation(&state, 0)),
        replace_last_turn: Some(operation(&state, 1)),
        finalize_last_turn_replacement: Some(operation(&state, 2)),
    }
}

fn metadata_entry(entries: &[JsValue], prefix: &[JsValue], time: &str) -> Option<JsValue> {
    let mut meta = JsValue::Object(JsObject::new());
    for e in entries {
        if text(e, "type") == "session_metadata" {
            let patch = field(e, "metadata");
            let previous = meta.clone();
            meta = merge(&meta, patch);
            for k in ["title", "linkedPullRequest"] {
                if matches!(field(patch, k), JsValue::Undefined | JsValue::Null) {
                    set(&mut meta, k, field(&previous, k).clone());
                }
            }
        }
    }
    meta = omit(&meta, &["lastPrompt"]);
    if !prefix.iter().any(|e| text(e, "type") == "accepted_input") {
        meta = omit(&meta, &["firstPrompt", "aiTitle"]);
    }
    if !meta.as_object().is_some_and(|o| {
        o.entries().iter().any(|(k, v)| {
            !k.equals_str("isSnapshot")
                && !k.equals_str("updatedAt")
                && !matches!(v, JsValue::Undefined)
        })
    }) {
        return None;
    }
    set(&mut meta, "isSnapshot", JsValue::Bool(true));
    set(&mut meta, "updatedAt", string(time));
    let seq = prefix
        .iter()
        .map(|e| field(e, "sequence").as_number().unwrap_or(0.))
        .fold(0., f64::max)
        + 1.;
    Some(object([
        ("type", string("session_metadata")),
        (
            "sessionId",
            entries
                .first()
                .map(|e| field(e, "sessionId").clone())
                .unwrap_or_else(|| string("")),
        ),
        ("turnId", string("metadata-replace")),
        ("sequence", JsValue::Number(seq)),
        ("createdAt", string(time)),
        ("entryId", string(uuid())),
        (
            "parentEntryId",
            prefix
                .iter()
                .rev()
                .find_map(|e| {
                    field(e, "entryId")
                        .as_string()
                        .filter(|s| !s.is_empty())
                        .map(|s| JsValue::String(s.clone()))
                })
                .unwrap_or(JsValue::Null),
        ),
        ("metadata", meta),
    ]))
}
fn replacement_plan(
    entries: &[JsValue],
    input: &JsValue,
    time: &str,
) -> Result<(JsString, usize, Vec<JsValue>), ReplacementError> {
    let index = entries
        .iter()
        .rposition(|e| text(e, "type") == "accepted_input")
        .ok_or_else(|| {
            error(
                "replace_empty_transcript",
                "No user turn is available to replace.",
            )
        })?;
    let turn = field(&entries[index], "turnId")
        .as_string()
        .cloned()
        .unwrap_or_default();
    if !strict_id(
        field(&entries[index], "turnId"),
        field(input, "expectedTurnId"),
    ) {
        return Err(error(
            "replace_turn_conflict",
            "The selected message is no longer the latest user turn.",
        ));
    }
    let mut prefix = entries[..index].to_vec();
    if !prefix.iter().any(|e| text(e, "type") == "accepted_input") {
        for e in &mut prefix {
            if text(e, "type") == "session_metadata" {
                let meta = omit(field(e, "metadata"), &["aiTitle"]);
                set(e, "metadata", meta);
            }
        }
    }
    if let Some(meta) = metadata_entry(entries, &prefix, time) {
        prefix.push(meta);
    }
    Ok((turn, entries.len() - index, prefix))
}
fn transaction_paths(chat: &Path, path: &Path, id: &str) -> (PathBuf, PathBuf) {
    let safe = path.file_stem().unwrap_or_default().to_string_lossy();
    (
        chat.join(format!(".{safe}.{id}.replace.bak")),
        chat.join(format!(".{safe}.{id}.replace.json")),
    )
}
fn replace(state: &State, input: &JsValue, chat: &Path, path: &Path) -> ReplacementResult {
    if ["sessionKey", "expectedTurnId", "replacementTurnId"]
        .iter()
        .any(|k| {
            field(input, k).as_string().is_none_or(|s| {
                crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty()
            })
        })
    {
        return Err(error(
            "replace_invalid_input",
            "sessionKey, expectedTurnId, and replacementTurnId are required.",
        ));
    }
    let all = entries(path, true)?;
    let time = now()?;
    let (turn, removed, prefix) = replacement_plan(&all, input, &time)?;
    let id = uuid();
    let (backup, journal) = transaction_paths(chat, path, &id);
    let original = read_bytes(path)?;
    let record = object([
        ("version", JsValue::Number(2.)),
        ("transactionId", string(id.as_str())),
        ("sessionKey", field(input, "sessionKey").clone()),
        (
            "replacementTurnId",
            field(input, "replacementTurnId").clone(),
        ),
        ("preparedAt", string(time)),
        (
            "owner",
            object([
                ("instanceId", string(state.instance.as_str())),
                ("pid", JsValue::Number(std::process::id() as f64)),
            ]),
        ),
    ]);
    let operation = (|| {
        write_new(&journal, &body(&[record])?)?;
        write_new(&backup, &original)?;
        publish(path, &body(&prefix)?, "replace")
    })();
    if operation.is_err() {
        let _ = remove(&backup);
        let _ = remove(&journal);
    }
    operation?;
    Ok(object([
        ("sessionKey", field(input, "sessionKey").clone()),
        ("replacedTurnId", string(turn)),
        ("removedEntryCount", JsValue::Number(removed as f64)),
        ("transactionId", string(id)),
    ]))
}
fn finalize(input: &JsValue, chat: &Path, path: &Path) -> ReplacementResult {
    let id = text(input, "transactionId");
    if crate::metadata::js_trim(&text(input, "sessionKey")).is_empty()
        || crate::metadata::js_trim(&id).is_empty()
    {
        return Err(error(
            "replace_invalid_transaction",
            "sessionKey and transactionId are required.",
        ));
    }
    if !valid_uuid(&id) {
        return Err(error(
            "replace_invalid_transaction",
            "The replacement transaction is invalid.",
        ));
    }
    let action = text(input, "action");
    if action != "commit" && action != "rollback" {
        return Err(error(
            "replace_invalid_action",
            "Replacement action must be commit or rollback.",
        ));
    }
    let (backup, journal) = transaction_paths(chat, path, &id);
    if action == "rollback" {
        // Deletion may have committed before a crash interrupted sidecar
        // cleanup. A late rollback must not recreate that deleted lifetime.
        // The caller retains the canonical writer lease and mutation fence.
        // Existence/type, not the archive reader's size limit: a current
        // transcript may have grown since prepare without being deleted.
        require_current_transcript(path).map_err(|e| {
            error(
                "replace_transaction_missing",
                format!("The current transcript could not be safely restored: {e}"),
            )
        })?;
        let original = read_bytes(&backup).map_err(|e| {
            error(
                "replace_transaction_missing",
                format!("The replacement transaction could not be restored: {e}"),
            )
        })?;
        publish(path, &original, "rollback")?;
    }
    remove(&backup)?;
    remove(&journal)?;
    Ok(object([
        ("sessionKey", field(input, "sessionKey").clone()),
        ("transactionId", string(id)),
        ("action", string(action)),
    ]))
}

fn whitespace(unit: u16) -> bool {
    matches!(unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn trim_units(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|u| !whitespace(*u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !whitespace(*u))
        .map(|i| i + 1)
        .unwrap_or(start);
    &units[start..end]
}
fn accepted_text(entry: &JsValue) -> JsString {
    let mut out = Vec::new();
    if let JsValue::Array(messages) = field(entry, "messages") {
        for m in messages {
            if let JsValue::Array(blocks) = field(m, "content") {
                for b in blocks {
                    if text(b, "type") == "text"
                        && let Some(value) = field(b, "text").as_string()
                    {
                        let units = trim_units(value.units());
                        if !units.is_empty() {
                            if !out.is_empty() {
                                out.extend([10, 10]);
                            }
                            out.extend_from_slice(units);
                        }
                    }
                }
            }
        }
    }
    JsString::from_utf16(out)
}
fn fork_title(prompt: &JsString, count: usize, metadata: Option<&JsValue>) -> JsValue {
    let mut normalized = Vec::new();
    let mut pending = false;
    for unit in prompt.units() {
        if whitespace(*unit) {
            pending = !normalized.is_empty();
        } else {
            if pending {
                normalized.push(32);
                pending = false;
            }
            normalized.push(*unit);
        }
    }
    let title = if !normalized.is_empty() {
        let truncated = normalized.len() > 48;
        let snippet = trim_units(&normalized[..normalized.len().min(48)]);
        let mut out = "⑂ ".encode_utf16().collect::<Vec<_>>();
        out.extend_from_slice(snippet);
        if truncated {
            out.push(0x2026);
        }
        JsString::from_utf16(out)
    } else {
        let inherited = metadata.and_then(|m| {
            field(m, "title")
                .as_string()
                .filter(|s| !s.is_empty())
                .or_else(|| field(m, "aiTitle").as_string().filter(|s| !s.is_empty()))
        });
        if let Some(value) = inherited {
            let mut out = "⑂ ".encode_utf16().collect::<Vec<_>>();
            out.extend_from_slice(value.units());
            JsString::from_utf16(out)
        } else if count > 0 {
            "⑂ Forked session".into()
        } else {
            "⑂ New branch".into()
        }
    };
    JsValue::String(title)
}

fn message_carry(message: &JsValue, source: &JsValue, turn: &JsValue) -> JsValue {
    let mut message = message.clone();
    let meta = merge(
        field(&message, "metadata"),
        &object([(
            "forkCarryover",
            object([
                ("sourceSessionId", source.clone()),
                ("sourceTurnId", turn.clone()),
            ]),
        )]),
    );
    set(&mut message, "metadata", meta);
    message
}

fn valid_block(block: &JsValue) -> bool {
    if !matches!(block, JsValue::Object(_)) {
        return false;
    }
    let has_string = |key| field(block, key).as_string().is_some();
    match text(block, "type").as_str() {
        "text" | "thinking" => has_string("text"),
        "image" | "audio" => {
            matches!(text(block, "source").as_str(), "base64" | "url")
                && has_string("data")
                && has_string("mimeType")
        }
        "pdf" => {
            text(block, "source") == "base64"
                && has_string("data")
                && text(block, "mimeType") == "application/pdf"
                && field(block, "bytes").as_number().is_some()
        }
        "tool_call" => has_string("id") && has_string("name"),
        "tool_result" => {
            has_string("toolCallId")
                && matches!(field(block,"content"),JsValue::Array(content)if content.iter().all(|b|matches!(text(b,"type").as_str(),"text"|"image"|"pdf")&&valid_block(b)))
        }
        "tool_result_reference" | "media_reference" => {
            has_string("path")
                && field(block, "originalBytes").as_number().is_some()
                && has_string("preview")
                && matches!(field(block, "hasMore"), JsValue::Bool(_))
                && if text(block, "type") == "tool_result_reference" {
                    has_string("toolCallId")
                } else {
                    has_string("mimeType")
                        && matches!(text(block, "mediaType").as_str(), "image" | "pdf" | "audio")
                }
        }
        _ => false,
    }
}
fn compact_messages(boundary: &JsValue) -> Option<&Vec<JsValue>> {
    if text(boundary, "kind") != "compact" || text(boundary, "subtype") != "compact_boundary" {
        return None;
    }
    let snapshot = field(boundary, "snapshot");
    if field(snapshot, "version") != &JsValue::Number(1.) {
        return None;
    }
    let JsValue::Array(messages) = field(snapshot, "messages") else {
        return None;
    };
    (!messages.is_empty()&&messages.iter().all(|m|matches!(text(m,"role").as_str(),"user"|"assistant")&&matches!(field(m,"metadata"),JsValue::Undefined|JsValue::Object(_))&&matches!(field(m,"content"),JsValue::Array(content)if content.iter().all(valid_block)))).then_some(messages)
}
fn retarget(entry: &JsValue, new_key: &str) -> JsValue {
    let source = field(entry, "sessionId");
    let turn = field(entry, "turnId");
    let mut e = entry.clone();
    set(&mut e, "sessionId", string(new_key));
    // The default TS application consumer has a storageProvider, not the
    // deprecated storageForSession pair. It carries message metadata but
    // preserves reference paths in primary entries (forkSession.ts:241-270).
    let carry = |m: &JsValue| message_carry(m, source, turn);
    match text(entry, "type").as_str() {
        "accepted_input" => {
            if let JsValue::Array(messages) = field(entry, "messages") {
                set(
                    &mut e,
                    "messages",
                    JsValue::Array(messages.iter().map(carry).collect()),
                );
            }
        }
        "assistant_message" | "tool_result_message" | "durable_message" => {
            set(&mut e, "message", carry(field(entry, "message")))
        }
        "control_boundary" => {
            let mut boundary = field(entry, "boundary").clone();
            if text(&boundary, "kind") == "compact"
                && text(&boundary, "subtype") == "compact_boundary"
            {
                let messages = compact_messages(&boundary);
                if let Some(messages) = messages {
                    let replacement = object([
                        ("version", JsValue::Number(1.)),
                        (
                            "messages",
                            JsValue::Array(messages.iter().map(carry).collect()),
                        ),
                    ]);
                    set(&mut boundary, "snapshot", replacement);
                    set(&mut e, "boundary", boundary);
                }
            }
        }
        _ => {}
    }
    e
}
fn auxiliary_path(value: &JsValue, source_dir: &Path, target_dir: &Path) -> JsValue {
    let Some(raw) = value.as_string() else {
        return value.clone();
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let absolute = crate::metadata::resolve_units(&cwd.to_string_lossy(), raw);
    let Ok(absolute) = absolute.to_utf8() else {
        return value.clone();
    };
    let absolute = Path::new(&absolute);
    if let Ok(relative) = absolute.strip_prefix(source_dir)
        && !relative.as_os_str().is_empty()
        && !relative.to_string_lossy().starts_with("..")
    {
        return string(target_dir.join(relative).to_string_lossy().as_ref());
    }
    value.clone()
}
fn auxiliary_message(message: &JsValue, source_dir: &Path, target_dir: &Path) -> JsValue {
    let mut out = message.clone();
    if let JsValue::Array(blocks) = field(message, "content") {
        let blocks = blocks
            .iter()
            .map(|block| {
                let mut block = block.clone();
                if matches!(
                    text(&block, "type").as_str(),
                    "tool_result_reference" | "media_reference"
                ) {
                    let path = auxiliary_path(field(&block, "path"), source_dir, target_dir);
                    set(&mut block, "path", path);
                }
                block
            })
            .collect();
        set(&mut out, "content", JsValue::Array(blocks));
    }
    out
}
/// The Node provider's second stage only retargets paths under its chat session.
fn auxiliary_entry(
    entry: &JsValue,
    source_dir: &Path,
    target_dir: &Path,
    relative_subagents: bool,
) -> Result<JsValue, ReplacementError> {
    if matches!(entry, JsValue::Null) {
        return Err(io("Cannot read properties of null (reading 'type')"));
    }
    let mut out = entry.clone();
    let map = |m: &JsValue| auxiliary_message(m, source_dir, target_dir);
    match text(entry, "type").as_str() {
        "accepted_input" => {
            if let JsValue::Array(messages) = field(entry, "messages") {
                set(
                    &mut out,
                    "messages",
                    JsValue::Array(messages.iter().map(map).collect()),
                );
            }
        }
        "assistant_message" | "tool_result_message" | "durable_message" => {
            set(&mut out, "message", map(field(entry, "message")))
        }
        "control_boundary" => {
            let mut boundary = field(entry, "boundary").clone();
            if let Some(messages) = compact_messages(&boundary) {
                let snapshot = object([
                    ("version", JsValue::Number(1.)),
                    (
                        "messages",
                        JsValue::Array(messages.iter().map(map).collect()),
                    ),
                ]);
                set(&mut boundary, "snapshot", snapshot);
                set(&mut out, "boundary", boundary);
            }
        }
        "subagent_started" if relative_subagents => {
            let relative = text(entry, "transcriptRelativePath");
            let parts = relative.split(['/', '\\']).collect::<Vec<_>>();
            if parts
                .first()
                .is_some_and(|part| source_dir.file_name().is_some_and(|name| name == *part))
            {
                let mut parts = parts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
                parts[0] = target_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                set(&mut out, "transcriptRelativePath", string(parts.join("/")));
            }
        }
        _ => {}
    }
    Ok(out)
}
fn json_work_bound(bytes: &[u8]) -> Result<(), ReplacementError> {
    let (mut quoted, mut escaped, mut depth, mut tokens) = (false, false, 0usize, 0usize);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => {
                quoted = true;
                tokens += 1;
            }
            b'[' | b'{' => {
                depth += 1;
                tokens += 1;
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            b',' | b':' => tokens += 1,
            _ => {}
        }
        if depth > 128 || tokens > 100_000 {
            return Err(io("Native fork JSON work exceeds its resource bound."));
        }
    }
    Ok(())
}
fn rewrite_subagents(
    path: &Path,
    source_dir: &Path,
    target_dir: &Path,
    depth: usize,
) -> Result<(), ReplacementError> {
    if depth > 128 {
        return Err(io("Native fork subagent depth exceeds 128."));
    }
    let items = match fs::read_dir(path) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    for item in items {
        let item = item.map_err(io)?;
        let meta = item.file_type().map_err(io)?;
        if meta.is_dir() {
            rewrite_subagents(&item.path(), source_dir, target_dir, depth + 1)?;
        } else if meta.is_file() && item.file_name().to_string_lossy().ends_with(".jsonl") {
            let bytes = read_bytes(&item.path())?;
            let source = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            let mut lines = Vec::new();
            for line in source.split('\n') {
                if crate::metadata::js_trim(line).is_empty() {
                    lines.push(line.to_owned());
                    continue;
                }
                json_work_bound(line.as_bytes())?;
                let rewritten = match parse_json(line) {
                    Ok(entry) => {
                        stringify_json(&auxiliary_entry(&entry, source_dir, target_dir, false)?)
                            .map_err(io)?
                            .unwrap_or_default()
                    }
                    Err(_) => line.to_owned(),
                };
                lines.push(rewritten);
            }
            publish(&item.path(), lines.join("\n").as_bytes(), "fork-subagent")?;
        }
    }
    Ok(())
}
fn copy_tree_inner(
    source: &Path,
    target: &Path,
    budget: &mut (usize, u64),
    depth: usize,
) -> Result<(), ReplacementError> {
    if depth > 128 {
        return Err(io("Native fork auxiliary directory depth exceeds 128."));
    }
    let meta = match fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(io("Fork auxiliary paths must not be symlinks."));
    }
    budget.0 += 1;
    budget.1 = budget.1.saturating_add(meta.len());
    if budget.0 > 16384 || budget.1 > 100 * 1024 * 1024 {
        return Err(io("Native fork auxiliary copy exceeds its resource bound."));
    }
    if meta.is_dir() {
        dirs(target)?;
        for child in fs::read_dir(source).map_err(io)? {
            let child = child.map_err(io)?;
            copy_tree_inner(
                &child.path(),
                &target.join(child.file_name()),
                budget,
                depth + 1,
            )?;
        }
    } else if meta.is_file() {
        let bytes = read_bytes(source)?;
        budget.1 = budget
            .1
            .saturating_sub(meta.len())
            .saturating_add(bytes.len() as u64);
        if budget.1 > 100 * 1024 * 1024 {
            return Err(io("Native fork auxiliary copy exceeds its resource bound."));
        }
        write_new_mode(target, &bytes, meta.permissions().mode() & 0o7777)?;
    } else {
        return Err(io(
            "Fork auxiliary files must be regular files or directories.",
        ));
    }
    Ok(())
}
fn fork(
    state: &State,
    input: &JsValue,
    project: &Path,
    chat: &Path,
    path: &Path,
) -> ReplacementResult {
    let all = entries(path, false)?;
    if all.is_empty() {
        return Err(error(
            "fork_empty_transcript",
            "Cannot fork an empty session transcript.",
        ));
    }
    let target = match all
        .iter()
        .find(|e| strict_id(field(e, "entryId"), field(input, "fromEntryId")))
    {
        Some(target) => target,
        None => {
            return Err(error(
                "fork_entry_not_found",
                prefixed_value("Transcript entry not found: ", field(input, "fromEntryId"))?,
            ));
        }
    };
    let preserve = text(target, "type") != "accepted_input"
        || field(input, "resumeAt") == &JsValue::Bool(true);
    let accepted = if !preserve {
        target
    } else {
        match all.iter().find(|e| {
            text(e, "type") == "accepted_input"
                && strict_id(field(e, "turnId"), field(target, "turnId"))
        }) {
            Some(accepted) => accepted,
            None => {
                return Err(error(
                    "fork_turn_not_found",
                    prefixed_value("No accepted_input found for turn ", field(target, "turnId"))?,
                ));
            }
        }
    };
    let sequence = |e: &JsValue| field(e, "sequence").as_number().unwrap_or(0.);
    if !matches!(field(input, "resumeDropsTurn"), JsValue::Undefined) {
        let dropped=all.iter().find(|e|strict_id(field(e,"entryId"),field(input,"resumeDropsTurn"))&&text(e,"type")=="accepted_input").ok_or_else(||error("resume_drops_turn_invalid","Resume rejected by --resume-drops-turn: the supplied UUID is not an accepted-input entry."))?;
        if sequence(dropped) <= sequence(target) {
            return Err(error(
                "resume_drops_turn_invalid",
                "Resume rejected by --resume-drops-turn: the dropped turn must occur after the resume point.",
            ));
        }
        let discarded = all
            .iter()
            .filter(|e| sequence(e) > sequence(target))
            .collect::<Vec<_>>();
        if discarded.is_empty()
            || discarded
                .iter()
                .any(|e| !strict_id(field(e, "turnId"), field(dropped, "turnId")))
        {
            return Err(error(
                "resume_drops_turn_mismatch",
                "Resume rejected by --resume-drops-turn: entries after the resume point are not attributable solely to the requested turn.",
            ));
        }
    }
    if !preserve&&let JsValue::Array(messages)=field(accepted,"messages")&&messages.iter().any(|m|matches!(field(m,"content"),JsValue::Array(blocks)if blocks.iter().any(|b|text(b,"type")!="text"))){return Err(error("fork_unsupported_content","Forking messages with attachments or non-text input is not supported yet."));}
    let source_key = field(input, "sessionKey")
        .as_string()
        .cloned()
        .unwrap_or_default();
    let new_key = format!("web:s_{}", uuid());
    let safe = sanitize_session_id(&new_key.as_str().into());
    let target_path = chat.join(format!("{safe}.jsonl"));
    let target_dir = chat.join(&safe);
    let source_safe = sanitize_session_id(&source_key);
    let source_dir = chat.join(&source_safe);
    let source_tools = source_dir.join("tool-results");
    let target_tools = target_dir.join("tool-results");
    let _guard = state
        .owner
        .try_reserve_archive_with_project(
            &new_key.as_str().into(),
            &project.to_string_lossy().as_ref().into(),
            true,
        )
        .map_err(io)?
        .ok_or_else(|| error("SESSION_BUSY", "Session is busy."))?;
    if target_path.exists() || target_dir.exists() {
        return Err(error(
            "fork_target_exists",
            "Fork target session already exists.",
        ));
    }
    let preserved = all
        .iter()
        .filter(|e| {
            if !preserve {
                sequence(e) < sequence(accepted)
            } else {
                sequence(e) <= sequence(target)
                    || strict_id(field(e, "turnId"), field(target, "turnId"))
                        && matches!(text(e, "type").as_str(), "turn_result" | "file_artifacts")
            }
        })
        .collect::<Vec<_>>();
    let count = preserved
        .iter()
        .map(|e| match text(e, "type").as_str() {
            "accepted_input" => match field(e, "messages") {
                JsValue::Array(m) => m.len(),
                _ => 0,
            },
            "assistant_message" | "tool_result_message" | "durable_message" => 1,
            _ => 0,
        })
        .sum::<usize>();
    let mut rewritten = preserved
        .iter()
        .map(|e| auxiliary_entry(&retarget(e, &new_key), &source_dir, &target_dir, true))
        .collect::<Result<Vec<_>, _>>()?;
    let prompt = accepted_text(accepted);
    let title = fork_title(
        &prompt,
        count,
        all.iter()
            .rev()
            .find(|e| text(e, "type") == "session_metadata")
            .map(|e| field(e, "metadata")),
    );
    let time = now()?;
    let mut metadata = object([
        ("parentSessionId", JsValue::String(source_key)),
        ("forkedFromTurnId", field(accepted, "turnId").clone()),
        ("title", title),
        ("updatedAt", string(time.as_str())),
    ]);
    if !prompt.is_empty() {
        set(
            &mut metadata,
            "firstPrompt",
            JsValue::String(prompt.clone()),
        );
    }
    rewritten.push(object([
        ("type", string("session_metadata")),
        ("sessionId", string(new_key.as_str())),
        ("turnId", string(format!("fork-{}", uuid()))),
        (
            "sequence",
            JsValue::Number(rewritten.iter().map(sequence).fold(0., f64::max) + 1.),
        ),
        ("createdAt", string(time)),
        ("entryId", string(uuid())),
        (
            "parentEntryId",
            rewritten
                .last()
                .map(|e| field(e, "entryId").clone())
                .unwrap_or(JsValue::Null),
        ),
        ("metadata", metadata),
    ]));
    dirs(chat)?;
    new_dir(&target_dir)?;
    let operation = (|| {
        let mut budget = (0, 0);
        for (source, target) in [
            (&source_tools, &target_tools),
            (
                &source_dir.join("file-history"),
                &target_dir.join("file-history"),
            ),
            (&source_dir.join("subagents"), &target_dir.join("subagents")),
        ] {
            copy_tree_inner(source, target, &mut budget, 0)?;
        }
        rewrite_subagents(&target_dir.join("subagents"), &source_dir, &target_dir, 0)?;
        let temp = target_path.with_extension(format!("jsonl.{}.fork.tmp", uuid()));
        write_new(&temp, &body(&rewritten)?)?;
        let (fd, target_name) = parent(&target_path)?;
        let result = linkat(
            &fd,
            temp.file_name().unwrap(),
            &fd,
            target_name,
            AtFlags::empty(),
        )
        .map_err(io);
        let _ = remove(&temp);
        result
    })();
    if operation.is_err() {
        let _ = remove_tree(&target_dir);
        let _ = remove_tree(&target_tools);
    }
    operation?;
    let mut output = object([
        ("newSessionKey", string(new_key)),
        (
            "prefillText",
            if preserve {
                string("")
            } else {
                JsValue::String(prompt.clone())
            },
        ),
        ("carriedMessageCount", JsValue::Number(count as f64)),
    ]);
    let meta = field(accepted, "metadata");
    if text(meta, "permissionMode") == "plan" {
        set(&mut output, "mode", string("plan"));
    }
    if matches!(text(meta, "runMode").as_str(), "agent" | "plan" | "ask") {
        set(&mut output, "runMode", field(meta, "runMode").clone());
    }
    Ok(output)
}

#[derive(Debug, Default)]
pub(super) struct RecoveryReport {
    pub committed: usize,
    pub rolled_back: usize,
    pub cleaned: usize,
    pub skipped: usize,
    pub failures: Vec<RecoveryFailure>,
}
#[derive(Debug)]
pub(super) struct RecoveryFailure {
    pub scope: PathBuf,
    pub message: String,
}
struct Artifact {
    id: String,
    backup: PathBuf,
    journal: PathBuf,
    record: Option<JsValue>,
    order: f64,
    has_backup: bool,
}
fn accepted_ids(bytes: &[u8]) -> HashSet<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| parse_json(line).ok())
        .filter(|e| text(e, "type") == "accepted_input")
        .map(|e| text(&e, "turnId"))
        .collect()
}
fn read_journal(path: &Path, id: &str) -> Result<Option<JsValue>, ReplacementError> {
    let file = match crate::project_files::open_beneath(Path::new("/"), path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let bytes = read_open_artifact(file)?;
    Ok(parse_journal(&bytes, id))
}
fn parse_journal(bytes: &[u8], id: &str) -> Option<JsValue> {
    let value = parse_json(std::str::from_utf8(bytes).ok()?).ok()?;
    let version = field(&value, "version").as_number()?;
    if !matches!(version, 1. | 2.)
        || text(&value, "transactionId") != id
        || crate::metadata::js_trim(&text(&value, "sessionKey")).is_empty()
        || crate::metadata::js_trim(&text(&value, "replacementTurnId")).is_empty()
        || field(&value, "preparedAt").as_string().is_none()
    {
        return None;
    }
    if version == 2. {
        let owner = field(&value, "owner");
        let pid = field(owner, "pid").as_number()?;
        if crate::metadata::js_trim(&text(owner, "instanceId")).is_empty()
            || pid <= 0.
            || pid.fract() != 0.
            || pid > 9_007_199_254_740_991.
        {
            return None;
        }
    }
    Some(value)
}
fn mtime(path: &Path) -> f64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.)
        .unwrap_or(0.)
}
fn recover_chats(chats: &[PathBuf]) -> RecoveryReport {
    let mut report = RecoveryReport::default();
    let mut groups: HashMap<PathBuf, Vec<Artifact>> = HashMap::new();
    let mut unreadable = HashSet::new();
    let mut unreadable_chats = HashSet::new();
    for chat in chats {
        let names = match fs::read_dir(chat) {
            Ok(names) => names,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report.failures.push(RecoveryFailure {
                    scope: chat.clone(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        for name in names {
            let name = match name {
                Ok(name) => name,
                Err(e) => {
                    unreadable_chats.insert(chat.clone());
                    report.failures.push(RecoveryFailure {
                        scope: chat.clone(),
                        message: e.to_string(),
                    });
                    continue;
                }
            };
            let name = name.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_prefix('.').and_then(|n| {
                n.strip_suffix(".replace.bak")
                    .or_else(|| n.strip_suffix(".replace.json"))
            }) else {
                continue;
            };
            if stem.len() < 38 {
                continue;
            }
            let split = stem.len() - 37;
            if stem.as_bytes().get(split) != Some(&b'.') {
                continue;
            }
            let (safe, id) = (&stem[..split], &stem[split + 1..]);
            if safe.is_empty() || !valid_uuid(id) {
                continue;
            }
            let path = chat.join(format!("{safe}.jsonl"));
            let artifacts = groups.entry(path.clone()).or_default();
            if artifacts.iter().any(|a| a.id == id) {
                continue;
            }
            let backup = chat.join(format!(".{safe}.{id}.replace.bak"));
            let journal = chat.join(format!(".{safe}.{id}.replace.json"));
            let record = match read_journal(&journal, id) {
                Ok(record) => record,
                Err(e) => {
                    unreadable.insert(path.clone());
                    report.failures.push(RecoveryFailure {
                        scope: journal.clone(),
                        message: e.to_string(),
                    });
                    None
                }
            };
            let prepared = record
                .as_ref()
                .and_then(|r| chrono::DateTime::parse_from_rfc3339(&text(r, "preparedAt")).ok())
                .map(|t| t.timestamp_millis() as f64)
                .unwrap_or(0.);
            let order = prepared.max(mtime(&backup)).max(mtime(&journal));
            let has_backup = fs::metadata(&backup).is_ok();
            artifacts.push(Artifact {
                id: id.into(),
                backup,
                journal,
                record,
                order,
                has_backup,
            });
        }
    }
    for (path, mut artifacts) in groups {
        if unreadable.contains(&path)
            || path
                .parent()
                .is_some_and(|parent| unreadable_chats.contains(parent))
        {
            continue;
        }
        artifacts.sort_by(|a, b| b.order.total_cmp(&a.order));
        let latest = &artifacts[0];
        let operation = (|| -> Result<u8, ReplacementError> {
            if let Some(record) = &latest.record
                && let Some(pid) = field(field(record, "owner"), "pid").as_number()
                && pid <= i32::MAX as f64
                && let Some(pid) = rustix::process::Pid::from_raw(pid as i32)
                && rustix::process::test_kill_process(pid).is_ok()
            {
                return Ok(0);
            }
            // A read failure is not evidence that replacement input is absent.
            // In particular our native size bound must never turn a committed
            // large transcript (or a deleted/unreadable path) into a rollback.
            // Preserve current data and every sidecar for explicit recovery.
            let current = accepted_ids(&read_bytes(&path)?);
            let committed = if let Some(record) = &latest.record {
                current.contains(&text(record, "replacementTurnId"))
            } else {
                read_bytes(&latest.backup)
                    .map(|body| {
                        let original = accepted_ids(&body);
                        current.iter().any(|turn| !original.contains(turn))
                    })
                    .unwrap_or(false)
            };
            if !committed {
                if !latest.has_backup {
                    return Err(io(format!(
                        "Newest replacement transaction {} has no backup to restore.",
                        latest.id
                    )));
                }
                publish(&path, &read_bytes(&latest.backup)?, "recovery")?;
            }
            for artifact in &artifacts {
                remove(&artifact.backup)?;
                remove(&artifact.journal)?;
            }
            Ok(if committed { 1 } else { 2 })
        })();
        match operation {
            Ok(0) => report.skipped += 1,
            Ok(1) => {
                report.committed += 1;
                report.cleaned += artifacts.len();
            }
            Ok(_) => {
                report.rolled_back += 1;
                report.cleaned += artifacts.len();
            }
            Err(e) => report.failures.push(RecoveryFailure {
                scope: path,
                message: e.to_string(),
            }),
        }
    }
    report
}
/// Call before owner admission. Like TS, report failures and retain their
/// sidecars; the application logs each failure and does not silently certify it.
pub(super) fn recover(home: &Path) -> Result<RecoveryReport, ReplacementError> {
    // A caller-selected home alias is supported, like NativeJournalFiles.
    // Resolve only this root; descendant components still use NOFOLLOW IO.
    let home = match home.canonicalize() {
        Ok(home) => home,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RecoveryReport::default()),
        Err(e) => return Err(io(e)),
    };
    let mut chats = Vec::new();
    let projects = match fs::read_dir(home.join("projects")) {
        Ok(projects) => projects,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RecoveryReport::default()),
        Err(e) => return Err(io(e)),
    };
    let mut failures = Vec::new();
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                failures.push(RecoveryFailure {
                    scope: home.join("projects"),
                    message: error.to_string(),
                });
                continue;
            }
        };
        match project.file_type() {
            Ok(kind) if kind.is_dir() => chats.push(project.path().join("chats")),
            Ok(_) => {}
            Err(error) => failures.push(RecoveryFailure {
                scope: project.path(),
                message: error.to_string(),
            }),
        }
    }
    let mut report = recover_chats(&chats);
    report.failures.extend(failures);
    Ok(report)
}

#[cfg(test)]
#[path = "session_history_tests.rs"]
mod tests;
