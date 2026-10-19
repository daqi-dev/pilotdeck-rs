//! TS `context/compaction/SnipEngine.ts`: prune middle turns while keeping a
//! head/tail anchor and a `<snip-boundary>` marker.
use super::*;
use pilotdeck_tokenizer::TokenizeError;

#[derive(Clone, Debug, Default)]
pub struct SnipEngineOptions {
    /// Number of head turns to keep (default 2). JS numbers are accepted so
    /// fractional/NaN values behave like the baseline.
    pub keep_head_turns: Option<f64>,
    /// Number of tail turns to keep (default 4).
    pub keep_tail_turns: Option<f64>,
    /// Master enable flag; when false `snip` is a no-op (default true).
    pub enabled: Option<bool>,
    /// Tool names whose turns should be preserved verbatim.
    pub protected_tool_names: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct SnipCallOptions {
    /// Optional token target for the retained live tail.
    pub target_tokens: Option<f64>,
    /// Optional token target for the complete prompt, including checkpoints.
    pub target_total_tokens: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct SnipResult {
    pub messages: Vec<JsValue>,
    pub applied: bool,
    /// Number of turns removed from the middle.
    pub turns_snipped: usize,
    /// Dangling tool_call ids whose results were removed.
    pub dangling_tool_call_ids: Vec<JsString>,
}

const SNIP_BOUNDARY_TEXT_PREFIX: &str = "<snip-boundary";

pub fn create_snip_boundary(turns_snipped: usize, head_turns: f64, tail_turns: f64) -> JsValue {
    let text = format!(
        "{SNIP_BOUNDARY_TEXT_PREFIX} turnsSnipped=\"{turns_snipped}\" headTurns=\"{}\" tailTurns=\"{}\" />",
        crate::js_value::number_string(head_turns),
        crate::js_value::number_string(tail_turns)
    );
    object([
        ("role", JsValue::String("user".into())),
        (
            "content",
            JsValue::Array(vec![text_block(JsString::from(text))]),
        ),
    ])
}

pub fn is_snip_boundary_message(message: &JsValue) -> bool {
    if !is_role(message, "user") || content(message).len() != 1 {
        return false;
    }
    let block = &content(message)[0];
    is_block(block, "text")
        && string_field(block, "text")
            .is_some_and(|text| starts_with(text, SNIP_BOUNDARY_TEXT_PREFIX))
}

pub struct SnipEngine {
    keep_head_turns: f64,
    keep_tail_turns: f64,
    enabled: bool,
    protected_tool_names: ProtectedNames,
}

impl SnipEngine {
    pub fn new(options: SnipEngineOptions) -> Self {
        Self {
            keep_head_turns: js_max(0.0, options.keep_head_turns.unwrap_or(2.0)),
            keep_tail_turns: js_max(1.0, options.keep_tail_turns.unwrap_or(4.0)),
            enabled: options.enabled.unwrap_or(true),
            protected_tool_names: ProtectedNames::from_options(options.protected_tool_names),
        }
    }

    pub fn snip(
        &self,
        messages: &[JsValue],
        options: SnipCallOptions,
    ) -> Result<SnipResult, TokenizeError> {
        let unchanged = || SnipResult {
            messages: messages.to_vec(),
            applied: false,
            turns_snipped: 0,
            dangling_tool_call_ids: Vec::new(),
        };
        if !self.enabled {
            return Ok(unchanged());
        }
        let prefix_count = checkpoint_prefix_message_count(messages);
        let stable_prefix = &messages[..prefix_count];
        let stable_prefix_tokens = estimate_messages_tokens(stable_prefix)?;
        let target_tokens = match options.target_total_tokens {
            Some(total) => Some(js_max(1.0, total.floor() - stable_prefix_tokens as f64)),
            None => options.target_tokens,
        };
        let constrained = target_tokens.is_some();
        let live_messages = &messages[prefix_count..];
        let turns = if constrained {
            split_into_tool_cycles(live_messages)
        } else {
            split_into_turns(live_messages)
        };
        if !constrained && (turns.len() as f64) <= self.keep_head_turns + self.keep_tail_turns {
            return Ok(unchanged());
        }

        let mut keep = vec![false; turns.len()];
        let mut keep_count = 0usize;
        let keep_index = |value: &mut Vec<bool>, index: usize, count: &mut usize| {
            if index < value.len() && !value[index] {
                value[index] = true;
                *count += 1;
            }
        };
        if target_tokens.is_none() {
            let head_limit = js_min(self.keep_head_turns, turns.len() as f64);
            for index in 0..turns.len() {
                if (index as f64) < head_limit {
                    keep_index(&mut keep, index, &mut keep_count);
                }
            }
        }
        let mut accumulated_tail_tokens = 0usize;
        let mut tail_start_index = turns.len();
        let tail_floor = js_min(
            if constrained {
                1.0
            } else {
                self.keep_tail_turns
            },
            turns.len() as f64,
        );
        for index in (0..turns.len()).rev() {
            let tokens = estimate_messages_tokens(&turns[index])?;
            let must_keep = ((turns.len() - index) as f64) <= tail_floor;
            if !must_keep
                && let Some(target) = target_tokens
                && accumulated_tail_tokens as f64 + tokens as f64 > js_max(1.0, target)
            {
                break;
            }
            keep_index(&mut keep, index, &mut keep_count);
            tail_start_index = index;
            accumulated_tail_tokens += tokens;
        }
        if let Some(index) = find_latest_user_request_group_index(&turns, tail_start_index) {
            keep_index(&mut keep, index, &mut keep_count);
        }
        let protected_indexes = if constrained {
            collect_protected_cycle_indexes(&turns, live_messages, &self.protected_tool_names)
        } else {
            collect_protected_turn_indexes(live_messages, &self.protected_tool_names)
        };
        for index in protected_indexes {
            keep_index(&mut keep, index, &mut keep_count);
        }
        if keep_count >= turns.len() {
            return Ok(unchanged());
        }

        let turns_snipped = turns.len() - keep_count;
        let mut projected = stable_prefix.to_vec();
        projected.extend(stitch_kept_turns_with_boundaries(
            &turns,
            &keep,
            self.keep_head_turns,
            self.keep_tail_turns,
        ));

        let tool_result_ids = collect_tool_result_ids(&projected);
        let tool_call_ids = collect_tool_call_ids(&projected);
        let without_dangling_calls = strip_unpaired_tool_calls(&projected, &tool_result_ids);
        let paired_tool_call_ids = collect_tool_call_ids(&without_dangling_calls);
        let cleaned = strip_unpaired_tool_results(&without_dangling_calls, &paired_tool_call_ids);

        let dangling = tool_call_ids
            .iter()
            .filter(|id| !tool_result_ids.contains(id))
            .cloned()
            .collect();

        Ok(SnipResult {
            messages: ensure_trailing_user_message(cleaned),
            applied: true,
            turns_snipped,
            dangling_tool_call_ids: dangling,
        })
    }
}

/// S6: one-shot projection; returns the input verbatim when nothing is snipped.
pub fn project_snipped_view(
    messages: &[JsValue],
    options: SnipEngineOptions,
) -> Result<Vec<JsValue>, TokenizeError> {
    Ok(SnipEngine::new(options)
        .snip(messages, SnipCallOptions::default())?
        .messages)
}

fn checkpoint_prefix_message_count(messages: &[JsValue]) -> usize {
    let mut index = 0usize;
    while index + 1 < messages.len()
        && is_compact_boundary(&messages[index])
        && is_summary_message(&messages[index + 1])
    {
        index += 2;
    }
    index
}

fn is_compact_boundary(message: &JsValue) -> bool {
    is_role(message, "user")
        && content(message).iter().any(|block| {
            is_block(block, "text")
                && string_field(block, "text")
                    .is_some_and(|text| starts_with(text, "<compact-boundary"))
        })
}

fn is_summary_message(message: &JsValue) -> bool {
    is_role(message, "assistant")
        && content(message).iter().any(|block| {
            is_block(block, "text")
                && string_field(block, "text")
                    .is_some_and(|text| starts_with(text, "[CONTEXT COMPACTION - REFERENCE ONLY]"))
        })
}

fn split_into_turns(messages: &[JsValue]) -> Vec<Vec<JsValue>> {
    let mut turns: Vec<Vec<JsValue>> = Vec::new();
    let mut current: Vec<JsValue> = Vec::new();
    for message in messages {
        let is_user_start = is_role(message, "user") && !is_tool_result_only(message);
        if is_user_start && !current.is_empty() {
            turns.push(std::mem::take(&mut current));
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        turns.push(current);
    }
    turns
}

fn split_into_tool_cycles(messages: &[JsValue]) -> Vec<Vec<JsValue>> {
    let mut groups: Vec<Vec<JsValue>> = Vec::new();
    let mut current: Vec<JsValue> = Vec::new();
    for message in messages {
        if ((is_role(message, "user") && !is_tool_result_only(message))
            || is_role(message, "assistant"))
            && !current.is_empty()
        {
            groups.push(std::mem::take(&mut current));
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

fn collect_protected_cycle_indexes(
    groups: &[Vec<JsValue>],
    messages: &[JsValue],
    protected: &ProtectedNames,
) -> Vec<usize> {
    let names_by_call_id = collect_tool_names_by_call_id(messages);
    let mut indexes = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if !group
            .iter()
            .any(|message| is_protected_context_message(message, protected, &names_by_call_id))
        {
            continue;
        }
        indexes.push(index);
        if let Some(anchor) = find_latest_user_request_group_index(groups, index) {
            indexes.push(anchor);
        }
    }
    indexes
}

fn collect_protected_turn_indexes(messages: &[JsValue], protected: &ProtectedNames) -> Vec<usize> {
    let names_by_call_id = collect_tool_names_by_call_id(messages);
    let mut indexes = Vec::new();
    let mut turn_index = 0usize;
    let mut current: Vec<&JsValue> = Vec::new();
    let flush = |current: &mut Vec<&JsValue>, turn_index: &mut usize, indexes: &mut Vec<usize>| {
        if current.is_empty() {
            return;
        }
        if current
            .iter()
            .any(|message| is_protected_context_message(message, protected, &names_by_call_id))
        {
            indexes.push(*turn_index);
        }
        *turn_index += 1;
        current.clear();
    };
    for message in messages {
        let is_user_start = is_role(message, "user") && !is_tool_result_only_strict(message);
        if is_user_start && !current.is_empty() {
            flush(&mut current, &mut turn_index, &mut indexes);
        }
        current.push(message);
    }
    flush(&mut current, &mut turn_index, &mut indexes);
    indexes
}

fn find_latest_user_request_group_index(
    groups: &[Vec<JsValue>],
    at_or_before: usize,
) -> Option<usize> {
    if groups.is_empty() {
        return None;
    }
    (0..=at_or_before.min(groups.len() - 1))
        .rev()
        .find(|&index| groups[index].iter().any(is_real_user_request_message))
}

fn stitch_kept_turns_with_boundaries(
    turns: &[Vec<JsValue>],
    keep: &[bool],
    head_turns: f64,
    tail_turns: f64,
) -> Vec<JsValue> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (index, turn) in turns.iter().enumerate() {
        if !keep[index] {
            skipped += 1;
            continue;
        }
        if skipped > 0 {
            out.push(create_snip_boundary(skipped, head_turns, tail_turns));
            skipped = 0;
        }
        out.extend(turn.iter().cloned());
    }
    if skipped > 0 {
        out.push(create_snip_boundary(skipped, head_turns, tail_turns));
    }
    out
}
