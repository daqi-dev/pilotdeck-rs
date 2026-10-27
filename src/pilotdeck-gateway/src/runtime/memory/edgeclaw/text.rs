//! Pure text, id, General-workspace, trace and recall-context helpers.
//!
//! Ports the fixed TS baseline pieces owned by A6.6-edgeclaw-1:
//!
//! * `core/utils/text.ts` (`truncate`, escaped-unicode decoding,
//!   `normalizeText`, `scoreMatch`, `safeJsonParse`)
//! * `core/utils/id.ts` (sha1-based ids and `nowIso`)
//! * `core/general-projects.ts` (the `~/.pilotdeck` General workspace and
//!   external record/project ids)
//! * `core/trace-i18n.ts`
//! * `service.ts:429-541` (raw recall section parsing, recall system context,
//!   raw-context detection and case-trace preview normalization)
//!
//! JavaScript strings carry UTF-16 code units, including unpaired surrogates,
//! so every string input/output is a [`JsString`] instead of `&str`.

use crate::{
    js_value::to_string_units,
    metadata::resolve_units,
    runtime::memory::{js_is_space_unit, js_trim_units},
};
use icu_locale_core::LanguageIdentifier;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::path::{Path, PathBuf};

// -- `core/utils/text.ts` ---------------------------------------------------

pub fn truncate(text: &JsString, max_length: f64) -> JsString {
    if text.is_empty() {
        return JsString::default();
    }
    if max_length <= 0.0 || (text.len_utf16() as f64) <= max_length {
        return text.clone();
    }
    // `String.prototype.slice` applies ToIntegerOrInfinity: NaN becomes 0 and
    // fractions truncate. A lone surrogate may be split off deliberately.
    let end = if max_length.is_nan() {
        0
    } else if max_length.is_infinite() {
        text.len_utf16()
    } else {
        max_length.trunc() as usize
    };
    let mut units = text.units()[..end.min(text.len_utf16())].to_vec();
    units.extend("...".encode_utf16());
    JsString::from_utf16(units)
}

pub fn decode_escaped_unicode_text(text: &JsString, decode_common_escapes: bool) -> JsString {
    let units = text.units();
    let unicode = (0..units.len()).any(|i| is_unicode_escape(&units[i..]));
    let common = decode_common_escapes && (0..units.len()).any(|i| is_common_escape(&units[i..]));
    if !unicode && !common {
        return text.clone();
    }
    let mut next = if unicode {
        let mut out = Vec::with_capacity(units.len());
        let mut i = 0;
        while i < units.len() {
            if is_unicode_escape(&units[i..]) {
                let hex = &units[i + 2..i + 6];
                out.push(hex_value(hex));
                i += 6;
            } else {
                out.push(units[i]);
                i += 1;
            }
        }
        out
    } else {
        units.to_vec()
    };
    if common {
        // The TS replacements run in this order and each pass is global.
        for (from, to) in [
            ('r', '\r'),
            ('n', '\n'),
            ('t', '\t'),
            ('"', '"'),
            ('\\', '\\'),
        ] {
            next = replace_pair(&next, from as u16, to as u16);
        }
    }
    JsString::from_utf16(next)
}

pub fn decode_escaped_unicode_value(value: &JsValue, decode_common_escapes: bool) -> JsValue {
    // Explicit stack: the TS baseline recurses, so a deeply nested JSON value
    // overflows V8's call stack. Wire values may nest past any Rust stack too.
    enum Task<'a> {
        Visit(&'a JsValue),
        FinishArray(usize),
        FinishObject(Vec<JsString>),
    }
    let mut tasks = vec![Task::Visit(value)];
    let mut done: Vec<JsValue> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(JsValue::String(text)) => done.push(JsValue::String(
                decode_escaped_unicode_text(text, decode_common_escapes),
            )),
            Task::Visit(JsValue::Array(items)) => {
                tasks.push(Task::FinishArray(items.len()));
                tasks.extend(items.iter().rev().map(Task::Visit));
            }
            Task::Visit(JsValue::Object(object)) => {
                let entries = object.entries();
                tasks.push(Task::FinishObject(
                    entries.iter().map(|(key, _)| key.clone()).collect(),
                ));
                tasks.extend(entries.iter().rev().map(|(_, item)| Task::Visit(item)));
            }
            Task::Visit(other) => done.push(other.clone()),
            Task::FinishArray(len) => {
                let items = done.split_off(done.len() - len);
                done.push(JsValue::Array(items));
            }
            Task::FinishObject(keys) => {
                let values = done.split_off(done.len() - keys.len());
                let mut decoded = JsObject::new();
                for (key, value) in keys.into_iter().zip(values) {
                    decoded.insert(key, value);
                }
                done.push(JsValue::Object(decoded));
            }
        }
    }
    done.pop().expect("one decoded value")
}

