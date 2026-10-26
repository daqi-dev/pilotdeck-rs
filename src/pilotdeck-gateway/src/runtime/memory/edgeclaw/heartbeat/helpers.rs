//! Pure heartbeat helpers from `core/pipeline/heartbeat.ts:22-446`.
//!
//! Everything here is a direct port of the fixed TS baseline. Values that
//! cross the trace boundary stay [`JsValue`]/[`JsString`] so UTF-16 units,
//! property insertion order and optional `undefined` fields match the wire
//! bytes of `JSON.stringify`; no helper re-encodes a string through `String`.

use crate::project_files::{node_collator, node_compare_utf16, node_locale};
use crate::runtime::memory::edgeclaw::{
    messages::{MemoryMessage, MemoryRole},
    storage::{L0SessionRecord, js_truthy},
    text,
};
use icu_collator::CollatorBorrowed;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::sync::OnceLock;

// -- duplicate detection ----------------------------------------------------

/// `sameMessage`: both operands must exist and agree on role and content.
pub fn same_message(left: Option<&MemoryMessage>, right: Option<&MemoryMessage>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.role == right.role && left.content == right.content
}

/// `hasNewContent`: the incoming transcript is new if it is non-empty and
/// either the previous transcript is empty, it grew, or any shared index
/// differs by role/content.
pub fn has_new_content(previous: &[MemoryMessage], incoming: &[MemoryMessage]) -> bool {
    if incoming.is_empty() {
        return false;
    }
    if previous.is_empty() {
        return true;
    }
    if incoming.len() > previous.len() {
        return true;
    }
    for index in 0..incoming.len() {
        if !same_message(previous.get(index), incoming.get(index)) {
            return true;
        }
    }
    false
}

/// Rebuild the typed message list stored in an `L0SessionRecord.messages`
/// wire array. Storage keeps every non-empty string role (`sqlite.ts:145`),
/// so comparison/merge preserve it; only [`derive_focus_turns`] filters the
/// final focus selection down to `user`.
pub fn messages_from_value(value: &JsValue) -> Vec<MemoryMessage> {
    let JsValue::Array(items) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let role = item.get("role").and_then(JsValue::as_string)?;
            let role = MemoryRole::from_js(role);
            let content = item.get("content").and_then(JsValue::as_string)?.clone();
            let msg_id = item.get("msgId").and_then(JsValue::as_string).cloned();
            Some(MemoryMessage {
                msg_id,
                role,
                content,
            })
        })
        .collect()
}

// -- search tokenization ----------------------------------------------------

const STOPWORDS: [&str; 21] = [
    "项目", "当前", "这个", "那个", "现在", "一下", "关于", "里面", "这里", "那里", "怎么", "怎样",
    "如何", "什么", "哪些", "进展", "情况", "默认", "应该", "需要", "general",
];

