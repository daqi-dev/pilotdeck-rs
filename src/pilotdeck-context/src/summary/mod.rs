//! Original rolling-checkpoint summary planning with a narrow model port.
pub mod projection;
pub mod prompt;
pub mod strip_multimedia;
use crate::*;
pub use pilotdeck_protocol::compaction::{SUMMARY_END, SUMMARY_PREFIX};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Clone, Debug)]
pub struct SummaryError {
    pub message: String,
}
impl SummaryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
type HostError = SummaryError;
type ModuleError = SummaryError;
pub mod port {
    pub struct CheckpointTruncation {
        pub messages: Vec<pilotdeck_protocol::JsValue>,
        pub changed: bool,
    }
}
pub const COMPACT_MAX_OUTPUT_TOKENS: f64 = 4_000.0;
pub const COMPACT_SYSTEM_PROMPT_DEFAULT: &str = "You are a conversation summarizer for a coding agent. Your summary will replace the early conversation history, so it MUST preserve all information the agent needs to continue working without repeating past steps.";

pub fn truncate_head(messages: &[JsValue], keep_ratio: f64) -> Vec<JsValue> {
    let keep = ((messages.len() as f64 * clamp(keep_ratio, 0.05, 1.0)).floor() as usize).max(1);
    messages[messages.len().saturating_sub(keep)..].to_vec()
}
pub fn truncate_head_preserving_checkpoint(messages: &[JsValue], ratio: f64) -> Vec<JsValue> {
    truncate_checkpoint_with_change(messages, ratio).messages
}
pub fn truncate_checkpoint_with_change(
    messages: &[JsValue],
    ratio: f64,
) -> port::CheckpointTruncation {
    let prefix = checkpoint_prefix_len(messages);
    let live = &messages[prefix..];
    let mut tail = truncate_head(live, ratio);
    let start = live.len() - tail.len();
    let mut same_indices = start == 0;
    if !live.is_empty()
        && let Some(anchor) = (0..=start.min(live.len() - 1))
            .rev()
            .find(|i| is_real_user_request_message(&live[*i]))
        && anchor < start
    {
        tail.insert(0, live[anchor].clone());
        same_indices = start == 1 && anchor == 0;
    }
    let cleaned = pair_clean(&tail);
    // Pair cleanup preserves object references exactly when it neither drops a
    // message nor removes a block. This also works for NaN-valued metadata.
    let same_references = cleaned.len() == tail.len()
        && cleaned
            .iter()
            .zip(&tail)
            .all(|(a, b)| content(a).len() == content(b).len());
    let mut out = messages[..prefix].to_vec();
    out.extend(cleaned);
    let changed =
        !same_indices || !same_references || out.last().is_some_and(|m| is_role(m, "assistant"));
    port::CheckpointTruncation {
        messages: ensure_trailing_user_message(out),
        changed,
    }
}
fn pair_clean(messages: &[JsValue]) -> Vec<JsValue> {
    let calls = strip_unpaired_tool_calls(messages, &collect_tool_result_ids(messages));
    strip_unpaired_tool_results(&calls, &collect_tool_call_ids(&calls))
}
fn groups(messages: &[JsValue]) -> Vec<Vec<JsValue>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for message in messages {
        if (is_role(message, "user") && !is_tool_result_only_strict(message))
            || is_role(message, "assistant")
        {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            if is_role(message, "user") {
                groups.push(vec![message.clone()]);
                continue;
            }
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}
fn standalone(group: &[JsValue]) -> bool {
    group.len() == 1 && is_real_user_request_message(&group[0])
}
fn latest_request_group(groups: &[Vec<JsValue>], cut: usize) -> Option<usize> {
    if groups.is_empty() {
        return None;
    }
    (0..=cut.min(groups.len() - 1))
        .rev()
        .find(|i| groups[*i].iter().any(is_real_user_request_message))
}
fn checkpoint_prefix_len(messages: &[JsValue]) -> usize {
    let has = |m: &JsValue, role, prefix| {
        is_role(m, role)
            && content(m).iter().any(|b| {
                is_block(b, "text")
                    && string_field(b, "text").is_some_and(|s| starts_with(s, prefix))
            })
    };
    let mut i = 0;
    while i + 1 < messages.len()
        && has(&messages[i], "user", "<compact-boundary")
        && has(&messages[i + 1], "assistant", SUMMARY_PREFIX)
    {
        i += 2;
    }
    i
}
fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|n| n.is_finite() && *n > 0.0).map(f64::floor)
}
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value.is_nan() {
        min
    } else {
        value.clamp(min, max)
    }
}
fn message(role: &str, value: JsString) -> JsValue {
    object([
        ("role", text(role)),
        ("content", JsValue::Array(vec![text_block(value)])),
    ])
}
fn text(value: &str) -> JsValue {
    JsValue::String(value.into())
}
fn text_opt(value: &Option<JsString>) -> JsValue {
    JsValue::String(value.clone().unwrap_or_default())
}
fn num(value: usize) -> JsValue {
    JsValue::Number(value as f64)
}
fn set(value: &mut JsValue, key: &str, child: JsValue) {
    if let JsValue::Object(value) = value {
        value.insert(key.into(), child);
    }
}
fn failure(value: &str) -> HostError {
    ModuleError::new(value)
}
fn diagnostic(code: &str, value: JsString) -> JsValue {
    object([
        ("code", text(code)),
        ("severity", text("warning")),
        ("message", JsValue::String(value)),
    ])
}