/// `text.replace(/\s+/g, " ").trim()` over ECMAScript whitespace units.
pub fn normalize_text(text: &JsString) -> JsString {
    let mut out = Vec::with_capacity(text.units().len());
    let mut i = 0;
    let units = text.units();
    while i < units.len() {
        if js_is_space_unit(units[i]) {
            while i < units.len() && js_is_space_unit(units[i]) {
                i += 1;
            }
            out.push(u16::from(b' '));
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    JsString::from_utf16(js_trim_units(&out).to_vec())
}

pub fn score_match(query: &JsString, text: &JsString) -> f64 {
    let q = lowercase(&normalize_text(query));
    let t = lowercase(&normalize_text(text));
    let (q, t) = (q.units(), t.units());
    if q.is_empty() || t.is_empty() {
        return 0.0;
    }
    if t == q {
        return 1.0;
    }
    if t.starts_with(q) {
        return 0.92;
    }
    if contains(t, q) {
        return 0.82;
    }
    let words: Vec<&[u16]> = q.split(|unit| *unit == u16::from(b' ')).collect();
    let words: Vec<&[u16]> = words.into_iter().filter(|word| !word.is_empty()).collect();
    if words.is_empty() {
        return 0.0;
    }
    let hits = words.iter().filter(|word| contains(t, word)).count();
    let word_score = hits as f64 / words.len() as f64 * 0.7;
    let q_compact = without_space(q);
    let t_compact = without_space(t);
    if q_compact.len() < 2 || t_compact.len() < 2 {
        return word_score;
    }
    let mut gram_hits = 0usize;
    let mut grams = 0usize;
    for i in 0..q_compact.len() - 1 {
        grams += 1;
        if contains(&t_compact, &q_compact[i..i + 2]) {
            gram_hits += 1;
        }
    }
    let gram_score = if grams > 0 {
        gram_hits as f64 / grams as f64 * 0.75
    } else {
        0.0
    };
    word_score.max(gram_score)
}

pub fn safe_json_parse(raw: &JsString, fallback: JsValue) -> JsValue {
    parse_json(&json_text(raw.units())).unwrap_or(fallback)
}

// -- `core/utils/id.ts` -----------------------------------------------------

pub fn hash_text(input: &JsString) -> String {
    // Node hashes the UTF-8 encoding, which replaces each unpaired surrogate
    // with U+FFFD; `from_utf16_lossy` performs the same replacement.
    super::file_memory::helpers::hash(&String::from_utf16_lossy(input.units()))
}

pub fn now_iso() -> String {
    super::file_memory::now_iso()
}

/// `new Date(ms).toISOString()`, exposed for the frozen-clock fixture.
pub fn now_iso_at(epoch_ms: i64) -> String {
    pilotdeck_transport::system_clock::format_epoch_iso(epoch_ms)
}

pub fn build_l0_index_id(
    session_key: &JsString,
    timestamp: &JsString,
    payload: &JsString,
) -> JsString {
    let key = if session_key.is_empty() {
        JsString::from("session")
    } else {
        session_key.clone()
    };
    let mut seed = timestamp.units().to_vec();
    seed.push(u16::from(b':'));
    seed.extend(payload.units());
    let hash = hash_text(&JsString::from_utf16(seed));
    concat(&[&key, &JsString::from(format!("_{hash}_raw"))])
}

pub fn build_l1_index_id(timestamp: &JsString, source_ids: &[JsString]) -> JsString {
    let mut sorted: Vec<&JsString> = source_ids.iter().collect();
    // Array.prototype.sort default order is UTF-16 code-unit lexicographic.
    sorted.sort_by(|a, b| a.units().cmp(b.units()));
    let mut joined = Vec::new();
    for (i, id) in sorted.iter().enumerate() {
        if i > 0 {
            joined.push(u16::from(b','));
        }
        joined.extend(id.units());
    }
    let mut seed = timestamp.units().to_vec();
    seed.push(u16::from(b':'));
    seed.extend(joined);
    JsString::from(format!("l1_{}", hash_text(&JsString::from_utf16(seed))))
}

pub fn build_l2_time_index_id(date_key: &JsString) -> JsString {
    JsString::from(format!("time_{}", hash_text(date_key)))
}

pub fn build_l2_project_index_id(project_key: &JsString) -> JsString {
    JsString::from(format!("project_{}", hash_text(&lowercase(project_key))))
}

pub fn build_fact_id(fact_key: &JsString) -> JsString {
    JsString::from(format!("fact_{}", hash_text(&lowercase(fact_key))))
}

pub fn build_link_id(
    from_level: &JsString,
    from_id: &JsString,
    to_level: &JsString,
    to_id: &JsString,
) -> JsString {
    let mut seed = Vec::new();
    seed.extend(from_level.units());
    seed.push(u16::from(b':'));
    seed.extend(from_id.units());
    seed.extend("->".encode_utf16());
    seed.extend(to_level.units());
    seed.push(u16::from(b':'));
    seed.extend(to_id.units());
    JsString::from(format!("link_{}", hash_text(&JsString::from_utf16(seed))))
}

// -- `core/general-projects.ts` ---------------------------------------------

pub const GENERAL_PROJECT_META_DIR: &str = "GeneralProjects";
pub const GENERAL_PROJECT_MEMORY_DIR: &str = "Project";
pub const GENERAL_FEEDBACK_MEMORY_DIR: &str = "Feedback";
pub const EXTERNAL_RECORD_PREFIX: &str = "external:";
pub const EXTERNAL_PROJECT_PREFIX: &str = "external-project:";

/// `GENERAL_WORKSPACE_DIR = join(homedir(), ".pilotdeck")`, computed lazily so
/// an isolated `HOME` (or the passwd fallback) is observed per call.
pub fn general_workspace_dir() -> Result<JsString, super::file_memory::FileMemoryError> {
    Ok(general_workspace_dir_in(&general_workspace_path()?))
}

/// `os.homedir()/.pilotdeck` as a path; the shared source of the literal.
pub fn general_workspace_path() -> Result<PathBuf, super::file_memory::FileMemoryError> {
    Ok(super::storage::accessors::homedir()?.join(".pilotdeck"))
}

/// The same constant for an explicit `$HOME`, including an empty one.
pub fn general_workspace_dir_in(home: &std::path::Path) -> JsString {
    JsString::from(home.join(".pilotdeck").to_string_lossy().into_owned())
}

/// `path.resolve(workspacePath)` on POSIX, preserving UTF-16 units.
///
/// A relative path needs `process.cwd()`; `path.resolve` throws
/// `ENOENT: ... uv_cwd` when it is unavailable (Node probe: with the cwd
/// deleted, `/abs/x` still resolves but `rel/x` throws). Absolute inputs never
/// consult it and therefore keep working.
pub fn normalize_workspace_path(
    workspace_path: &JsString,
) -> Result<JsString, super::file_memory::FileMemoryError> {
    normalize_workspace_path_from(std::env::current_dir(), workspace_path)
}

/// Injection seam for the `process.cwd()` lookup, exercised by the failure test.
pub fn normalize_workspace_path_from(
    cwd: std::io::Result<PathBuf>,
    workspace_path: &JsString,
) -> Result<JsString, super::file_memory::FileMemoryError> {
    if workspace_path.units().first() == Some(&u16::from(b'/')) {
        return Ok(resolve_units("", workspace_path));
    }
    let cwd = cwd?;
    Ok(resolve_units(&cwd.to_string_lossy(), workspace_path))
}

pub fn is_general_workspace_dir(
    workspace_dir: &JsString,
) -> Result<bool, super::file_memory::FileMemoryError> {
    is_general_workspace_dir_with_home(workspace_dir, super::storage::accessors::homedir())
}

pub fn is_general_workspace_dir_in(
    workspace_dir: &JsString,
    home: &Path,
) -> Result<bool, super::file_memory::FileMemoryError> {
    is_general_workspace_dir_with_home(workspace_dir, Ok(home.to_path_buf()))
}

/// `isGeneralWorkspaceDir` with an injectable `os.homedir()`. The TS constant is
/// evaluated when `general-projects.ts` loads, so a failing `homedir()` rejects
/// the import instead of misclassifying every workspace (Node loader probe:
/// `IMPORT-ERR ENOENT ... uv_os_homedir`).
pub fn is_general_workspace_dir_with_home(
    workspace_dir: &JsString,
    home: Result<PathBuf, super::file_memory::FileMemoryError>,
) -> Result<bool, super::file_memory::FileMemoryError> {
    let general = general_workspace_dir_in(&home?);
    Ok(normalize_workspace_path(workspace_dir)?.units()
        == normalize_workspace_path(&general)?.units())
}

/// `isGeneralWorkspaceDir` for paths already made absolute by `absolute`:
/// both inputs are lexically canonical, so `path.resolve` equality is `==`.
pub fn is_general_workspace_path(workspace_dir: &Path, general: &Path) -> bool {
    workspace_dir == general
}

/// `getWorkspaceMemoryMode`: `"general"` for `~/.pilotdeck`, else `"single"`.
pub fn get_workspace_memory_mode(
    workspace_dir: &JsString,
) -> Result<&'static str, super::file_memory::FileMemoryError> {
    Ok(if is_general_workspace_dir(workspace_dir)? {
        "general"
    } else {
        "single"
    })
}

pub fn get_workspace_memory_mode_in(
    workspace_dir: &JsString,
    home: &Path,
) -> Result<&'static str, super::file_memory::FileMemoryError> {
    Ok(if is_general_workspace_dir_in(workspace_dir, home)? {
        "general"
    } else {
        "single"
    })
}

