//! Pure dream-review helpers (`core/review/dream-review.ts:73-531`).
//!
//! Every string input/output stays UTF-16 ([`JsString`]); records are the
//! shared camelCase [`JsObject`] codec. `localeCompare` sorts reuse the
//! gateway's pinned Node/ICU collator. Nondeterminism is injected: callers
//! pass the frozen clock and the random values (or the already-derived
//! `Math.random().toString(36).slice(2, 10)` fragment for trace ids).
use crate::runtime::memory::edgeclaw::{
    file_memory::{FileMemoryError, FileMemoryStore},
    storage::js_truthy,
    text::{hash_text, normalize_text},
};
use crate::{
    js_value::{ascii_fold_haystack, number_string, to_string_units},
    project_files::{node_collator, node_compare_utf16, node_locale},
    runtime::memory::js_trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

const DREAM_USER_NOTE_MAX_FILES: usize = 200;
const DREAM_USER_NOTE_CHAR_BUDGET: f64 = 120_000.0;
const GENERAL_MERGE_REASON: &str = "Model identified these project metas as duplicates.";

fn put(object: &mut JsObject, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value
        && !value.is_undefined()
    {
        object.insert(key.into(), value.clone());
    }
}

fn put_truthy(object: &mut JsObject, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value
        && js_truthy(value)
    {
        object.insert(key.into(), value.clone());
    }
}

fn string_field(object: &JsObject, key: &str) -> Option<JsString> {
    object.get(key).and_then(JsValue::as_string).cloned()
}

/// TS `normalizeWhitespace(value)`: `(value ?? "").replace(/\s+/g, " ").trim()`.
/// Nullish values normalize to empty; any other non-string reaches
/// `String.prototype.replace` and throws the V8 `TypeError` instead of being
/// silently normalized to empty (`dream-review.ts:224-226`).
fn normalize_whitespace(value: Option<&JsValue>) -> Result<JsString, FileMemoryError> {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => Ok(JsString::default()),
        Some(JsValue::String(value)) => Ok(normalize_text(value)),
        Some(_) => Err(type_error("(value ?? \"\").replace is not a function")),
    }
}

fn type_error(message: &'static str) -> FileMemoryError {
    FileMemoryError::Type(message.into())
}

fn join_units(values: &[JsString], separator: &str) -> JsString {
    let mut units = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend(value.units());
    }
    JsString::from_utf16(units)
}

fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn warning(text: String) -> JsValue {
    JsValue::String(JsString::from(text))
}

fn field_or<'a>(object: &'a JsObject, key: &str, fallback: &'a JsValue) -> &'a JsValue {
    match object.get(key) {
        Some(JsValue::Null | JsValue::Undefined) | None => fallback,
        Some(value) => value,
    }
}

fn array_or_empty(object: &JsObject, key: &str) -> JsValue {
    match object.get(key) {
        Some(JsValue::Array(items)) => JsValue::Array(items.clone()),
        _ => JsValue::Array(Vec::new()),
    }
}

fn objects(value: &JsValue) -> Vec<JsObject> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(|item| item.as_object().cloned())
            .collect(),
        _ => Vec::new(),
    }
}

// -- trace / step / mutation ------------------------------------------------