/// `tokenizeSearchText`: lowercase, treat every non-`\p{L}\p{N}` unit as a
/// separator, split Han runs into the whole run (<= 8 units) plus every 2/3
/// unit slice, de-duplicate in insertion order, drop 1-unit tokens and
/// stopwords.
pub fn tokenize_search_text(value: &JsString) -> Vec<JsString> {
    let lower = text::lowercase(value);
    let units = lower.units();
    let classes = classify_units(units);
    let mut raw_tokens = Vec::new();
    let mut current = Vec::new();
    for class in &classes {
        if class.alphanumeric {
            current.extend_from_slice(&units[class.start..class.end]);
        } else if !current.is_empty() {
            raw_tokens.push(JsString::from_utf16(std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        raw_tokens.push(JsString::from_utf16(current));
    }

    let mut expanded = Vec::new();
    for token in &raw_tokens {
        expand_cjk_token(token, &mut expanded);
    }
    let mut seen: Vec<JsString> = Vec::new();
    let mut tokens = Vec::new();
    for token in expanded {
        if token.len_utf16() < 2 || seen.contains(&token) {
            continue;
        }
        seen.push(token.clone());
        let stopword = STOPWORDS.iter().any(|word| token.equals_str(word));
        if !stopword {
            tokens.push(token);
        }
    }
    tokens
}

fn expand_cjk_token(token: &JsString, out: &mut Vec<JsString>) {
    let classes = classify_units(token.units());
    if !classes.iter().any(|class| class.han) {
        out.push(token.clone());
        return;
    }
    let mut pieces: Vec<(usize, usize, bool)> = Vec::new();
    for class in &classes {
        if let Some(last) = pieces.last_mut()
            && last.2 == class.han
        {
            last.1 = class.end;
            continue;
        }
        pieces.push((class.start, class.end, class.han));
    }
    for (start, end, han) in pieces {
        if !han {
            out.push(JsString::from_utf16(token.units()[start..end].to_vec()));
            continue;
        }
        let length = end - start;
        if length <= 8 {
            out.push(JsString::from_utf16(token.units()[start..end].to_vec()));
        }
        for size in [2, 3] {
            if length < size {
                continue;
            }
            for index in start..=end - size {
                out.push(JsString::from_utf16(
                    token.units()[index..index + size].to_vec(),
                ));
            }
        }
    }
}

struct UnitClass {
    start: usize,
    end: usize,
    han: bool,
    alphanumeric: bool,
}

fn classify_units(units: &[u16]) -> Vec<UnitClass> {
    let mut classes = Vec::new();
    let mut index = 0;
    while index < units.len() {
        let (end, character) = if is_high_surrogate(units[index])
            && units
                .get(index + 1)
                .is_some_and(|unit| is_low_surrogate(*unit))
        {
            let point = 0x10000
                + (u32::from(units[index] - 0xD800) << 10)
                + u32::from(units[index + 1] - 0xDC00);
            (index + 2, char::from_u32(point))
        } else if is_surrogate(units[index]) {
            (index + 1, None)
        } else {
            (index + 1, char::from_u32(u32::from(units[index])))
        };
        let (han, alphanumeric) = character.map_or((false, false), |character| {
            (is_han(character), is_letter_or_number(character))
        });
        classes.push(UnitClass {
            start: index,
            end,
            han,
            alphanumeric,
        });
        index = end;
    }
    classes
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&unit)
}

fn is_surrogate(unit: u16) -> bool {
    (0xD800..=0xDFFF).contains(&unit)
}

fn letter_or_number_regex() -> &'static Regex {
    static LETTER_OR_NUMBER: OnceLock<Regex> = OnceLock::new();
    LETTER_OR_NUMBER.get_or_init(|| Regex::new(r"^[\p{L}\p{N}]$").expect("Unicode property regex"))
}

fn han_regex() -> &'static Regex {
    static HAN: OnceLock<Regex> = OnceLock::new();
    HAN.get_or_init(|| Regex::new(r"^\p{Han}$").expect("Unicode property regex"))
}

fn is_letter_or_number(character: char) -> bool {
    letter_or_number_regex().is_match(&character.to_string())
}

fn is_han(character: char) -> bool {
    han_regex().is_match(&character.to_string())
}

// -- General project shortlist ---------------------------------------------

const GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT: usize = 30;

fn shortlist_collator() -> &'static CollatorBorrowed<'static> {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR.get_or_init(|| {
        let locale = node_locale(|key| std::env::var(key).ok());
        node_collator(&locale).expect("bundled collation data")
    })
}

struct ShortlistRow {
    score: usize,
    updated_at: JsString,
    candidate: JsValue,
}