pub fn build_external_project_logical_id(
    workspace_path: &JsString,
    project_id: &JsString,
) -> Result<JsString, super::file_memory::FileMemoryError> {
    let mut seed = normalize_workspace_path(workspace_path)?.units().to_vec();
    seed.extend("::".encode_utf16());
    seed.extend(project_id.units());
    Ok(JsString::from(format!(
        "{EXTERNAL_PROJECT_PREFIX}{}",
        hash_text(&JsString::from_utf16(seed))
    )))
}

/// `buildExternalProjectLogicalId` for an already-absolute workspace path.
pub fn build_external_project_logical_id_path(
    workspace_path: &Path,
    project_id: &str,
) -> Result<String, super::file_memory::FileMemoryError> {
    let workspace = JsString::from(workspace_path.to_string_lossy().into_owned());
    let id = build_external_project_logical_id(&workspace, &JsString::from(project_id))?;
    id.to_utf8().map_err(|_| {
        super::file_memory::FileMemoryError::Invalid("external project id is not UTF-8".into())
    })
}

pub fn build_external_record_id(
    workspace_path: &JsString,
    relative_path: &JsString,
) -> Result<JsString, super::file_memory::FileMemoryError> {
    let hash = hash_text(&normalize_workspace_path(workspace_path)?);
    let prefix = JsString::from(format!("{EXTERNAL_RECORD_PREFIX}{hash}:"));
    let relative = JsString::from_utf16(
        relative_path
            .units()
            .iter()
            .map(|unit| {
                if *unit == u16::from(b'\\') {
                    u16::from(b'/')
                } else {
                    *unit
                }
            })
            .collect(),
    );
    Ok(concat(&[&prefix, &relative]))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalRecordId {
    pub workspace_key: JsString,
    pub relative_path: JsString,
}

pub fn parse_external_record_id(value: &JsString) -> Option<ExternalRecordId> {
    let units = value.units();
    let prefix = EXTERNAL_RECORD_PREFIX.encode_utf16().collect::<Vec<_>>();
    if !units.starts_with(&prefix) {
        return None;
    }
    let payload = &units[prefix.len()..];
    let separator = payload.iter().position(|unit| *unit == u16::from(b':'))?;
    if separator == 0 {
        return None;
    }
    let workspace_key = JsString::from_utf16(js_trim_units(&payload[..separator]).to_vec());
    let relative_path = JsString::from_utf16(
        js_trim_units(&payload[separator + 1..])
            .iter()
            .map(|unit| {
                if *unit == u16::from(b'\\') {
                    u16::from(b'/')
                } else {
                    *unit
                }
            })
            .collect(),
    );
    if workspace_key.is_empty() || relative_path.is_empty() {
        return None;
    }
    Some(ExternalRecordId {
        workspace_key,
        relative_path,
    })
}

pub fn is_external_record_id(value: &JsString) -> bool {
    value
        .units()
        .starts_with(&EXTERNAL_RECORD_PREFIX.encode_utf16().collect::<Vec<_>>())
}

// -- `core/trace-i18n.ts` ---------------------------------------------------

/// `String(value)` for a trace argument exceeded the join depth. V8 raises
/// `RangeError: Maximum call stack size exceeded` at the same point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceArgumentOverflow;
impl std::fmt::Display for TraceArgumentOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::js_value::StackOverflow::MESSAGE)
    }
}