/// `createDreamTrace(trigger)`. `started_at` is the frozen `nowIso()` and
/// `random_suffix` is `Math.random().toString(36).slice(2, 10)`: V8's
/// shortest-round-trip radix-36 formatting is not reimplemented here, the
/// caller supplies the same fragment its random source produced.
pub fn create_dream_trace(
    trigger: &JsString,
    started_at: &JsString,
    random_suffix: &JsString,
) -> JsObject {
    let mut seed = trigger.units().to_vec();
    seed.extend(":".encode_utf16());
    seed.extend(started_at.units());
    seed.extend(":".encode_utf16());
    seed.extend(random_suffix.units());
    let id = JsString::from(format!(
        "dream_trace_{}",
        hash_text(&JsString::from_utf16(seed))
    ));
    let mut snapshot = JsObject::new();
    snapshot.insert("projectMetaPresent".into(), JsValue::Bool(false));
    snapshot.insert("projectFileCount".into(), JsValue::Number(0.0));
    snapshot.insert("feedbackFileCount".into(), JsValue::Number(0.0));
    snapshot.insert("hasUserProfile".into(), JsValue::Bool(false));
    let mut outcome = JsObject::new();
    outcome.insert("rewrittenProjects".into(), JsValue::Number(0.0));
    outcome.insert("deletedProjects".into(), JsValue::Number(0.0));
    outcome.insert("deletedFiles".into(), JsValue::Number(0.0));
    outcome.insert("profileUpdated".into(), JsValue::Bool(false));
    outcome.insert("summary".into(), "".into());
    let mut trace = JsObject::new();
    trace.insert("dreamTraceId".into(), JsValue::String(id));
    trace.insert("trigger".into(), JsValue::String(trigger.clone()));
    trace.insert("startedAt".into(), JsValue::String(started_at.clone()));
    trace.insert("status".into(), "running".into());
    trace.insert("isNoOp".into(), JsValue::Bool(false));
    trace.insert("displayStatus".into(), "Running".into());
    trace.insert("snapshotSummary".into(), JsValue::Object(snapshot));
    trace.insert("steps".into(), JsValue::Array(Vec::new()));
    trace.insert("mutations".into(), JsValue::Array(Vec::new()));
    trace.insert("outcome".into(), JsValue::Object(outcome));
    trace
}

