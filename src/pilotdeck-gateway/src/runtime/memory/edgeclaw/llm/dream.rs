//! Dream-side LLM calls (`llm-extraction.ts` at cc7c4e6).
//!
//! Ports the five Dream review model calls owned by A6.6-edgeclaw-14:
//! `rewriteUserProfile` and its profile helpers, `planDreamClusters`,
//! `refineDreamCluster`, `planGeneralProjectMetaMerges` and
//! `reviewDreamProjectMeta` (`:565-586`, `:696-830`, `:918-994`,
//! `:1235-1312`, `:1563-1618`, `:1687-1708`, `:2487-2515`, `:2685-2790`).
//!
//! Prompt bodies are `JSON.stringify(value, null, 2)` bytes, so object
//! insertion order and UTF-16 strings matter; model output is untrusted and
//! goes through the shared `extractFirstJsonObject` / `JSON.parse` pair.
//!
//! The `impl LlmMemoryExtractor` block only calls the public
//! [`LlmMemoryExtractor::call_structured_json_with_debug`] seam. It does not
//! touch the extractor's private config/transport fields, which keeps the
//! shared `transport.rs` untouchable by this package.

use super::{
    envelope::{
        EnvelopeError, extract_first_json_object, normalize_boolean, normalize_string_array,
        normalize_whitespace, truncate, truncate_for_prompt, unique_strings,
    },
    transport::{LlmError, LlmMemoryExtractor, StructuredInput},
};
use crate::{
    js_value::{number_string, to_string_units_checked},
    runtime::memory::{edgeclaw::storage::js_truthy, js_trim_units},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, json_syntax, parse_json, quote_json_string};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

// -- system prompts (`llm-extraction.ts:565-586,696-830`) --------------------

pub const USER_PROFILE_REWRITE_SYSTEM_PROMPT: &str = r###"You rewrite the single "身份背景" section of a global user profile for a conversational memory system.

Rules:
- Return JSON only.
- The existing profile markdown is the previous draft. The incoming user notes are the newest evidence.
- Rewrite the section from scratch. Do not append blindly, and do not keep duplicate or near-duplicate facts just because they already exist.
- Keep only durable personal identity/background information that should persist across future sessions.
- If old profile content conflicts with newer, clearer incoming evidence, prefer the newer evidence and rewrite the section accordingly.
- If the incoming evidence only describes reply preferences, formatting habits, style choices, language choices, file/tool boundaries, or project collaboration rules, do not include them in the rewritten section.
- Do not include project progress, project-specific collaboration rules, deadlines, blockers, or temporary tasks.
- Keep the language aligned with the user's language in the incoming content.
- "identity_background_markdown" must contain only the markdown content that belongs under the "## 身份背景" heading.
- Do not include the heading itself.
- Prefer concise bullet-list markdown when possible.

Use this exact JSON shape:
{
  "identity_background_markdown": "- ..."
}"###;

pub const GENERAL_PROJECT_META_MERGE_SYSTEM_PROMPT: &str = r#"You are the General Dream project-meta merge planner for a file-memory system.

Your job is to inspect all General project metadata records and decide which project nodes clearly describe the same real project.

Rules:
- Use only the supplied project metadata records as evidence.
- Be conservative. If there is any meaningful uncertainty, do not merge.
- Merge only when multiple project metas clearly refer to the same real project, same ongoing workstream, same external mirrored project identity, or an obvious alias/rename of the same project.
- Do not merge merely because projects share a domain, platform, customer type, content format, date, model, workflow, or broad business category.
- Do not merge separate named workstreams with different goals or deliverables.
- Example: "GBX-A 20260423 HoneydewPulse" and "GBX-B 20260423 ClinicFlow" must remain separate because they name different projects with different targets.
- For external mirrors, matching source_workspace_path plus source_project_id is strong evidence for merging.
- keeper_project_id and every duplicate_project_id must be one of the supplied project ids.
- A project id may appear in at most one merge group.
- The keeper must not appear in duplicate_project_ids.
- Return an empty merge_groups array when no merge is clearly justified.
- Natural-language output fields should follow the dominant language already present in the supplied project metas.
- Return valid JSON only.

Use this exact JSON shape:
{
  "summary": "short merge planning summary",
  "merge_groups": [
    {
      "keeper_project_id": "project id to keep",
      "duplicate_project_ids": ["project id to merge into keeper"],
      "reason": "specific evidence that these metas are the same real project"
    }
  ]
}"#;

pub const DREAM_PROJECT_META_REVIEW_SYSTEM_PROMPT: &str = r#"You are the Dream project metadata reviewer for a file-memory system.

Your job is to decide whether the current project metadata is clearly incorrect or outdated after project/feedback refinement.

Rules:
- Use only the supplied current metadata and the recent project/feedback files as evidence.
- Be conservative. Keep the current metadata unless the supplied evidence clearly supports a change.
- You may update only:
  - project_name
  - description
  - status
- Do not rewrite metadata just to paraphrase it.
- Natural-language output fields must follow the dominant language already present in the supplied project/feedback files.
- Return valid JSON only.

Use this exact JSON shape:
{
  "should_update": false,
  "reason": "why metadata should or should not change",
  "project_name": "final project name",
  "description": "final description",
  "status": "in_progress"
}"#;

const DREAM_CLUSTER_PLAN_PROMPT: &str = r#"You are the $KIND Dream cluster planner for a file-memory system.

Your job is to inspect lightweight header information only and decide which files should be refined together.

