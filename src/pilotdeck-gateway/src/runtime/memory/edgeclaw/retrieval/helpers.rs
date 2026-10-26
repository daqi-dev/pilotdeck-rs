//! Pure retrieval helpers owned by A6.6-edgeclaw-9.
//!
//! Ports the helper block of the fixed TS baseline
//! `core/retrieval/reasoning-loop.ts:23-363`:
//! query-key normalization, trace/step construction, recall-context rendering,
//! CJK tokenization with the stopword set, project shortlisting, trace detail
//! construction and fallback selection.
//!
//! Every ambient input is injected: `Date.now()` becomes `now_ms` and
//! `Math.random().toString(36).slice(2, 10)` becomes `random_token`.
//! `localeCompare` accepts a caller-provided collator so sorting never reads
//! the process locale here. JavaScript strings stay [`JsString`] (UTF-16 code
//! units) end to end.

use super::text::{hash_text, now_iso_at};
use crate::js_value::to_string_units;
use crate::project_files::node_compare_utf16;
use crate::runtime::memory::{js_is_space_unit, js_trim_units};
use icu_collator::CollatorBorrowed;
use icu_locale_core::LanguageIdentifier;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::OnceLock;

/// `GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT` in the TS baseline.
pub const GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT: usize = 30;

pub type MemoryMessage = JsObject;
pub type MemoryUserSummary = JsObject;
pub type ProjectMetaRecord = JsObject;
pub type ReadableProjectCatalogEntry = JsObject;
pub type ProjectShortlistCandidate = JsObject;
pub type RetrievalTrace = JsObject;
pub type RecallHeaderEntry = JsObject;

// -- shared JS primitives ---------------------------------------------------

/// `String.prototype.toLowerCase` (locale-independent), reusing the catalog
/// projection's canonical case mapper.
fn lowercase(value: &JsString) -> JsString {
    crate::model_catalog::lower(value, &LanguageIdentifier::UNKNOWN, false)
}

fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(js_trim_units(value.units()).to_vec())
}

fn contains(haystack: &[u16], needle: &[u16]) -> bool {
    needle.is_empty()
        || (haystack.len() >= needle.len()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle))
}

fn is_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(flag) => *flag,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn text_field<'a>(object: &'a JsObject, key: &str) -> Option<&'a JsString> {
    object.get(key).and_then(JsValue::as_string)
}

fn string_field(object: &JsObject, key: &str) -> JsString {
    text_field(object, key).cloned().unwrap_or_default()
}

/// `${name}` where a missing field is only reachable through untyped input;
/// the typed TS record always carries these strings.
fn template_string(object: &JsObject, key: &str) -> JsString {
    string_field(object, key)
}

fn first_file(user_summary: &JsObject) -> Option<&JsObject> {
    match user_summary.get("files") {
        Some(JsValue::Array(items)) => items.first().and_then(JsValue::as_object),
        _ => None,
    }
}

fn identity_background(user_summary: &JsObject) -> &[JsValue] {
    match user_summary.get("identityBackground") {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}

fn join_lines(lines: &[JsString]) -> JsString {
    let mut units = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend(line.units());
    }
    JsString::from_utf16(units)
}

fn concat(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend(part.units());
    }
    JsString::from_utf16(units)
}

fn join_text(parts: &[JsString], separator: &str) -> JsString {
    let separator: Vec<u16> = separator.encode_utf16().collect();
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.extend(&separator);
        }
        units.extend(part.units());
    }
    JsString::from_utf16(units)
}

// -- reasoning-loop.ts:54-112 ----------------------------------------------

/// `normalizeQueryKey`: lowercase, collapse `\s+` to one space, then trim.
pub fn normalize_query_key(query: &JsString) -> JsString {
    let lowered = lowercase(query);
    let mut units = Vec::new();
    let mut pending_space = false;
    for &unit in lowered.units() {
        if js_is_space_unit(unit) {
            pending_space = true;
        } else {
            if pending_space && !units.is_empty() {
                units.push(u16::from(b' '));
            }
            pending_space = false;
            units.push(unit);
        }
    }
    JsString::from_utf16(units)
}