/// `buildGeneralProjectShortlist`: exact-name hits weigh 10; every matched
/// token adds 1. Ties break by `updatedAt` descending via `localeCompare`, and
/// the list is cut to 30 entries.
pub fn build_general_project_shortlist(catalog: &[JsValue], text_value: &JsString) -> Vec<JsValue> {
    let tokens = tokenize_search_text(text_value);
    let lowered_text = text::lowercase(text_value);
    let mut rows = Vec::with_capacity(catalog.len());
    for project in catalog {
        let project_name = field_string(project, "projectName");
        let description = field_string(project, "description");
        let haystack = text::lowercase(&text::join_text_refs(
            [&project_name, &JsString::from(" "), &description],
            "",
        ));
        let exact = usize::from(text::contains(
            lowered_text.units(),
            text::lowercase(&project_name).units(),
        )) * 2;
        let matched: Vec<&JsString> = tokens
            .iter()
            .filter(|token| text::contains(haystack.units(), token.units()))
            .collect();
        let score = exact * 10 + matched.len();
        let latest_memory_at = project
            .get("summary")
            .and_then(|summary| summary.get("latestMemoryAt"))
            .and_then(JsValue::as_string)
            .filter(|value| !value.is_empty());
        let updated_at = latest_memory_at
            .or_else(|| project.get("updatedAt").and_then(JsValue::as_string))
            .cloned()
            .unwrap_or_default();
        let source_type = field_string(project, "sourceType");
        let source_type = if source_type.equals_str("workspace_external") {
            "workspace_external"
        } else {
            "general_local"
        };
        let source = if exact > 0 || !matched.is_empty() {
            "query"
        } else {
            "recent"
        };
        let candidate = fields(vec![
            ("projectId", string_value(project, "logicalProjectId")),
            ("projectName", JsValue::String(project_name)),
            ("description", JsValue::String(description)),
            ("status", string_value(project, "status")),
            ("updatedAt", JsValue::String(updated_at.clone())),
            ("sourceType", JsValue::String(source_type.into())),
            ("score", JsValue::Number(score as f64)),
            ("exact", JsValue::Number(exact as f64)),
            ("source", JsValue::String(source.into())),
            (
                "matchedText",
                JsValue::String(text::join_text_refs(matched.iter().copied(), ", ")),
            ),
        ]);
        rows.push(ShortlistRow {
            score,
            updated_at,
            candidate: JsValue::Object(candidate),
        });
    }
    rows.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| {
            node_compare_utf16(shortlist_collator(), &right.updated_at, &left.updated_at)
        })
    });
    rows.into_iter()
        .take(GENERAL_INDEX_PROJECT_CANDIDATE_LIMIT)
        .map(|row| row.candidate)
        .collect()
}

// -- candidate routing ------------------------------------------------------

/// `buildCandidateRoutingQuery`: focus turn first, then the candidate's
/// non-empty scalar fields and list items, joined by newlines.
pub fn build_candidate_routing_query(candidate: &JsValue, focus_turn: &MemoryMessage) -> JsString {
    let mut parts = Vec::new();
    if !focus_turn.content.is_empty() {
        parts.push(focus_turn.content.clone());
    }
    for key in ["name", "description", "rule", "summary", "stage"] {
        push_non_empty(candidate.get(key), &mut parts);
    }
    for key in ["constraints", "decisions", "blockers"] {
        if let Some(JsValue::Array(items)) = candidate.get(key) {
            for item in items {
                push_non_empty(Some(item), &mut parts);
            }
        }
    }
    text::join_text(&parts, "\n")
}

fn push_non_empty(value: Option<&JsValue>, parts: &mut Vec<JsString>) {
    if let Some(value) = value.and_then(JsValue::as_string)
        && !value.is_empty()
    {
        parts.push(value.clone());
    }
}