Rules:
- Use only the supplied header metadata as evidence.
- Do not assume full file contents beyond what the header says.
- $CATEGORY
- Return mutually exclusive candidate clusters only.
- A file may appear in at most one cluster.
- Only create a cluster when at least two files likely overlap, conflict, or should be merged into one cleaner memory file.
- If files are distinct and should remain separate, leave them out of clusters.
- Files belonging to the same current project is not, by itself, a merge reason.
- Shared workspace, shared project membership, shared domain, or shared topic is not enough unless the headers show concrete semantic overlap, fact conflict, rule duplication, or obvious consolidation value.
- Each cluster reason must name the specific overlap, conflict, repeated rule, repeated fact, or consolidation topic that justifies refinement.
- Keep reasons concise and specific.
- Natural-language output should follow the dominant language already visible in the supplied headers.
- Return valid JSON only.

Use this exact JSON shape:
{
  "summary": "short planning summary",
  "clusters": [
    {
      "member_relative_paths": ["Project/a.md", "Project/b.md"],
      "reason": "why these files should be refined together"
    }
  ]
}"#;

const DREAM_CLUSTER_REFINE_PROMPT: &str = r#"You are the $KIND Dream refine engine for a file-memory system.

Your job is to merge one cluster of existing memory files into exactly one cleaner memory file.

Rules:
- Use only the supplied full file contents as evidence.
- Resolve overlap, deduplicate repeated details, and keep the most useful durable facts.
- Do not invent new facts.
- Output exactly one refined file.
- The visible output language must follow the dominant language already present in the supplied files. If the supplied files are mixed, prefer the dominant language of the cluster.
- Apply this language rule consistently to the title/name, description, markdown headings, and markdown body text.
- $INSTRUCTION
- Return valid JSON only.

Use this exact JSON shape:
{
  "summary": "short refine summary",
  "name": "refined file title",
  "description": "one-line description",
  "markdown": "full markdown body"
}"#;

const PROJECT_CATEGORY: &str = "Project memory files capture durable project facts such as project definition, scope, goals, blockers, risks, and important progress.";
const FEEDBACK_CATEGORY: &str = "Feedback memory files capture durable collaboration rules, delivery rules, style rules, title/body template rules, and confirmed output constraints.";
const PROJECT_REFINE_INSTRUCTION: &str = "Produce exactly one project memory file.\n- Keep durable project facts only: what the project is, stable scope, goals, important progress, blockers, risks, and important decisions.\n- Do not reduce the file to a vague status line.\n- Prefer readable markdown headings such as ## Summary, ## Current Stage, ## Constraints, ## Blockers, ## Next Steps, ## Timeline, ## Notes when useful.";
const FEEDBACK_REFINE_INSTRUCTION: &str = "Produce exactly one feedback memory file.\n- Keep durable collaboration rules only: delivery order, output structure, style constraints, title/body template guidance, and confirmed review preferences.\n- Prefer readable markdown headings such as ## Rule, ## Why, ## How To Apply, ## Notes when useful.";

const DEFAULT_USER_PROFILE_REWRITE_TIMEOUT_MS: f64 = 45_000.0;
const DEFAULT_DREAM_CLUSTER_PLAN_TIMEOUT_MS: f64 = 180_000.0;
const DEFAULT_DREAM_CLUSTER_REFINE_TIMEOUT_MS: f64 = 180_000.0;
const DEFAULT_DREAM_PROJECT_META_REVIEW_TIMEOUT_MS: f64 = 120_000.0;
const DEFAULT_GENERAL_PROJECT_META_MERGE_TIMEOUT_MS: f64 = 120_000.0;

fn is_project(input: &JsObject) -> bool {
    input
        .get("kind")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("project"))
}

/// `${input.kind}` for the early-return/fallback summaries.
fn kind_text(input: &JsObject) -> Result<JsString, LlmError> {
    js_string(value_or_undefined(input, "kind"))
}

pub fn build_dream_cluster_plan_system_prompt(kind: &str) -> JsString {
    let label = if kind == "project" {
        "Project"
    } else {
        "Feedback"
    };
    let category = if kind == "project" {
        PROJECT_CATEGORY
    } else {
        FEEDBACK_CATEGORY
    };
    JsString::from(
        DREAM_CLUSTER_PLAN_PROMPT
            .replace("$KIND", label)
            .replace("$CATEGORY", category),
    )
}

pub fn build_dream_cluster_refine_system_prompt(kind: &str) -> JsString {
    let label = if kind == "project" {
        "Project"
    } else {
        "Feedback"
    };
    let instruction = if kind == "project" {
        PROJECT_REFINE_INSTRUCTION
    } else {
        FEEDBACK_REFINE_INSTRUCTION
    };
    JsString::from(
        DREAM_CLUSTER_REFINE_PROMPT
            .replace("$KIND", label)
            .replace("$INSTRUCTION", instruction),
    )
}

// -- small value helpers -----------------------------------------------------

static UNDEFINED: JsValue = JsValue::Undefined;

fn value_or_undefined<'a>(object: &'a JsObject, key: &str) -> &'a JsValue {
    object.get(key).unwrap_or(&UNDEFINED)
}

fn string_field<'a>(object: &'a JsObject, key: &str) -> Option<&'a JsString> {
    object.get(key).and_then(JsValue::as_string)
}

fn array_field<'a>(object: &'a JsObject, key: &str) -> &'a [JsValue] {
    match object.get(key) {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}

fn put(object: &mut JsObject, key: &str, value: JsValue) {
    object.insert(key.into(), value);
}

fn put_string(object: &mut JsObject, key: &str, value: JsString) {
    put(object, key, JsValue::String(value));
}

fn put_optional(object: &mut JsObject, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value
        && !value.is_undefined()
    {
        put(object, key, value.clone());
    }
}

fn empty_string() -> JsString {
    JsString::default()
}

/// JS `value ?? ""`: nullish becomes an empty string, every other JSON value
/// keeps its original type (so `false`/`0` stay numbers/booleans in the wire
/// prompt, exactly like `JSON.stringify`).
fn nullish_value(object: &JsObject, key: &str) -> JsValue {
    match object.get(key) {
        Some(JsValue::Null | JsValue::Undefined) | None => JsValue::String(empty_string()),
        Some(value) => value.clone(),
    }
}

