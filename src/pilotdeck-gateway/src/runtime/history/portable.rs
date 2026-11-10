//! Portable archive validation matching createLocalGateway.ts.
use crate::runtime::connection::RequestError;
use pilotdeck_protocol::JsValue;

const MAX_MESSAGES: usize = 10_000;
const MAX_TITLE_UNITS: usize = 512;
const MAX_BYTES: usize = 2 * 1024 * 1024;

fn invalid(message: &str) -> RequestError {
    RequestError {
        code: "INVALID_SESSION_ARCHIVE".into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

// The header checks are bounded by the already-parsed object's top level;
// reject malformed/count/title inputs before reserving a blocking IO slot.
fn validate_archive_header(value: &JsValue) -> Result<&[JsValue], RequestError> {
    let Some(object) = value.as_object() else {
        return Err(invalid(
            "Unsupported or malformed session transcript archive.",
        ));
    };
    if object.get("schemaVersion") != Some(&JsValue::Number(1.0))
        || !object
            .get("format")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("portable_text_messages"))
    {
        return Err(invalid(
            "Unsupported or malformed session transcript archive.",
        ));
    }
    let Some(JsValue::Array(messages)) = object.get("messages") else {
        return Err(invalid(
            "Unsupported or malformed session transcript archive.",
        ));
    };
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(invalid(
            "Session transcript archive has an invalid message count.",
        ));
    }
    if let Some(title) = object.get("title")
        && !title.is_undefined()
        && !title
            .as_string()
            .is_some_and(|s| s.units().len() <= MAX_TITLE_UNITS)
    {
        return Err(invalid("Session transcript archive title is invalid."));
    }
    Ok(messages)
}

pub(super) fn validate_archive(value: &JsValue) -> Result<(), RequestError> {
    for message in validate_archive_header(value)? {
        if !message
            .get("role")
            .and_then(JsValue::as_string)
            .is_some_and(|role| role.equals_str("user") || role.equals_str("assistant"))
            || message.get("text").and_then(JsValue::as_string).is_none()
        {
            return Err(invalid(
                "Session transcript archive contains an invalid message.",
            ));
        }
    }
    if !serialized_size_at_most(value, MAX_BYTES) {
        return Err(invalid("Session transcript archive is too large."));
    }
    Ok(())
}

/// Count the actual JSON UTF-8 representation without allocating another full
/// archive string. The iterator stack grows with depth, not sibling count;
/// every addition consumes the remaining byte budget before descending.
fn serialized_size_at_most(value: &JsValue, mut remaining: usize) -> bool {
    fn charge(remaining: &mut usize, bytes: usize) -> bool {
        match remaining.checked_sub(bytes) {
            Some(left) => {
                *remaining = left;
                true
            }
            None => false,
        }
    }
    fn string_size(remaining: &mut usize, value: &pilotdeck_protocol::JsString) -> bool {
        if !charge(remaining, 2) {
            return false;
        }
        let units = value.units();
        let mut index = 0;
        while index < units.len() {
            let unit = units[index];
            let bytes = match unit {
                0x22 | 0x5c | 0x08 | 0x0c | 0x0a | 0x0d | 0x09 => 2,
                0..=0x1f => 6,
                0x20..=0x7f => 1,
                0x80..=0x7ff => 2,
                0xd800..=0xdbff
                    if units
                        .get(index + 1)
                        .is_some_and(|u| (0xdc00..=0xdfff).contains(u)) =>
                {
                    index += 1;
                    4
                }
                0xd800..=0xdfff => 6,
                _ => 3,
            };
            if !charge(remaining, bytes) {
                return false;
            }
            index += 1;
        }
        true
    }
    let mut stack: Vec<Box<dyn Iterator<Item = &JsValue> + '_>> =
        vec![Box::new(std::iter::once(value))];
    while let Some(iter) = stack.last_mut() {
        let Some(value) = iter.next() else {
            stack.pop();
            continue;
        };
        match value {
            JsValue::Undefined | JsValue::Null => {
                if !charge(&mut remaining, 4) {
                    return false;
                }
            }
            JsValue::Bool(value) => {
                if !charge(&mut remaining, if *value { 4 } else { 5 }) {
                    return false;
                }
            }
            JsValue::Number(value) => {
                let length = if value.is_finite() {
                    crate::js_value::number_string(*value).len()
                } else {
                    4
                };
                if !charge(&mut remaining, length) {
                    return false;
                }
            }
            JsValue::String(value) => {
                if !string_size(&mut remaining, value) {
                    return false;
                }
            }
            JsValue::Array(values) => {
                if !charge(&mut remaining, 2)
                    || !charge(&mut remaining, values.len().saturating_sub(1))
                {
                    return false;
                }
                stack.push(Box::new(values.iter()));
            }
            JsValue::Object(object) => {
                if !charge(&mut remaining, 2) {
                    return false;
                }
                let mut first = true;
                for (key, value) in object.entries().iter() {
                    if value.is_undefined() {
                        continue;
                    }
                    if !first && !charge(&mut remaining, 1) {
                        return false;
                    }
                    first = false;
                    if !string_size(&mut remaining, key) || !charge(&mut remaining, 1) {
                        return false;
                    }
                }
                stack.push(Box::new(
                    object
                        .entries()
                        .iter()
                        .filter_map(|(_, value)| (!value.is_undefined()).then_some(value)),
                ));
            }
        }
    }
    true
}