/// `buildCandidateMemoryPreview`: the 3000-unit preview written next to a
/// routed candidate.
pub fn build_candidate_memory_preview(candidate: &JsValue) -> JsString {
    let candidate_type = field_string(candidate, "type");
    let name = field_string(candidate, "name");
    let description = field_string(candidate, "description");
    let mut lines = vec![
        text::join_text_refs([&JsString::from("# "), &name], ""),
        JsString::default(),
        text::join_text_refs([&JsString::from("type: "), &candidate_type], ""),
        text::join_text_refs([&JsString::from("description: "), &description], ""),
    ];
    if candidate_type.equals_str("feedback") {
        let rule = first_truthy(&[
            field_string(candidate, "rule"),
            description.clone(),
            field_string(candidate, "summary"),
            field_string(candidate, "name"),
        ]);
        lines.push(JsString::default());
        lines.push(JsString::from("## Rule"));
        lines.push(rule);
        push_optional_section(&mut lines, candidate.get("why"), "## Why");
        push_optional_section(&mut lines, candidate.get("howToApply"), "## How To Apply");
    }
    if candidate_type.equals_str("project") {
        if let Some(stage) = candidate.get("stage").and_then(JsValue::as_string)
            && !stage.is_empty()
        {
            lines.push(JsString::default());
            lines.push(JsString::from("## Current Stage"));
            lines.push(stage.clone());
        }
        for (title, key) in [
            ("Decisions", "decisions"),
            ("Constraints", "constraints"),
            ("Next Steps", "nextSteps"),
            ("Blockers", "blockers"),
            ("Timeline", "timeline"),
            ("Notes", "notes"),
        ] {
            let mut normalized = Vec::new();
            if let Some(JsValue::Array(items)) = candidate.get(key) {
                for item in items {
                    if let Some(item) = item.as_string() {
                        let item = text::trim_js(item);
                        if !item.is_empty() {
                            normalized.push(item);
                        }
                    }
                }
            }
            if normalized.is_empty() {
                continue;
            }
            lines.push(JsString::default());
            lines.push(JsString::from(format!("## {title}")));
            for item in normalized {
                lines.push(text::join_text_refs([&JsString::from("- "), &item], ""));
            }
        }
        if let Some(summary) = candidate.get("summary").and_then(JsValue::as_string)
            && !summary.is_empty()
        {
            lines.push(JsString::default());
            lines.push(JsString::from("## Summary"));
            lines.push(summary.clone());
        }
    }
    if let Some(body) = candidate.get("body").and_then(JsValue::as_string)
        && !body.is_empty()
    {
        lines.push(JsString::default());
        lines.push(JsString::from("## Body"));
        lines.push(body.clone());
    }
    let preview = text::trim_js(&text::join_text(&lines, "\n"));
    if preview.len_utf16() <= 3000 {
        preview
    } else {
        text::join_text_refs(
            [
                &JsString::from_utf16(preview.units()[..3000].to_vec()),
                &JsString::from("..."),
            ],
            "",
        )
    }
}

fn push_optional_section(lines: &mut Vec<JsString>, value: Option<&JsValue>, title: &str) {
    if let Some(value) = value.and_then(JsValue::as_string)
        && !value.is_empty()
    {
        lines.push(JsString::default());
        lines.push(JsString::from(title));
        lines.push(value.clone());
    }
}

fn first_truthy(values: &[JsString]) -> JsString {
    values
        .iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

// -- batch flatten / merge --------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct MergeSessionMessages {
    pub merged_messages: Vec<MemoryMessage>,
    pub new_messages: Vec<MemoryMessage>,
}

/// `flattenBatchMessages`: fold every session through `mergeSessionMessages`.
pub fn flatten_batch_messages(
    sessions: &[L0SessionRecord],
    seed_messages: &[MemoryMessage],
) -> Vec<MemoryMessage> {
    let mut previous = seed_messages.to_vec();
    for session in sessions {
        let incoming = messages_from_value(&session.messages);
        previous = merge_session_messages(&previous, &incoming).merged_messages;
    }
    previous
}

/// `commonPrefixLength`: longest index prefix of equal messages.
pub fn common_prefix_length(previous: &[MemoryMessage], incoming: &[MemoryMessage]) -> usize {
    let limit = previous.len().min(incoming.len());
    let mut index = 0;
    while index < limit && same_message(previous.get(index), incoming.get(index)) {
        index += 1;
    }
    index
}

/// `mergeSessionMessages`: a non-empty shared prefix replaces the previous
/// transcript with the incoming one and only the tail is new; otherwise the
/// incoming transcript is appended and all of it is new.
pub fn merge_session_messages(
    previous: &[MemoryMessage],
    incoming: &[MemoryMessage],
) -> MergeSessionMessages {
    if previous.is_empty() {
        return MergeSessionMessages {
            merged_messages: incoming.to_vec(),
            new_messages: incoming.to_vec(),
        };
    }
    let prefix_length = common_prefix_length(previous, incoming);
    if prefix_length > 0 {
        return MergeSessionMessages {
            merged_messages: incoming.to_vec(),
            new_messages: incoming[prefix_length..].to_vec(),
        };
    }
    let mut merged = previous.to_vec();
    merged.extend_from_slice(incoming);
    MergeSessionMessages {
        merged_messages: merged,
        new_messages: incoming.to_vec(),
    }
}