fn js_string(value: &JsValue) -> Result<JsString, LlmError> {
    to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(string_error)
}

fn string_error(error: crate::js_value::ToStringError) -> LlmError {
    LlmError {
        name: error.name(),
        status: None,
        message: error.message().into(),
    }
}

fn type_error(message: impl Into<JsString>) -> LlmError {
    LlmError {
        name: "TypeError",
        status: None,
        message: message.into(),
    }
}

fn required_string<'a>(object: &'a JsObject, key: &str) -> Result<&'a JsString, LlmError> {
    string_field(object, key).ok_or_else(|| {
        let display = match object.get(key) {
            None | Some(JsValue::Undefined) => "undefined",
            Some(JsValue::Null) => "null",
            Some(_) => "value",
        };
        type_error(format!(
            "Cannot read properties of {display} (reading 'replace')"
        ))
    })
}

fn timeout(input: &JsObject, default: f64) -> f64 {
    match input.get("timeoutMs") {
        Some(JsValue::Number(value)) => *value,
        _ => default,
    }
}

fn agent_id(input: &JsObject) -> Option<JsString> {
    match input.get("agentId") {
        Some(JsValue::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

// -- JSON.stringify(value, null, 2) -----------------------------------------

/// `JSON.stringify(value, null, 2)`: two-space indent, `": "`, no trailing
/// newline, object `undefined` values dropped, array `undefined` -> `null`.
fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    match value {
        JsValue::Undefined | JsValue::Null => out.push_str("null"),
        JsValue::Bool(bit) => out.push_str(if *bit { "true" } else { "false" }),
        JsValue::Number(number) => out.push_str(&number_string(*number)),
        JsValue::String(text) => out.push_str(&quote_json_string(text)),
        JsValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        JsValue::Object(object) => {
            let entries: Vec<_> = object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .collect();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                out.push_str(&quote_json_string(key));
                out.push_str(": ");
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

// -- user prompt builders (`llm-extraction.ts:918-994,1235-1312`) -----------

/// `buildUserProfileRewritePrompt`. `existingProfile.files[0].content` is
/// truthy-checked; all candidates (not only `type === "user"`) are recorded.
pub fn build_user_profile_rewrite_prompt(input: &JsObject) -> Result<JsString, LlmError> {
    let mut root = JsObject::new();
    let existing = match input
        .get("existingProfile")
        .and_then(JsValue::as_object)
        .and_then(|profile| profile.get("files"))
    {
        Some(JsValue::Array(files)) => files
            .first()
            .and_then(JsValue::as_object)
            .and_then(|file| file.get("content")),
        _ => None,
    };
    match existing {
        Some(JsValue::String(content)) if !content.is_empty() => {
            put_string(
                &mut root,
                "existing_profile_markdown",
                truncate(content, 3_200.0),
            );
        }
        _ => put(&mut root, "existing_profile_markdown", JsValue::Null),
    }

    let mut notes = Vec::new();
    for candidate in array_field(input, "candidates") {
        let candidate = candidate
            .as_object()
            .ok_or_else(|| type_error("Cannot read properties of undefined (reading 'body')"))?;
        let note_markdown = ["body", "profile", "summary", "description"]
            .iter()
            .filter_map(|key| candidate.get(key))
            .find(|value| js_truthy(value))
            .map(js_string)
            .transpose()?
            .unwrap_or_default();
        let mut note = JsObject::new();
        let description = required_string(candidate, "description")?;
        put_string(
            &mut note,
            "description",
            truncate_for_prompt(description, 180.0),
        );
        put_string(
            &mut note,
            "note_markdown",
            truncate(&note_markdown, 1_400.0),
        );
        put(
            &mut note,
            "captured_at",
            nullish_value(candidate, "capturedAt"),
        );
        put(
            &mut note,
            "source_session_key",
            nullish_value(candidate, "sourceSessionKey"),
        );
        notes.push(JsValue::Object(note));
    }
    put(&mut root, "incoming_user_notes", JsValue::Array(notes));
    Ok(JsString::from(stringify_pretty(&JsValue::Object(root))))
}

/// `buildDreamClusterPlanPrompt`.
pub fn build_dream_cluster_plan_prompt(input: &JsObject) -> Result<JsString, LlmError> {
    let mut root = JsObject::new();
    put_optional(&mut root, "category", input.get("kind"));
    let mut headers = Vec::new();
    for header in array_field(input, "headers") {
        let header = header.as_object().ok_or_else(|| {
            type_error("Cannot read properties of undefined (reading 'relativePath')")
        })?;
        let mut item = JsObject::new();
        put_string(
            &mut item,
            "relative_path",
            required_string(header, "relativePath")?.clone(),
        );
        put_string(
            &mut item,
            "name",
            truncate_for_prompt(required_string(header, "name")?, 120.0),
        );
        put_string(
            &mut item,
            "description",
            truncate_for_prompt(required_string(header, "description")?, 220.0),
        );
        put_optional(&mut item, "updated_at", header.get("updatedAt"));
        headers.push(JsValue::Object(item));
    }
    put(&mut root, "headers", JsValue::Array(headers));
    Ok(JsString::from(stringify_pretty(&JsValue::Object(root))))
}

/// `buildDreamClusterRefinePrompt`.
pub fn build_dream_cluster_refine_prompt(input: &JsObject) -> Result<JsString, LlmError> {
    let mut root = JsObject::new();
    put_optional(&mut root, "category", input.get("kind"));
    let mut records = Vec::new();
    for record in array_field(input, "records") {
        let record = record
            .as_object()
            .ok_or_else(|| type_error("Cannot read properties of undefined (reading 'entryId')"))?;
        let mut item = JsObject::new();
        put_string(
            &mut item,
            "entry_id",
            required_string(record, "entryId")?.clone(),
        );
        put_string(
            &mut item,
            "relative_path",
            required_string(record, "relativePath")?.clone(),
        );
        put_optional(&mut item, "type", record.get("type"));
        put_string(&mut item, "name", required_string(record, "name")?.clone());
        put_string(
            &mut item,
            "description",
            truncate_for_prompt(required_string(record, "description")?, 220.0),
        );
        put_optional(&mut item, "updated_at", record.get("updatedAt"));
        put(
            &mut item,
            "captured_at",
            nullish_value(record, "capturedAt"),
        );
        put(
            &mut item,
            "source_session_key",
            nullish_value(record, "sourceSessionKey"),
        );
        put_string(
            &mut item,
            "content",
            required_string(record, "content")?.clone(),
        );
        records.push(JsValue::Object(item));
    }
    put(&mut root, "records", JsValue::Array(records));
    Ok(JsString::from(stringify_pretty(&JsValue::Object(root))))
}

/// `buildDreamProjectMetaReviewPrompt`.
pub fn build_dream_project_meta_review_prompt(input: &JsObject) -> Result<JsString, LlmError> {
    let current = input
        .get("currentMeta")
        .and_then(JsValue::as_object)
        .ok_or_else(|| type_error("Cannot read properties of undefined (reading 'projectId')"))?;
    let mut meta = JsObject::new();
    put_string(
        &mut meta,
        "project_id",
        required_string(current, "projectId")?.clone(),
    );
    put_string(
        &mut meta,
        "project_name",
        required_string(current, "projectName")?.clone(),
    );
    put_string(
        &mut meta,
        "description",
        truncate_for_prompt(required_string(current, "description")?, 220.0),
    );
    put_optional(&mut meta, "status", current.get("status"));
    put_optional(&mut meta, "updated_at", current.get("updatedAt"));
    put(
        &mut meta,
        "dream_updated_at",
        nullish_value(current, "dreamUpdatedAt"),
    );
    let mut root = JsObject::new();
    put(&mut root, "current_project_meta", JsValue::Object(meta));
    put(
        &mut root,
        "recent_project_files",
        JsValue::Array(recent_files(input, "recentProjectRecords")?),
    );
    put(
        &mut root,
        "recent_feedback_files",
        JsValue::Array(recent_files(input, "recentFeedbackRecords")?),
    );
    Ok(JsString::from(stringify_pretty(&JsValue::Object(root))))
}

fn recent_files(input: &JsObject, key: &str) -> Result<Vec<JsValue>, LlmError> {
    let mut files = Vec::new();
    for record in array_field(input, key) {
        let record = record.as_object().ok_or_else(|| {
            type_error("Cannot read properties of undefined (reading 'relativePath')")
        })?;
        let mut item = JsObject::new();
        put_string(
            &mut item,
            "relative_path",
            required_string(record, "relativePath")?.clone(),
        );
        put_string(&mut item, "name", required_string(record, "name")?.clone());
        put_string(
            &mut item,
            "description",
            truncate_for_prompt(required_string(record, "description")?, 220.0),
        );
        put_optional(&mut item, "updated_at", record.get("updatedAt"));
        put_string(
            &mut item,
            "content",
            required_string(record, "content")?.clone(),
        );
        files.push(JsValue::Object(item));
    }
    Ok(files)
}

/// `buildGeneralProjectMetaMergePrompt`.
pub fn build_general_project_meta_merge_prompt(input: &JsObject) -> Result<JsString, LlmError> {
    let mut scope = JsObject::new();
    put_string(&mut scope, "mode", "general_project_meta_merge_plan".into());
    put_string(
        &mut scope,
        "primary_truth",
        "supplied_general_project_meta_only".into(),
    );
    put(
        &mut scope,
        "writable_targets",
        JsValue::Array(vec![JsValue::String("GeneralProjects/*.md".into())]),
    );
    put(
        &mut scope,
        "forbidden_outputs",
        JsValue::Array(
            [
                "new project meta",
                "project memory rewrite",
                "feedback memory rewrite",
                "user profile rewrite",
            ]
            .into_iter()
            .map(|value| JsValue::String(value.into()))
            .collect(),
        ),
    );
    let mut metas = Vec::new();
    for project in array_field(input, "projectMetas") {
        let project = project.as_object().ok_or_else(|| {
            type_error("Cannot read properties of undefined (reading 'projectId')")
        })?;
        let mut item = JsObject::new();
        put_string(
            &mut item,
            "project_id",
            required_string(project, "projectId")?.clone(),
        );
        put_string(
            &mut item,
            "project_name",
            required_string(project, "projectName")?.clone(),
        );
        put_string(
            &mut item,
            "description",
            truncate_for_prompt(required_string(project, "description")?, 260.0),
        );
        put_optional(&mut item, "status", project.get("status"));
        put_optional(&mut item, "updated_at", project.get("updatedAt"));
        put(
            &mut item,
            "dream_updated_at",
            nullish_value(project, "dreamUpdatedAt"),
        );
        put(
            &mut item,
            "source_kind",
            nullish_value(project, "sourceKind"),
        );
        put(
            &mut item,
            "source_workspace_path",
            nullish_value(project, "sourceWorkspacePath"),
        );
        put(
            &mut item,
            "source_project_id",
            nullish_value(project, "sourceProjectId"),
        );
        metas.push(JsValue::Object(item));
    }
    let mut root = JsObject::new();
    put(&mut root, "governance_scope", JsValue::Object(scope));
    put(&mut root, "project_metas", JsValue::Array(metas));
    Ok(JsString::from(stringify_pretty(&JsValue::Object(root))))
}

// -- profile helpers (`llm-extraction.ts:943-994,1687-1708`) ----------------

const JS_SPACE: &str =
    r"[\t-\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant dream regex")
}

fn profile_heading_regex() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| rx(&format!(r"^#{{1,6}}{JS_SPACE}*身份背景{JS_SPACE}*\n+")));
    &RE
}

fn fence_prefix_regex() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| rx(&format!(r"(?i)^```(?:markdown)?{JS_SPACE}*")));
    &RE
}

