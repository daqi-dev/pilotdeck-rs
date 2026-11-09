//! Portable archive validation matching createLocalGateway.ts.
use crate::runtime::connection::RequestError;
use pilotdeck_protocol::{JsValue, stringify_json};

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

pub(super) fn validate_archive(value: &JsValue) -> Result<(), RequestError> {
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
    for message in messages {
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
    let serialized = stringify_json(value)
        .map_err(|_| invalid("Unsupported or malformed session transcript archive."))?
        .ok_or_else(|| invalid("Unsupported or malformed session transcript archive."))?;
    if serialized.len() > MAX_BYTES {
        return Err(invalid("Session transcript archive is too large."));
    }
    Ok(())
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
    validate_archive(&archive)?;
    let permit = super::reader::READ_SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| RequestError::new(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::Arc;
        let (_permit, _guard) = (permit, guard);
        let path = super::reader::history_path(&files, &session).map_err(RequestError::new)?;
        let _lease = crate::runtime::session::transcript_locks::cold_write_lease(&path)
            .ok_or_else(|| {
                domain(
                    "SESSION_BUSY",
                    "Cannot restore over an active Gateway session.",
                )
            })?;
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
        let restored = restore_guarded(files.clone(), session.clone(), archive.clone(), ())
            .await
            .unwrap();
        assert_eq!(
            restored.get("importedMessages"),
            Some(&JsValue::Number(2.0))
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
        let error = restore_guarded(files.clone(), session.clone(), archive.clone(), ())
            .await
            .unwrap_err();
        assert_eq!(error.code, "SESSION_RESTORE_CONFLICT");
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
        let archive = project_archive(&entries).unwrap();
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
}