/// `deriveFocusTurns`: walk the batch with a cursor, record each session's new
/// user turns, and preserve `Map.set` semantics for duplicate ids.
pub fn derive_focus_turns(
    previous: &[MemoryMessage],
    sessions: &[L0SessionRecord],
) -> Vec<(String, Vec<MemoryMessage>)> {
    let mut focus_turns: Vec<(String, Vec<MemoryMessage>)> = Vec::new();
    let mut cursor = previous.to_vec();
    for session in sessions {
        let incoming = messages_from_value(&session.messages);
        let merged = merge_session_messages(&cursor, &incoming);
        let focus: Vec<MemoryMessage> = merged
            .new_messages
            .into_iter()
            .filter(|message| message.role == MemoryRole::User)
            .collect();
        if let Some(entry) = focus_turns
            .iter_mut()
            .find(|(key, _)| key == &session.l0_index_id)
        {
            entry.1 = focus;
        } else {
            focus_turns.push((session.l0_index_id.clone(), focus));
        }
        cursor = merged.merged_messages;
    }
    focus_turns
}

// -- trace helpers ----------------------------------------------------------

/// `buildIndexTraceId`: `index_trace_` + sha1 of `sessionKey:startedAt:ids`.
pub fn build_index_trace_id(
    session_key: &JsString,
    started_at: &JsString,
    l0_ids: &[JsString],
) -> JsString {
    let joined = text::join_text_refs(l0_ids.iter(), ",");
    let seed = text::join_text_refs(
        [
            session_key,
            &JsString::from(":"),
            started_at,
            &JsString::from(":"),
            &joined,
        ],
        "",
    );
    JsString::from(format!("index_trace_{}", text::hash_text(&seed)))
}

/// `normalizeTrigger`: a reason containing "scheduled" after trim+lowercase is
/// scheduled; everything else is a manual sync.
pub fn normalize_trigger(reason: Option<&JsString>) -> &'static str {
    let normalized = text::lowercase(&text::trim_js(reason.unwrap_or(&JsString::default())));
    if text::contains(
        normalized.units(),
        &"scheduled".encode_utf16().collect::<Vec<_>>(),
    ) {
        "scheduled"
    } else {
        "manual_sync"
    }
}

/// `previewText`: decode, collapse ECMAScript whitespace runs to one space,
/// trim, then cut at `max_chars` UTF-16 units.
pub fn preview_text(value: &JsString, max_chars: usize) -> JsString {
    let normalized = text::normalize_text(&text::decode_escaped_unicode_text(value, true));
    if normalized.len_utf16() <= max_chars {
        normalized
    } else {
        text::join_text_refs(
            [
                &JsString::from_utf16(normalized.units()[..max_chars].to_vec()),
                &JsString::from("..."),
            ],
            "",
        )
    }
}

/// `inferStorageKind`: the trace storage label derived from record type and
/// global user-note directory.
pub fn infer_storage_kind(record_type: &JsString, relative_path: &JsString) -> &'static str {
    if record_type.equals_str("general_project_meta") {
        return "general_project_meta";
    }
    if record_type.equals_str("user") {
        let user_note = text::contains(
            relative_path.units(),
            &"UserNotes/".encode_utf16().collect::<Vec<_>>(),
        ) || text::contains(
            relative_path.units(),
            &"UserIdentityNotes/".encode_utf16().collect::<Vec<_>>(),
        );
        return if user_note {
            "global_user_note"
        } else {
            "global_user"
        };
    }
    if record_type.equals_str("feedback") {
        "feedback"
    } else {
        "project"
    }
}

/// `exposeStoredRelativePath`: global records are exposed under `global/`.
pub fn expose_stored_relative_path(scope: &JsString, relative_path: &JsString) -> JsString {
    if scope.equals_str("global") {
        text::join_text_refs([&JsString::from("global/"), relative_path], "")
    } else {
        relative_path.clone()
    }
}