fn fence_suffix_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)```$"));
    &RE
}

fn markdown_heading_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| rx(&format!(r"(?m)^#{{1,6}}{JS_SPACE}*")));
    &RE
}

fn markdown_bullet_regex() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| rx(&format!(r"(?m)^{JS_SPACE}*[-*+]{JS_SPACE}*")));
    &RE
}

fn markdown_bold_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| rx(r"\*\*([^\n\u{2028}\u{2029}]*?)\*\*"));
    &RE
}

fn markdown_italic_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| rx(r"\*([^\n\u{2028}\u{2029}]*?)\*"));
    &RE
}

const SURROGATE_BASE: u32 = 0xF0000;

/// Lossless UTF-16 -> `str` projection for JavaScript regex semantics.
fn encode(value: &JsString) -> String {
    value
        .units()
        .iter()
        .map(|&unit| {
            let point = if (0xD800..=0xDFFF).contains(&unit) {
                SURROGATE_BASE + u32::from(unit - 0xD800)
            } else {
                u32::from(unit)
            };
            char::from_u32(point).expect("BMP unit or private-use surrogate image")
        })
        .collect()
}

fn decode(value: &str) -> JsString {
    let mut units = Vec::with_capacity(value.len());
    for character in value.chars() {
        let point = character as u32;
        if (SURROGATE_BASE..SURROGATE_BASE + 0x800).contains(&point) {
            units.push(0xD800 + (point - SURROGATE_BASE) as u16);
        } else {
            let mut buffer = [0u16; 2];
            units.extend_from_slice(character.encode_utf16(&mut buffer));
        }
    }
    JsString::from_utf16(units)
}

/// `stripMarkdownSyntax(text)`.
pub fn strip_markdown_syntax(text: &JsString) -> JsString {
    let mut value = encode(text).replace('\r', "\n");
    value = markdown_heading_regex()
        .replace_all(&value, "")
        .into_owned();
    value = markdown_bullet_regex().replace_all(&value, "").into_owned();
    value = value.replace('`', "");
    value = markdown_bold_regex().replace_all(&value, "$1").into_owned();
    value = markdown_italic_regex()
        .replace_all(&value, "$1")
        .into_owned();
    normalize_whitespace(&decode(&value))
}

