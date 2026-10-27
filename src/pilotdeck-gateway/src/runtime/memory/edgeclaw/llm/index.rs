//! Index-side structured LLM calls (A6.6-edgeclaw-11).
//!
//! Ports the fixed TS baseline `core/skills/llm-extraction.ts`: `:218-330`
//! (the four system prompts), `:994-1106` (`buildConversationTurns`,
//! `findFocusTurnIndex`, `serializeTurnsForPrompt`, `buildIndexPromptWindow`,
//! `normalizeClassificationLabels`, `buildCandidateFromCreatePayload`),
//! `:2519-2684` (`classifyMemoryTurn`, `createMemoryNote` and the three
//! public wrappers) and `:3018-3125` (`selectIndexProject`), plus
//! `isGenericProjectCandidateName` (`:1903-1906`) that `:1095` calls.
//! The user-profile helpers at `:918-993` are owned by the dream-side
//! package (only `rewriteUserProfile` calls them) and are not ported here.
//!
//! Two TS control-flow edges are preserved exactly:
//! * `classifyMemoryTurn` and `selectIndexProject` build their `userPrompt`
//!   inside the `try`, so a window-building [`IndexError::Type`] is swallowed
//!   into the fallback result and emits **no** debug event (the throw happens
//!   before `callStructuredJsonWithDebug` is entered).
//! * `createMemoryNote` builds its `userPrompt` **before** the `try`, so the
//!   same error escapes the method instead of producing `null`; the Rust
//!   signature returns [`Result`] for that reason.
//!
//! Every prompt is `JSON.stringify(value, null, 2)`, reproduced by the local
//! [`stringify_pretty`] with the same rules as the snapshot codec's private
//! pretty printer (two-space indent, `": "`, `undefined` keys dropped,
//! non-finite numbers as `null`, insertion key order).

use super::{
    envelope::{
        EnvelopeError, LooseMemoryCreatePayload, extract_first_json_object, truncate_for_prompt,
        try_parse_loose_memory_create_payload,
    },
    transport::{LlmError, LlmMemoryExtractor, StructuredInput},
};
use crate::runtime::memory::{
    edgeclaw::{
        messages::{MemoryMessage, MemoryRole},
        storage::js_truthy,
        text,
    },
    js_trim_units,
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, json_syntax, parse_json, quote_json_string, stringify_json,
};

const DEFAULT_FILE_MEMORY_EXTRACTION_TIMEOUT_MS: f64 = 75_000.0;
const DEFAULT_FILE_MEMORY_PROJECT_SELECTION_TIMEOUT_MS: f64 = 45_000.0;

/// `MEMORY_CLASSIFICATION_SYSTEM_PROMPT` (`llm-extraction.ts`), already `.trim()`med as in TS.
const MEMORY_CLASSIFICATION_SYSTEM_PROMPT: &str = r#"You classify one focus user turn for a long-term memory indexing pipeline.

You are only deciding categories. Do not generate the memory file yet.

Rules:
- Base the decision on the focus user turn first.
- You may use the neighboring user/assistant turns only to disambiguate the focus turn.
- Assistant text is context only. Never classify something that exists only in assistant wording.
- A turn can match multiple categories, but at most once per category.
- Allowed categories:
  - user: cross-project durable personal identity/background facts about who the user is, such as name, profession, long-term role context, life background, or durable relationship context.
  - project: durable current-project facts such as what the project is, goals, scope, important progress, blockers, risks, key decisions.
  - feedback: current-project collaboration rules, delivery rules, output structure, title/body template rules, confirmed style guidance, language rules, and file/tool boundaries.
- Identity test: only use user when the focus turn is describing the user as a person.
- Override test: if another project could reasonably override this rule or preference, it is not user; classify it as feedback.
- Output test: if the turn is constraining how the assistant should reply, write, format, deliver, or touch files/tools, classify it as feedback.
- Project memory should prefer stable facts. Do not classify short-lived time-flow updates, percentages, or fleeting scheduling notes as project memory unless they carry a durable blocker/risk/fact.
- If the user explicitly says "请记住", "帮我记住", or "remember this", treat that as a stronger signal for durable memory. This is still inferred from the visible user text only.
- If nothing durable should be remembered, return should_store=false and labels=[].
- Return JSON only.

Use this exact JSON shape:
{
  "should_store": true,
  "labels": [
    {
      "type": "user | project | feedback",
      "reason": "why this category applies",
      "evidence": "short quote or evidence summary from the focus turn"
    }
  ]
}"#;

/// `USER_NOTE_CREATE_SYSTEM_PROMPT` (`llm-extraction.ts`), already `.trim()`med as in TS.
const USER_NOTE_CREATE_SYSTEM_PROMPT: &str = r#"You create one append-only user memory note from a focus user turn.

Rules:
- Create at most one user note.
- The note must capture only durable cross-project personal identity/background information about who the user is.
- Keep only long-lived identity facts such as name, profession, stable role context, life background, or durable relationship context.
- Do not include language choices, answer structure, formatting habits, style preferences, file boundaries, tool boundaries, or project-specific collaboration rules.
- One note should express one durable identity/background fact rather than a full profile rewrite.
- The visible output language must follow the dominant user language in the focus user turn and neighboring user turns.
- If the surrounding dialogue mixes languages, prefer the focus user turn language first, then the nearest neighboring user language.
- Apply this language rule consistently to the title/name, description, markdown headings, and markdown body text.
- Keep the note readable markdown.
- Do not force the note into a fixed profile template. Use headings only when they genuinely help readability.
- Return JSON only.

Use this exact JSON shape:
{
  "skip": false,
  "reason": "",
  "name": "short user-memory title",
  "description": "one-line description",
  "markdown": "markdown body"
}"#;