// -- trace detail constructors ---------------------------------------------

pub fn text_detail(
    key: &str,
    label: &str,
    text: &JsString,
    label_i18n: Option<&JsValue>,
) -> JsValue {
    detail(
        "text",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text::decode_escaped_unicode_text(text, true)),
        )],
    )
}

pub fn note_detail(
    key: &str,
    label: &str,
    text: &JsString,
    label_i18n: Option<&JsValue>,
) -> JsValue {
    detail(
        "note",
        key,
        label,
        label_i18n,
        vec![(
            "text",
            JsValue::String(text::decode_escaped_unicode_text(text, true)),
        )],
    )
}

pub fn list_detail(
    key: &str,
    label: &str,
    items: &[JsString],
    label_i18n: Option<&JsValue>,
) -> JsValue {
    let items = items
        .iter()
        .map(|item| JsValue::String(text::decode_escaped_unicode_text(item, true)))
        .collect();
    detail(
        "list",
        key,
        label,
        label_i18n,
        vec![("items", JsValue::Array(items))],
    )
}

/// Why `String(entry.value ?? "")` throws in `kvDetail`. TS lets the
/// conversion error escape the pure constructor, so the Rust port returns it
/// instead of panicking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvValueError {
    /// `RangeError: Maximum call stack size exceeded` (too-deep array join).
    StackOverflow,
    /// `TypeError: Cannot convert object to primitive value`.
    NotPrimitive,
}

impl KvValueError {
    pub fn name(self) -> &'static str {
        match self {
            Self::StackOverflow => "RangeError",
            Self::NotPrimitive => "TypeError",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::StackOverflow => crate::js_value::StackOverflow::MESSAGE,
            Self::NotPrimitive => crate::js_value::ToStringError::NOT_PRIMITIVE,
        }
    }

    fn from_conversion(error: crate::js_value::ToStringError) -> Self {
        match error {
            crate::js_value::ToStringError::StackOverflow => Self::StackOverflow,
            crate::js_value::ToStringError::NotPrimitive => Self::NotPrimitive,
        }
    }
}

pub fn kv_detail(
    key: &str,
    label: &str,
    entries: &[JsValue],
    label_i18n: Option<&JsValue>,
) -> Result<JsValue, KvValueError> {
    let entries = entries
        .iter()
        .map(|entry| {
            let units = match entry.get("value") {
                None | Some(JsValue::Undefined) | Some(JsValue::Null) => Vec::new(),
                Some(value) => crate::js_value::to_string_units_checked(value)
                    .map_err(KvValueError::from_conversion)?,
            };
            Ok(JsValue::Object(fields(vec![
                ("label", JsValue::String(field_string(entry, "label"))),
                (
                    "value",
                    JsValue::String(text::decode_escaped_unicode_text(
                        &JsString::from_utf16(units),
                        true,
                    )),
                ),
            ])))
        })
        .collect::<Result<Vec<_>, KvValueError>>()?;
    Ok(detail(
        "kv",
        key,
        label,
        label_i18n,
        vec![("entries", JsValue::Array(entries))],
    ))
}

pub fn json_detail(
    key: &str,
    label: &str,
    json: &JsValue,
    label_i18n: Option<&JsValue>,
) -> JsValue {
    detail(
        "json",
        key,
        label,
        label_i18n,
        vec![("json", text::decode_escaped_unicode_value(json, true))],
    )
}

fn detail(
    kind: &str,
    key: &str,
    label: &str,
    label_i18n: Option<&JsValue>,
    payload: Vec<(&str, JsValue)>,
) -> JsValue {
    let mut detail = JsObject::new();
    detail.insert("key".into(), JsValue::String(key.into()));
    detail.insert("label".into(), JsValue::String(label.into()));
    if let Some(label_i18n) = label_i18n
        && js_truthy(label_i18n)
    {
        detail.insert("labelI18n".into(), label_i18n.clone());
    }
    detail.insert("kind".into(), JsValue::String(kind.into()));
    for (key, value) in payload {
        detail.insert(key.into(), value);
    }
    JsValue::Object(detail)
}