/// `splitProfileFacts(text)`: split on CR/LF and the seven punctuation marks,
/// normalize each piece, keep JS length >= 2, dedupe, cap at 20.
pub fn split_profile_facts(text: &JsString) -> Vec<JsString> {
    let normalized_newlines: Vec<u16> = text
        .units()
        .iter()
        .map(|&unit| {
            if unit == u16::from(b'\r') {
                u16::from(b'\n')
            } else {
                unit
            }
        })
        .collect();
    let mut facts = Vec::new();
    let mut start = 0;
    for (index, &unit) in normalized_newlines.iter().enumerate() {
        let delimiter = unit == u16::from(b'\n')
            || matches!(
                char::from_u32(u32::from(unit)),
                Some('，' | ',' | '；' | ';' | '。' | '.' | '!' | '?')
            );
        if delimiter {
            push_fact(&mut facts, &normalized_newlines[start..index]);
            start = index + 1;
        }
    }
    push_fact(&mut facts, &normalized_newlines[start..]);
    unique_strings(&facts, 20.0)
}

fn push_fact(facts: &mut Vec<JsString>, units: &[u16]) {
    let fact = normalize_whitespace(&JsString::from_utf16(units.to_vec()));
    if fact.len_utf16() >= 2 {
        facts.push(fact);
    }
}

/// `renderIdentityBackgroundMarkdownFromItems(items)`.
pub fn render_identity_background_markdown_from_items(items: &[JsString]) -> JsString {
    let stripped: Vec<JsString> = items.iter().map(strip_markdown_syntax).collect();
    let normalized = unique_strings(&stripped, 20.0);
    let mut units = Vec::new();
    for (index, item) in normalized.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend("- ".encode_utf16());
        units.extend(item.units());
    }
    JsString::from_utf16(units)
}

/// `normalizeIdentityBackgroundSectionMarkdown(value)`.
pub fn normalize_identity_background_section_markdown(value: &JsValue) -> JsString {
    match value {
        JsValue::String(text) => {
            let mut normalized = encode(text).replace('\r', "\n");
            normalized = fence_prefix_regex().replace(&normalized, "").into_owned();
            normalized = fence_suffix_regex().replace(&normalized, "").into_owned();
            let normalized =
                JsString::from_utf16(js_trim_units(decode(&normalized).units()).to_vec());
            let normalized = encode(&normalized);
            let normalized = profile_heading_regex()
                .replace(&normalized, "")
                .into_owned();
            JsString::from_utf16(js_trim_units(decode(&normalized).units()).to_vec())
        }
        JsValue::Array(items) => {
            let strings: Vec<JsString> = items
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect();
            let rendered = render_identity_background_markdown_from_items(&strings);
            JsString::from_utf16(js_trim_units(rendered.units()).to_vec())
        }
        _ => empty_string(),
    }
}