/// `pushStep(trace, ...)`; options carries the optional `refs`, `metrics`,
/// `details`, `promptDebug` and i18n keys. Mutates `trace.steps` in place.
pub fn push_step(
    trace: &mut JsObject,
    kind: &JsString,
    title: &JsString,
    status: &JsString,
    input_summary: &JsString,
    output_summary: &JsString,
    options: &JsObject,
) {
    let mut steps = match trace.get("steps") {
        Some(JsValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let mut step_id = string_field(trace, "dreamTraceId")
        .unwrap_or_default()
        .units()
        .to_vec();
    step_id.extend(format!(":step:{}", steps.len() + 1).encode_utf16());
    let mut step = JsObject::new();
    step.insert(
        "stepId".into(),
        JsValue::String(JsString::from_utf16(step_id)),
    );
    step.insert("kind".into(), JsValue::String(kind.clone()));
    step.insert("title".into(), JsValue::String(title.clone()));
    step.insert("status".into(), JsValue::String(status.clone()));
    step.insert(
        "inputSummary".into(),
        JsValue::String(input_summary.clone()),
    );
    step.insert(
        "outputSummary".into(),
        JsValue::String(output_summary.clone()),
    );
    for key in [
        "refs",
        "metrics",
        "details",
        "promptDebug",
        "titleI18n",
        "inputSummaryI18n",
        "outputSummaryI18n",
    ] {
        put_truthy(&mut step, key, options.get(key));
    }
    steps.push(JsValue::Object(step));
    trace.insert("steps".into(), JsValue::Array(steps));
}

/// `mutation(action, relativePath, options)` with `Date.now()` and
/// `Math.random()` injected as numbers; both render through JS `String(n)`.
pub fn mutation(
    action: &JsString,
    relative_path: &JsString,
    now_ms: f64,
    random_value: f64,
    options: &JsObject,
) -> JsObject {
    let now = number_string(now_ms);
    let random = number_string(random_value);
    let mut seed = action.units().to_vec();
    seed.push(u16::from(b':'));
    seed.extend(relative_path.units());
    seed.push(u16::from(b':'));
    seed.extend(now.encode_utf16());
    seed.push(u16::from(b':'));
    seed.extend(random.encode_utf16());
    let mut out = JsObject::new();
    out.insert(
        "mutationId".into(),
        JsValue::String(JsString::from(format!(
            "mutation_{}",
            hash_text(&JsString::from_utf16(seed))
        ))),
    );
    out.insert("action".into(), JsValue::String(action.clone()));
    out.insert(
        "relativePath".into(),
        JsValue::String(relative_path.clone()),
    );
    for key in ["candidateType", "name", "description", "preview"] {
        put_truthy(&mut out, key, options.get(key));
    }
    out
}

// -- sorts ------------------------------------------------------------------

fn js_string_key(record: &JsObject, key: &str) -> JsString {
    string_field(record, key).unwrap_or_default()
}

/// Stable `updatedAt` descending, then `tie_key` ascending, both through the
/// pinned Node/ICU `localeCompare` comparator (raw UTF-16, lone surrogates
/// included). `Array#sort` never calls the comparator for < 2 records, so the
/// collator is loaded only when needed.
fn sort_records(
    records: &[JsObject],
    time_key: &str,
    tie_key: &str,
) -> Result<Vec<JsObject>, FileMemoryError> {
    let mut out = records.to_vec();
    if out.len() >= 2 {
        let collator = node_collator(&node_locale(|key| std::env::var(key).ok()))
            .map_err(|(_, message)| FileMemoryError::Collation(message))?;
        out.sort_by(|left, right| {
            let left_time = js_string_key(left, time_key);
            let right_time = js_string_key(right, time_key);
            if left_time != right_time {
                node_compare_utf16(&collator, &right_time, &left_time)
            } else {
                node_compare_utf16(
                    &collator,
                    &js_string_key(left, tie_key),
                    &js_string_key(right, tie_key),
                )
            }
        });
    }
    Ok(out)
}

pub fn sort_entries(entries: &JsValue) -> Result<Vec<JsObject>, FileMemoryError> {
    sort_records(&objects(entries), "updatedAt", "relativePath")
}

pub fn sort_project_metas(entries: &JsValue) -> Result<Vec<JsObject>, FileMemoryError> {
    sort_records(&objects(entries), "updatedAt", "projectName")
}

// -- LLM input projections --------------------------------------------------

pub fn to_dream_meta_input(meta: &JsObject) -> JsObject {
    let mut out = JsObject::new();
    for key in [
        "projectId",
        "projectName",
        "description",
        "status",
        "updatedAt",
    ] {
        put(&mut out, key, meta.get(key));
    }
    for key in [
        "dreamUpdatedAt",
        "sourceKind",
        "sourceWorkspacePath",
        "sourceProjectId",
    ] {
        put_truthy(&mut out, key, meta.get(key));
    }
    out
}

pub fn to_dream_record_input(record: &JsObject) -> JsObject {
    let candidate = FileMemoryStore::to_candidate(record);
    let mut out = JsObject::new();
    put(&mut out, "entryId", record.get("relativePath"));
    put(&mut out, "relativePath", record.get("relativePath"));
    let feedback = matches!(
        record.get("type"),
        Some(JsValue::String(value)) if value.equals_str("feedback")
    );
    out.insert(
        "type".into(),
        JsValue::from(if feedback { "feedback" } else { "project" }),
    );
    out.insert("scope".into(), "project".into());
    put(&mut out, "projectId", record.get("projectId"));
    out.insert("isTmp".into(), JsValue::Bool(false));
    for key in ["name", "description", "updatedAt"] {
        put(&mut out, key, record.get(key));
    }
    for key in ["capturedAt", "sourceSessionKey"] {
        put_truthy(&mut out, key, record.get(key));
    }
    put(&mut out, "content", record.get("content"));
    let candidate_type = string_field(&candidate, "type").unwrap_or_default();
    if candidate_type.equals_str("project") {
        let empty = JsValue::from("");
        let mut project = JsObject::new();
        project.insert(
            "stage".into(),
            field_or(&candidate, "stage", &empty).clone(),
        );
        for key in [
            "decisions",
            "constraints",
            "nextSteps",
            "blockers",
            "timeline",
            "notes",
        ] {
            project.insert(key.into(), array_or_empty(&candidate, key));
        }
        out.insert("project".into(), JsValue::Object(project));
    } else if candidate_type.equals_str("feedback") {
        let empty = JsValue::from("");
        let mut feedback = JsObject::new();
        for key in ["rule", "why", "howToApply"] {
            feedback.insert(key.into(), field_or(&candidate, key, &empty).clone());
        }
        feedback.insert("notes".into(), array_or_empty(&candidate, "notes"));
        out.insert("feedback".into(), JsValue::Object(feedback));
    }
    out
}

pub fn to_header_input(entry: &JsObject) -> JsObject {
    let mut out = JsObject::new();
    for key in ["relativePath", "name", "description", "updatedAt"] {
        put(&mut out, key, entry.get(key));
    }
    out
}

pub fn to_refined_candidate(input: &JsObject) -> JsObject {
    let mut out = JsObject::new();
    put(&mut out, "type", input.get("kind"));
    out.insert("scope".into(), "project".into());
    for key in ["name", "description"] {
        put(&mut out, key, input.get(key));
    }
    let markdown = input
        .get("markdown")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let mut body = js_trim_units(markdown.units()).to_vec();
    body.push(u16::from(b'\n'));
    out.insert("body".into(), JsValue::String(JsString::from_utf16(body)));
    if let Some(source) = input.get("sourceRecord").and_then(JsValue::as_object) {
        for key in ["capturedAt", "sourceSessionKey"] {
            put_truthy(&mut out, key, source.get(key));
        }
    }
    out
}

// -- exclusive clusters -----------------------------------------------------

fn same_project_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)(?:same current project|same project|same workspace|belong to the same project|all [^\n\r\u{2028}\u{2029}]*same project|同一个项目|同一项目|都属于同一个项目|都属于同一项目|同属[^\n\r\u{2028}\u{2029}]*项目|属于当前项目|同一个工作区)",
        )
        .expect("static same-project pattern")
    })
}

