//! EdgeClaw retrieval-side LLM calls (A6.6-edgeclaw-8).
//!
//! Ports the fixed TS baseline `core/skills/llm-extraction.ts`:
//! `decideFileMemoryRoute` (:2877-2915), `selectRecallProject` (:2917-3016),
//! `selectFileManifestEntries` (:3126-3178) plus their prompts and the
//! `recallProjectSourcePriority` / `chooseBestRecallProjectFallback` sort
//! (:1623-1637). `normalizeMemoryRoute` / `normalizeStringArray` /
//! `truncateForPrompt` come from the already-locked [`super::envelope`];
//! HTTP, retries and debug come from [`super::transport`].
//!
//! `userPrompt` is `JSON.stringify(value, null, 2)` in TS, so the prompt bytes
//! depend on property insertion order and indentation. [`pretty_json`] writes
//! that exact shape; every object below is built in the TS literal order.
//! Model output is untrusted: envelope and syntax failures are returned as
//! [`LlmError`], then each public method applies the TS fallback. The catch
//! blocks also emit the TS `logger?.warn?.(...)` line through [`super::transport::LlmLogger`]
//! (installed with `LlmMemoryExtractor::with_logger`; silent by default).

use super::{
    envelope::{
        MemoryRoute, extract_first_json_object, normalize_memory_route, normalize_string_array,
        truncate_for_prompt,
    },
    transport::{LlmError, LlmMemoryExtractor, StructuredInput},
};
use crate::{
    project_files::{node_collator, node_compare_utf16, node_locale},
    runtime::memory::{edgeclaw::text::json_text, js_trim_units},
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, json_syntax, parse_json, quote_json_string, stringify_json,
};
use std::{cmp::Ordering, sync::OnceLock};

/// `DEFAULT_FILE_MEMORY_GATE_TIMEOUT_MS` (:213).
pub const DEFAULT_FILE_MEMORY_GATE_TIMEOUT_MS: f64 = 45_000.0;
/// `DEFAULT_FILE_MEMORY_PROJECT_SELECTION_TIMEOUT_MS` (:214).
pub const DEFAULT_FILE_MEMORY_PROJECT_SELECTION_TIMEOUT_MS: f64 = 45_000.0;
/// `DEFAULT_FILE_MEMORY_SELECTION_TIMEOUT_MS` (:215).
pub const DEFAULT_FILE_MEMORY_SELECTION_TIMEOUT_MS: f64 = 45_000.0;

/// A prompt-only projection of `MemoryMessage` (`role` + `content`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryMessage {
    pub role: JsString,
    pub content: JsString,
}

impl MemoryMessage {
    pub fn from_value(value: &JsValue) -> Self {
        let object = value.as_object();
        let field = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        };
        Self {
            role: field("role"),
            content: field("content"),
        }
    }
}

/// `ProjectShortlistCandidate` fields used by the recall prompt and fallback.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectShortlistCandidate {
    pub project_id: JsString,
    pub project_name: JsString,
    pub description: JsString,
    pub status: JsString,
    pub updated_at: JsString,
    pub source_type: Option<JsString>,
    pub score: f64,
    pub exact: f64,
    pub source: JsString,
    pub matched_text: JsString,
}

impl ProjectShortlistCandidate {
    pub fn from_value(value: &JsValue) -> Self {
        let object = value.as_object();
        let text = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        };
        let number = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
        };
        // `project.sourceType ?? "unknown"` treats only null/undefined as
        // absent; `from_value` mirrors that for an untyped object.
        let source_type = object
            .and_then(|object| object.get("sourceType"))
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .and_then(JsValue::as_string)
            .cloned();
        Self {
            project_id: text("projectId"),
            project_name: text("projectName"),
            description: text("description"),
            status: text("status"),
            updated_at: text("updatedAt"),
            source_type,
            score: number("score"),
            exact: number("exact"),
            source: text("source"),
            matched_text: text("matchedText"),
        }
    }
}

/// `ProjectMetaRecord` fields rendered into the manifest prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectMetaRecord {
    pub project_id: JsString,
    pub project_name: JsString,
    pub description: JsString,
    pub status: JsString,
}