pub fn trace_i18n(
    key: &JsString,
    fallback: &JsString,
    args: &[JsValue],
) -> Result<JsObject, TraceArgumentOverflow> {
    let mut out = JsObject::new();
    out.insert("key".into(), JsValue::String(key.clone()));
    if !args.is_empty() {
        let args = args
            .iter()
            .map(to_js_string)
            .collect::<Result<Vec<_>, _>>()?;
        out.insert("args".into(), JsValue::Array(args));
    }
    // Insertion order matters: key, optional args, fallback.
    out.insert("fallback".into(), JsValue::String(fallback.clone()));
    Ok(out)
}

fn to_js_string(value: &JsValue) -> Result<JsValue, TraceArgumentOverflow> {
    // ECMAScript `String(value)`: reuse the shared implementation, where
    // `Array.prototype.join` renders null/undefined elements as empty strings
    // (`String([1, null, 2]) === "1,,2"`).
    to_string_units(value)
        .map(|units| JsValue::String(JsString::from_utf16(units)))
        .map_err(|crate::js_value::StackOverflow| TraceArgumentOverflow)
}

// -- `service.ts:429-541` ---------------------------------------------------

#[derive(Default, Debug, PartialEq, Eq)]
pub struct CleanedRecallSections {
    pub user_profile: Vec<JsString>,
    pub project_meta: Vec<JsString>,
    pub project_memory: Vec<JsString>,
    pub feedback_memory: Vec<JsString>,
}