/// `PROJECT_NOTE_CREATE_SYSTEM_PROMPT` (`llm-extraction.ts`), already `.trim()`med as in TS.
const PROJECT_NOTE_CREATE_SYSTEM_PROMPT: &str = r#"You create one append-only project memory note from a focus user turn.

Rules:
- Create at most one project note.
- The note belongs to the current project only.
- Capture durable project facts: what the project is, stable scope, goals, key progress, blockers, risks, important decisions, important next steps.
- Do not reduce the note to a vague status line.
- Do not focus on highly volatile percentages, fleeting schedules, or trivial short-term updates unless they reveal a durable blocker/risk/fact.
- The visible output language must follow the dominant user language in the focus user turn and neighboring user turns.
- If the surrounding dialogue mixes languages, prefer the focus user turn language first, then the nearest neighboring user language.
- Apply this language rule consistently to the title/name, description, markdown headings, and markdown body text.
- Keep the note readable markdown.
- Prefer meaningful headings when useful, such as: ## Summary, ## Current Stage, ## Constraints, ## Blockers, ## Next Steps, ## Timeline, ## Notes.
- Return JSON only.

Use this exact JSON shape:
{
  "skip": false,
  "reason": "",
  "name": "short project-memory title",
  "description": "one-line description",
  "markdown": "markdown body"
}"#;

/// `FEEDBACK_NOTE_CREATE_SYSTEM_PROMPT` (`llm-extraction.ts`), already `.trim()`med as in TS.
const FEEDBACK_NOTE_CREATE_SYSTEM_PROMPT: &str = r#"You create one append-only feedback memory note from a focus user turn.

Rules:
- Create at most one feedback note.
- The note belongs to the current project only.
- Use feedback for collaboration rules, delivery order, style constraints, title/body template rules, confirmed output expectations, language rules, and file/tool boundaries.
- The visible output language must follow the dominant user language in the focus user turn and neighboring user turns.
- If the surrounding dialogue mixes languages, prefer the focus user turn language first, then the nearest neighboring user language.
- Apply this language rule consistently to the title/name, description, markdown headings, and markdown body text.
- Keep the note readable markdown.
- Prefer meaningful headings when useful, especially: ## Rule, ## Why, ## How To Apply, ## Notes.
- Return JSON only.

Use this exact JSON shape:
{
  "skip": false,
  "reason": "",
  "name": "short feedback-memory title",
  "description": "one-line description",
  "markdown": "markdown body"
}"#;

/// `selectIndexProject` system prompt: the 12 source lines joined by `\n`.
const SELECT_INDEX_PROJECT_SYSTEM_PROMPT_LINES: [&str; 12] = [
    r#"You assign a newly generated long-term memory item to a General Chat project."#,
    r#"This is index-time memory assignment, not recall."#,
    r#"Return JSON only with decision, selected_project_id, and reason."#,
    r#"decision must be one of: attach_existing, create_new."#,
    r#"The primary evidence is candidate_memory_preview: the memory item that will be written."#,
    r#"Use the focus user turn and recent user messages only as supporting context for disambiguation."#,
    r#"Choose attach_existing only when the candidate clearly belongs to exactly one existing General project."#,
    r#"Choose create_new when the candidate is a new project, evidence is insufficient, multiple projects remain plausible, or the match is only a broad domain similarity."#,
    r#"Do not attach just because projects share a category such as SaaS, copywriting, Xiaohongshu, marketing, planning, or content creation."#,
    r#"All shortlist projects are General-local assignment targets; never infer or write to an external workspace."#,
    r#"If decision is attach_existing, selected_project_id must be one id from the shortlist."#,
    r#"If decision is create_new, selected_project_id must be an empty string."#,
];

// -- types ------------------------------------------------------------------

/// `MemoryCreateKind` (`llm-extraction.ts:92`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemoryCreateKind {
    User,
    Project,
    Feedback,
}

impl MemoryCreateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Feedback => "feedback",
        }
    }
}

/// `MemoryClassificationLabel` (`llm-extraction.ts:96-100`).
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryClassificationLabel {
    pub kind: MemoryCreateKind,
    pub reason: JsString,
    pub evidence: JsString,
}

impl MemoryClassificationLabel {
    /// The `classification` object embedded in a create-note prompt.
    fn to_value(&self) -> JsValue {
        JsValue::Object(object([
            ("type", JsValue::String(self.kind.as_str().into())),
            ("reason", JsValue::String(self.reason.clone())),
            ("evidence", JsValue::String(self.evidence.clone())),
        ]))
    }
}

/// `FileMemoryClassificationResult` (`llm-extraction.ts:102-104`).
#[derive(Clone, Debug, PartialEq)]
pub struct FileMemoryClassificationResult {
    pub should_store: bool,
    pub labels: Vec<MemoryClassificationLabel>,
}

impl FileMemoryClassificationResult {
    pub fn to_value(&self) -> JsValue {
        let labels = self
            .labels
            .iter()
            .map(|label| label.to_value())
            .collect::<Vec<_>>();
        JsValue::Object(object([
            ("shouldStore", JsValue::Bool(self.should_store)),
            ("labels", JsValue::Array(labels)),
        ]))
    }
}

/// `selectIndexProject` return value: `decision`, optional `projectId`,
/// optional `reason`.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexProjectDecision {
    pub attach_existing: bool,
    pub project_id: Option<JsString>,
    pub reason: Option<JsString>,
}