impl ProjectMetaRecord {
    pub fn from_value(value: &JsValue) -> Self {
        let object = value.as_object();
        let text = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        };
        Self {
            project_id: text("projectId"),
            project_name: text("projectName"),
            description: text("description"),
            status: text("status"),
        }
    }
}

/// `RecallHeaderEntry` fields rendered into the manifest prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecallHeaderEntry {
    pub relative_path: JsString,
    pub kind: JsString,
    pub scope: JsString,
    pub project_id: Option<JsString>,
    pub updated_at: JsString,
    pub description: JsString,
}

impl RecallHeaderEntry {
    pub fn from_value(value: &JsValue) -> Self {
        let object = value.as_object();
        let text = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        };
        let project_id = object
            .and_then(|object| object.get("projectId"))
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .and_then(JsValue::as_string)
            .cloned();
        Self {
            relative_path: text("relativePath"),
            kind: text("type"),
            scope: text("scope"),
            project_id,
            updated_at: text("updatedAt"),
            description: text("description"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DecideFileMemoryRouteInput {
    pub query: JsString,
    pub recent_messages: Vec<MemoryMessage>,
    /// `...(input.agentId ? { agentId } : {})`: empty is omitted.
    pub agent_id: Option<JsString>,
    /// `input.timeoutMs ?? DEFAULT_...`
    pub timeout_ms: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct SelectRecallProjectInput {
    pub query: JsString,
    pub recent_user_messages: Vec<MemoryMessage>,
    pub shortlist: Vec<ProjectShortlistCandidate>,
    /// `Boolean(input.allowEmpty)`.
    pub allow_empty: bool,
    pub agent_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct SelectFileManifestEntriesInput {
    pub query: JsString,
    pub route: MemoryRoute,
    pub recent_user_messages: Vec<MemoryMessage>,
    pub project_meta: Option<ProjectMetaRecord>,
    pub manifest: Vec<RecallHeaderEntry>,
    /// `input.limit ?? 5`, then `Math.max(1, Math.min(5, ...))`.
    pub limit: Option<f64>,
    pub agent_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

impl LlmMemoryExtractor {
    /// `decideFileMemoryRoute`: invalid JSON, transport errors and unknown
    /// routes all collapse to `none`.
    pub async fn decide_file_memory_route(
        &self,
        input: &DecideFileMemoryRouteInput,
        debug: &mut dyn FnMut(JsValue),
    ) -> MemoryRoute {
        let system_prompt = JsString::from(
            [
                "You decide whether the current query should trigger long-term memory recall.",
                "Return JSON only with a single field route.",
                "Valid route values: none, user, project, mix.",
                "Use none unless the query clearly needs long-term memory.",
                "Use user only when the query is asking about stable personal identity/background facts about who the user is, such as name, profession, long-term role context, life background, or durable relationships.",
                "Do not use user for reply preferences, language choices, formatting rules, style guidance, file/tool boundaries, or delivery rules; those belong to project.",
                "Use project when the query only needs current project memory, including project facts, collaboration rules, delivery style, file boundaries, or project status.",
                "Use mix only when the query genuinely needs both current project memory and the user's stable identity/background at the same time.",
                "Do not use mix just because both could be helpful; choose mix only when both are actually necessary to answer well.",
            ]
            .join("\n"),
        );
        let mut payload = JsObject::new();
        payload.insert("query".into(), JsValue::String(input.query.clone()));
        payload.insert(
            "recent_messages".into(),
            JsValue::Array(recent_message_pairs(&input.recent_messages)),
        );
        let structured = StructuredInput {
            system_prompt,
            user_prompt: JsString::from(pretty_json(&JsValue::Object(payload))),
            agent_id: input.agent_id.clone().filter(|value| !value.is_empty()),
            request_label: "File memory gate".into(),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_GATE_TIMEOUT_MS),
            ),
        };
        match self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await
        {
            Ok(parsed) => match &parsed {
                JsValue::Object(object) => {
                    normalize_memory_route(object.get("route").unwrap_or(&JsValue::Undefined))
                }
                _ => MemoryRoute::None,
            },
            Err(error) => {
                self.warn(fallback_warning(
                    "[clawxmemory] file memory gate fallback: ",
                    &error,
                ));
                MemoryRoute::None
            }
        }
    }

    /// `selectRecallProject`: no shortlist means no request. A model id outside
    /// the shortlist falls back to the deterministic best project unless
    /// `allow_empty` is set.
    pub async fn select_recall_project(
        &self,
        input: &SelectRecallProjectInput,
        debug: &mut dyn FnMut(JsValue),
    ) -> JsObject {
        if input.shortlist.is_empty() {
            return JsObject::new();
        }
        let Some(fallback) = choose_best_recall_project_fallback(&input.shortlist) else {
            return JsObject::new();
        };
        let (fallback_id, fallback_name) =
            (fallback.project_id.clone(), fallback.project_name.clone());

        let system_lines = vec![
            if input.allow_empty {
                "You choose the most relevant existing formal project for long-term memory recall only when one clearly matches the current query."
            } else {
                "You choose the single most relevant formal project for long-term memory recall."
            },
            "Return JSON only with selected_project_id and reason.",
            if input.allow_empty {
                "Select at most one project from the provided shortlist."
            } else {
                "You must select exactly one project from the provided shortlist."
            },
            "Use the current query first, then recent user messages only for continuation/disambiguation.",
            "Do not infer a project from assistant wording.",
            "Similar project names are distinct by default; shared domain, shared workflow, or shared feedback do not make them the same project.",
            "If the query explicitly names one shortlist project, prefer that exact project instead of broadening to a nearby or umbrella project.",
            if input.allow_empty {
                "If the current query introduces or switches to a new project that is not represented in the shortlist, return an empty selected_project_id."
            } else {
                "If the current query introduces or switches to a new project, still choose the best shortlist project."
            },
            if input.allow_empty {
                "If no shortlist project is clearly relevant, return an empty selected_project_id."
            } else {
                "If multiple shortlist projects remain plausible, still choose the best one."
            },
            if input.allow_empty {
                "If multiple shortlist projects are plausible but evidence is not decisive, return an empty selected_project_id."
            } else {
                "When multiple shortlist projects are plausible, never return empty; choose the best match."
            },
            "When relevance is comparable, prefer general_local over workspace_external.",
            if input.allow_empty {
                "Use empty selected_project_id to skip project-scoped recall for a new or unrelated project; do not force unrelated memory into an existing project."
            } else {
                "Never return an empty selected_project_id when the shortlist is non-empty."
            },
        ];
        let system_prompt = JsString::from(system_lines.join("\n"));

        let mut payload = JsObject::new();
        payload.insert("query".into(), JsValue::String(input.query.clone()));
        payload.insert(
            "recent_user_messages".into(),
            JsValue::Array(recent_user_texts(&input.recent_user_messages)),
        );
        payload.insert(
            "shortlist".into(),
            JsValue::Array(
                input
                    .shortlist
                    .iter()
                    .map(shortlist_candidate_value)
                    .collect(),
            ),
        );
        let structured = StructuredInput {
            system_prompt,
            user_prompt: JsString::from(pretty_json(&JsValue::Object(payload))),
            agent_id: input.agent_id.clone().filter(|value| !value.is_empty()),
            request_label: "File memory project selection".into(),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_PROJECT_SELECTION_TIMEOUT_MS),
            ),
        };
        match self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await
        {
            Ok(parsed) => {
                let selected = selected_project_id(&parsed);
                let matched = input
                    .shortlist
                    .iter()
                    .find(|project| project.project_id == selected);
                let reason = reason_text(&parsed);
                if let Some(matched) = matched {
                    let mut out = JsObject::new();
                    out.insert(
                        "projectId".into(),
                        JsValue::String(matched.project_id.clone()),
                    );
                    if let Some(reason) = reason {
                        out.insert("reason".into(), JsValue::String(reason));
                    }
                    return out;
                }
                let mut out = JsObject::new();
                if input.allow_empty {
                    let reason = reason.unwrap_or_else(|| {
                        if selected.is_empty() {
                            JsString::from("Model returned no matching project.")
                        } else {
                            JsString::from("Model returned a project id outside the shortlist.")
                        }
                    });
                    out.insert("reason".into(), JsValue::String(reason));
                    return out;
                }
                out.insert("projectId".into(), JsValue::String(fallback_id));
                out.insert(
                    "reason".into(),
                    JsValue::String(reason.unwrap_or_else(|| {
                        concat_reason(
                            "Fallback selected ",
                            &fallback_name,
                            "; model returned no valid project id.",
                        )
                    })),
                );
                out
            }
            Err(error) => {
                self.warn(fallback_warning(
                    "[clawxmemory] file memory project selection fallback: ",
                    &error,
                ));
                let mut out = JsObject::new();
                if input.allow_empty {
                    out.insert(
                        "reason".into(),
                        JsValue::String(
                            "Project selection failed; no existing project was forced.".into(),
                        ),
                    );
                    return out;
                }
                out.insert("projectId".into(), JsValue::String(fallback_id));
                out.insert(
                    "reason".into(),
                    JsValue::String(concat_reason(
                        "Fallback selected ",
                        &fallback_name,
                        "; project selection failed.",
                    )),
                );
                out
            }
        }
    }

    /// `selectFileManifestEntries`: every failure (transport, envelope, JSON,
    /// non-array ids) yields the TS `[]` fallback.
    pub async fn select_file_manifest_entries(
        &self,
        input: &SelectFileManifestEntriesInput,
        debug: &mut dyn FnMut(JsValue),
    ) -> Vec<JsString> {
        let system_prompt = JsString::from(
            [
                "You select a small number of memory files from a compact manifest.",
                "Return JSON only with selected_ids.",
                "Select at most 5 ids and prefer recent items that are directly useful for the query.",
            ]
            .join("\n"),
        );
        let limit = selection_limit(input.limit);
        let mut payload = JsObject::new();
        payload.insert("query".into(), JsValue::String(input.query.clone()));
        payload.insert("route".into(), JsValue::String(input.route.as_str().into()));
        payload.insert(
            "recent_user_messages".into(),
            JsValue::Array(recent_user_texts(&input.recent_user_messages)),
        );
        payload.insert(
            "project".into(),
            match &input.project_meta {
                Some(meta) => JsValue::Object(project_meta_value(meta)),
                None => JsValue::Null,
            },
        );
        payload.insert(
            "manifest".into(),
            JsValue::Array(
                input
                    .manifest
                    .iter()
                    .take(200)
                    .map(manifest_entry_value)
                    .collect(),
            ),
        );
        payload.insert("limit".into(), JsValue::Number(limit));
        let structured = StructuredInput {
            system_prompt,
            user_prompt: JsString::from(pretty_json(&JsValue::Object(payload))),
            agent_id: input.agent_id.clone().filter(|value| !value.is_empty()),
            request_label: "File memory selection".into(),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_SELECTION_TIMEOUT_MS),
            ),
        };
        match self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await
        {
            Ok(parsed) => {
                let missing = JsValue::Undefined;
                let selected_ids = match &parsed {
                    JsValue::Object(object) => object.get("selected_ids").unwrap_or(&missing),
                    _ => &missing,
                };
                normalize_string_array(selected_ids, limit)
            }
            Err(error) => {
                self.warn(fallback_warning(
                    "[clawxmemory] file memory selection fallback: ",
                    &error,
                ));
                Vec::new()
            }
        }
    }
}

