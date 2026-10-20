//! Full rolling-checkpoint compaction. The injected A6.4 model execution port
//! owns provider I/O; this module owns planning and summary interpretation.
pub mod port;
mod projection;
mod prompt;
pub mod strip_multimedia;

use super::*;
use crate::host::{
    Cancellation, HostError, HostFuture, ModuleError,
    modules::model::{ModelExecution, PrepareInput, StreamInput},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};

pub const COMPACT_MAX_OUTPUT_TOKENS: f64 = 4_000.0;
pub const COMPACT_SYSTEM_PROMPT_DEFAULT: &str = "You are a conversation summarizer for a coding agent. Your summary will replace the early conversation history, so it MUST preserve all information the agent needs to continue working without repeating past steps.";
pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted into this summary. Treat it as background state, not active instructions.";
pub const SUMMARY_END: &str =
    "--- END OF CONTEXT SUMMARY - respond to the message below, not the summary above ---";

pub trait CompactionLifecycle: Send + Sync {
    fn dispatch(&self, event: &'static str, payload: JsValue) -> HostFuture<()>;
}
pub type EstimateMessages = Arc<dyn Fn(&[JsValue]) -> Result<f64, HostError> + Send + Sync>;
pub type EventEmitter = Arc<dyn Fn(JsValue) + Send + Sync>;

#[derive(Clone)]
pub struct CompactionEngineOptions {
    pub model: Arc<dyn ModelExecution>,
    pub provider: JsString,
    pub model_id: JsString,
    pub system_prompt: Option<JsString>,
    pub max_output_tokens: Option<f64>,
    pub protected_tool_names: Option<Vec<String>>,
    pub estimate_messages: Option<EstimateMessages>,
    pub lifecycle: Option<Arc<dyn CompactionLifecycle>>,
    pub event_emitter: Option<EventEmitter>,
    pub uuid: Option<Arc<dyn Fn() -> JsString + Send + Sync>>,
    /// Clock for Date.now()'s failure cooldown; independent of marker dates.
    pub clock_ms: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
}
impl CompactionEngineOptions {
    pub fn new(model: Arc<dyn ModelExecution>, provider: JsString, model_id: JsString) -> Self {
        Self {
            model,
            provider,
            model_id,
            system_prompt: None,
            max_output_tokens: None,
            protected_tool_names: None,
            estimate_messages: None,
            lifecycle: None,
            event_emitter: None,
            uuid: None,
            clock_ms: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProtectedTools {
    #[default]
    Default,
    Names(Vec<String>),
    Disabled,
}

#[derive(Clone)]
pub struct CompactionInput {
    pub trigger: JsString,
    pub messages: Vec<JsValue>,
    pub keep_tail_ratio: Option<f64>,
    pub effective_context_tokens: Option<f64>,
    pub target_post_tokens: Option<f64>,
    pub protected_tool_names: ProtectedTools,
    pub user_instruction: Option<JsString>,
    pub max_output_tokens: Option<f64>,
    pub cache_reset: Option<bool>,
    pub attachments: Vec<JsValue>,
    pub hook_results: Vec<JsValue>,
    pub signal: Option<Cancellation>,
    pub session_id: Option<JsString>,
    pub turn_id: Option<JsString>,
}
impl CompactionInput {
    pub fn new(trigger: impl Into<JsString>, messages: Vec<JsValue>) -> Self {
        Self {
            trigger: trigger.into(),
            messages,
            keep_tail_ratio: None,
            effective_context_tokens: None,
            target_post_tokens: None,
            protected_tool_names: ProtectedTools::Default,
            user_instruction: None,
            max_output_tokens: None,
            cache_reset: None,
            attachments: Vec::new(),
            hook_results: Vec::new(),
            signal: None,
            session_id: None,
            turn_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompactionResult {
    pub compaction_id: JsString,
    pub trigger: JsString,
    pub pre_tokens: f64,
    pub post_tokens: Option<f64>,
    pub messages_summarized: usize,
    pub summary_message: Option<JsValue>,
    pub boundary_marker: JsValue,
    pub messages_to_keep: Vec<JsValue>,
    pub stable_prefix: Vec<JsValue>,
    pub cache_reset: bool,
    pub target_post_tokens: Option<f64>,
    pub summary_generated: bool,
    pub checkpoint_merged: bool,
    pub summary_usage: Option<JsValue>,
    pub attachments: Vec<JsValue>,
    pub hook_results: Vec<JsValue>,
    pub diagnostics: Vec<JsValue>,
    pub error: Option<JsString>,
}
impl CompactionResult {
    /// Insertion order is the observable TS result-object order.
    pub fn to_value(&self) -> JsValue {
        let mut value = object([
            ("compactionId", JsValue::String(self.compaction_id.clone())),
            ("trigger", JsValue::String(self.trigger.clone())),
            ("preTokens", JsValue::Number(self.pre_tokens)),
            ("messagesSummarized", num(self.messages_summarized)),
            (
                "summaryMessage",
                self.summary_message.clone().unwrap_or(JsValue::Undefined),
            ),
            ("boundaryMarker", self.boundary_marker.clone()),
            (
                "messagesToKeep",
                JsValue::Array(self.messages_to_keep.clone()),
            ),
            ("stablePrefix", JsValue::Array(self.stable_prefix.clone())),
            ("cacheReset", JsValue::Bool(self.cache_reset)),
            (
                "targetPostTokens",
                self.target_post_tokens
                    .map_or(JsValue::Undefined, JsValue::Number),
            ),
            ("summaryGenerated", JsValue::Bool(self.summary_generated)),
            ("checkpointMerged", JsValue::Bool(self.checkpoint_merged)),
            (
                "summaryUsage",
                self.summary_usage.clone().unwrap_or(JsValue::Undefined),
            ),
            ("attachments", JsValue::Array(self.attachments.clone())),
            ("hookResults", JsValue::Array(self.hook_results.clone())),
            ("diagnostics", JsValue::Array(self.diagnostics.clone())),
            (
                "error",
                self.error
                    .clone()
                    .map_or(JsValue::Undefined, JsValue::String),
            ),
        ]);
        if let Some(tokens) = self.post_tokens {
            set(&mut value, "postTokens", JsValue::Number(tokens));
        }
        value
    }
}

#[derive(Clone)]
pub struct CompactionEngine {
    options: CompactionEngineOptions,
    protected: ProtectedNames,
    cooldown: Arc<Mutex<Option<(f64, JsString)>>>,
}
impl CompactionEngine {
    pub fn new(options: CompactionEngineOptions) -> Self {
        Self {
            protected: ProtectedNames::from_options(options.protected_tool_names.clone()),
            options,
            cooldown: Arc::new(Mutex::new(None)),
        }
    }
    fn now(&self) -> f64 {
        self.options.clock_ms.as_ref().map_or_else(
            || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.0
            },
            |clock| clock(),
        )
    }
    fn estimate(&self, messages: &[JsValue]) -> Result<f64, HostError> {
        match &self.options.estimate_messages {
            Some(estimate) => estimate(messages),
            None => port::CompactionBudgetPort::estimate_messages(
                &crate::runtime::model::token_accounting::TokenBudgetManager::default(),
                messages,
            ),
        }
    }
    pub async fn run(&self, input: CompactionInput) -> Result<CompactionResult, HostError> {
        let id = self
            .options
            .uuid
            .as_ref()
            .map_or_else(|| uuid::Uuid::new_v4().to_string().into(), |uuid| uuid());
        let prefix_len = checkpoint_prefix_len(&input.messages);
        let previous: Vec<_> = input.messages[..prefix_len]
            .iter()
            .skip(1)
            .step_by(2)
            .cloned()
            .collect();
        let planning = &input.messages[prefix_len..];
        let pre_tokens = self.estimate(&input.messages)?;
        let checkpoint_merged = !previous.is_empty();
        let cache_reset = input.cache_reset.unwrap_or(checkpoint_merged);
        let reserve = positive(input.max_output_tokens)
            .or_else(|| positive(self.options.max_output_tokens))
            .unwrap_or(COMPACT_MAX_OUTPUT_TOKENS);
        let target = positive(input.target_post_tokens);
        let ratio = clamp(input.keep_tail_ratio.unwrap_or(0.35), 0.0, 1.0);
        let tail_budget = target.map_or_else(
            || js_max(1.0, (pre_tokens * ratio).floor()),
            |target| js_max(1.0, target - reserve),
        );
        let protected = match &input.protected_tool_names {
            ProtectedTools::Default => self.protected.clone(),
            ProtectedTools::Names(names) => ProtectedNames::from_options(Some(names.clone())),
            ProtectedTools::Disabled => ProtectedNames::empty(),
        };
        let (summarized, mut kept) = self.plan(
            planning,
            tail_budget,
            &protected,
            if matches!(input.protected_tool_names, ProtectedTools::Disabled) {
                1
            } else {
                3
            },
        )?;
        let projected = if self.estimate(&kept)? > tail_budget {
            let (messages, changed) = projection::project_retained(
                &kept,
                &collect_tool_names_by_call_id(&input.messages),
            );
            kept = messages;
            changed
        } else {
            false
        };
        self.lifecycle(
            "PreCompact",
            object([
                ("trigger", JsValue::String(input.trigger.clone())),
                ("preTokens", JsValue::Number(pre_tokens)),
                ("messagesSummarized", num(summarized)),
            ]),
        )
        .await?;
        self.emit(object([
            ("type", text("compact_started")),
            ("sessionId", text_opt(&input.session_id)),
            ("turnId", text_opt(&input.turn_id)),
            ("compactionId", JsValue::String(id.clone())),
            ("trigger", JsValue::String(input.trigger.clone())),
            ("preTokens", JsValue::Number(pre_tokens)),
        ]));
        let mut summary = None;
        let mut error = None;
        let mut usage = None;
        if summarized > 0 || !previous.is_empty() {
            let anchors = if matches!(input.protected_tool_names, ProtectedTools::Disabled) {
                projection::protected_anchors(planning, &self.protected)?
            } else {
                None
            };
            let stable_anchors = projection::stable_anchors(planning)?;
            let summary_input = projection::project_summary(planning);
            let cooldown = self
                .cooldown
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(until, _)| *until > self.now())
                .map(|(_, error)| error.clone());
            if let Some(reason) = cooldown {
                error = Some(reason);
            } else {
                match self
                    .summarize(summary_input, &input, anchors, &previous)
                    .await
                {
                    Ok((message, summary_usage)) => {
                        summary = Some(prompt::wrap_summary(message, stable_anchors));
                        usage = summary_usage;
                        *self.cooldown.lock().unwrap() = None;
                    }
                    Err(reason) => {
                        let reason = reason.module_error.as_ref().map_or_else(
                            || JsString::from(reason.message.clone()),
                            |e| e.message.clone(),
                        );
                        *self.cooldown.lock().unwrap() =
                            Some((self.now() + 60_000.0, reason.clone()));
                        error = Some(reason);
                    }
                }
            }
        }
        let status = if summary.is_some() && error.is_none() {
            "ok"
        } else if summarized == 0 {
            "no_content"
        } else {
            "summary_failed"
        };
        let pre_token_text = crate::js_value::number_string(pre_tokens);
        let boundary = message("user", prompt::join(&[
            "<compact-boundary trigger=\"".into(), input.trigger.clone(),
            format!("\" preTokens=\"{pre_token_text}\" messagesSummarized=\"{summarized}\" status=\"{status}\" />").into(),
        ], ""));
        let mut diagnostics = if let Some(error) = error.as_ref().filter(|s| !s.is_empty()) {
            vec![diagnostic("compact_summary_failed", error.clone())]
        } else {
            summary
                .as_ref()
                .map_or_else(Vec::new, prompt::validate_structure)
        };
        if projected {
            diagnostics.push(diagnostic("compact_retained_tool_output_truncated",
            "Oversized retained tool output was replaced with a bounded preview so the compacted context can fit the tail budget.".into()));
        }
        let mut result = CompactionResult {
            compaction_id: id,
            trigger: input.trigger.clone(),
            pre_tokens,
            post_tokens: None,
            messages_summarized: summarized,
            summary_generated: summary.is_some() && error.is_none(),
            summary_message: summary,
            boundary_marker: boundary,
            messages_to_keep: kept,
            stable_prefix: Vec::new(),
            cache_reset,
            target_post_tokens: target,
            checkpoint_merged,
            summary_usage: usage,
            attachments: input.attachments.clone(),
            hook_results: input.hook_results.clone(),
            diagnostics,
            error,
        };
        if result.summary_message.is_some() {
            result.post_tokens = Some(self.estimate(&build_post_compact_messages(&result))?);
        }
        let status = if result.error.as_ref().is_some_and(|s| !s.is_empty()) {
            "failed"
        } else if result.summary_message.is_some() {
            "success"
        } else {
            "skipped"
        };
        self.lifecycle(
            "PostCompact",
            object([
                ("trigger", JsValue::String(input.trigger.clone())),
                ("status", text(status)),
                (
                    "error",
                    result
                        .error
                        .clone()
                        .map_or(JsValue::Undefined, JsValue::String),
                ),
                ("preTokens", JsValue::Number(pre_tokens)),
                (
                    "postTokens",
                    result
                        .post_tokens
                        .map_or(JsValue::Undefined, JsValue::Number),
                ),
                (
                    "summaryUsage",
                    result.summary_usage.clone().unwrap_or(JsValue::Undefined),
                ),
                ("cacheReset", JsValue::Bool(cache_reset)),
            ]),
        )
        .await?;
        let missing_usage = JsValue::Undefined;
        let summary_usage = result.summary_usage.as_ref().unwrap_or(&missing_usage);
        self.emit(object([
            ("type", text("compact_completed")),
            ("sessionId", text_opt(&input.session_id)),
            ("turnId", text_opt(&input.turn_id)),
            (
                "compactionId",
                JsValue::String(result.compaction_id.clone()),
            ),
            ("trigger", JsValue::String(input.trigger)),
            ("status", text(status)),
            ("preTokens", JsValue::Number(pre_tokens)),
            (
                "postTokens",
                result
                    .post_tokens
                    .map_or(JsValue::Undefined, JsValue::Number),
            ),
            ("messagesSummarized", num(summarized)),
            ("cacheReset", JsValue::Bool(cache_reset)),
            (
                "cacheReadTokens",
                field(summary_usage, "cacheReadTokens").clone(),
            ),
            (
                "cacheWriteTokens",
                field(summary_usage, "cacheWriteTokens").clone(),
            ),
        ]));
        Ok(result)
    }
    async fn lifecycle(&self, event: &'static str, payload: JsValue) -> Result<(), HostError> {
        if let Some(lifecycle) = &self.options.lifecycle {
            lifecycle.dispatch(event, payload).await?;
        }
        Ok(())
    }
    fn emit(&self, event: JsValue) {
        if let Some(emit) = &self.options.event_emitter {
            emit(event);
        }
    }
    async fn summarize(
        &self,
        mut messages: Vec<JsValue>,
        input: &CompactionInput,
        anchors: Option<JsString>,
        previous: &[JsValue],
    ) -> Result<(JsValue, Option<JsValue>), HostError> {
        let mut control = message(
            "user",
            prompt::user_prompt(input.user_instruction.as_ref(), anchors.as_ref(), previous),
        );
        set(
            &mut control,
            "metadata",
            object([
                ("synthetic", JsValue::Bool(true)),
                ("purpose", text("context-summary-control")),
            ]),
        );
        messages.push(control);
        let request = object([
            ("provider", JsValue::String(self.options.provider.clone())),
            ("model", JsValue::String(self.options.model_id.clone())),
            ("messages", JsValue::Array(messages)),
            (
                "systemPrompt",
                JsValue::String(prompt::system_prompt(self.options.system_prompt.as_ref())),
            ),
            (
                "maxOutputTokens",
                JsValue::Number(
                    input
                        .max_output_tokens
                        .or(self.options.max_output_tokens)
                        .unwrap_or(COMPACT_MAX_OUTPUT_TOKENS),
                ),
            ),
            ("stream", JsValue::Bool(true)),
            ("thinking", object([("enabled", JsValue::Bool(false))])),
            ("cacheBreakpoints", JsValue::Array(Vec::new())),
        ]);
        let signal = input.signal.as_ref();
        let aborted = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(aborted);
        let prepared = tokio::select! { biased;
            _ = &mut aborted => return Err(abort_failure(signal)),
            result = self.options.model.prepare(PrepareInput { request, context: JsObject::new() }) => result?,
        };
        let mut stream = tokio::select! { biased;
            _ = &mut aborted => return Err(abort_failure(signal)),
            result = self.options.model.stream(StreamInput { prepared, context: JsObject::new() }) => result?,
        };
        let mut units = Vec::new();
        let mut usage = None;
        let mut finish = JsValue::Undefined;
        let outcome = loop {
            let next = tokio::select! { biased;
                _ = &mut aborted => break Err(abort_failure(signal)),
                event = stream.next() => event,
            };
            match next {
                Err(error) => break Err(error.into()),
                Ok(None) => break Ok(()),
                Ok(Some(event)) => match string_field(&event, "type")
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .as_deref()
                {
                    Some("text_delta") => units.extend_from_slice(
                        string_field(&event, "text")
                            .unwrap_or(&JsString::default())
                            .units(),
                    ),
                    Some("usage") => usage = Some(field(&event, "usage").clone()),
                    Some("message_end") => finish = field(&event, "finishReason").clone(),
                    Some("error") => {
                        break Err(ModuleError::new(
                            string_field(field(&event, "error"), "message")
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .into());
                    }
                    _ => {}
                },
            }
        };
        if outcome.is_err() {
            // Async-iterator return() is part of the error/abort path.
            let _ = stream.close().await;
        }
        outcome?;
        let summary = trim(&JsString::from_utf16(units));
        if summary.is_empty() {
            return Err(failure("Summary model returned empty content"));
        }
        if finish.as_string().is_some_and(|s| s.equals_str("length")) {
            return Err(failure(
                "Summary model output was truncated at the token limit",
            ));
        }
        if finish
            .as_string()
            .is_some_and(|s| s.equals_str("content_filter"))
        {
            return Err(failure(
                "Summary model output was stopped by content filtering",
            ));
        }
        Ok((message("assistant", summary), usage))
    }
    fn plan(
        &self,
        messages: &[JsValue],
        budget: f64,
        protected: &ProtectedNames,
        floor: usize,
    ) -> Result<(usize, Vec<JsValue>), HostError> {
        let groups = groups(messages);
        let min = if floor <= 1 {
            1
        } else {
            floor.max(messages.len() / 10).min(8)
        };
        let scan = |limit: f64| -> Result<(usize, f64), HostError> {
            let (mut tokens, mut count, mut cut) = (0.0, 0usize, groups.len());
            for (index, group) in groups.iter().enumerate().rev() {
                let next = self.estimate(group)?;
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
}

pub fn build_post_compact_messages(result: &CompactionResult) -> Vec<JsValue> {
    let mut out = result.stable_prefix.clone();
    out.push(result.boundary_marker.clone());
    out.extend(result.summary_message.clone());
    out.extend_from_slice(&result.messages_to_keep);
    out.extend_from_slice(&result.attachments);
    out.extend_from_slice(&result.hook_results);
    ensure_trailing_user_message(out)
}
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
    ModuleError::new(value).into()
}
/// TS `streamModel.createAbortError(signal.reason)` as reached through
/// `CompactionEngine.summarize`: the provider's AbortError text, not a local
/// placeholder. A default abort is Node's `DOMException("This operation was
/// aborted", "AbortError")`; an explicit string reason is used verbatim, and
/// the host's synthetic `host_abort` marker means "no reason given".
fn abort_failure(signal: Option<&Cancellation>) -> HostError {
    // `host_abort` is the host's synthetic "no reason" marker. A real empty
    // string reason is distinct: `createAbortError` falls back to
    // "Operation aborted." because of its `... && reason` test.
    let message = match signal.and_then(Cancellation::reason) {
        None => "This operation was aborted".to_string(),
        Some(reason) if reason.equals_str("host_abort") => "This operation was aborted".to_string(),
        Some(reason) if reason.units().is_empty() => "Operation aborted.".to_string(),
        Some(reason) => String::from_utf16_lossy(reason.units()),
    };
    failure(&message)
}
fn diagnostic(code: &str, value: JsString) -> JsValue {
    object([
        ("code", text(code)),
        ("severity", text("warning")),
        ("message", JsValue::String(value)),
    ])
}