fn literal(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}
fn block_text(block: &JsValue) -> Vec<u16> {
    let mut result = Vec::new();
    if let Some(JsValue::Array(parts)) = block.get("content") {
        for (i, part) in parts.iter().enumerate() {
            if i != 0 {
                literal(&mut result, "\n");
            }
            let kind = part.get("type").and_then(JsValue::as_string);
            if kind.is_some_and(|s| s.equals_str("text")) {
                if let Some(text) = part.get("text").and_then(JsValue::as_string) {
                    result.extend_from_slice(text.units());
                }
            } else if kind.is_some_and(|s| s.equals_str("image") || s.equals_str("pdf")) {
                let pdf = kind.is_some_and(|s| s.equals_str("pdf"));
                literal(&mut result, if pdf { "[PDF: " } else { "[Image: " });
                if let Some(mime) = part.get("mimeType").and_then(JsValue::as_string) {
                    result.extend_from_slice(mime.units());
                }
                literal(&mut result, ", ");
                let data = part.get("data").and_then(JsValue::as_string);
                if !pdf
                    && part
                        .get("source")
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| s.equals_str("url"))
                {
                    if let Some(data) = data {
                        result.extend_from_slice(data.units());
                    }
                } else {
                    literal(
                        &mut result,
                        &format!("{} base64 characters", data.map_or(0, |s| s.units().len())),
                    );
                }
                if pdf
                    && let Some(pages) = part
                        .get("pages")
                        .and_then(JsValue::as_number)
                        .filter(|n| *n != 0.0 && !n.is_nan())
                {
                    literal(
                        &mut result,
                        &format!(", {} pages", crate::js_value::number_string(pages)),
                    );
                }
                literal(&mut result, "]");
            }
        }
    }
    result
}