/// `buildUserProfileBodyFromSectionMarkdown(sectionMarkdown)`.
pub fn build_user_profile_body_from_section_markdown(section: &JsValue) -> Option<JsString> {
    let normalized = normalize_identity_background_section_markdown(section);
    if normalized.is_empty() {
        return None;
    }
    let mut units = "## 身份背景\n".encode_utf16().collect::<Vec<_>>();
    units.extend(js_trim_units(normalized.units()));
    units.push(u16::from(b'\n'));
    Some(JsString::from_utf16(units))
}

/// `extractIdentityBackgroundFactsFromProfileBody(body)`.
pub fn extract_identity_background_facts_from_profile_body(body: &JsString) -> Vec<JsString> {
    split_profile_facts(&strip_markdown_syntax(body))
}

/// `buildRewrittenUserProfileCandidate`.
pub fn build_rewritten_user_profile_candidate(
    section: &JsValue,
    latest: Option<&JsObject>,
) -> Option<JsObject> {
    let body = build_user_profile_body_from_section_markdown(section)?;
    let facts = extract_identity_background_facts_from_profile_body(&body);
    let description = facts
        .first()
        .cloned()
        .unwrap_or_else(|| JsString::from("User profile"));
    let mut candidate = JsObject::new();
    put_string(&mut candidate, "type", "user".into());
    put_string(&mut candidate, "scope", "global".into());
    put_string(&mut candidate, "name", "user-profile".into());
    put_string(
        &mut candidate,
        "description",
        truncate_for_prompt(&description, 120.0),
    );
    if let Some(latest) = latest {
        for key in ["capturedAt", "sourceSessionKey"] {
            if let Some(value) = latest.get(key)
                && js_truthy(value)
            {
                put(&mut candidate, key, value.clone());
            }
        }
    }
    put_string(&mut candidate, "body", body);
    if !facts.is_empty() {
        let mut joined = Vec::new();
        for (index, fact) in facts.iter().enumerate() {
            if index > 0 {
                joined.extend("；".encode_utf16());
            }
            joined.extend(fact.units());
        }
        put_string(&mut candidate, "profile", JsString::from_utf16(joined));
        put(
            &mut candidate,
            "relationships",
            JsValue::Array(facts.into_iter().map(JsValue::String).collect()),
        );
    }
    Some(candidate)
}

// -- normalizers (`llm-extraction.ts:1563-1618`) ----------------------------

/// `normalizeDreamCluster`.
pub fn normalize_dream_cluster(
    item: &JsValue,
    allowed_relative_paths: &HashSet<JsString>,
) -> Option<JsObject> {
    let item = item.as_object()?;
    let mut members = Vec::new();
    for value in array_field(item, "member_relative_paths") {
        let JsValue::String(value) = value else {
            continue;
        };
        let member = normalize_whitespace(value);
        if !member.is_empty()
            && allowed_relative_paths.contains(&member)
            && !members.contains(&member)
        {
            members.push(member);
        }
        if members.len() == 32 {
            break;
        }
    }
    if members.is_empty() {
        return None;
    }
    let reason = string_field(item, "reason")
        .map(|value| truncate(&normalize_whitespace(value), 320.0))
        .unwrap_or_default();
    let mut cluster = JsObject::new();
    put(
        &mut cluster,
        "memberRelativePaths",
        JsValue::Array(members.into_iter().map(JsValue::String).collect()),
    );
    put_string(&mut cluster, "reason", reason);
    Some(cluster)
}

/// `normalizeGeneralProjectMetaMergeGroup`.
pub fn normalize_general_project_meta_merge_group(item: &JsValue) -> Option<JsObject> {
    let item = item.as_object()?;
    let keeper = string_field(item, "keeper_project_id")
        .map(normalize_whitespace)
        .unwrap_or_default();
    let duplicates: Vec<JsString> =
        normalize_string_array(value_or_undefined(item, "duplicate_project_ids"), 100.0)
            .into_iter()
            .map(|value| normalize_whitespace(&value))
            .filter(|value| !value.is_empty())
            .collect();
    if keeper.is_empty() || duplicates.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let duplicates: Vec<JsValue> = duplicates
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .map(JsValue::String)
        .collect();
    let reason = string_field(item, "reason")
        .map(|value| truncate(&normalize_whitespace(value), 320.0))
        .unwrap_or_default();
    let mut group = JsObject::new();
    put_string(&mut group, "keeperProjectId", keeper);
    put(
        &mut group,
        "duplicateProjectIds",
        JsValue::Array(duplicates),
    );
    put_string(&mut group, "reason", reason);
    Some(group)
}

/// `normalizeDreamProjectMetaReview`.
pub fn normalize_dream_project_meta_review(payload: &JsObject, fallback: &JsObject) -> JsObject {
    let should_update = normalize_boolean(value_or_undefined(payload, "should_update"), false);
    let reason = string_field(payload, "reason")
        .map(|value| truncate(&normalize_whitespace(value), 320.0))
        .unwrap_or_default();
    let project_name = string_field(payload, "project_name")
        .map(|value| truncate(&normalize_whitespace(value), 120.0))
        .filter(|value| !value.is_empty())
        .or_else(|| string_field(fallback, "projectName").cloned())
        .unwrap_or_default();
    let description = string_field(payload, "description")
        .map(|value| truncate(&normalize_whitespace(value), 320.0))
        .filter(|value| !value.is_empty())
        .or_else(|| string_field(fallback, "description").cloned())
        .unwrap_or_default();
    let status_value = match payload.get("status") {
        Some(JsValue::Null | JsValue::Undefined) | None => value_or_undefined(fallback, "status"),
        Some(value) => value,
    };
    let status = normalize_dream_file_project_status(Some(status_value));
    let mut meta = JsObject::new();
    put_string(&mut meta, "projectName", project_name);
    put_string(&mut meta, "description", description);
    put_string(&mut meta, "status", status);
    let mut review = JsObject::new();
    put(&mut review, "shouldUpdate", JsValue::Bool(should_update));
    put_string(&mut review, "reason", reason);
    put(&mut review, "projectMeta", JsValue::Object(meta));
    review
}