/// `buildTraceId`: `${prefix}_${hashText(`${seed}:${now}:${random}`)}`.
///
/// `now_ms` is the injected `Date.now()` and `random_token` is the already
/// sliced `Math.random().toString(36).slice(2, 10)` string; the conversion
/// itself stays at the JS boundary.
pub fn build_trace_id(
    prefix: &JsString,
    seed: &JsString,
    now_ms: i64,
    random_token: &JsString,
) -> JsString {
    let mut seed_units = seed.units().to_vec();
    seed_units.push(u16::from(b':'));
    seed_units.extend(now_ms.to_string().encode_utf16());
    seed_units.push(u16::from(b':'));
    seed_units.extend(random_token.units());
    let hash = hash_text(&JsString::from_utf16(seed_units));
    let mut units = prefix.units().to_vec();
    units.push(u16::from(b'_'));
    units.extend(hash.encode_utf16());
    JsString::from_utf16(units)
}

fn with_optional(entries: &mut Vec<(JsString, JsValue)>, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value
        && is_truthy(value)
    {
        entries.push((JsString::from(key), value.clone()));
    }
}

fn into_object(mut entries: Vec<(JsString, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries.drain(..) {
        object.insert(key, value);
    }
    object
}

// -- reasoning-loop.ts:114-213 ---------------------------------------------

pub fn has_user_summary(user_summary: &JsObject) -> bool {
    !identity_background(user_summary).is_empty()
}

/// `renderUserSummaryBlock`, including the default profile path.
pub fn render_user_summary_block(user_summary: &JsObject) -> Vec<JsString> {
    if !has_user_summary(user_summary) {
        return Vec::new();
    }
    let file = first_file(user_summary);
    let updated_at = file
        .map(|file| string_field(file, "updatedAt"))
        .unwrap_or_default();
    let relative_path = file
        .map(|file| string_field(file, "relativePath"))
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| JsString::from("global/UserIdentity/user-profile.md"));
    let mut header = vec![JsString::from("### [user] "), relative_path];
    if !updated_at.is_empty() {
        header.push(JsString::from(" ("));
        header.push(updated_at);
        header.push(JsString::from(")"));
    }
    let mut lines = vec![
        concat(&header.iter().collect::<Vec<_>>()),
        JsString::from("## 身份背景"),
    ];
    let background = identity_background(user_summary);
    if background.is_empty() {
        lines.push(JsString::from("- 暂无稳定用户画像信息。"));
    } else {
        let prefix = JsString::from("- ");
        for item in background {
            let text = match item {
                JsValue::String(text) => text.clone(),
                JsValue::Undefined | JsValue::Null => JsString::default(),
                other => JsString::from_utf16(to_string_units(other).unwrap_or_default()),
            };
            lines.push(concat(&[&prefix, &text]));
        }
    }
    lines.push(JsString::default());
    lines
}

/// `renderProjectMetaBlock`; `None` corresponds to the TS `null` branch.
pub fn render_project_meta_block(project_meta: Option<&JsObject>) -> Vec<JsString> {
    let Some(project_meta) = project_meta else {
        return Vec::new();
    };
    let description = match text_field(project_meta, "description") {
        Some(description) if !description.is_empty() => description.clone(),
        _ => JsString::from("No project description yet."),
    };
    vec![
        concat(&[
            &JsString::from("### [project_meta] "),
            &template_string(project_meta, "relativePath"),
            &JsString::from(" ("),
            &template_string(project_meta, "updatedAt"),
            &JsString::from(")"),
        ]),
        JsString::from("## Project Name"),
        template_string(project_meta, "projectName"),
        JsString::default(),
        JsString::from("## Description"),
        description,
        JsString::default(),
        JsString::from("## Status"),
        template_string(project_meta, "status"),
        JsString::default(),
    ]
}