// -- pure helpers (:1623-1637, :2877-3178) ---------------------------------

/// `recallProjectSourcePriority`: local/mirror 2, external 1, anything else 0.
pub fn recall_project_source_priority(project: &ProjectShortlistCandidate) -> f64 {
    match project.source_type.as_ref() {
        Some(value)
            if value.equals_str("general_local")
                || value.equals_str("workspace_external_mirror") =>
        {
            2.0
        }
        Some(value) if value.equals_str("workspace_external") => 1.0,
        _ => 0.0,
    }
}

/// `chooseBestRecallProjectFallback`: the TS comparator (`:1629-1636`) uses
/// `!==` on numbers, so +0 and -0 are equal and NaN is unequal, then returns
/// the subtraction. `Array#sort` folds one NaN (or -0) comparison result to
/// +0, but V8 sorts with TimSort, so a list containing NaN is not a total
/// order and is not required to keep the input order. A NaN shortlist is not
/// reachable from `buildProjectShortlist`, which only produces `exact` 0/2 and
/// `score = exact * 10 + matched token count`; the reachable domain is pinned
/// against the real TS sort by `js_fallback_sort_matches_ts_reachable_domain`
/// rather than by porting TimSort.
pub fn choose_best_recall_project_fallback(
    shortlist: &[ProjectShortlistCandidate],
) -> Option<&ProjectShortlistCandidate> {
    let mut sorted: Vec<&ProjectShortlistCandidate> = shortlist.iter().collect();
    sorted.sort_by(|left, right| js_sort_order(recall_fallback_delta(left, right)));
    sorted.first().copied()
}

