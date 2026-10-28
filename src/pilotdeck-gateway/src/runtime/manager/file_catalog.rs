//! Native JSONL file catalog for `managerSessions`.
//!
//! This is the Rust port of `SessionList.ts::listProjectSessions`, the catalog
//! selected by `createLocalGateway.ts:685` for the native JSONL data plane
//! (`createNodeSessionCatalog`). The manager projection previously bridged the
//! unrelated `AsyncTranscriptStorageAdapter` projection; that provider has a
//! different response shape (`cwd` absent, metadata titles trimmed) and rejects
//! a whole session on a partially written tail. The file catalog is
//! intentionally a lightweight head/tail reader, exactly like TS:
//!
//! - `readSessionLite`: first/last 64 KiB; a size-0 or unreadable file is
//!   skipped;
//! - `parseSessionInfoFromLite`: metadata lines are scanned textually, prompts
//!   come from complete `accepted_input` lines, and a half-written tail line is
//!   simply not parsed;
//! - large files first try the tail `session_metadata` snapshot, then fall back
//!   to a 64 KiB streaming scan with the same 128 KiB per-line cap as TS.
//!
//! Field order and `cwd` follow the TS object literal. Internal `always-on-*`
//! sessions are filtered from the directory listing before any file is opened.

use crate::metadata;
use crate::runtime::memory::js_trim_units;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// `SESSION_LITE_READ_BYTES`.
const SESSION_LITE_READ_BYTES: u64 = 65_536;
/// `SESSION_METADATA_SCAN_CHUNK_BYTES`.
const SESSION_METADATA_SCAN_CHUNK_BYTES: usize = 64 * 1024;
/// `MAX_SESSION_METADATA_LINE_BYTES`.
const MAX_SESSION_METADATA_LINE_BYTES: usize = 128 * 1024;