// -- step / batch trace -----------------------------------------------------

/// `createStep`: append a step with a 1-based id and only the optional fields
/// that are truthy, preserving TS property order.
pub fn create_step(
    trace: &mut JsObject,
    kind: &str,
    title: &JsString,
    status: &str,
    input_summary: &JsString,
    output_summary: &JsString,
    options: &JsObject,
) {
    let mut steps = match trace.get("steps") {
        Some(JsValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let index_trace_id = trace
        .get("indexTraceId")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .unwrap_or_default();
    let mut step = fields(vec![
        (
            "stepId",
            JsValue::String(JsString::from(format!(
                "{index_trace_id}:step:{}",
                steps.len() + 1
            ))),
        ),
        ("kind", JsValue::String(kind.into())),
        ("title", JsValue::String(title.clone())),
        ("status", JsValue::String(status.into())),
        ("inputSummary", JsValue::String(input_summary.clone())),
        ("outputSummary", JsValue::String(output_summary.clone())),
    ]);
    for (source, target) in [
        ("refs", "refs"),
        ("metrics", "metrics"),
        ("details", "details"),
        ("promptDebug", "promptDebug"),
        ("titleI18n", "titleI18n"),
        ("inputSummaryI18n", "inputSummaryI18n"),
        ("outputSummaryI18n", "outputSummaryI18n"),
    ] {
        if let Some(value) = options.get(source)
            && js_truthy(value)
        {
            step.insert(target.into(), value.clone());
        }
    }
    steps.push(JsValue::Object(step));
    trace.insert("steps".into(), JsValue::Array(steps));
}

/// `createBatchTrace`: start a running trace for the batch. `started_at` is the
/// frozen-clock seam; production callers pass `text::now_iso()`.
pub fn create_batch_trace(
    session_key: &JsString,
    sessions: &[L0SessionRecord],
    trigger: &str,
    focus_user_turn_count: usize,
    started_at: &JsString,
) -> JsObject {
    let l0_ids: Vec<JsString> = sessions
        .iter()
        .map(|session| JsString::from(session.l0_index_id.as_str()))
        .collect();
    let mut timestamps: Vec<&str> = sessions
        .iter()
        .map(|session| session.timestamp.as_str())
        .filter(|timestamp| !timestamp.is_empty())
        .collect();
    timestamps.sort_by(|left, right| {
        left.encode_utf16()
            .collect::<Vec<_>>()
            .cmp(&right.encode_utf16().collect::<Vec<_>>())
    });
    let from_timestamp = timestamps.first().copied().unwrap_or("");
    let to_timestamp = timestamps.last().copied().unwrap_or("");

    let batch_summary = fields(vec![
        (
            "l0Ids",
            JsValue::Array(l0_ids.iter().cloned().map(JsValue::String).collect()),
        ),
        ("segmentCount", JsValue::Number(sessions.len() as f64)),
        (
            "focusUserTurnCount",
            JsValue::Number(focus_user_turn_count as f64),
        ),
        ("fromTimestamp", JsValue::String(from_timestamp.into())),
        ("toTimestamp", JsValue::String(to_timestamp.into())),
    ]);
    fields(vec![
        (
            "indexTraceId",
            JsValue::String(build_index_trace_id(session_key, started_at, &l0_ids)),
        ),
        ("sessionKey", JsValue::String(session_key.clone())),
        ("trigger", JsValue::String(trigger.into())),
        ("startedAt", JsValue::String(started_at.clone())),
        ("status", JsValue::String("running".into())),
        ("isNoOp", JsValue::Bool(false)),
        ("displayStatus", JsValue::String("Running".into())),
        ("batchSummary", JsValue::Object(batch_summary)),
        ("steps", JsValue::Array(Vec::new())),
        ("storedResults", JsValue::Array(Vec::new())),
    ])
}

// -- JS value helpers -------------------------------------------------------

fn field_string(value: &JsValue, key: &str) -> JsString {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

fn string_value(value: &JsValue, key: &str) -> JsValue {
    JsValue::String(field_string(value, key))
}

fn fields(entries: Vec<(&str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}