fn normalize_dream_file_project_status(value: Option<&JsValue>) -> JsString {
    let normalized = match value {
        Some(JsValue::String(value)) => normalize_whitespace(value),
        _ => empty_string(),
    };
    let base = if normalized.is_empty() {
        JsString::from("active")
    } else {
        normalized
    };
    truncate(&base, 80.0)
}

// -- model output parsing ----------------------------------------------------

fn parse_model_json(raw: &JsString) -> Result<JsValue, LlmError> {
    let envelope = extract_first_json_object(raw).map_err(envelope_error)?;
    let decoded = String::from_utf16_lossy(envelope.units());
    parse_json(&decoded).map_err(|error| LlmError {
        name: "SyntaxError",
        status: None,
        message: json_syntax::parse_error_units(&decoded.encode_utf16().collect::<Vec<_>>())
            .unwrap_or_else(|| error.to_string().into()),
    })
}

fn envelope_error(error: EnvelopeError) -> LlmError {
    LlmError::new(error.message())
}

fn parsed_object(value: &JsValue, field: &str) -> Result<JsObject, LlmError> {
    match value {
        JsValue::Object(object) => Ok(object.clone()),
        JsValue::Null => Err(type_error(format!(
            "Cannot read properties of null (reading '{field}')"
        ))),
        JsValue::Undefined => Err(type_error(format!(
            "Cannot read properties of undefined (reading '{field}')"
        ))),
        _ => Ok(JsObject::new()),
    }
}

fn parsed_string(object: &JsObject, key: &str) -> Option<JsString> {
    string_field(object, key).cloned()
}

fn fallback_summary(object: &JsObject, key: &str, fallback: &str) -> JsString {
    match parsed_string(object, key) {
        Some(value) => truncate(&normalize_whitespace(&value), 320.0),
        None => JsString::from(fallback),
    }
}

// -- method implementations --------------------------------------------------

impl LlmMemoryExtractor {
    /// `rewriteUserProfile`: every failure is swallowed to `null`, like TS.
    pub async fn rewrite_user_profile(&self, input: &JsObject) -> Option<JsObject> {
        self.rewrite_user_profile_with_debug(input, |_| {}).await
    }

    /// Debug variant; `debug` sees the same record TS' `debugTrace` gets, including
    /// the errored record before the method returns `None`.
    pub async fn rewrite_user_profile_with_debug(
        &self,
        input: &JsObject,
        debug: impl FnMut(JsValue),
    ) -> Option<JsObject> {
        let user_candidates: Vec<&JsObject> = array_field(input, "candidates")
            .iter()
            .filter_map(JsValue::as_object)
            .filter(|candidate| {
                candidate
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("user"))
            })
            .collect();
        let latest = *user_candidates.last()?;
        let user_prompt = build_user_profile_rewrite_prompt(input).ok()?;
        let structured = StructuredInput {
            system_prompt: USER_PROFILE_REWRITE_SYSTEM_PROMPT.into(),
            user_prompt,
            agent_id: agent_id(input),
            request_label: "User profile rewrite".into(),
            timeout_ms: Some(timeout(input, DEFAULT_USER_PROFILE_REWRITE_TIMEOUT_MS)),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await
            .ok()?;
        let parsed = parsed.as_object()?;
        let section = match parsed.get("identity_background_markdown") {
            Some(JsValue::Null | JsValue::Undefined) | None => {
                value_or_undefined(parsed, "identity_background").clone()
            }
            Some(value) => value.clone(),
        };
        build_rewritten_user_profile_candidate(&section, Some(latest))
    }

    pub async fn plan_dream_clusters(&self, input: &JsObject) -> Result<JsObject, LlmError> {
        self.plan_dream_clusters_with_debug(input, |_| {}).await
    }