fn portable_text(message: &JsValue) -> Result<Option<pilotdeck_protocol::JsString>, RequestError> {
    let Some(JsValue::Array(content)) = message.get("content") else {
        return Err(RequestError::new("Portable message content is invalid."));
    };
    let mut parts: Vec<Vec<u16>> = Vec::new();
    for block in content {
        let kind = block.get("type").and_then(JsValue::as_string);
        if kind.is_some_and(|s| s.equals_str("text")) {
            if let Some(text) = block
                .get("text")
                .and_then(JsValue::as_string)
                .filter(|s| !s.units().is_empty())
            {
                parts.push(text.units().to_vec());
            }
        } else if kind.is_some_and(|s| s.equals_str("tool_call")) {
            let mut text = "[Tool call: ".encode_utf16().collect::<Vec<_>>();
            if let Some(name) = block.get("name").and_then(JsValue::as_string) {
                text.extend_from_slice(name.units());
            }
            literal(&mut text, "]\n");
            let undefined = JsValue::Undefined;
            let value = block.get("input").unwrap_or(&undefined);
            if let Some(input) = value.as_string() {
                text.extend_from_slice(input.units());
            } else if value.is_undefined() {
                literal(&mut text, "undefined");
            } else {
                let mut wrapper = pilotdeck_protocol::JsObject::new();
                wrapper.insert("input".into(), value.clone());
                crate::runtime::always_on::storage::check_object(&wrapper)
                    .map_err(|e| RequestError::new(e.to_string()))?;
                literal(&mut text, &crate::runtime::cron::stringify_pretty(value));
            }
            parts.push(text);
        } else if kind.is_some_and(|s| s.equals_str("tool_result")) {
            let mut text = "[Tool result]".encode_utf16().collect::<Vec<_>>();
            let inner = block_text(block);
            if !inner.is_empty() {
                literal(&mut text, "\n");
                text.extend(inner);
            }
            parts.push(text);
        } else if kind.is_some_and(|s| s.equals_str("tool_result_reference")) {
            let mut text = "[Tool result reference: "
                .encode_utf16()
                .collect::<Vec<_>>();
            if let Some(id) = block.get("toolCallId").and_then(JsValue::as_string) {
                text.extend_from_slice(id.units());
            }
            literal(&mut text, "]");
            parts.push(text);
        } else if kind.is_some_and(|s| s.equals_str("image")) {
            parts.push("[Image]".encode_utf16().collect());
        }
    }
    let mut text = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i != 0 {
            literal(&mut text, "\n\n");
        }
        text.extend_from_slice(part);
    }
    let text = crate::runtime::memory::js_trim_units(&text);
    Ok((!text.is_empty()).then(|| pilotdeck_protocol::JsString::from_utf16(text.to_vec())))
}

pub(super) fn project_archive(entries: &[JsValue]) -> Result<JsValue, RequestError> {
    use pilotdeck_protocol::JsObject;
    let replay = crate::runtime::session::project_transcript(entries)
        .map_err(|e| RequestError::new(e.message))?;
    let mut messages = Vec::new();
    for message in &replay.messages {
        if message.get("metadata").and_then(|m| m.get("synthetic")) == Some(&JsValue::Bool(true)) {
            continue;
        }
        let Some(text) = portable_text(message)? else {
            continue;
        };
        let mut item = JsObject::new();
        item.insert(
            "role".into(),
            message.get("role").cloned().unwrap_or(JsValue::Undefined),
        );
        item.insert("text".into(), JsValue::String(text));
        messages.push(JsValue::Object(item));
    }
    let mut archive = JsObject::new();
    archive.insert("schemaVersion".into(), JsValue::Number(1.0));
    archive.insert("format".into(), "portable_text_messages".into());
    archive.insert("messages".into(), JsValue::Array(messages));
    let title = replay
        .metadata
        .get("title")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .or_else(|| replay.metadata.get("aiTitle"));
    if let Some(title) = title.and_then(JsValue::as_string)
        && !title.units().is_empty()
        && title.units().len() <= MAX_TITLE_UNITS
    {
        archive.insert("title".into(), JsValue::String(title.clone()));
    }
    let archive = JsValue::Object(archive);
    validate_archive(&archive)?;
    Ok(archive)
}

fn domain(code: &str, message: impl Into<pilotdeck_protocol::JsString>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

/// The caller reserves the session before passing the guard. The guard is
/// deliberately owned by the blocking worker, including when its awaiter drops.
pub(crate) async fn export_guarded<G: Send + 'static>(
    files: super::HistoryFiles,
    session: pilotdeck_protocol::JsString,
    guard: G,
) -> Result<JsValue, RequestError> {
    let permit = super::reader::READ_SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| RequestError::new(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let (_permit, _guard) = (permit, guard);
        let path = super::reader::history_path(&files, &session).map_err(RequestError::new)?;
        let parent = path
            .parent()
            .ok_or_else(|| RequestError::new("Transcript path has no parent."))?;
        let root = match std::fs::canonicalize(parent) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(domain(
                    "SESSION_NOT_FOUND",
                    format!(
                        "Session not found: {}",
                        String::from_utf16_lossy(session.units())
                    ),
                ));
            }
            Err(error) => return Err(RequestError::new(error.to_string())),
        };
        let leaf = path
            .file_name()
            .ok_or_else(|| RequestError::new("Transcript path has no filename."))?;
        let resolved = root.join(leaf);
        let transcript = crate::storage::read_transcript_beneath(&root, &resolved)
            .map_err(|e| RequestError::new(e.to_string()))?
            .read;
        if transcript
            .diagnostics
            .iter()
            .any(|d| d.code == "transcript_missing")
        {
            return Err(domain(
                "SESSION_NOT_FOUND",
                format!(
                    "Session not found: {}",
                    String::from_utf16_lossy(session.units())
                ),
            ));
        }
        if transcript.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(domain(
                "SESSION_TRANSCRIPT_INVALID",
                "Cannot export a transcript with invalid entries.",
            ));
        }
        project_archive(&transcript.entries)
    })
    .await
    .map_err(|e| RequestError::new(e.to_string()))?
}