fn semantic_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)(?:overlap|overlapping|duplicate|duplicated|redundant|redundancy|conflict|conflicting|inconsistent|merge|consolidat|same rule|same constraint|same risk|same blocker|same goal|same stage|same definition|same style|same audience|重复|冗余|冲突|重叠|可合并|可统一|内容重合|语义重合|事实重合|相同规则|相同约束|相同风险|相同阻塞|相同目标|相同阶段|相同定义|相同风格|相同受众)",
        )
        .expect("static semantic pattern")
    })
}

/// V8 regexes consume UTF-16; keep unpaired surrogates distinct instead of
/// letting `String::from_utf16_lossy` fold them to U+FFFD. The patterns above
/// never assert a surrogate literally, so mapping leftovers into the PUA only
/// affects `.` wildcards, where either unit still matches.
fn regex_text(value: &JsString) -> String {
    let mut out = String::with_capacity(value.len_utf16());
    for decoded in char::decode_utf16(value.units().iter().copied()) {
        match decoded {
            Ok(character) => out.push(character),
            Err(error) => out.push(
                char::from_u32(0xE000 + u32::from(error.unpaired_surrogate() - 0xD800))
                    .expect("PUA surrogate mapping"),
            ),
        }
    }
    out
}

fn is_generic_same_project_reason(reason: Option<&JsValue>) -> Result<bool, FileMemoryError> {
    let normalized = normalize_whitespace(reason)?;
    if normalized.is_empty() {
        return Ok(false);
    }
    let text = regex_text(&normalized);
    // dream-review.ts:342-355 uses `/i` without the `u` flag: V8 folds ASCII
    // only, while Rust `(?i)` also maps U+017F/U+212A onto `s`/`k`.
    let text = ascii_fold_haystack(&text);
    Ok(same_project_pattern().is_match(text.as_ref())
        && !semantic_pattern().is_match(text.as_ref()))
}