    pub async fn plan_dream_clusters_with_debug(
        &self,
        input: &JsObject,
        debug: impl FnMut(JsValue),
    ) -> Result<JsObject, LlmError> {
        let headers = array_field(input, "headers");
        if headers.len() < 2 {
            let kind = kind_text(input)?;
            let mut out = JsObject::new();
            put_string(
                &mut out,
                "summary",
                format!(
                    "Not enough {} files to form Dream clusters.",
                    String::from_utf16_lossy(kind.units())
                )
                .into(),
            );
            put(&mut out, "clusters", JsValue::Array(Vec::new()));
            return Ok(out);
        }
        let allowed: HashSet<JsString> = headers
            .iter()
            .filter_map(|header| header.get("relativePath"))
            .filter_map(JsValue::as_string)
            .cloned()
            .collect();
        let structured = StructuredInput {
            system_prompt: build_dream_cluster_plan_system_prompt(if is_project(input) {
                "project"
            } else {
                "feedback"
            }),
            user_prompt: build_dream_cluster_plan_prompt(input)?,
            agent_id: agent_id(input),
            request_label: if is_project(input) {
                "Dream project cluster plan".into()
            } else {
                "Dream feedback cluster plan".into()
            },
            timeout_ms: Some(timeout(input, DEFAULT_DREAM_CLUSTER_PLAN_TIMEOUT_MS)),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await?;
        let parsed = parsed_object(&parsed, "summary")?;
        let mut out = JsObject::new();
        let kind = kind_text(input)?;
        put_string(
            &mut out,
            "summary",
            fallback_summary(
                &parsed,
                "summary",
                &format!(
                    "Dream {} cluster plan completed.",
                    String::from_utf16_lossy(kind.units())
                ),
            ),
        );
        let clusters = match parsed.get("clusters") {
            Some(JsValue::Array(items)) => items
                .iter()
                .filter_map(|item| normalize_dream_cluster(item, &allowed))
                .map(JsValue::Object)
                .collect(),
            _ => Vec::new(),
        };
        put(&mut out, "clusters", JsValue::Array(clusters));
        Ok(out)
    }

    pub async fn refine_dream_cluster(&self, input: &JsObject) -> Result<JsObject, LlmError> {
        self.refine_dream_cluster_with_debug(input, |_| {}).await
    }

    pub async fn refine_dream_cluster_with_debug(
        &self,
        input: &JsObject,
        debug: impl FnMut(JsValue),
    ) -> Result<JsObject, LlmError> {
        let records = array_field(input, "records");
        if records.is_empty() {
            let kind = kind_text(input)?;
            let mut out = JsObject::new();
            put_string(
                &mut out,
                "summary",
                format!(
                    "No {} files were supplied for Dream refine.",
                    String::from_utf16_lossy(kind.units())
                )
                .into(),
            );
            put(&mut out, "file", JsValue::Null);
            return Ok(out);
        }
        let structured = StructuredInput {
            system_prompt: build_dream_cluster_refine_system_prompt(if is_project(input) {
                "project"
            } else {
                "feedback"
            }),
            user_prompt: build_dream_cluster_refine_prompt(input)?,
            agent_id: agent_id(input),
            request_label: if is_project(input) {
                "Dream project cluster refine".into()
            } else {
                "Dream feedback cluster refine".into()
            },
            timeout_ms: Some(timeout(input, DEFAULT_DREAM_CLUSTER_REFINE_TIMEOUT_MS)),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await?;
        let parsed = parsed_object(&parsed, "summary")?;
        let name = parsed_string(&parsed, "name")
            .map(|value| truncate(&normalize_whitespace(&value), 120.0))
            .unwrap_or_default();
        let description = parsed_string(&parsed, "description")
            .map(|value| truncate(&normalize_whitespace(&value), 320.0))
            .unwrap_or_default();
        let markdown = parsed_string(&parsed, "markdown")
            .map(|value| JsString::from_utf16(js_trim_units(value.units()).to_vec()))
            .unwrap_or_default();
        let file = if !name.is_empty() && !description.is_empty() && !markdown.is_empty() {
            let mut file = JsObject::new();
            put_string(&mut file, "name", name);
            put_string(&mut file, "description", description);
            put_string(&mut file, "markdown", markdown);
            JsValue::Object(file)
        } else {
            JsValue::Null
        };
        let mut out = JsObject::new();
        let kind = kind_text(input)?;
        put_string(
            &mut out,
            "summary",
            fallback_summary(
                &parsed,
                "summary",
                &format!(
                    "Dream {} cluster refine completed.",
                    String::from_utf16_lossy(kind.units())
                ),
            ),
        );
        put(&mut out, "file", file);
        Ok(out)
    }

    pub async fn plan_general_project_meta_merges(
        &self,
        input: &JsObject,
    ) -> Result<JsObject, LlmError> {
        self.plan_general_project_meta_merges_with_debug(input, |_| {})
            .await
    }

    pub async fn plan_general_project_meta_merges_with_debug(
        &self,
        input: &JsObject,
        debug: impl FnMut(JsValue),
    ) -> Result<JsObject, LlmError> {
        if array_field(input, "projectMetas").len() < 2 {
            let mut out = JsObject::new();
            put_string(
                &mut out,
                "summary",
                "Fewer than two General project metadata records were available for merge planning."
                    .into(),
            );
            put(&mut out, "mergeGroups", JsValue::Array(Vec::new()));
            return Ok(out);
        }
        let structured = StructuredInput {
            system_prompt: GENERAL_PROJECT_META_MERGE_SYSTEM_PROMPT.into(),
            user_prompt: build_general_project_meta_merge_prompt(input)?,
            agent_id: agent_id(input),
            request_label: "General project meta merge plan".into(),
            timeout_ms: Some(timeout(
                input,
                DEFAULT_GENERAL_PROJECT_META_MERGE_TIMEOUT_MS,
            )),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await?;
        let parsed = parsed_object(&parsed, "summary")?;
        let mut out = JsObject::new();
        put_string(
            &mut out,
            "summary",
            fallback_summary(
                &parsed,
                "summary",
                "General project meta merge planning completed.",
            ),
        );
        let groups = match parsed.get("merge_groups") {
            Some(JsValue::Array(items)) => items
                .iter()
                .filter_map(normalize_general_project_meta_merge_group)
                .map(JsValue::Object)
                .collect(),
            _ => Vec::new(),
        };
        put(&mut out, "mergeGroups", JsValue::Array(groups));
        Ok(out)
    }

    pub async fn review_dream_project_meta(&self, input: &JsObject) -> Result<JsObject, LlmError> {
        self.review_dream_project_meta_with_debug(input, |_| {})
            .await
    }

    pub async fn review_dream_project_meta_with_debug(
        &self,
        input: &JsObject,
        debug: impl FnMut(JsValue),
    ) -> Result<JsObject, LlmError> {
        let current = input
            .get("currentMeta")
            .and_then(JsValue::as_object)
            .ok_or_else(|| {
                type_error("Cannot read properties of undefined (reading 'projectName')")
            })?;
        let structured = StructuredInput {
            system_prompt: DREAM_PROJECT_META_REVIEW_SYSTEM_PROMPT.into(),
            user_prompt: build_dream_project_meta_review_prompt(input)?,
            agent_id: agent_id(input),
            request_label: "Dream project meta review".into(),
            timeout_ms: Some(timeout(input, DEFAULT_DREAM_PROJECT_META_REVIEW_TIMEOUT_MS)),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, parse_model_json, debug)
            .await?;
        let parsed = parsed_object(&parsed, "should_update")?;
        Ok(normalize_dream_project_meta_review(&parsed, current))
    }
}