#[derive(Clone, Copy)]
enum RecallSection {
    User,
    ProjectMeta,
    Project,
    Feedback,
}

pub fn parse_raw_recall_sections(value: &JsString) -> CleanedRecallSections {
    let mut sections = CleanedRecallSections::default();
    let mut current: Option<RecallSection> = None;
    let mut buffer: Vec<JsString> = Vec::new();
    let flush = |sections: &mut CleanedRecallSections,
                 current: Option<RecallSection>,
                 buffer: &mut Vec<JsString>| {
        if let Some(section) = current {
            let content = trim_js(&join_lines(buffer.iter().map(JsString::units)));
            if !content.is_empty() {
                match section {
                    RecallSection::User => sections.user_profile.push(content),
                    RecallSection::ProjectMeta => sections.project_meta.push(content),
                    RecallSection::Project => sections.project_memory.push(content),
                    RecallSection::Feedback => sections.feedback_memory.push(content),
                }
            }
        }
        buffer.clear();
    };
    for raw_line in split_lines(value) {
        let line = trim_js(&raw_line);
        let header = if starts_with(&line, "### [user]") {
            Some(RecallSection::User)
        } else if starts_with(&line, "### [project_meta]") {
            Some(RecallSection::ProjectMeta)
        } else if starts_with(&line, "### [project]") {
            Some(RecallSection::Project)
        } else if starts_with(&line, "### [feedback]") {
            Some(RecallSection::Feedback)
        } else {
            None
        };
        if let Some(section) = header {
            flush(&mut sections, current, &mut buffer);
            current = Some(section);
            continue;
        }
        if current.is_none() {
            continue;
        }
        if line.is_empty() {
            buffer.push(JsString::default());
            continue;
        }
        if line.equals_str("## ClawXMemory Recall")
            || starts_with(&line, "route=")
            || line.equals_str(
                "Treat these file memories as the authoritative long-term memory for this turn when relevant.",
            )
        {
            continue;
        }
        buffer.push(raw_line);
    }
    flush(&mut sections, current, &mut buffer);
    sections
}