/// `${cluster.reason || members.join(", ")}` in a dropped warning: a falsy
/// non-string falls back to the joiner, a truthy one is coerced with JS
/// `String()` (numbers, booleans, arrays and `[object Object]`).
fn drop_reason(
    reason: Option<&JsValue>,
    members: &[JsString],
) -> Result<JsString, FileMemoryError> {
    match reason {
        Some(value) if js_truthy(value) => to_string_units(value)
            .map(JsString::from_utf16)
            .map_err(|_| FileMemoryError::Invalid("dream reason too deep for String()".into())),
        _ => Ok(join_units(members, ", ")),
    }
}

/// `cluster.memberRelativePaths.map(...)`: a nullish receiver reads as null /
/// undefined, a non-array receiver has no `.map`, and null/undefined cluster
/// entries fail while reading `.memberRelativePaths` (`dream-review.ts:403-406`).
fn cluster_members(cluster: &JsValue) -> Result<&[JsValue], FileMemoryError> {
    let members = match cluster {
        JsValue::Null => {
            return Err(type_error(
                "Cannot read properties of null (reading 'memberRelativePaths')",
            ));
        }
        JsValue::Undefined => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'memberRelativePaths')",
            ));
        }
        JsValue::Object(object) => object.get("memberRelativePaths"),
        JsValue::Array(_) | JsValue::Bool(_) | JsValue::Number(_) | JsValue::String(_) => None,
    };
    match members {
        Some(JsValue::Array(items)) => Ok(items),
        Some(JsValue::Null) => Err(type_error("Cannot read properties of null (reading 'map')")),
        Some(JsValue::Undefined) | None => Err(type_error(
            "Cannot read properties of undefined (reading 'map')",
        )),
        Some(_) => Err(type_error(
            "cluster.memberRelativePaths.map is not a function",
        )),
    }
}

pub fn validate_exclusive_clusters(
    clusters: &JsValue,
    allowed_relative_paths: &HashSet<JsString>,
    max_files: f64,
) -> Result<JsObject, FileMemoryError> {
    let mut used = HashSet::new();
    let mut accepted = Vec::new();
    let mut dropped_warnings = Vec::new();
    // `for (const cluster of clusters)` accepts arrays and strings; every
    // other JSON shape throws `TypeError: clusters is not iterable`.
    let clusters = match clusters {
        JsValue::Array(items) => items.as_slice(),
        JsValue::String(value) if value.is_empty() => &[],
        // A non-empty string yields characters; `'x'.memberRelativePaths` is
        // undefined, so the first character fails on `.map`.
        JsValue::String(_) => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'map')",
            ));
        }
        _ => return Err(type_error("clusters is not iterable")),
    };
    for cluster in clusters {
        let members = cluster_members(cluster)?;
        let mut unique_members = Vec::new();
        for member in members {
            let member = normalize_whitespace(Some(member))?;
            if !member.is_empty()
                && allowed_relative_paths.contains(&member)
                && !unique_members.contains(&member)
            {
                unique_members.push(member);
            }
        }
        let reason = match cluster {
            JsValue::Object(object) => object.get("reason"),
            _ => None,
        };
        if unique_members.len() < 2 {
            let reason_text = drop_reason(reason, &unique_members)?;
            dropped_warnings.push(warning(format!(
                "Dropped cluster because it had fewer than 2 valid files: {}",
                lossy(&reason_text)
            )));
            continue;
        }
        if unique_members.len() as f64 > max_files {
            dropped_warnings.push(warning(format!(
                "Dropped cluster because it exceeded the {}-file limit: {}",
                number_string(max_files),
                lossy(&join_units(&unique_members, ", "))
            )));
            continue;
        }
        if unique_members.iter().any(|member| used.contains(member)) {
            dropped_warnings.push(warning(format!(
                "Dropped overlapping cluster: {}",
                lossy(&join_units(&unique_members, ", "))
            )));
            continue;
        }
        if is_generic_same_project_reason(reason)? {
            dropped_warnings.push(warning(format!(
                "Dropped low-quality cluster because the reason only referenced same-project membership without concrete overlap/conflict: {}",
                lossy(&join_units(&unique_members, ", "))
            )));
            continue;
        }
        for member in &unique_members {
            used.insert(member.clone());
        }
        let mut entry = JsObject::new();
        entry.insert(
            "memberRelativePaths".into(),
            JsValue::Array(unique_members.into_iter().map(JsValue::String).collect()),
        );
        put(&mut entry, "reason", reason);
        accepted.push(JsValue::Object(entry));
    }
    let mut out = JsObject::new();
    out.insert("clusters".into(), JsValue::Array(accepted));
    out.insert("droppedWarnings".into(), JsValue::Array(dropped_warnings));
    Ok(out)
}