/// `renderSelectedRecordsBlock`: `content.trim()` per record.
pub fn render_selected_records_block(records: &[JsObject]) -> Vec<JsString> {
    let mut lines = Vec::new();
    for record in records {
        lines.push(concat(&[
            &JsString::from("### ["),
            &template_string(record, "type"),
            &JsString::from("] "),
            &template_string(record, "relativePath"),
            &JsString::from(" ("),
            &template_string(record, "updatedAt"),
            &JsString::from(")"),
        ]));
        lines.push(trim(&string_field(record, "content")));
        lines.push(JsString::default());
    }
    lines
}

/// `renderContext`: route dispatch, user-file de-duplication and final trim.
pub fn render_context(
    route: &JsString,
    user_summary: &JsObject,
    project_meta: Option<&JsObject>,
    records: &[JsObject],
) -> JsString {
    let user_paths: Vec<JsString> = match user_summary.get("files") {
        Some(JsValue::Array(files)) => files
            .iter()
            .filter_map(JsValue::as_object)
            .map(|file| string_field(file, "relativePath"))
            .collect(),
        _ => Vec::new(),
    };
    let unique_records: Vec<JsObject> = records
        .iter()
        .filter(|record| !user_paths.contains(&string_field(record, "relativePath")))
        .cloned()
        .collect();
    let mut lines = vec![
        JsString::from("## ClawXMemory Recall"),
        concat(&[&JsString::from("route="), route]),
        JsString::default(),
    ];
    if route.equals_str("user") {
        if !has_user_summary(user_summary) {
            return JsString::default();
        }
        lines.extend(render_user_summary_block(user_summary));
    } else if route.equals_str("project") {
        if project_meta.is_none() && unique_records.is_empty() {
            return JsString::default();
        }
        lines.extend(render_project_meta_block(project_meta));
        lines.extend(render_selected_records_block(&unique_records));
    } else if route.equals_str("mix") {
        if !has_user_summary(user_summary) && project_meta.is_none() && unique_records.is_empty() {
            return JsString::default();
        }
        lines.extend(render_user_summary_block(user_summary));
        lines.extend(render_project_meta_block(project_meta));
        lines.extend(render_selected_records_block(&unique_records));
    } else {
        return JsString::default();
    }
    lines.push(JsString::from(
        "Treat these file memories as the authoritative long-term memory for this turn when relevant.",
    ));
    trim(&join_lines(&lines))
}

/// `recentUserMessages`: user role only, last four in order.
pub fn recent_user_messages(messages: Option<&[JsObject]>) -> Vec<JsObject> {
    let users: Vec<JsObject> = messages
        .unwrap_or_default()
        .iter()
        .filter(|message| text_field(message, "role").is_some_and(|role| role.equals_str("user")))
        .cloned()
        .collect();
    users[users.len().saturating_sub(4)..].to_vec()
}

// -- reasoning-loop.ts:215-305 ---------------------------------------------

const CJK_TOKEN_STOPWORDS: &[&str] = &[
    "项目", "当前", "这个", "那个", "现在", "一下", "关于", "里面", "这里", "那里", "怎么", "怎样",
    "如何", "什么", "哪些", "进展", "情况", "默认", "应该", "需要", "general",
];

fn han_regex() -> &'static regex::Regex {
    static HAN: OnceLock<regex::Regex> = OnceLock::new();
    HAN.get_or_init(|| regex::Regex::new(r"\p{Script=Han}").expect("Unicode Han script class"))
}

fn letter_or_number_regex() -> &'static regex::Regex {
    static LETTER_OR_NUMBER: OnceLock<regex::Regex> = OnceLock::new();
    LETTER_OR_NUMBER.get_or_init(|| {
        regex::Regex::new(r"^[\p{L}\p{N}]$").expect("Unicode letter/number classes")
    })
}