fn join_recall_bodies(bodies: &[JsString]) -> Option<JsString> {
    let bodies: Vec<JsString> = bodies
        .iter()
        .map(trim_js)
        .filter(|body| !body.is_empty())
        .collect();
    if bodies.is_empty() {
        return None;
    }
    Some(join_text(&bodies, "\n\n---\n\n"))
}

pub fn build_memory_recall_system_context(evidence_block: &JsString) -> JsString {
    let sections = parse_raw_recall_sections(&trim_js(evidence_block));
    let mut blocks: Vec<JsString> = Vec::new();
    for (header, bodies) in [
        ("## User Profile", &sections.user_profile),
        ("## Project Meta", &sections.project_meta),
        ("## Project Memory", &sections.project_memory),
        ("## Feedback Memory", &sections.feedback_memory),
    ] {
        if bodies.is_empty() {
            continue;
        }
        let body = join_recall_bodies(bodies).expect("non-empty section");
        blocks.push(join_text(&[JsString::from(header), body], "\n\n"));
    }
    if blocks.is_empty() {
        return JsString::default();
    }
    let mut parts = vec![
        JsString::from("## ClawXMemory Recall"),
        JsString::from("These are retrieved long-term memory references for the current turn."),
        JsString::from("Some content may be relevant while some may not be directly useful."),
        JsString::from("Use only the parts that are relevant to the current question."),
        JsString::from(
            "If retrieved memory conflicts with explicit new user instructions in the current turn, follow the current-turn user instructions.",
        ),
    ];
    parts.extend(blocks);
    join_text(&parts, "\n\n")
}

pub fn looks_like_raw_recall_context(value: Option<&JsString>) -> bool {
    let normalized = trim_js(value.unwrap_or(&JsString::default()));
    if normalized.is_empty() {
        return false;
    }
    line_starts_with(&normalized, "route=")
        || [
            "### [user]",
            "### [project_meta]",
            "### [project]",
            "### [feedback]",
        ]
        .iter()
        .any(|header| line_starts_with(&normalized, header))
}

pub fn normalize_case_trace_context_preview(record: &JsObject) -> JsObject {
    let retrieval = record.get("retrieval").and_then(JsValue::as_object);
    let preview = retrieval
        .and_then(|retrieval| retrieval.get("contextPreview"))
        .and_then(JsValue::as_string)
        .filter(|preview| !preview.is_empty());
    let (Some(retrieval), Some(preview)) = (retrieval, preview) else {
        return record.clone();
    };
    if !looks_like_raw_recall_context(Some(preview)) {
        return record.clone();
    }
    let mut next_retrieval = retrieval.clone();
    next_retrieval.insert(
        "contextPreview".into(),
        JsValue::String(build_memory_recall_system_context(preview)),
    );
    let mut next = record.clone();
    next.insert("retrieval".into(), JsValue::Object(next_retrieval));
    next
}

// -- UTF-16 helpers ---------------------------------------------------------

fn is_unicode_escape(units: &[u16]) -> bool {
    units.len() >= 6
        && units[0] == u16::from(b'\\')
        && units[1] == u16::from(b'u')
        && units[2..6]
            .iter()
            .all(|unit| matches!(unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66))
}