impl IndexProjectDecision {
    pub fn to_value(&self) -> JsValue {
        let mut decision = JsObject::new();
        decision.insert(
            "decision".into(),
            JsValue::String(
                if self.attach_existing {
                    "attach_existing"
                } else {
                    "create_new"
                }
                .into(),
            ),
        );
        if let Some(project_id) = &self.project_id {
            decision.insert("projectId".into(), JsValue::String(project_id.clone()));
        }
        if let Some(reason) = &self.reason {
            decision.insert("reason".into(), JsValue::String(reason.clone()));
        }
        JsValue::Object(decision)
    }
}

/// One of the throw shapes that can leave an index-side call: an LLM
/// transport error, an envelope extractor `Error`, or a V8 `TypeError`
/// (for example `truncateForPrompt(undefined)` reading `replace`).
#[derive(Clone, Debug, PartialEq)]
pub enum IndexError {
    Llm(LlmError),
    Envelope(EnvelopeError),
    Type(JsString),
}

impl IndexError {
    pub fn message(&self) -> JsString {
        match self {
            Self::Llm(error) => error.message.clone(),
            Self::Envelope(error) => JsString::from(error.message()),
            Self::Type(message) => message.clone(),
        }
    }

    /// `isTimeoutError(error) || (error instanceof Error && /timed out/i.test(error.message))`.
    fn timed_out(&self) -> bool {
        let message = String::from_utf16_lossy(self.message().units()).to_lowercase();
        match self {
            Self::Llm(error) => error.timeout() || message.contains("timed out"),
            _ => message.contains("timeout") || message.contains("timed out"),
        }
    }
}

fn type_error(message: impl Into<JsString>) -> IndexError {
    IndexError::Type(message.into())
}

// -- shared input structs ---------------------------------------------------

/// `classifyMemoryTurn` input. `timestamp`/`session_key` exist on the TS
/// signature; the TS body ignores them, and so does this port.
pub struct ClassifyTurnInput<'a> {
    pub timestamp: JsString,
    pub session_key: Option<JsString>,
    pub focus_user_turn: &'a MemoryMessage,
    pub batch_context_messages: &'a [MemoryMessage],
    pub current_project_meta: Option<&'a JsValue>,
    pub agent_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

/// The shared `create{User,Project,Feedback}MemoryNote` input.
pub struct MemoryNoteInput<'a> {
    pub timestamp: JsString,
    pub session_key: Option<JsString>,
    pub focus_user_turn: &'a MemoryMessage,
    pub batch_context_messages: &'a [MemoryMessage],
    pub current_project_meta: Option<&'a JsValue>,
    pub classification: &'a MemoryClassificationLabel,
    pub agent_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

/// `selectIndexProject` input.
pub struct SelectIndexProjectInput<'a> {
    pub candidate: &'a JsValue,
    pub candidate_preview: &'a JsString,
    pub focus_turn: &'a MemoryMessage,
    pub recent_user_messages: Option<&'a [MemoryMessage]>,
    pub shortlist: &'a [JsValue],
    pub agent_id: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

/// TS spreads `...(input.agentId ? { agentId } : {})`: only a truthy (here
/// non-empty) agent id reaches the request headers.
fn truthy_agent_id(agent_id: &Option<JsString>) -> Option<JsString> {
    agent_id.clone().filter(|agent_id| !agent_id.is_empty())
}

// -- pure helpers (`llm-extraction.ts:994-1106`) ----------------------------

/// `buildConversationTurns`: keep user/assistant messages, start a new turn on
/// every user message, drop assistant messages before the first user one.
fn build_conversation_turns(messages: &[MemoryMessage]) -> Vec<Vec<&MemoryMessage>> {
    let mut turns: Vec<Vec<&MemoryMessage>> = Vec::new();
    let mut current: Vec<&MemoryMessage> = Vec::new();
    for message in messages
        .iter()
        .filter(|message| matches!(message.role, MemoryRole::User | MemoryRole::Assistant))
    {
        if matches!(message.role, MemoryRole::User) {
            if !current.is_empty() {
                turns.push(std::mem::take(&mut current));
            }
            current = vec![message];
            continue;
        }
        if !current.is_empty() {
            current.push(message);
        }
    }
    if !current.is_empty() {
        turns.push(current);
    }
    turns
}

/// `findFocusTurnIndex`: reference identity first, then role+content value
/// equality. Reference identity needs the turn members to be the caller's
/// messages, which the `&MemoryMessage` borrow keeps alive here.
fn find_focus_turn_index(turns: &[Vec<&MemoryMessage>], focus: &MemoryMessage) -> isize {
    if let Some(index) = turns
        .iter()
        .position(|turn| turn.iter().any(|message| std::ptr::eq(*message, focus)))
    {
        return index as isize;
    }
    turns
        .iter()
        .position(|turn| {
            turn.iter()
                .any(|message| message.role == focus.role && message.content == focus.content)
        })
        .map_or(-1, |index| index as isize)
}