fn is_han(character: char) -> bool {
    let mut buffer = [0u8; 4];
    han_regex().is_match(character.encode_utf8(&mut buffer))
}

fn is_letter_or_number(character: char) -> bool {
    let mut buffer = [0u8; 4];
    letter_or_number_regex().is_match(character.encode_utf8(&mut buffer))
}

fn decode_span(units: &[u16], index: usize) -> (Option<char>, usize) {
    let mut decoded = char::decode_utf16(units[index..].iter().copied());
    match decoded.next() {
        Some(Ok(character)) => (Some(character), character.len_utf16()),
        Some(Err(_)) => (None, 1),
        None => (None, 0),
    }
}

/// `expandCjkSearchToken`: Han runs keep up to eight units plus every 2- and
/// 3-unit slice; slices operate on UTF-16 units like `String#slice`.
pub fn expand_cjk_search_token(token: &JsString) -> Vec<JsString> {
    let units = token.units();
    let mut has_han = false;
    let mut index = 0;
    while index < units.len() {
        let (character, width) = decode_span(units, index);
        if character.is_some_and(is_han) {
            has_han = true;
            break;
        }
        index += width;
    }
    if !has_han {
        return vec![token.clone()];
    }
    let mut spans: Vec<(bool, usize, usize)> = Vec::new();
    index = 0;
    while index < units.len() {
        let (character, width) = decode_span(units, index);
        let han = character.is_some_and(is_han);
        if let Some(last) = spans.last_mut()
            && last.0 == han
        {
            last.2 = index + width;
        } else {
            spans.push((han, index, index + width));
        }
        index += width;
    }
    let mut expanded = Vec::new();
    for (han, start, end) in spans {
        let piece = &units[start..end];
        if !han {
            expanded.push(JsString::from_utf16(piece.to_vec()));
            continue;
        }
        if piece.len() <= 8 {
            expanded.push(JsString::from_utf16(piece.to_vec()));
        }
        for size in [2usize, 3] {
            if piece.len() < size {
                continue;
            }
            for offset in 0..=piece.len() - size {
                expanded.push(JsString::from_utf16(piece[offset..offset + size].to_vec()));
            }
        }
    }
    expanded
}

/// `tokenizeSearchText`: lowercase, replace non-letter/number/space runs, split
/// on JS whitespace, expand Han tokens, drop short/stopword tokens, preserving
/// first-seen order like `Array.from(new Set(...))`.
pub fn tokenize_search_text(value: &JsString) -> Vec<JsString> {
    let lowered = lowercase(value);
    let mut cleaned = Vec::with_capacity(lowered.len_utf16());
    let mut index = 0;
    while index < lowered.units().len() {
        let (character, width) = decode_span(lowered.units(), index);
        match character {
            Some(character) if is_letter_or_number(character) => {
                let mut buffer = [0u16; 2];
                cleaned.extend_from_slice(character.encode_utf16(&mut buffer));
            }
            Some(character)
                if character.len_utf16() == 1 && js_is_space_unit(character as u32 as u16) =>
            {
                cleaned.push(character as u32 as u16);
            }
            _ => cleaned.push(u16::from(b' ')),
        }
        index += width;
    }
    let mut raw_tokens: Vec<JsString> = Vec::new();
    let mut current = Vec::new();
    for unit in cleaned {
        if js_is_space_unit(unit) {
            if !current.is_empty() {
                raw_tokens.push(JsString::from_utf16(std::mem::take(&mut current)));
            }
        } else {
            current.push(unit);
        }
    }
    if !current.is_empty() {
        raw_tokens.push(JsString::from_utf16(current));
    }
    let mut expanded: Vec<JsString> = raw_tokens
        .iter()
        .flat_map(expand_cjk_search_token)
        .filter(|token| token.len_utf16() >= 2)
        .collect();
    let mut unique: Vec<JsString> = Vec::new();
    for token in expanded.drain(..) {
        if !unique.contains(&token) {
            unique.push(token);
        }
    }
    unique
        .into_iter()
        .filter(|token| {
            !CJK_TOKEN_STOPWORDS
                .iter()
                .any(|stopword| token.equals_str(stopword))
        })
        .collect()
}