fn is_common_escape(units: &[u16]) -> bool {
    units.len() >= 2
        && units[0] == u16::from(b'\\')
        && matches!(units[1], 0x6e | 0x72 | 0x74 | 0x22 | 0x5c)
}

fn hex_value(units: &[u16]) -> u16 {
    u16::from_str_radix(&String::from_utf16_lossy(units), 16).expect("four hex digits")
}

fn replace_pair(units: &[u16], from: u16, to: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        if units[i] == u16::from(b'\\') && units.get(i + 1) == Some(&from) {
            out.push(to);
            i += 2;
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    out
}

/// `String.prototype.toLowerCase` (locale-independent), reusing the catalog
/// projection's canonical case mapper.
pub(crate) fn lowercase(value: &JsString) -> JsString {
    crate::model_catalog::lower(value, &LanguageIdentifier::UNKNOWN, false)
}

fn without_space(units: &[u16]) -> Vec<u16> {
    units
        .iter()
        .copied()
        .filter(|unit| !js_is_space_unit(*unit))
        .collect()
}

pub(crate) fn contains(haystack: &[u16], needle: &[u16]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

pub(crate) fn starts_with(value: &JsString, prefix: &str) -> bool {
    value
        .units()
        .starts_with(&prefix.encode_utf16().collect::<Vec<_>>())
}

pub(crate) fn trim_js(value: &JsString) -> JsString {
    JsString::from_utf16(js_trim_units(value.units()).to_vec())
}

fn concat(parts: &[&JsString]) -> JsString {
    join_text_refs(parts.iter().copied(), "")
}

pub(crate) fn join_lines<'a>(parts: impl IntoIterator<Item = &'a [u16]>) -> JsString {
    let mut units = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend(part);
    }
    JsString::from_utf16(units)
}

pub(crate) fn join_text(parts: &[JsString], separator: &str) -> JsString {
    join_text_refs(parts, separator)
}

/// [`join_text`] over borrowed parts, so callers that already hold `&JsString`
/// values do not clone just to join.
pub(crate) fn join_text_refs<'a>(
    parts: impl IntoIterator<Item = &'a JsString>,
    separator: &str,
) -> JsString {
    let mut units = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend(part.units());
    }
    JsString::from_utf16(units)
}

fn split_lines(value: &JsString) -> Vec<JsString> {
    let units = value.units();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, unit) in units.iter().enumerate() {
        if *unit == u16::from(b'\n') {
            let end = if i > start && units[i - 1] == u16::from(b'\r') {
                i - 1
            } else {
                i
            };
            out.push(JsString::from_utf16(units[start..end].to_vec()));
            start = i + 1;
        }
    }
    out.push(JsString::from_utf16(units[start..].to_vec()));
    out
}

fn line_starts_with(value: &JsString, prefix: &str) -> bool {
    let prefix = prefix.encode_utf16().collect::<Vec<_>>();
    let units = value.units();
    for i in 0..units.len() {
        let line_start = i == 0 || matches!(units[i - 1], 0x000a | 0x000d | 0x2028 | 0x2029);
        if line_start && units[i..].starts_with(&prefix) {
            return true;
        }
    }
    false
}

/// UTF-16 JSON text as a Rust string for `parse_json`. An unpaired surrogate is
/// only legal inside a JSON string, where `\uXXXX` denotes the same unit; one
/// that would follow an escaping backslash must stay invalid, so it becomes
/// U+FFFD instead of a `\uXXXX` escape (odd/even backslash parity, as V8 and
/// `JSON.parse` see it). Anywhere else both forms are a syntax error. Shared by
/// index-side model output (#768 fix-r3), Dream model output (#771 fix-r3),
/// `safeJsonParse` and sender hints.
pub(crate) fn json_text(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut backslashes = 0usize;
    for item in char::decode_utf16(units.iter().copied()) {
        match item {
            Ok(c) => {
                out.push(c);
                backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            }
            Err(error) => {
                if backslashes % 2 == 1 {
                    out.push('\u{FFFD}');
                } else {
                    out.push_str(&format!("\\u{:04x}", error.unpaired_surrogate()));
                }
                backslashes = 0;
            }
        }
    }
    out
}