// -- user-note window -------------------------------------------------------

pub fn select_user_note_window(records: &JsValue) -> Result<JsObject, FileMemoryError> {
    let sorted = sort_records(&objects(records), "updatedAt", "relativePath")?;
    let mut selected = Vec::new();
    let mut chars = 0.0;
    for record in &sorted {
        if selected.len() >= DREAM_USER_NOTE_MAX_FILES {
            break;
        }
        let content_length = record
            .get("content")
            .and_then(JsValue::as_string)
            .map_or(0, JsString::len_utf16);
        let next_chars = chars + content_length as f64;
        if next_chars > DREAM_USER_NOTE_CHAR_BUDGET {
            break;
        }
        selected.push(record.clone());
        chars = next_chars;
    }
    let selected_ids: HashSet<JsString> = selected
        .iter()
        .filter_map(|record| string_field(record, "relativePath"))
        .collect();
    let kept: Vec<JsValue> = sorted
        .into_iter()
        .filter(|record| {
            string_field(record, "relativePath").is_none_or(|path| !selected_ids.contains(&path))
        })
        .map(JsValue::Object)
        .collect();
    let mut out = JsObject::new();
    out.insert(
        "selectedRecords".into(),
        JsValue::Array(selected.into_iter().map(JsValue::Object).collect()),
    );
    out.insert("selectedChars".into(), JsValue::Number(chars));
    out.insert("keptRecords".into(), JsValue::Array(kept));
    Ok(out)
}

// -- General project merge groups -------------------------------------------