/// One `{turn_index, messages}` record of `serializeTurnsForPrompt`:
/// contents truncated for the prompt.
fn serialize_turn_for_prompt(turn: &[&MemoryMessage], index: usize) -> JsValue {
    let messages = turn
        .iter()
        .map(|message| {
            JsValue::Object(object([
                ("role", JsValue::String(message.role.as_string())),
                (
                    "content",
                    JsValue::String(truncate_for_prompt(&message.content, 320.0)),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    JsValue::Object(object([
        ("turn_index", JsValue::Number((index + 1) as f64)),
        ("messages", JsValue::Array(messages)),
    ]))
}

/// `buildIndexPromptWindow` as the `JsValue` behind its returned
/// `JSON.stringify(..., null, 2)` text. `current_project_meta.description`
/// goes through `truncateForPrompt`, whose TS reading of `.replace` throws
/// for a missing or non-string description; that error is surfaced here so
/// each caller can reproduce its own TS control flow.
fn build_index_prompt_window(
    batch: &[MemoryMessage],
    focus: &MemoryMessage,
    current_project_meta: Option<&JsValue>,
) -> Result<JsValue, IndexError> {
    let turns = build_conversation_turns(batch);
    let focus_index = find_focus_turn_index(&turns, focus);
    // TS serializes each window group with its own `serializeTurnsForPrompt`
    // call, so `turn_index` restarts at 1 inside every group.
    let serialize_group = |group: &[Vec<&MemoryMessage>]| -> Vec<JsValue> {
        group
            .iter()
            .enumerate()
            .map(|(index, turn)| serialize_turn_for_prompt(turn, index))
            .collect()
    };
    // `serializeTurnsForPrompt([focusTurn])[0]`: the matched batch turn, or
    // the focus message on its own when no batch turn matched.
    let focus_turn = if focus_index >= 0 {
        serialize_turn_for_prompt(&turns[focus_index as usize], 0)
    } else {
        serialize_turn_for_prompt(std::slice::from_ref(&focus), 0)
    };
    let (previous_turns, next_turns): (Vec<JsValue>, Vec<JsValue>) = if focus_index >= 0 {
        let index = focus_index as usize;
        let start = index.saturating_sub(2);
        let end = (index + 3).min(turns.len());
        (
            serialize_group(&turns[start..index]),
            serialize_group(&turns[index + 1..end]),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    let mut window = JsObject::new();
    match current_project_meta.filter(|meta| js_truthy(meta)) {
        Some(meta) => {
            let description = replace_target(meta.get("description"))?;
            let mut record = JsObject::new();
            insert_raw(&mut record, "project_id", meta.get("projectId"));
            insert_raw(&mut record, "project_name", meta.get("projectName"));
            record.insert(
                "description".into(),
                JsValue::String(truncate_for_prompt(description, 220.0)),
            );
            insert_raw(&mut record, "status", meta.get("status"));
            insert_raw(&mut record, "updated_at", meta.get("updatedAt"));
            window.insert("current_project_meta".into(), JsValue::Object(record));
        }
        None => {
            window.insert("current_project_meta".into(), JsValue::Null);
        }
    }
    window.insert(
        "focus_user_turn".into(),
        JsValue::Object(object([
            ("role", JsValue::String(focus.role.as_string())),
            (
                "content",
                JsValue::String(truncate_for_prompt(&focus.content, 400.0)),
            ),
        ])),
    );
    window.insert(
        "focus_turn_with_neighbor_assistant_context".into(),
        focus_turn,
    );
    window.insert("previous_turns".into(), JsValue::Array(previous_turns));
    window.insert("next_turns".into(), JsValue::Array(next_turns));
    Ok(JsValue::Object(window))
}

/// `normalizeClassificationLabels` (`:1063-1080`): non-arrays are `[]`,
/// unknown or duplicated types are dropped, non-string `reason`/`evidence`
/// become `""`, all three truncated for the prompt.
pub fn normalize_classification_labels(value: Option<&JsValue>) -> Vec<MemoryClassificationLabel> {
    let Some(JsValue::Array(items)) = value else {
        return Vec::new();
    };
    // TS `isRecord` also accepts arrays, but `"type"` is never an array
    // index, so an array item behaves exactly like an object without a
    // valid `type`.
    let mut labels = Vec::new();
    let mut seen: Vec<MemoryCreateKind> = Vec::new();
    for item in items {
        let Some(record) = item.as_object() else {
            continue;
        };
        let Some(kind) = record
            .get("type")
            .and_then(JsValue::as_string)
            .and_then(|kind| {
                if kind.equals_str("user") {
                    Some(MemoryCreateKind::User)
                } else if kind.equals_str("project") {
                    Some(MemoryCreateKind::Project)
                } else if kind.equals_str("feedback") {
                    Some(MemoryCreateKind::Feedback)
                } else {
                    None
                }
            })
        else {
            continue;
        };
        if seen.contains(&kind) {
            continue;
        }
        seen.push(kind);
        labels.push(MemoryClassificationLabel {
            kind,
            reason: prompt_string(record.get("reason"), 220.0),
            evidence: prompt_string(record.get("evidence"), 220.0),
        });
    }
    labels
}

/// `typeof value === "string" ? truncateForPrompt(value, max) : ""`.
fn prompt_string(value: Option<&JsValue>, max: f64) -> JsString {
    value
        .and_then(JsValue::as_string)
        .map(|value| truncate_for_prompt(value, max))
        .unwrap_or_default()
}

/// `isGenericProjectCandidateName` (`:1903-1906`).
pub fn is_generic_project_candidate_name(name: &JsString) -> bool {
    let normalized = text::lowercase(&super::envelope::normalize_whitespace(name));
    normalized.is_empty()
        || normalized.equals_str("overview")
        || normalized.equals_str("project")
        || normalized.equals_str("project-item")
        || normalized.equals_str("memory-item")
}

/// `buildCandidateFromCreatePayload` (`:1083-1105`): `null` unless all three
/// fields are non-empty strings (after truncation/trimming), or the project
/// name is generic. The returned object is the wire `MemoryCandidate`.
pub fn build_candidate_from_create_payload(
    kind: MemoryCreateKind,
    payload: &JsValue,
    timestamp: &JsString,
    session_key: Option<&JsString>,
) -> Option<JsObject> {
    let record = payload.as_object();
    let field = |key: &str| record.and_then(|record| record.get(key));
    let name = prompt_string(field("name"), 80.0);
    let description = prompt_string(field("description"), 180.0);
    let markdown = field("markdown")
        .and_then(JsValue::as_string)
        .map(|markdown| JsString::from_utf16(js_trim_units(markdown.units()).to_vec()))
        .unwrap_or_default();
    if name.is_empty() || description.is_empty() || markdown.is_empty() {
        return None;
    }
    if kind == MemoryCreateKind::Project && is_generic_project_candidate_name(&name) {
        return None;
    }
    let mut candidate = JsObject::new();
    candidate.insert("type".into(), JsValue::String(kind.as_str().into()));
    candidate.insert(
        "scope".into(),
        JsValue::String(
            if kind == MemoryCreateKind::User {
                "global"
            } else {
                "project"
            }
            .into(),
        ),
    );
    candidate.insert("name".into(), JsValue::String(name));
    candidate.insert("description".into(), JsValue::String(description));
    candidate.insert("body".into(), JsValue::String(markdown));
    candidate.insert("capturedAt".into(), JsValue::String(timestamp.clone()));
    // TS spreads `...(input.sessionKey ? { sessionKey } : {})`: an empty
    // string stays out of the candidate too.
    if let Some(session_key) = session_key.filter(|session_key| !session_key.is_empty()) {
        candidate.insert(
            "sourceSessionKey".into(),
            JsValue::String(session_key.clone()),
        );
    }
    Some(candidate)
}

// -- V8 property-access errors ----------------------------------------------

/// The string a TS `.replace` read needs; any other shape reproduces the
/// V8 `TypeError` the pipeline would see. Messages measured on Node v22.22.0.
fn replace_target(value: Option<&JsValue>) -> Result<&JsString, IndexError> {
    match value {
        Some(JsValue::String(text)) => Ok(text),
        None | Some(JsValue::Undefined) => Err(type_error(
            "Cannot read properties of undefined (reading 'replace')",
        )),
        Some(JsValue::Null) => Err(type_error(
            "Cannot read properties of null (reading 'replace')",
        )),
        Some(JsValue::Number(number)) => Err(type_error(join_units(
            &primitive_text(*number),
            ".replace is not a function",
        ))),
        Some(JsValue::Bool(bit)) => Err(type_error(join_units(
            &primitive_text(f64::from(u8::from(*bit))),
            ".replace is not a function",
        ))),
        Some(JsValue::Array(_)) => Err(type_error(join_units(
            &receiver_text(value),
            ".replace is not a function",
        ))),
        Some(JsValue::Object(record)) => Err(type_error(join_units(
            if record.entries().is_empty() {
                "{}"
            } else {
                "{(intermediate value)}"
            },
            ".replace is not a function",
        ))),
    }
}

/// The V8 receiver text of an array in a property-access `TypeError`
/// (`[1,2]`, `[]`); JSON text matches for the probed shapes.
fn receiver_text(value: Option<&JsValue>) -> String {
    stringify_json(value.unwrap_or(&JsValue::Null))
        .ok()
        .flatten()
        .unwrap_or_else(|| "null".into())
}

/// `String(number)` including the non-finite corner cases JSON cannot print.
fn primitive_text(number: f64) -> String {
    if number.is_nan() {
        "NaN".into()
    } else if number.is_infinite() {
        if number > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into()
    } else {
        crate::js_value::number_string(number)
    }
}

fn join_units(left: &str, right: &str) -> JsString {
    let mut units: Vec<u16> = left.encode_utf16().collect();
    units.extend(right.encode_utf16());
    JsString::from_utf16(units)
}

// -- JSON helpers ------------------------------------------------------------

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}

/// TS object literals hold `undefined` for absent fields; `JSON.stringify`
/// drops those keys, so only defined values are inserted here.
fn insert_raw(target: &mut JsObject, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value.filter(|value| !value.is_undefined()) {
        target.insert(key.into(), value.clone());
    }
}

/// `JSON.stringify(value, null, 2)`: two-space indent, `": "` between key and
/// value, no trailing newline, `[]`/`{}` stay closed, object `undefined`
/// values drop out and array `undefined` becomes `null`, non-finite numbers
/// become `null`; number text and string quoting follow the compact codec,
/// keys keep insertion order.
fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    match value {
        JsValue::Undefined | JsValue::Null => out.push_str("null"),
        JsValue::Bool(bit) => out.push_str(if *bit { "true" } else { "false" }),
        JsValue::Number(number) => out.push_str(&number_text(*number)),
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
        JsValue::Object(record) => {
            let entries: Vec<&(JsString, JsValue)> = record
                .entries()
                .iter()
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

fn number_text(number: f64) -> String {
    if number.is_finite() {
        crate::js_value::number_string(number)
    } else {
        "null".into()
    }
}

/// `parse: (raw) => JSON.parse(extractFirstJsonObject(raw))`, shared by
/// `classifyMemoryTurn` and `selectIndexProject`.
fn strict_parse(raw: &JsString) -> Result<JsValue, LlmError> {
    let envelope =
        extract_first_json_object(raw).map_err(|error| LlmError::new(error.message()))?;
    let decoded = String::from_utf16_lossy(envelope.units());
    parse_json(&decoded).map_err(|error| LlmError {
        name: "SyntaxError",
        status: None,
        // V8's exact SyntaxError text; the codec message is only a fallback
        // for the shapes the scanner does not classify.
        message: json_syntax::parse_error_units(envelope.units())
            .unwrap_or_else(|| error.to_string().into()),
    })
}

/// The loose create payload as the object TS passes on to
/// `buildCandidateFromCreatePayload` and the debug trace; property order
/// matches the TS `parsedResult` literal.
fn loose_payload_value(payload: &LooseMemoryCreatePayload) -> JsValue {
    let mut object = JsObject::new();
    if let Some(skip) = payload.skip {
        object.insert("skip".into(), JsValue::Bool(skip));
    }
    if let Some(reason) = &payload.reason {
        object.insert("reason".into(), JsValue::String(reason.clone()));
    }
    if let Some(name) = &payload.name {
        object.insert("name".into(), JsValue::String(name.clone()));
    }
    if let Some(description) = &payload.description {
        object.insert("description".into(), JsValue::String(description.clone()));
    }
    if let Some(markdown) = &payload.markdown {
        object.insert("markdown".into(), JsValue::String(markdown.clone()));
    }
    JsValue::Object(object)
}

// -- extractor methods (`llm-extraction.ts:2519-2684,3018-3125`) ------------

impl LlmMemoryExtractor {
    /// `classifyMemoryTurn` (`:2519-2550`). Errors of every kind collapse
    /// into the no-store fallback, as in the TS `catch`.
    pub async fn classify_memory_turn(
        &self,
        input: &ClassifyTurnInput<'_>,
        mut debug: impl FnMut(JsValue),
    ) -> FileMemoryClassificationResult {
        match self.classify_memory_turn_call(input, &mut debug).await {
            Ok(result) => result,
            Err(_) => FileMemoryClassificationResult {
                should_store: false,
                labels: Vec::new(),
            },
        }
    }

    async fn classify_memory_turn_call(
        &self,
        input: &ClassifyTurnInput<'_>,
        debug: &mut dyn FnMut(JsValue),
    ) -> Result<FileMemoryClassificationResult, IndexError> {
        // The TS `userPrompt` is evaluated inside the `try`: a window error
        // is swallowed by the caller and never reaches the debug sink.
        let window = build_index_prompt_window(
            input.batch_context_messages,
            input.focus_user_turn,
            input.current_project_meta,
        )?;
        let structured = StructuredInput {
            system_prompt: MEMORY_CLASSIFICATION_SYSTEM_PROMPT.into(),
            user_prompt: stringify_pretty(&window).into(),
            request_label: "Memory turn classification".into(),
            agent_id: truthy_agent_id(&input.agent_id),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_EXTRACTION_TIMEOUT_MS),
            ),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, strict_parse, debug)
            .await
            .map_err(IndexError::Llm)?;
        let labels = normalize_classification_labels(parsed.get("labels"));
        let should_store = parsed.get("should_store").is_some_and(js_truthy) && !labels.is_empty();
        Ok(FileMemoryClassificationResult {
            should_store,
            labels,
        })
    }

    /// `createUserMemoryNote` (`:2643-2655`).
    pub async fn create_user_memory_note(
        &self,
        input: &MemoryNoteInput<'_>,
        debug: impl FnMut(JsValue),
    ) -> Result<Option<JsObject>, IndexError> {
        self.create_memory_note(MemoryCreateKind::User, input, debug)
            .await
    }

    /// `createProjectMemoryNote` (`:2657-2669`).
    pub async fn create_project_memory_note(
        &self,
        input: &MemoryNoteInput<'_>,
        debug: impl FnMut(JsValue),
    ) -> Result<Option<JsObject>, IndexError> {
        self.create_memory_note(MemoryCreateKind::Project, input, debug)
            .await
    }

    /// `createFeedbackMemoryNote` (`:2671-2683`).
    pub async fn create_feedback_memory_note(
        &self,
        input: &MemoryNoteInput<'_>,
        debug: impl FnMut(JsValue),
    ) -> Result<Option<JsObject>, IndexError> {
        self.create_memory_note(MemoryCreateKind::Feedback, input, debug)
            .await
    }

    /// `createMemoryNote` (`:2552-2641`). The user prompt is built before the
    /// TS `try`, so a window-building error escapes (the `Err` here); every
    /// error inside the call becomes `Ok(None)` after its debug event.
    async fn create_memory_note(
        &self,
        kind: MemoryCreateKind,
        input: &MemoryNoteInput<'_>,
        mut debug: impl FnMut(JsValue),
    ) -> Result<Option<JsObject>, IndexError> {
        let request_label = match kind {
            MemoryCreateKind::User => "User memory create",
            MemoryCreateKind::Project => "Project memory create",
            MemoryCreateKind::Feedback => "Feedback memory create",
        };
        let system_prompt = match kind {
            MemoryCreateKind::User => USER_NOTE_CREATE_SYSTEM_PROMPT,
            MemoryCreateKind::Project => PROJECT_NOTE_CREATE_SYSTEM_PROMPT,
            MemoryCreateKind::Feedback => FEEDBACK_NOTE_CREATE_SYSTEM_PROMPT,
        };
        // `context: JSON.parse(buildIndexPromptWindow(...))`: the window
        // value embedded directly, equal to the TS stringify->parse round
        // trip.
        let window = build_index_prompt_window(
            input.batch_context_messages,
            input.focus_user_turn,
            input.current_project_meta,
        )?;
        let user_prompt = stringify_pretty(&JsValue::Object(object([
            ("classification", input.classification.to_value()),
            ("context", window),
        ])));
        let structured = StructuredInput {
            system_prompt: system_prompt.into(),
            user_prompt: user_prompt.into(),
            request_label: request_label.into(),
            agent_id: truthy_agent_id(&input.agent_id),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_EXTRACTION_TIMEOUT_MS),
            ),
        };
        // TS declares `let rawResponse = ""` before the `try`: it stays
        // empty when the transport call fails and holds the received text
        // when a later parse step fails.
        let mut raw_response = JsString::default();
        let outcome = self
            .create_note_call(kind, input, &structured, &mut raw_response, &mut debug)
            .await;
        match outcome {
            Ok(candidate) => Ok(candidate),
            Err(error) => {
                debug(JsValue::Object(object([
                    (
                        "requestLabel",
                        JsValue::String(structured.request_label.clone()),
                    ),
                    (
                        "systemPrompt",
                        JsValue::String(structured.system_prompt.clone()),
                    ),
                    (
                        "userPrompt",
                        JsValue::String(structured.user_prompt.clone()),
                    ),
                    ("rawResponse", JsValue::String(raw_response)),
                    ("errored", JsValue::Bool(true)),
                    ("timedOut", JsValue::Bool(error.timed_out())),
                    ("errorMessage", JsValue::String(error.message())),
                ])));
                Ok(None)
            }
        }
    }

    async fn create_note_call(
        &self,
        kind: MemoryCreateKind,
        input: &MemoryNoteInput<'_>,
        structured: &StructuredInput,
        raw_response: &mut JsString,
        debug: &mut dyn FnMut(JsValue),
    ) -> Result<Option<JsObject>, IndexError> {
        *raw_response = self
            .call_structured_json(structured)
            .await
            .map_err(IndexError::Llm)?;
        // `strictParseError` stays "" on the strict path; it doubles as the
        // parse-mode discriminator for the debug event.
        let (parsed, strict_parse_error) = match strict_parse(raw_response) {
            Ok(parsed) => (parsed, JsString::default()),
            Err(strict_error) => {
                let strict_parse_error = strict_error.message.clone();
                match try_parse_loose_memory_create_payload(raw_response) {
                    Ok(Some(fallback)) => (loose_payload_value(&fallback), strict_parse_error),
                    // No loose fallback: rethrow the strict error.
                    Ok(None) => return Err(IndexError::Llm(strict_error)),
                    // `tryParseLooseMemoryCreatePayload` throws propagate
                    // past the inner TS `catch` into the outer one.
                    Err(envelope) => return Err(IndexError::Envelope(envelope)),
                }
            }
        };
        let parsed_result = if strict_parse_error.is_empty() {
            parsed.clone()
        } else {
            JsValue::Object(object([
                ("parseMode", JsValue::String("fallback".into())),
                (
                    "strictParseError",
                    JsValue::String(strict_parse_error.clone()),
                ),
                ("payload", parsed.clone()),
            ]))
        };
        debug(JsValue::Object(object([
            (
                "requestLabel",
                JsValue::String(structured.request_label.clone()),
            ),
            (
                "systemPrompt",
                JsValue::String(structured.system_prompt.clone()),
            ),
            (
                "userPrompt",
                JsValue::String(structured.user_prompt.clone()),
            ),
            ("rawResponse", JsValue::String(raw_response.clone())),
            ("parsedResult", parsed_result),
        ])));
        if matches!(parsed.get("skip"), Some(JsValue::Bool(true))) {
            return Ok(None);
        }
        Ok(build_candidate_from_create_payload(
            kind,
            &parsed,
            &input.timestamp,
            input.session_key.as_ref(),
        ))
    }

    /// `selectIndexProject` (`:3018-3125`). An empty shortlist short-circuits
    /// before any request; every other error takes the create-new fallback.
    pub async fn select_index_project(
        &self,
        input: &SelectIndexProjectInput<'_>,
        mut debug: impl FnMut(JsValue),
    ) -> IndexProjectDecision {
        if input.shortlist.is_empty() {
            return IndexProjectDecision {
                attach_existing: false,
                project_id: None,
                reason: Some(
                    "No existing General projects are available for index assignment.".into(),
                ),
            };
        }
        match self.select_index_project_call(input, &mut debug).await {
            Ok(decision) => decision,
            Err(_) => IndexProjectDecision {
                attach_existing: false,
                project_id: None,
                reason: Some(
                    "Project assignment failed; creating a new General project is safer than forcing an existing project."
                        .into(),
                ),
            },
        }
    }

    async fn select_index_project_call(
        &self,
        input: &SelectIndexProjectInput<'_>,
        debug: &mut dyn FnMut(JsValue),
    ) -> Result<IndexProjectDecision, IndexError> {
        // The user prompt is evaluated inside the TS `try`: a candidate
        // field error takes the create-new fallback without a debug event.
        let user_prompt = stringify_pretty(&JsValue::Object(select_prompt_object(input)?));
        let structured = StructuredInput {
            system_prompt: SELECT_INDEX_PROJECT_SYSTEM_PROMPT_LINES.join("\n").into(),
            user_prompt: user_prompt.into(),
            request_label: "File memory project assignment".into(),
            agent_id: truthy_agent_id(&input.agent_id),
            timeout_ms: Some(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_FILE_MEMORY_PROJECT_SELECTION_TIMEOUT_MS),
            ),
        };
        let parsed = self
            .call_structured_json_with_debug(&structured, strict_parse, debug)
            .await
            .map_err(IndexError::Llm)?;
        let attach_existing = parsed
            .get("decision")
            .and_then(JsValue::as_string)
            .is_some_and(|decision| decision.equals_str("attach_existing"));
        let selected_project_id = match parsed
            .get("selected_project_id")
            .and_then(JsValue::as_string)
        {
            Some(selected) => JsString::from_utf16(js_trim_units(selected.units()).to_vec()),
            None => JsString::default(),
        };
        let matched = input.shortlist.iter().find(|project| {
            project
                .get("projectId")
                .and_then(JsValue::as_string)
                .is_some_and(|project_id| project_id == &selected_project_id)
        });
        let reason = match parsed.get("reason").and_then(JsValue::as_string) {
            Some(reason) if !js_trim_units(reason.units()).is_empty() => {
                Some(truncate_for_prompt(reason, 260.0))
            }
            _ => None,
        };
        if attach_existing && let Some(matched) = matched {
            return Ok(IndexProjectDecision {
                attach_existing: true,
                project_id: matched
                    .get("projectId")
                    .and_then(JsValue::as_string)
                    .cloned(),
                reason,
            });
        }
        Ok(IndexProjectDecision {
            attach_existing: false,
            project_id: None,
            reason: Some(reason.unwrap_or_else(|| {
                JsString::from(if attach_existing {
                    "Model selected an invalid project id."
                } else {
                    "Model chose to create a new General project."
                })
            })),
        })
    }
}

/// The `userPrompt` object of `selectIndexProject` (`:3050-3085`).
fn select_prompt_object(input: &SelectIndexProjectInput<'_>) -> Result<JsObject, IndexError> {
    let candidate = input.candidate;
    let mut candidate_record = JsObject::new();
    insert_raw(&mut candidate_record, "type", candidate.get("type"));
    candidate_record.insert(
        "name".into(),
        JsValue::String(truncate_for_prompt(
            replace_target(candidate.get("name"))?,
            120.0,
        )),
    );
    candidate_record.insert(
        "description".into(),
        JsValue::String(truncate_for_prompt(
            replace_target(candidate.get("description"))?,
            220.0,
        )),
    );
    for (key, field) in [
        ("rule", "rule"),
        ("summary", "summary"),
        ("why", "why"),
        ("how_to_apply", "howToApply"),
        ("stage", "stage"),
    ] {
        optional_truncated(&mut candidate_record, key, candidate.get(field), 220.0)?;
    }
    for (key, field) in [
        ("decisions", "decisions"),
        ("constraints", "constraints"),
        ("next_steps", "nextSteps"),
        ("blockers", "blockers"),
        ("timeline", "timeline"),
        ("notes", "notes"),
    ] {
        candidate_record.insert(
            key.into(),
            JsValue::Array(truncated_list(candidate.get(field))?),
        );
    }

    let recent = input.recent_user_messages.unwrap_or(&[]);
    let recent_start = recent.len().saturating_sub(4);
    let mut shortlist = Vec::with_capacity(input.shortlist.len());
    for project in input.shortlist {
        let mut record = JsObject::new();
        insert_raw(&mut record, "project_id", project.get("projectId"));
        insert_raw(&mut record, "project_name", project.get("projectName"));
        record.insert(
            "description".into(),
            JsValue::String(truncate_for_prompt(
                replace_target(project.get("description"))?,
                180.0,
            )),
        );
        insert_raw(&mut record, "status", project.get("status"));
        insert_raw(&mut record, "updated_at", project.get("updatedAt"));
        insert_raw(&mut record, "shortlist_score", project.get("score"));
        insert_raw(&mut record, "shortlist_exact", project.get("exact"));
        insert_raw(&mut record, "shortlist_source", project.get("source"));
        record.insert(
            "matched_text".into(),
            JsValue::String(truncate_for_prompt(
                replace_target(project.get("matchedText"))?,
                180.0,
            )),
        );
        shortlist.push(JsValue::Object(record));
    }

    Ok(object([
        ("candidate", JsValue::Object(candidate_record)),
        (
            "candidate_memory_preview",
            JsValue::String(truncate_for_prompt(input.candidate_preview, 1600.0)),
        ),
        (
            "focus_user_turn",
            JsValue::String(truncate_for_prompt(&input.focus_turn.content, 360.0)),
        ),
        (
            "recent_user_messages",
            JsValue::Array(
                recent[recent_start..]
                    .iter()
                    .map(|message| JsValue::String(truncate_for_prompt(&message.content, 220.0)))
                    .collect(),
            ),
        ),
        ("shortlist", JsValue::Array(shortlist)),
    ]))
}

/// `field ? truncateForPrompt(field, max) : null` for the optional scalar
/// candidate fields: falsy (including absent) becomes `null`, a truthy
/// non-string reproduces the `.replace` `TypeError`.
fn optional_truncated(
    target: &mut JsObject,
    key: &str,
    value: Option<&JsValue>,
    max: f64,
) -> Result<(), IndexError> {
    match value.filter(|value| js_truthy(value)) {
        Some(value) => {
            target.insert(
                key.into(),
                JsValue::String(truncate_for_prompt(replace_target(Some(value))?, max)),
            );
        }
        None => {
            target.insert(key.into(), JsValue::Null);
        }
    }
    Ok(())
}

/// `(field ?? []).slice(0, 10).map(item => truncateForPrompt(item, 160))`.
/// A non-null non-array field throws in TS: primitives at `.slice`, strings
/// at `.map` (after slicing). Only the primitive messages are exact; the
/// object message is inferred (a pipeline-built candidate always uses
/// arrays).
fn truncated_list(value: Option<&JsValue>) -> Result<Vec<JsValue>, IndexError> {
    let items = match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => return Ok(Vec::new()),
        Some(JsValue::Array(items)) => items,
        Some(JsValue::Number(number)) => {
            return Err(type_error(join_units(
                &primitive_text(*number),
                ".slice is not a function",
            )));
        }
        Some(JsValue::Bool(bit)) => {
            return Err(type_error(join_units(
                &primitive_text(f64::from(u8::from(*bit))),
                ".slice is not a function",
            )));
        }
        Some(JsValue::String(text)) => {
            let end = text.len_utf16().min(10);
            return Err(type_error(join_units(
                &String::from_utf16_lossy(&text.units()[..end]),
                ".map is not a function",
            )));
        }
        Some(JsValue::Object(_)) => {
            return Err(type_error("{(intermediate value)}.slice is not a function"));
        }
    };
    items
        .iter()
        .take(10)
        .map(|item| {
            Ok(JsValue::String(truncate_for_prompt(
                replace_target(Some(item))?,
                160.0,
            )))
        })
        .collect()
}