/// The TS comparator body, returning the same `number` it returns.
fn recall_fallback_delta(
    left: &ProjectShortlistCandidate,
    right: &ProjectShortlistCandidate,
) -> f64 {
    // Rust's f64 `!=` has JS `!==` number semantics: +0 == -0 and NaN != NaN.
    if right.exact != left.exact {
        return right.exact - left.exact;
    }
    if right.score != left.score {
        return right.score - left.score;
    }
    let delta = recall_project_source_priority(right) - recall_project_source_priority(left);
    if delta != 0.0 {
        return delta;
    }
    match node_compare_utf16(recall_collator(), &right.updated_at, &left.updated_at) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }
}

/// `Array#sort` treats NaN, +0 and -0 comparator results as equality.
///
/// This per-comparison fold is not a TimSort emulation: with NaN in the list
/// the comparator has no total order, so V8 can pick a different first element
/// than Rust's stable sort. NaN is unreachable from the shortlist producer
/// (see `choose_best_recall_project_fallback`).
fn js_sort_order(delta: f64) -> Ordering {
    if delta.is_nan() || delta == 0.0 {
        Ordering::Equal
    } else if delta < 0.0 {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

/// `[clawxmemory] <label> fallback: ${String(error)}` from the TS catch blocks.
fn fallback_warning(prefix: &str, error: &LlmError) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(error.js_string().units());
    JsString::from_utf16(units)
}

fn recall_collator() -> &'static icu_collator::CollatorBorrowed<'static> {
    static COLLATOR: OnceLock<icu_collator::CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR.get_or_init(|| {
        let locale = node_locale(|key| std::env::var(key).ok());
        node_collator(&locale).expect("bundled collation data")
    })
}

fn selection_limit(limit: Option<f64>) -> f64 {
    js_max(1.0, js_min(5.0, limit.unwrap_or(5.0)))
}

fn js_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left < right {
        left
    } else {
        right
    }
}

fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left > right {
        left
    } else {
        right
    }
}

/// `(input.recentMessages ?? []).slice(-4).map((m) => ({role, content}))`.
fn recent_message_pairs(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);
    messages[start..]
        .iter()
        .map(|message| {
            let mut object = JsObject::new();
            object.insert("role".into(), JsValue::String(message.role.clone()));
            object.insert(
                "content".into(),
                JsValue::String(truncate_for_prompt(&message.content, 220.0)),
            );
            JsValue::Object(object)
        })
        .collect()
}

/// `(input.recentUserMessages ?? []).slice(-4).map((m) => truncateForPrompt(m.content, 220))`.
fn recent_user_texts(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);
    messages[start..]
        .iter()
        .map(|message| JsValue::String(truncate_for_prompt(&message.content, 220.0)))
        .collect()
}

fn shortlist_candidate_value(project: &ProjectShortlistCandidate) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "project_id".into(),
        JsValue::String(project.project_id.clone()),
    );
    object.insert(
        "project_name".into(),
        JsValue::String(project.project_name.clone()),
    );
    object.insert(
        "description".into(),
        JsValue::String(truncate_for_prompt(&project.description, 180.0)),
    );
    object.insert("status".into(), JsValue::String(project.status.clone()));
    object.insert(
        "source_type".into(),
        match &project.source_type {
            Some(value) => JsValue::String(value.clone()),
            None => JsValue::String("unknown".into()),
        },
    );
    object.insert(
        "updated_at".into(),
        JsValue::String(project.updated_at.clone()),
    );
    object.insert("shortlist_score".into(), JsValue::Number(project.score));
    object.insert("shortlist_exact".into(), JsValue::Number(project.exact));
    object.insert(
        "shortlist_source".into(),
        JsValue::String(project.source.clone()),
    );
    object.insert(
        "matched_text".into(),
        JsValue::String(truncate_for_prompt(&project.matched_text, 180.0)),
    );
    JsValue::Object(object)
}