#[cfg(unix)]
pub(crate) async fn restore_guarded<G: Send + 'static>(
    files: super::HistoryFiles,
    session: pilotdeck_protocol::JsString,
    archive: JsValue,
    guard: G,
) -> Result<JsValue, RequestError> {
    validate_archive_header(&archive)?;
    let permit = super::reader::READ_SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| RequestError::new(e.to_string()))?;
    let check_files = files.clone();
    let check_session = session.clone();
    let (path, lease, archive, permit, guard) = tokio::task::spawn_blocking(move || {
        // Keep the IO reservation in every worker that can panic, including
        // validation/path lookup, and carry it into the publication worker.
        let (permit, guard) = (permit, guard);
        validate_archive(&archive)?;
        let path =
            super::reader::history_path(&check_files, &check_session).map_err(RequestError::new)?;
        let lease = crate::runtime::session::transcript_locks::cold_write_lease(&path)?
            .ok_or_else(|| {
                domain(
                    "SESSION_BUSY",
                    "Cannot restore over an active Gateway session.",
                )
            })?;
        Ok::<_, RequestError>((path, lease, archive, permit, guard))
    })
    .await
    .map_err(|e| RequestError::new(e.to_string()))??;
    let fence = crate::runtime::session::transcript_locks::acquire(&path).await?;
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::Arc;
        let (_permit, _guard) = (permit, guard);
        let (_lease, _fence) = (lease, fence);
        let target = crate::atomic_file::Target::prepare_new(&path).map_err(restore_io)?;
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch_path =
            std::env::temp_dir().join(format!("pdrs-portable-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch_path)
            .map_err(|e| RequestError::new(e.to_string()))?;
        let scratch = Scratch(scratch_path);
        let chat = scratch.0.clone();
        let adapter = crate::storage::AsyncTranscriptAdapter::new(
            Arc::new(crate::storage::JsonlTranscriptStore::new_bounded()),
            Arc::new(move |_| chat.clone()),
        );
        let storage = adapter
            .create_session_storage(crate::storage::SessionStorageInput {
                project_root: files.project,
                pilot_home: files.home,
                session_id: session.clone(),
                now: None,
            })
            .map_err(|e| RequestError::new(e.to_string()))?;
        let turn =
            pilotdeck_protocol::JsString::from(format!("sdk:restore:{}", uuid::Uuid::new_v4()));
        if let Some(title) = archive
            .get("title")
            .and_then(JsValue::as_string)
            .filter(|s| !s.units().is_empty())
        {
            let mut metadata = pilotdeck_protocol::JsObject::new();
            metadata.insert("title".into(), JsValue::String(title.clone()));
            metadata.insert(
                "updatedAt".into(),
                JsValue::String(
                    crate::storage::transcript_timestamp_for_history()
                        .map_err(|e| RequestError::new(e.to_string()))?
                        .into(),
                ),
            );
            storage
                .transcript
                .record_session_metadata(&session, &turn, JsValue::Object(metadata))
                .map_err(|e| RequestError::new(e.to_string()))?;
        }
        let Some(JsValue::Array(items)) = archive.get("messages") else {
            unreachable!("validated archive")
        };
        let mut messages = Vec::with_capacity(items.len());
        for item in items {
            let mut block = pilotdeck_protocol::JsObject::new();
            block.insert("type".into(), "text".into());
            block.insert(
                "text".into(),
                item.get("text").cloned().unwrap_or(JsValue::Undefined),
            );
            let mut message = pilotdeck_protocol::JsObject::new();
            message.insert(
                "role".into(),
                item.get("role").cloned().unwrap_or(JsValue::Undefined),
            );
            message.insert(
                "content".into(),
                JsValue::Array(vec![JsValue::Object(block)]),
            );
            messages.push(JsValue::Object(message));
        }
        let mut metadata = pilotdeck_protocol::JsObject::new();
        metadata.insert("restoredFromGatewayArchive".into(), JsValue::Bool(true));
        metadata.insert("archiveSchemaVersion".into(), JsValue::Number(1.0));
        storage
            .transcript
            .record_accepted_input(&session, &turn, messages, Some(metadata))
            .map_err(|e| RequestError::new(e.to_string()))?;
        storage.transcript.close();
        let bytes = std::fs::read(&storage.transcript_path)
            .map_err(|e| RequestError::new(e.to_string()))?;
        crate::storage::replacement_artifacts::cleanup_if_missing(&path)
            .map_err(|e| RequestError::new(e.to_string()))?;
        target.publish(&bytes).map_err(restore_io)?;
        let mut result = pilotdeck_protocol::JsObject::new();
        result.insert("sessionKey".into(), JsValue::String(session));
        result.insert(
            "importedMessages".into(),
            JsValue::Number(items.len() as f64),
        );
        Ok(JsValue::Object(result))
    })
    .await
    .map_err(|e| RequestError::new(e.to_string()))?
}