fn matched_tokens(haystack: &JsString, tokens: &[JsString]) -> Vec<JsString> {
    tokens
        .iter()
        .filter(|token| contains(haystack.units(), token.units()))
        .cloned()
        .collect()
}

/// `buildProjectShortlist`, including `localeCompare` ordering through the
/// caller's collator (descending `updatedAt` after score/exact).
pub fn build_project_shortlist(
    catalog: &[ReadableProjectCatalogEntry],
    query: &JsString,
    recent_messages: &[MemoryMessage],
    collator: &CollatorBorrowed<'static>,
) -> Vec<ProjectShortlistCandidate> {
    let query_tokens = tokenize_search_text(query);
    let recent_text = join_text(
        &recent_messages
            .iter()
            .map(|message| string_field(message, "content"))
            .collect::<Vec<_>>(),
        " ",
    );
    let recency_tokens = tokenize_search_text(&recent_text);
    let mut all_tokens: Vec<JsString> = Vec::new();
    for token in query_tokens.iter().chain(recency_tokens.iter()) {
        if !all_tokens.contains(token) {
            all_tokens.push(token.clone());
        }
    }
    let query_lower = lowercase(query);
    let mut candidates: Vec<ProjectShortlistCandidate> = catalog
        .iter()
        .map(|entry| {
            let project_name = template_string(entry, "projectName");
            let description = template_string(entry, "description");
            let haystack = lowercase(&concat(&[
                &project_name,
                &JsString::from(" "),
                &description,
            ]));
            let exact = if contains(query_lower.units(), lowercase(&project_name).units()) {
                2
            } else {
                0
            };
            let matched = matched_tokens(&haystack, &all_tokens);
            let score = exact * 10 + matched.len() as i64;
            let latest = entry
                .get("summary")
                .and_then(JsValue::as_object)
                .and_then(|summary| text_field(summary, "latestMemoryAt"))
                .filter(|value| !value.is_empty());
            let updated_at = latest
                .cloned()
                .unwrap_or_else(|| template_string(entry, "updatedAt"));
            let source_type = if text_field(entry, "sourceType")
                .is_some_and(|value| value.equals_str("workspace_external"))
            {
                "workspace_external"
            } else {
                "general_local"
            };
            into_object(vec![
                (
                    JsString::from("projectId"),
                    JsValue::String(template_string(entry, "logicalProjectId")),
                ),
                (JsString::from("projectName"), JsValue::String(project_name)),
                (JsString::from("description"), JsValue::String(description)),
                (
                    JsString::from("status"),
                    JsValue::String(template_string(entry, "status")),
                ),
                (JsString::from("updatedAt"), JsValue::String(updated_at)),
                (JsString::from("sourceType"), JsValue::from(source_type)),
                (JsString::from("score"), JsValue::Number(score as f64)),
                (JsString::from("exact"), JsValue::Number(exact as f64)),
                (
                    JsString::from("source"),
                    JsValue::from(if exact > 0 || !matched.is_empty() {
                        "query"
                    } else {
                        "recent"
                    }),
                ),
                (
                    JsString::from("matchedText"),
                    JsValue::String(join_text(&matched, ", ")),
                ),
            ])
        })
        .collect();
    candidates.sort_by(|left, right| {
        let left_score = left
            .get("score")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let right_score = right
            .get("score")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let left_exact = left
            .get("exact")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let right_exact = right
            .get("exact")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        right_score
            .total_cmp(&left_score)
            .then_with(|| right_exact.total_cmp(&left_exact))
            .then_with(|| {
                node_compare_utf16(
                    collator,
                    &string_field(right, "updatedAt"),
                    &string_field(left, "updatedAt"),
                )
            })
    });
    candidates.truncate(GENERAL_RECALL_PROJECT_CANDIDATE_LIMIT);
    candidates
}