// Test-only observation of which paths the catalog actually opened. The
// production path stays allocation-free; this proves "filter before reading"
// rather than only observing the final hidden result.
#[cfg(test)]
thread_local! {
    static READ_PATHS: std::cell::RefCell<Vec<std::path::PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn reset_read_paths() {
    READ_PATHS.with(|paths| paths.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn recorded_read_paths() -> Vec<std::path::PathBuf> {
    READ_PATHS.with(|paths| paths.borrow().clone())
}

/// `isInternalSession` (`SessionList.ts:10`), shared with the metadata reader.
pub(crate) use crate::metadata::is_internal_session;

struct SessionLite {
    mtime: f64,
    size: u64,
    head: String,
    tail: String,
}

/// `readSessionLite`: any stat/read failure or an empty file yields `None`.
fn read_session_lite(path: &Path) -> Option<SessionLite> {
    #[cfg(test)]
    READ_PATHS.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
    let metadata = fs::metadata(path).ok()?;
    let size = metadata.len();
    if size == 0 {
        return None;
    }
    let mtime = system_time_ms(metadata.modified().ok()?);
    let mut file = File::open(path).ok()?;
    let head_len = size.min(SESSION_LITE_READ_BYTES) as usize;
    let mut head = vec![0; head_len];
    file.read_exact(&mut head).ok()?;
    let mut tail = head.clone();
    let tail_offset = size.saturating_sub(SESSION_LITE_READ_BYTES);
    if tail_offset > 0 {
        file.seek(SeekFrom::Start(tail_offset)).ok()?;
        let tail_len = (size - tail_offset) as usize;
        let mut buffer = vec![0; tail_len];
        file.read_exact(&mut buffer).ok()?;
        tail = buffer;
    }
    Some(SessionLite {
        mtime,
        size,
        head: String::from_utf8_lossy(&head).into_owned(),
        tail: String::from_utf8_lossy(&tail).into_owned(),
    })
}

/// Node `Stats.mtime.getTime()`: `mtimeMs` is `sec*1000 + nsec/1e6` in a
/// double, then V8 rounds half toward +Infinity. Truncating (the previous
/// `as_millis()`) is off by one whenever the stored fraction reaches 0.5.
fn system_time_ms(time: std::time::SystemTime) -> f64 {
    metadata::node_date_ms(metadata::millis(time))
}

/// `listProjectSessions`: Node `readdir` returns names sorted bytewise (libuv
/// `alphasort`), so equal `lastModified` sessions keep that name order after
/// the stable mtime sort. `fs::read_dir` has no ordering contract; sort first.
pub(crate) fn list_sessions(
    chat_dir: &Path,
    project_root: &Path,
    include_internal: bool,
) -> Result<Vec<(f64, JsValue)>, JsString> {
    let Ok(entries) = fs::read_dir(chat_dir) else {
        return Ok(Vec::new());
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    let mut sessions = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session_id) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if !include_internal && is_internal_session(session_id) {
            continue;
        }
        let path = entry.path();
        if let Some((modified, info)) =
            read_session_info(&path, &JsString::from(session_id), project_root)?
        {
            sessions.push((modified, info));
        }
    }
    // TS `sessions.sort((left, right) => right.lastModified - left.lastModified)`:
    // a NaN subtraction is treated as 0 by the comparator.
    sessions.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(sessions)
}

/// `readSessionInfo` including both metadata fallbacks. A malformed JSON escape
/// inside a scanned metadata field is a V8 `SyntaxError` in TS and must abort
/// the whole listing, not degrade to "field absent".
fn read_session_info(
    path: &Path,
    session_id: &JsString,
    project_root: &Path,
) -> Result<Option<(f64, JsValue)>, JsString> {
    let Some(lite) = read_session_lite(path) else {
        return Ok(None);
    };
    let fast = parse_session_info_from_lite(session_id, &lite, project_root)?;
    if fast.is_some() && lite.size <= SESSION_LITE_READ_BYTES {
        return Ok(fast.map(|info| (lite.mtime, info)));
    }
    if let Some(snapshot) = read_latest_tail_snapshot(&lite)
        && let Some(info) =
            parse_session_info_from_metadata(session_id, &lite, &snapshot, project_root)?
    {
        let Some(merged) = merge_session_info(fast, Some(info)) else {
            return Ok(None);
        };
        return Ok(Some((lite.mtime, merged)));
    }
    let metadata = read_last_session_metadata(path);
    let metadata_info = match metadata.as_ref() {
        Some(metadata) => {
            parse_session_info_from_metadata(session_id, &lite, metadata, project_root)?
        }
        None => None,
    };
    Ok(merge_session_info(fast, metadata_info).map(|info| (lite.mtime, info)))
}

/// `parseSessionInfoFromLite`.
fn parse_session_info_from_lite(
    session_id: &JsString,
    lite: &SessionLite,
    project_root: &Path,
) -> Result<Option<JsValue>, JsString> {
    let source = format!("{}\n{}", lite.head, lite.tail);
    let custom_title = last_metadata_string_field(&source, "title")?;
    let ai_title = last_metadata_string_field(&source, "aiTitle")?;
    let tag = last_metadata_string_field(&source, "tag")?;
    let parent_session_id = last_metadata_string_field(&source, "parentSessionId")?;
    let forked_from_turn_id = last_metadata_string_field(&source, "forkedFromTurnId")?;
    let first_prompt = metadata::accepted_text(&lite.head, false);
    let last_prompt = metadata::accepted_text(&lite.tail, true).or_else(|| first_prompt.clone());
    let Some(summary) = custom_title.clone().or(ai_title.clone()).or(last_prompt) else {
        return Ok(None);
    };
    if summary.is_empty() {
        return Ok(None);
    }
    let created_at = created_at_from_head(&lite.head)?;
    Ok(Some(session_info(
        session_id,
        &summary,
        lite,
        project_root,
        created_at,
        custom_title,
        ai_title,
        first_prompt,
        tag,
        parent_session_id,
        forked_from_turn_id,
    )))
}

/// `parseSessionInfoFromMetadata`.
fn parse_session_info_from_metadata(
    session_id: &JsString,
    lite: &SessionLite,
    metadata: &JsObject,
    project_root: &Path,
) -> Result<Option<JsValue>, JsString> {
    let string = |key: &str| metadata.get(key).and_then(JsValue::as_string).cloned();
    let Some(summary) = string("title")
        .or_else(|| string("aiTitle"))
        .or_else(|| string("lastPrompt"))
        .or_else(|| string("firstPrompt"))
    else {
        return Ok(None);
    };
    if js_trim_units(summary.units()).is_empty() {
        return Ok(None);
    }
    let created_at = created_at_from_head(&lite.head)?;
    Ok(Some(session_info(
        session_id,
        &summary,
        lite,
        project_root,
        created_at,
        string("title"),
        string("aiTitle"),
        string("firstPrompt"),
        string("tag"),
        string("parentSessionId"),
        string("forkedFromTurnId"),
    )))
}

/// `mergeSessionInfo`: later metadata wins except that `firstPrompt` and
/// `createdAt` fall back to the lite projection only when the metadata value is
/// undefined (`??`, not falsy).
fn merge_session_info(fast: Option<JsValue>, metadata: Option<JsValue>) -> Option<JsValue> {
    match (fast, metadata) {
        (None, metadata) => metadata,
        (fast, None) => fast,
        (Some(fast), Some(metadata)) => {
            let undefined = JsValue::Undefined;
            let fallback = |key: &str| {
                let value = metadata.get(key).unwrap_or(&undefined);
                if matches!(value, JsValue::Undefined) {
                    fast.get(key).cloned().unwrap_or(JsValue::Undefined)
                } else {
                    value.clone()
                }
            };
            let mut merged = JsObject::new();
            for key in [
                "sessionId",
                "summary",
                "lastModified",
                "fileSize",
                "customTitle",
                "aiTitle",
                "firstPrompt",
                "cwd",
                "tag",
                "createdAt",
                "parentSessionId",
                "forkedFromTurnId",
            ] {
                let value = if matches!(key, "firstPrompt" | "createdAt") {
                    fallback(key)
                } else {
                    metadata.get(key).cloned().unwrap_or(JsValue::Undefined)
                };
                merged.insert(key.into(), value);
            }
            Some(JsValue::Object(merged))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn session_info(
    session_id: &JsString,
    summary: &JsString,
    lite: &SessionLite,
    project_root: &Path,
    created_at: JsValue,
    custom_title: Option<JsString>,
    ai_title: Option<JsString>,
    first_prompt: Option<JsString>,
    tag: Option<JsString>,
    parent_session_id: Option<JsString>,
    forked_from_turn_id: Option<JsString>,
) -> JsValue {
    let string = |value: Option<JsString>| value.map_or(JsValue::Undefined, JsValue::String);
    let mut info = JsObject::new();
    info.insert("sessionId".into(), JsValue::String(session_id.clone()));
    info.insert("summary".into(), JsValue::String(summary.clone()));
    info.insert("lastModified".into(), JsValue::Number(lite.mtime));
    info.insert("fileSize".into(), JsValue::Number(lite.size as f64));
    info.insert("customTitle".into(), string(custom_title));
    info.insert("aiTitle".into(), string(ai_title));
    info.insert("firstPrompt".into(), string(first_prompt));
    info.insert(
        "cwd".into(),
        JsValue::from(project_root.to_string_lossy().as_ref()),
    );
    info.insert("tag".into(), string(tag));
    info.insert("createdAt".into(), created_at);
    info.insert("parentSessionId".into(), string(parent_session_id));
    info.insert("forkedFromTurnId".into(), string(forked_from_turn_id));
    JsValue::Object(info)
}

/// `firstJsonStringField(head, "createdAt")` then `Date.parse`; absent text is
/// `undefined`, unparseable text is `NaN` (JSON `null`). A bad escape throws
/// the `JSON.parse` `SyntaxError` before `Date.parse`.
fn created_at_from_head(head: &str) -> Result<JsValue, JsString> {
    match metadata::captured_field(head, "createdAt")? {
        Some(text) => match pilotdeck_transport::system_clock::parse_node22_date(&text) {
            Ok(Some(ms)) => Ok(JsValue::Number(ms)),
            Ok(None) | Err(_) => Ok(JsValue::Number(f64::NAN)),
        },
        None => Ok(JsValue::Undefined),
    }
}

/// `lastMetadataStringField`: the last matching metadata line wins; within a
/// line the first matching field occurrence wins, like `String#match`.
fn last_metadata_string_field(source: &str, field: &str) -> Result<Option<JsString>, JsString> {
    let mut value = None;
    for line in source.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.contains("\"session_metadata\"") {
            continue;
        }
        if let Some(captured) = metadata::captured_field(line, field)? {
            value = Some(captured);
        }
    }
    Ok(value)
}

/// `readLatestTailSnapshot`: only large files, skip the first (possibly
/// partial) tail line, and accept only an explicit full snapshot with a
/// non-empty summary field.
fn read_latest_tail_snapshot(lite: &SessionLite) -> Option<JsObject> {
    if lite.size <= SESSION_LITE_READ_BYTES {
        return None;
    }
    let mut latest = None;
    for line in lite.tail.split('\n').skip(1) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.contains("\"type\":\"session_metadata\"") {
            continue;
        }
        let value = metadata::strict_metadata(line).and_then(|value| value.as_object().cloned())?;
        latest = Some(value);
    }
    let latest = latest?;
    if latest.get("isSnapshot").and_then(JsValue::as_bool) != Some(true) {
        return None;
    }
    let has_summary = ["title", "aiTitle", "lastPrompt", "firstPrompt"]
        .iter()
        .any(|key| {
            latest
                .get(key)
                .and_then(JsValue::as_string)
                .is_some_and(|value| !js_trim_units(value.units()).is_empty())
        });
    has_summary.then_some(latest)
}

/// `readLastSessionMetadata`: streaming 64 KiB scan that skips oversized
/// non-metadata lines and merges every complete `session_metadata` record.
fn read_last_session_metadata(path: &Path) -> Option<JsObject> {
    let mut file = File::open(path).ok()?;
    let mut merged = JsObject::new();
    let mut buffer = vec![0; SESSION_METADATA_SCAN_CHUNK_BYTES];
    let mut line = Vec::new();
    let mut line_bytes = 0usize;
    let mut too_large = false;
    let mut is_metadata = false;
    loop {
        let length = file.read(&mut buffer).ok()?;
        if length == 0 {
            break;
        }
        let mut start = 0;
        for end in 0..length {
            if buffer[end] != b'\n' {
                continue;
            }
            append_metadata_line(
                &buffer[start..end],
                &mut line,
                &mut line_bytes,
                &mut too_large,
                &mut is_metadata,
            );
            finish_metadata_line(
                &mut line,
                &mut line_bytes,
                &mut too_large,
                &mut is_metadata,
                &mut merged,
            );
            start = end + 1;
        }
        append_metadata_line(
            &buffer[start..length],
            &mut line,
            &mut line_bytes,
            &mut too_large,
            &mut is_metadata,
        );
    }
    if line_bytes > 0 || !line.is_empty() {
        finish_metadata_line(
            &mut line,
            &mut line_bytes,
            &mut too_large,
            &mut is_metadata,
            &mut merged,
        );
    }
    Some(merged)
}

fn append_metadata_line(
    segment: &[u8],
    line: &mut Vec<u8>,
    line_bytes: &mut usize,
    too_large: &mut bool,
    is_metadata: &mut bool,
) {
    if segment.is_empty() {
        return;
    }
    *line_bytes += segment.len();
    if *too_large {
        return;
    }
    if !*is_metadata {
        let mut prefix = line.clone();
        prefix.extend_from_slice(segment);
        *is_metadata = String::from_utf8_lossy(&prefix).contains("\"type\":\"session_metadata\"");
    }
    if !*is_metadata && *line_bytes > MAX_SESSION_METADATA_LINE_BYTES {
        line.clear();
        *too_large = true;
        return;
    }
    line.extend_from_slice(segment);
}

fn finish_metadata_line(
    line: &mut Vec<u8>,
    line_bytes: &mut usize,
    too_large: &mut bool,
    is_metadata: &mut bool,
    merged: &mut JsObject,
) {
    if !*too_large {
        let text = String::from_utf8_lossy(line);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        if let Some(metadata) =
            metadata::strict_metadata(text).and_then(|value| value.as_object().cloned())
        {
            merge_metadata(merged, &metadata);
        }
    }
    line.clear();
    *line_bytes = 0;
    *too_large = false;
    *is_metadata = false;
}

/// `mergeMetadata`: records are patches; a present key overrides (including
/// `isSnapshot`), while an absent title keeps the previous one.
fn merge_metadata(merged: &mut JsObject, next: &JsObject) {
    for (key, value) in next.entries() {
        merged.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_session_pattern_matches_ts_regex() {
        for id in [
            "always-on-discovery:1",
            "always-on-workspace-2",
            "always-on-report:x",
        ] {
            assert!(
                is_internal_session(id),
                "#934 internal pattern must match {id}"
            );
        }
        for id in [
            "always-on-discovery",
            "always-on-discovery1",
            "always-on-other:1",
            "always-on-discovery/x",
            "normal",
        ] {
            assert!(
                !is_internal_session(id),
                "#934 internal pattern must not match {id}"
            );
        }
    }
}