#[cfg(unix)]
fn restore_io(error: crate::atomic_file::Error) -> RequestError {
    if matches!(&error, crate::atomic_file::Error::Io { error, .. } if error.kind() == std::io::ErrorKind::AlreadyExists)
    {
        domain(
            "SESSION_RESTORE_CONFLICT",
            "A persistent transcript already exists for this session.",
        )
    } else {
        RequestError::new(error.to_string())
    }
}

#[cfg(not(unix))]
pub(crate) async fn restore_guarded<G: Send + 'static>(
    _: super::HistoryFiles,
    _: pilotdeck_protocol::JsString,
    _: JsValue,
    _: G,
) -> Result<JsValue, RequestError> {
    Err(domain(
        "SESSION_RESTORE_UNSUPPORTED",
        "Atomic transcript restore is unavailable on this platform.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString, parse_json};

    #[test]
    fn portable_rejects_bad_role_missing_content_and_oversized_json() {
        for raw in [
            r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"system","text":"x"}]}"#,
            r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user"}]}"#,
            r#"{"schemaVersion":2,"format":"portable_text_messages","messages":[{"role":"user","text":"x"}]}"#,
        ] {
            assert_eq!(
                validate_archive(&parse_json(raw).unwrap())
                    .unwrap_err()
                    .code,
                "INVALID_SESSION_ARCHIVE"
            );
        }
        let mut message = JsObject::new();
        message.insert("role".into(), "user".into());
        message.insert("text".into(), JsValue::String("x".repeat(MAX_BYTES).into()));
        let mut archive = JsObject::new();
        archive.insert("schemaVersion".into(), JsValue::Number(1.0));
        archive.insert("format".into(), "portable_text_messages".into());
        archive.insert(
            "messages".into(),
            JsValue::Array(vec![JsValue::Object(message)]),
        );
        assert_eq!(
            validate_archive(&JsValue::Object(archive))
                .unwrap_err()
                .code,
            "INVALID_SESSION_ARCHIVE"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_export_roundtrip_and_conflict_preserve_original() {
        let root =
            std::env::temp_dir().join(format!("pdrs-portable-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&project).unwrap();
        let files = super::super::HistoryFiles { home, project };
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-portable-history/oracle.json"
        ))
        .unwrap();
        let archive = oracle
            .get("cases")
            .and_then(|v| v.get("export_restored"))
            .and_then(|v| v.get("value"))
            .unwrap()
            .clone();
        let session = JsString::from("portable-test");
        let target_path = super::super::reader::history_path(&files, &session).unwrap();
        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        let stem = target_path.file_stem().unwrap().to_str().unwrap();
        let id = uuid::Uuid::new_v4();
        let orphan_backup = target_path
            .parent()
            .unwrap()
            .join(format!(".{stem}.{id}.replace.bak"));
        let orphan_journal = target_path
            .parent()
            .unwrap()
            .join(format!(".{stem}.{id}.replace.json"));
        std::fs::write(&orphan_backup, b"deleted old lifetime").unwrap();
        std::fs::write(&orphan_journal, b"old restore rollback").unwrap();
        assert!(!target_path.exists());
        let restored = restore_guarded(files.clone(), session.clone(), archive.clone(), ())
            .await
            .unwrap();
        assert_eq!(
            restored.get("importedMessages"),
            Some(&JsValue::Number(2.0))
        );
        assert!(
            !orphan_backup.exists() && !orphan_journal.exists(),
            "a genuinely new portable restore invalidates orphan replacement state"
        );
        let quarantine_snapshot = || -> std::collections::BTreeMap<_, _> {
            std::fs::read_dir(target_path.parent().unwrap())
                .unwrap()
                .map(Result::unwrap)
                .filter_map(|entry| {
                    let name = entry.file_name().into_string().unwrap();
                    name.starts_with(".pdrs-orphan.")
                        .then(|| (name, std::fs::read(entry.path()).unwrap()))
                })
                .collect()
        };
        let preserved = quarantine_snapshot();
        assert_eq!(
            preserved.len(),
            2,
            "both recovery artifacts survive quarantine"
        );
        assert!(
            preserved
                .values()
                .any(|bytes| bytes == b"deleted old lifetime")
        );
        assert!(
            preserved
                .values()
                .any(|bytes| bytes == b"old restore rollback")
        );
        let path = super::super::reader::history_path(&files, &session).unwrap();
        let read = crate::storage::read_transcript_bounded(&path).unwrap().read;
        let disk = oracle
            .get("restoreDisk")
            .and_then(|v| {
                if let JsValue::Array(a) = v {
                    Some(a)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(read.entries.len(), disk.len());
        for (actual, expected) in read.entries.iter().zip(disk) {
            for key in ["type", "sequence", "messages"] {
                assert_eq!(actual.get(key), expected.get(key), "{key}");
            }
            if actual
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str("accepted_input"))
            {
                assert_eq!(actual.get("metadata"), expected.get("metadata"));
            } else {
                assert_eq!(
                    actual.get("metadata").and_then(|v| v.get("title")),
                    expected.get("metadata").and_then(|v| v.get("title"))
                );
            }
        }
        assert_eq!(read.entries[0].get("parentEntryId"), Some(&JsValue::Null));
        assert_eq!(
            read.entries[1].get("parentEntryId"),
            read.entries[0].get("entryId")
        );
        assert_eq!(read.entries[0].get("turnId"), read.entries[1].get("turnId"));
        assert!(
            read.entries[0]
                .get("turnId")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.to_utf8().is_ok_and(|s| s.starts_with("sdk:restore:")))
        );
        assert!(
            read.entries
                .iter()
                .all(|e| e.get("createdAt").and_then(JsValue::as_string).is_some())
        );
        let replay = crate::runtime::session::project_transcript(&read.entries).unwrap();
        let expected_resume = oracle
            .get("resumeMessages")
            .and_then(|v| {
                if let JsValue::Array(a) = v {
                    Some(a)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(&replay.messages, &expected_resume[..2]);
        let exported = export_guarded(files.clone(), session.clone(), ())
            .await
            .unwrap();
        assert_eq!(exported, archive);
        std::fs::write(&orphan_backup, b"current live backup").unwrap();
        std::fs::write(&orphan_journal, b"current live journal").unwrap();
        let error = restore_guarded(files.clone(), session.clone(), archive.clone(), ())
            .await
            .unwrap_err();
        assert_eq!(error.code, "SESSION_RESTORE_CONFLICT");
        assert_eq!(
            std::fs::read(&orphan_backup).unwrap(),
            b"current live backup"
        );
        assert_eq!(
            std::fs::read(&orphan_journal).unwrap(),
            b"current live journal"
        );
        assert_eq!(
            quarantine_snapshot(),
            preserved,
            "second restore neither duplicates quarantine nor unlinks recovery bytes"
        );
        assert_eq!(
            export_guarded(files.clone(), session, ()).await.unwrap(),
            archive
        );
        let outside = root.join("outside.jsonl");
        std::fs::write(&outside, b"private outside").unwrap();
        let linked = JsString::from("symlink-leaf");
        let linked_path = super::super::reader::history_path(&files, &linked).unwrap();
        std::os::unix::fs::symlink(&outside, &linked_path).unwrap();
        assert!(export_guarded(files, linked, ()).await.is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"private outside");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_export_matches_real_ts_gateway() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-portable-history/oracle.json"
        ))
        .unwrap();
        let compact = oracle.get("compact").unwrap();
        let events = compact
            .get("events")
            .and_then(|v| {
                if let JsValue::Array(a) = v {
                    Some(a)
                } else {
                    None
                }
            })
            .unwrap();
        let mut entries = vec![parse_json(r#"{"type":"accepted_input","turnId":"old","messages":[{"role":"user","content":[{"type":"text","text":"old"}]}]}"#).unwrap()];
        entries.extend(events.iter().cloned());
        assert_eq!(
            &project_archive(&entries).unwrap(),
            compact.get("value").unwrap()
        );
    }

    #[test]
    fn portable_projection_uses_durable_replay_and_skips_synthetic() {
        let entries = parse_json(r#"[{"type":"accepted_input","turnId":"t","messages":[{"role":"user","content":[{"type":"text","text":"  hello  "}]},{"role":"user","content":[{"type":"text","text":"hidden"}],"metadata":{"synthetic":true}}]},{"type":"session_metadata","metadata":{"title":"Example"}}]"#).unwrap();
        let JsValue::Array(ref entries) = entries else {
            panic!("entries")
        };
        let archive = project_archive(entries).unwrap();
        let expected = parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":"hello"}],"title":"Example"}"#).unwrap();
        assert_eq!(archive, expected);
    }

    #[test]
    fn portable_validation_checks_wire_contract_and_utf16_title_length() {
        let mut archive = parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":"ok"}]}"#).unwrap();
        assert!(validate_archive(&archive).is_ok());
        let JsValue::Object(ref mut object) = archive else {
            panic!("archive")
        };
        object.insert(
            "title".into(),
            JsValue::String(JsString::from_utf16(vec![0xd800; 512])),
        );
        assert!(validate_archive(&archive).is_ok());
        let JsValue::Object(ref mut object) = archive else {
            panic!("archive")
        };
        object.insert(
            "title".into(),
            JsValue::String(JsString::from_utf16(vec![0xd800; 513])),
        );
        assert_eq!(
            validate_archive(&archive).unwrap_err().code,
            "INVALID_SESSION_ARCHIVE"
        );
        let mut empty = JsObject::new();
        empty.insert("schemaVersion".into(), JsValue::Number(1.0));
        empty.insert("format".into(), "portable_text_messages".into());
        empty.insert("messages".into(), JsValue::Array(vec![]));
        assert_eq!(
            validate_archive(&JsValue::Object(empty)).unwrap_err().code,
            "INVALID_SESSION_ARCHIVE"
        );
    }
    fn nontext_oracle() -> JsValue {
        parse_json(include_str!(
            "../../../tests/fixtures/native-portable-history/oracle.json"
        ))
        .unwrap()
        .get("nontext")
        .unwrap()
        .clone()
    }
    #[test]
    fn nontext_export_matches_actual_fixed_ts_public_gateway() {
        let oracle = nontext_oracle();
        let JsValue::Array(projections) = oracle.get("projections").unwrap() else {
            panic!("projections")
        };
        assert_eq!(projections.len(), 4);
        for case in projections {
            assert_eq!(case.get("ok"), Some(&JsValue::Bool(true)));
            let JsValue::Array(entries) = case.get("entries").unwrap() else {
                panic!("entries")
            };
            assert_eq!(
                &project_archive(entries).unwrap(),
                case.get("value").unwrap(),
                "{:?}",
                case.get("name")
            );
        }
    }
    #[test]
    fn archive_validation_matches_actual_ts_limits_and_precedence() {
        let oracle = nontext_oracle();
        let JsValue::Array(cases) = oracle.get("validation").unwrap() else {
            panic!("validation")
        };
        assert_eq!(cases.len(), 12);
        for case in cases {
            let recipe = case.get("recipe").unwrap();
            let mut archive=parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":""}]}"#).unwrap();
            let JsValue::Object(ref mut object) = archive else {
                unreachable!()
            };
            if let Some(title) = recipe.get("title") {
                object.insert("title".into(), title.clone());
            }
            if let Some(count) = recipe.get("count").and_then(JsValue::as_number) {
                let message = parse_json(r#"{"role":"user","text":"x"}"#).unwrap();
                object.insert(
                    "messages".into(),
                    JsValue::Array(vec![message; count as usize]),
                );
            }
            if recipe.get("badRole") == Some(&JsValue::Bool(true)) {
                let mut messages = match object.get("messages") {
                    Some(JsValue::Array(messages)) => messages.clone(),
                    _ => unreachable!(),
                };
                let JsValue::Object(ref mut message) = messages[0] else {
                    unreachable!()
                };
                message.insert("role".into(), "system".into());
                object.insert("messages".into(), JsValue::Array(messages));
            }
            if let Some(bytes) = recipe.get("bytes").and_then(JsValue::as_number) {
                let overhead = pilotdeck_protocol::stringify_json(&archive)
                    .unwrap()
                    .unwrap()
                    .len();
                let fill = recipe
                    .get("fill")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                let count = (bytes as usize)
                    .checked_sub(overhead)
                    .expect("archive recipe must include its JSON overhead");
                let text = fill.repeat(count / fill.len()) + &"x".repeat(count % fill.len());
                let JsValue::Object(ref mut object) = archive else {
                    unreachable!()
                };
                let mut messages = match object.get("messages") {
                    Some(JsValue::Array(messages)) => messages.clone(),
                    _ => unreachable!(),
                };
                let JsValue::Object(ref mut message) = messages[0] else {
                    unreachable!()
                };
                message.insert("text".into(), JsValue::String(text.into()));
                object.insert("messages".into(), JsValue::Array(messages));
            }
            let actual_bytes = pilotdeck_protocol::stringify_json(&archive)
                .unwrap()
                .unwrap()
                .len();
            assert_eq!(
                Some(&JsValue::Number(actual_bytes as f64)),
                case.get("bytes")
            );
            let result = validate_archive(&archive);
            if case.get("ok") == Some(&JsValue::Bool(true)) {
                assert!(result.is_ok(), "{:?}", recipe.get("name"));
            } else {
                let error = result.unwrap_err();
                assert_eq!(Some(&JsValue::String(error.code.into())), case.get("code"));
                assert_eq!(Some(&JsValue::String(error.message)), case.get("message"));
            }
        }
    }
    #[test]
    fn bounded_json_size_matches_codec_for_unicode_escapes_and_nonfinite_values() {
        let mut object = JsObject::new();
        object.insert("omit".into(), JsValue::Undefined);
        object.insert(
            JsString::from_utf16(vec![0xd800]),
            JsValue::String(JsString::from_utf16(vec![
                0xd83d, 0xde00, 0xd800, 0xdc00, 0x08, 0x0c, 0x0a, 0x0d, 0x09, 0x01, 0x22, 0x5c,
                0x7f, 0x80, 0x7ff, 0x800,
            ])),
        );
        object.insert(
            "numbers".into(),
            JsValue::Array(vec![
                JsValue::Undefined,
                JsValue::Number(f64::NAN),
                JsValue::Number(f64::INFINITY),
                JsValue::Number(-0.0),
                JsValue::Number(1e21),
                JsValue::Number(1e-7),
            ]),
        );
        let value = JsValue::Object(object);
        let bytes = pilotdeck_protocol::stringify_json(&value)
            .unwrap()
            .unwrap()
            .len();
        assert!(serialized_size_at_most(&value, bytes));
        assert!(!serialized_size_at_most(&value, bytes - 1));
    }
}