fn project_meta_value(meta: &ProjectMetaRecord) -> JsObject {
    let mut object = JsObject::new();
    object.insert(
        "project_id".into(),
        JsValue::String(meta.project_id.clone()),
    );
    object.insert(
        "project_name".into(),
        JsValue::String(meta.project_name.clone()),
    );
    object.insert(
        "description".into(),
        JsValue::String(truncate_for_prompt(&meta.description, 180.0)),
    );
    object.insert("status".into(), JsValue::String(meta.status.clone()));
    object
}

fn manifest_entry_value(entry: &RecallHeaderEntry) -> JsValue {
    let mut object = JsObject::new();
    object.insert("id".into(), JsValue::String(entry.relative_path.clone()));
    object.insert("type".into(), JsValue::String(entry.kind.clone()));
    object.insert("scope".into(), JsValue::String(entry.scope.clone()));
    object.insert(
        "project_id".into(),
        match &entry.project_id {
            Some(value) => JsValue::String(value.clone()),
            None => JsValue::Null,
        },
    );
    object.insert(
        "updated_at".into(),
        JsValue::String(entry.updated_at.clone()),
    );
    object.insert(
        "description".into(),
        JsValue::String(truncate_for_prompt(&entry.description, 200.0)),
    );
    JsValue::Object(object)
}

/// `typeof parsed.selected_project_id === "string" ? trim() : ""`.
fn selected_project_id(parsed: &JsValue) -> JsString {
    parsed
        .as_object()
        .and_then(|object| object.get("selected_project_id"))
        .and_then(JsValue::as_string)
        .map(|value| JsString::from_utf16(js_trim_units(value.units()).to_vec()))
        .unwrap_or_default()
}

/// `typeof parsed.reason === "string" && parsed.reason.trim() ? truncateForPrompt(reason, 220) : {}`.
fn reason_text(parsed: &JsValue) -> Option<JsString> {
    let reason = parsed
        .as_object()
        .and_then(|object| object.get("reason"))
        .and_then(JsValue::as_string)?;
    if js_trim_units(reason.units()).is_empty() {
        return None;
    }
    Some(truncate_for_prompt(reason, 220.0))
}

/// UTF-16 concatenation for a template literal; never goes through lossy UTF-8.
fn concat_reason(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(value.units());
    units.extend(suffix.encode_utf16());
    JsString::from_utf16(units)
}

/// `JSON.parse(extractFirstJsonObject(raw))`.
fn parse_model_json(raw: &JsString) -> Result<JsValue, LlmError> {
    let envelope =
        extract_first_json_object(raw).map_err(|error| LlmError::new(error.message()))?;
    let source = json_text(envelope.units());
    parse_json(&source).map_err(|error| LlmError {
        name: "SyntaxError",
        status: None,
        message: json_syntax::parse_error_units(envelope.units())
            .unwrap_or_else(|| error.to_string().into()),
    })
}

/// `JSON.stringify(value, null, 2)`.
pub fn pretty_json(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    match value {
        JsValue::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                write_pretty(item, depth + 1, out);
            }
            newline(out, depth);
            out.push(']');
        }
        JsValue::Object(object) => {
            let entries: Vec<(&JsString, &JsValue)> = object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .collect();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                out.push_str(&quote_json_string(key));
                out.push_str(": ");
                write_pretty(item, depth + 1, out);
            }
            newline(out, depth);
            out.push('}');
        }
        other => out.push_str(&scalar_json(other)),
    }
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    out.extend(std::iter::repeat_n(' ', depth * 2));
}

fn scalar_json(value: &JsValue) -> String {
    match value {
        // `JSON.stringify(undefined)` yields undefined; inside an array V8
        // writes null. No prompt field is undefined today, but keep the rule.
        JsValue::Undefined => "null".into(),
        _ => stringify_json(value)
            .ok()
            .flatten()
            .unwrap_or_else(|| "null".into()),
    }
}