pub fn validate_general_project_merge_groups(
    input: &JsObject,
) -> Result<JsObject, FileMemoryError> {
    // `new Map(input.metas.map(...))` evaluates eagerly: a non-array throws
    // before any group is inspected, and each nullish meta throws while
    // reading `.projectId` (`dream-review.ts:487`).
    let metas = match input.get("metas") {
        Some(JsValue::Array(items)) => items.as_slice(),
        Some(JsValue::Null) => {
            return Err(type_error("Cannot read properties of null (reading 'map')"));
        }
        Some(JsValue::Undefined) | None => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'map')",
            ));
        }
        Some(_) => return Err(type_error("input.metas.map is not a function")),
    };
    let mut project_ids = HashSet::new();
    for meta in metas {
        match meta {
            JsValue::Null => {
                return Err(type_error(
                    "Cannot read properties of null (reading 'projectId')",
                ));
            }
            JsValue::Undefined => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'projectId')",
                ));
            }
            JsValue::Object(object) => {
                if let Some(JsValue::String(id)) = object.get("projectId") {
                    project_ids.insert(id.clone());
                }
            }
            _ => {}
        }
    }
    // `input.groups.entries()`: arrays expose it; nullish and every other
    // non-array shape throw the V8 messages below.
    let groups = match input.get("groups") {
        Some(JsValue::Array(items)) => items.as_slice(),
        Some(JsValue::Null) => {
            return Err(type_error(
                "Cannot read properties of null (reading 'entries')",
            ));
        }
        Some(JsValue::Undefined) | None => {
            return Err(type_error(
                "Cannot read properties of undefined (reading 'entries')",
            ));
        }
        Some(_) => {
            return Err(type_error(
                "input.groups.entries is not a function or its return value is not iterable",
            ));
        }
    };
    let mut used_project_ids = HashSet::new();
    let mut accepted_groups = Vec::new();
    let mut dropped_warnings = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let label = format!("group {}", index + 1);
        let keeper_value = match group {
            JsValue::Null => {
                return Err(type_error(
                    "Cannot read properties of null (reading 'keeperProjectId')",
                ));
            }
            JsValue::Undefined => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'keeperProjectId')",
                ));
            }
            JsValue::Object(object) => object.get("keeperProjectId"),
            _ => None,
        };
        let keeper = normalize_whitespace(keeper_value)?;
        let duplicate_values = match group {
            JsValue::Object(object) => object.get("duplicateProjectIds"),
            _ => None,
        };
        let duplicate_values = match duplicate_values {
            Some(JsValue::Array(items)) => items.as_slice(),
            Some(JsValue::Null) => {
                return Err(type_error("Cannot read properties of null (reading 'map')"));
            }
            Some(JsValue::Undefined) | None => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'map')",
                ));
            }
            Some(_) => {
                return Err(type_error(
                    "group.duplicateProjectIds.map is not a function",
                ));
            }
        };
        let mut duplicates: Vec<JsString> = Vec::new();
        for value in duplicate_values {
            let value = normalize_whitespace(Some(value))?;
            if !value.is_empty() && !duplicates.contains(&value) {
                duplicates.push(value);
            }
        }
        if !project_ids.contains(&keeper) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: unknown keeper project id {}.",
                if keeper.is_empty() {
                    "(empty)".into()
                } else {
                    lossy(&keeper)
                }
            )));
            continue;
        }
        if let Some(unknown) = duplicates.iter().find(|id| !project_ids.contains(*id)) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: unknown duplicate project id {}.",
                lossy(unknown)
            )));
            continue;
        }
        if duplicates.contains(&keeper) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: keeper project id was also listed as a duplicate."
            )));
            continue;
        }
        if duplicates.is_empty() {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: no duplicate project ids were supplied."
            )));
            continue;
        }
        let mut all_ids = vec![keeper.clone()];
        all_ids.extend(duplicates.iter().cloned());
        if let Some(reused) = all_ids.iter().find(|id| used_project_ids.contains(*id)) {
            dropped_warnings.push(warning(format!(
                "Dropped {label}: project id {} was already used in another merge group.",
                lossy(reused)
            )));
            continue;
        }
        for id in &all_ids {
            used_project_ids.insert(id.clone());
        }
        let reason = match group {
            JsValue::Object(object) => match object.get("reason") {
                Some(reason) if js_truthy(reason) => reason.clone(),
                _ => JsValue::from(GENERAL_MERGE_REASON),
            },
            _ => JsValue::from(GENERAL_MERGE_REASON),
        };
        let mut accepted = JsObject::new();
        accepted.insert("keeperProjectId".into(), JsValue::String(keeper));
        accepted.insert(
            "duplicateProjectIds".into(),
            JsValue::Array(duplicates.into_iter().map(JsValue::String).collect()),
        );
        accepted.insert("reason".into(), reason);
        accepted_groups.push(JsValue::Object(accepted));
    }
    let mut out = JsObject::new();
    out.insert("acceptedGroups".into(), JsValue::Array(accepted_groups));
    out.insert("droppedWarnings".into(), JsValue::Array(dropped_warnings));
    Ok(out)
}