impl From<pilotdeck_tokenizer::TokenizeError> for SummaryError {
    fn from(error: pilotdeck_tokenizer::TokenizeError) -> Self {
        Self::new(format!("{error:?}"))
    }
}

pub fn plan_messages(
    messages: &[JsValue],
    budget: f64,
    protected: &ProtectedNames,
    floor: usize,
    estimate: &dyn Fn(&[JsValue]) -> Result<f64, SummaryError>,
) -> Result<(usize, Vec<JsValue>), SummaryError> {
    let groups = groups(messages);
    let min = if floor <= 1 {
        1
    } else {
        floor.max(messages.len() / 10).min(8)
    };
    let scan = |limit: f64| -> Result<(usize, f64), HostError> {
        let (mut tokens, mut count, mut cut) = (0.0, 0usize, groups.len());
        for (index, group) in groups.iter().enumerate().rev() {
            let next = estimate(group)?;
            if tokens + next > limit && count >= min {
                break;
            }
            tokens += next;
            count += group.len();
            cut = index;
        }
        Ok((cut, tokens))
    };
    let soft = js_max(1.0, (budget * 1.5).floor());
    let (mut cut, tokens) = scan(soft)?;
    if cut == 0 && tokens > 0.0 && tokens <= soft {
        cut = scan(budget)?.0;
    }
    if cut == 0 && groups.len() > 1 {
        let mut count = 0;
        cut = groups.len();
        for (index, group) in groups.iter().enumerate().rev() {
            count += group.len();
            cut = index;
            if count >= min {
                break;
            }
        }
    }
    let names = collect_tool_names_by_call_id(messages);
    let has_protected = |group: &[JsValue]| {
        group
            .iter()
            .any(|m| is_protected_context_message(m, protected, &names))
    };
    if cut > 0 && cut < groups.len() {
        if let Some(index) = (0..cut).find(|i| has_protected(&groups[*i])) {
            cut = if index > 0 && standalone(&groups[index - 1]) {
                index - 1
            } else {
                index
            };
        } else if has_protected(&groups[cut]) && standalone(&groups[cut - 1]) {
            cut -= 1;
        }
    }
    let prefix: Vec<_> = groups[..cut].iter().flatten().cloned().collect();
    let prefix_names = collect_tool_names_by_call_id(&prefix);
    let mut keep = HashSet::new();
    for (index, group) in groups[..cut].iter().enumerate() {
        if group
            .iter()
            .any(|m| is_protected_context_message(m, protected, &prefix_names))
        {
            keep.insert(index);
            if index > 0 && standalone(&groups[index - 1]) {
                keep.insert(index - 1);
            }
        }
    }
    if let Some(anchor) = latest_request_group(&groups, cut)
        && anchor < cut
    {
        keep.insert(anchor);
    }
    let mut summarized = 0;
    let mut preserved = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if index >= cut || keep.contains(&index) {
            preserved.extend_from_slice(group);
        } else {
            summarized += group.len();
        }
    }
    Ok((summarized, pair_clean(&preserved)))
}