// -- reasoning-loop.ts:307-362 ---------------------------------------------

/// `createTrace`: clock and random token are injected.
pub fn create_trace(
    query: &JsString,
    mode: &JsString,
    now_ms: i64,
    random_token: &JsString,
) -> RetrievalTrace {
    let timestamp = JsString::from(now_iso_at(now_ms));
    into_object(vec![
        (
            JsString::from("traceId"),
            JsValue::String(build_trace_id(
                &JsString::from("trace"),
                query,
                now_ms,
                random_token,
            )),
        ),
        (JsString::from("query"), JsValue::String(query.clone())),
        (JsString::from("mode"), JsValue::String(mode.clone())),
        (
            JsString::from("startedAt"),
            JsValue::String(timestamp.clone()),
        ),
        (JsString::from("finishedAt"), JsValue::String(timestamp)),
        (JsString::from("steps"), JsValue::Array(Vec::new())),
    ])
}

#[derive(Default)]
pub struct StepOptions {
    pub refs: Option<JsValue>,
    pub metrics: Option<JsValue>,
    pub details: Option<JsValue>,
    pub prompt_debug: Option<JsValue>,
    pub title_i18n: Option<JsValue>,
    pub input_summary_i18n: Option<JsValue>,
    pub output_summary_i18n: Option<JsValue>,
}

/// `pushStep`: append the next `stepId` and only the present, truthy options.
pub fn push_step(
    trace: &mut RetrievalTrace,
    kind: &JsString,
    title: &JsString,
    status: &JsString,
    input_summary: &JsString,
    output_summary: &JsString,
    options: &StepOptions,
) {
    let trace_id = string_field(trace, "traceId");
    let mut steps = match trace.get("steps") {
        Some(JsValue::Array(steps)) => steps.clone(),
        _ => Vec::new(),
    };
    let mut step_id = trace_id.units().to_vec();
    step_id.extend(":step:".encode_utf16());
    step_id.extend((steps.len() + 1).to_string().encode_utf16());
    let mut entries = vec![
        (
            JsString::from("stepId"),
            JsValue::String(JsString::from_utf16(step_id)),
        ),
        (JsString::from("kind"), JsValue::String(kind.clone())),
        (JsString::from("title"), JsValue::String(title.clone())),
        (JsString::from("status"), JsValue::String(status.clone())),
        (
            JsString::from("inputSummary"),
            JsValue::String(input_summary.clone()),
        ),
        (
            JsString::from("outputSummary"),
            JsValue::String(output_summary.clone()),
        ),
    ];
    with_optional(&mut entries, "refs", options.refs.as_ref());
    with_optional(&mut entries, "metrics", options.metrics.as_ref());
    with_optional(&mut entries, "details", options.details.as_ref());
    with_optional(&mut entries, "promptDebug", options.prompt_debug.as_ref());
    with_optional(&mut entries, "titleI18n", options.title_i18n.as_ref());
    with_optional(
        &mut entries,
        "inputSummaryI18n",
        options.input_summary_i18n.as_ref(),
    );
    with_optional(
        &mut entries,
        "outputSummaryI18n",
        options.output_summary_i18n.as_ref(),
    );
    steps.push(JsValue::Object(into_object(entries)));
    trace.insert(JsString::from("steps"), JsValue::Array(steps));
}

/// `fallbackSelection`: one id for the user route, otherwise at most three.
pub fn fallback_selection(route: &JsString, manifest: &[RecallHeaderEntry]) -> Vec<JsString> {
    if manifest.is_empty() {
        return Vec::new();
    }
    let limit = if route.equals_str("user") { 1 } else { 3 };
    manifest
        .iter()
        .take(limit)
        .map(|entry| template_string(entry, "relativePath"))
        .collect()
}
