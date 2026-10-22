//! Compaction capability contract; stage-only ports use the shared orchestrator.
use super::*;
use crate::host::modules::context::ContextInput;
use crate::runtime::model::token_accounting::TokenBudgetManager;

pub trait CompactionSummaryPort: Send + Sync {
    fn run(&self, input: CompactionInput) -> HostFuture<CompactionResult>;
}
impl CompactionSummaryPort for CompactionEngine {
    fn run(&self, input: CompactionInput) -> HostFuture<CompactionResult> {
        let engine = self.clone();
        Box::pin(async move { CompactionEngine::run(&engine, input).await })
    }
}
pub trait CompactionBudgetPort: Send + Sync {
    /// Keep JS values intact until the budget capability consumes them. A
    /// custom port can observe the same raw arguments as TS's budget.evaluate.
    fn evaluate_raw(
        &self,
        messages: &JsValue,
        context: &JsValue,
        reserved: &JsValue,
    ) -> Result<JsValue, HostError> {
        let JsValue::Array(messages) = messages else {
            return Err(failure("messages is not iterable"));
        };
        let number = |value: &JsValue| {
            crate::js_value::to_number(value)
                .map_err(|_| failure(crate::js_value::StackOverflow::MESSAGE))
        };
        self.evaluate(
            messages,
            number(context)?,
            if matches!(reserved, JsValue::Null | JsValue::Undefined) {
                None
            } else {
                Some(number(reserved)?)
            },
        )
    }
    fn evaluate(
        &self,
        messages: &[JsValue],
        max_context_tokens: f64,
        reserved: Option<f64>,
    ) -> Result<JsValue, HostError>;
    fn estimate_messages(&self, messages: &[JsValue]) -> Result<f64, HostError>;
}
impl CompactionBudgetPort for TokenBudgetManager {
    fn evaluate_raw(
        &self,
        messages: &JsValue,
        context: &JsValue,
        reserved: &JsValue,
    ) -> Result<JsValue, HostError> {
        let messages = match messages {
            JsValue::Array(messages) => messages.as_slice(),
            JsValue::String(s) if s.is_empty() => &[],
            JsValue::String(_) => return Err(failure("message.content is not iterable")),
            _ => return Err(failure("messages is not iterable")),
        };
        let number = |v: &JsValue| {
            crate::js_value::to_number(v)
                .map_err(|_| failure(crate::js_value::StackOverflow::MESSAGE))
        };
        let reserved = if matches!(reserved, JsValue::Null | JsValue::Undefined) {
            0.0
        } else {
            number(reserved)?
        };
        let mut result =
            CompactionBudgetPort::evaluate(self, messages, number(context)?, Some(reserved))?;
        // effectiveInputContextTokens uses Number.isFinite before coercion,
        // so non-number context values pass through to the public snapshot.
        if !matches!(context, JsValue::Number(_)) {
            let tokens = field(&result, "tokens").as_number().unwrap();
            let numeric = number(context)?;
            set(&mut result, "maxContextTokens", context.clone());
            set(&mut result, "effectiveContextTokens", context.clone());
            let ratio = if numeric > 0.0 { tokens / numeric } else { 0.0 };
            set(&mut result, "ratio", JsValue::Number(ratio));
            let state = if ratio >= self.blocking_ratio {
                "blocking"
            } else if ratio >= self.warning_ratio {
                "warning"
            } else {
                "ok"
            };
            set(&mut result, "state", state.into());
        }
        Ok(result)
    }
    fn evaluate(
        &self,
        messages: &[JsValue],
        context: f64,
        reserved: Option<f64>,
    ) -> Result<JsValue, HostError> {
        let mut metadata = JsObject::new();
        if let Some(reserved) = reserved {
            metadata.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        }
        Ok(self.snapshot_from_tokens(
            CompactionBudgetPort::estimate_messages(self, messages)?,
            context,
            &metadata,
        ))
    }
    fn estimate_messages(&self, messages: &[JsValue]) -> Result<f64, HostError> {
        let mut request = JsObject::new();
        // safeJsonStringify catches V8's depth failure. Bound it before the
        // typed model counter, which otherwise accepts arbitrary JSON depth.
        let projected = messages
            .iter()
            .map(|message| {
                let blocks = content(message)
                    .iter()
                    .map(|block| {
                        if !is_block(block, "tool_call") {
                            return Ok(block.clone());
                        }
                        let mut units = crate::js_value::to_string_units(field(block, "name"))
                            .map_err(|_| failure(crate::js_value::StackOverflow::MESSAGE))?;
                        let input = field(block, "input");
                        if !matches!(input, JsValue::Null | JsValue::Undefined) {
                            units.extend(
                                crate::js_value::json_stringify(input)
                                    .ok()
                                    .flatten()
                                    .unwrap_or_default()
                                    .encode_utf16(),
                            );
                        }
                        Ok(text_block(JsString::from_utf16(units)))
                    })
                    .collect::<Result<Vec<_>, HostError>>()?;
                Ok(replace_content(message, blocks))
            })
            .collect::<Result<Vec<_>, HostError>>()?;
        request.insert("messages".into(), JsValue::Array(projected));
        let (request, _) = crate::host::modules::budget::accounting_request(&request)?;
        self.estimate_messages_tokens(&request.messages)
    }
}
pub trait CompactionRunner: Send + Sync {
    fn auto_compact(&self, input: ContextInput) -> HostFuture<JsValue>;
}
pub trait CompactionPolicyPort: Send + Sync {
    fn evaluate_snapshot(&self, snapshot: JsValue) -> Result<JsValue, HostError>;
}
pub trait CompactionRecoveryPort: Send + Sync {
    fn decide(&self, input: JsValue) -> Result<JsValue, HostError>;
}
pub trait CompactionMicroPort: Send + Sync {
    fn apply(&self, input: &MicroCompactionInput<'_>) -> Result<MicroCompactionResult, HostError>;
}
impl CompactionMicroPort for MicroCompactionEngine {
    fn apply(&self, input: &MicroCompactionInput<'_>) -> Result<MicroCompactionResult, HostError> {
        MicroCompactionEngine::apply(self, input).map_err(|error| failure(&format!("{error:?}")))
    }
}
pub trait CompactionSnipPort: Send + Sync {
    fn snip(&self, messages: &[JsValue], options: SnipCallOptions)
    -> Result<SnipResult, HostError>;
}
impl CompactionSnipPort for SnipEngine {
    fn snip(
        &self,
        messages: &[JsValue],
        options: SnipCallOptions,
    ) -> Result<SnipResult, HostError> {
        SnipEngine::snip(self, messages, options).map_err(|error| failure(&format!("{error:?}")))
    }
}
pub type PostCompactBuilder = Arc<dyn Fn(&CompactionResult) -> Vec<JsValue> + Send + Sync>;
/// Rust message values do not retain JS object identity. A custom truncator
/// explicitly reports whether it replaced any message reference or position.
pub struct CheckpointTruncation {
    pub messages: Vec<JsValue>,
    pub changed: bool,
}
pub type CheckpointTruncator = Arc<dyn Fn(&[JsValue], f64) -> CheckpointTruncation + Send + Sync>;

#[derive(Clone)]
pub struct CompactionPort {
    pub dispose: Option<Arc<dyn Fn() -> HostFuture<()> + Send + Sync>>,
    pub auto_compact: Option<Arc<dyn CompactionRunner>>,
    pub budget: Option<Arc<dyn CompactionBudgetPort>>,
    pub policy: Option<Arc<dyn CompactionPolicyPort>>,
    pub summary: Option<Arc<dyn CompactionSummaryPort>>,
    pub micro: Option<Arc<dyn CompactionMicroPort>>,
    pub snip: Option<Arc<dyn CompactionSnipPort>>,
    pub recovery: Option<Arc<dyn CompactionRecoveryPort>>,
    pub build_post_compact_messages: PostCompactBuilder,
    pub truncate_head_preserving_checkpoint: CheckpointTruncator,
}
impl Default for CompactionPort {
    fn default() -> Self {
        Self {
            dispose: None,
            auto_compact: None,
            budget: None,
            policy: None,
            summary: None,
            micro: None,
            snip: None,
            recovery: None,
            build_post_compact_messages: Arc::new(build_post_compact_messages),
            truncate_head_preserving_checkpoint: Arc::new(truncate_checkpoint_with_change),
        }
    }
}
impl CompactionPort {
    pub fn build_post_compact_messages(&self, result: &CompactionResult) -> Vec<JsValue> {
        (self.build_post_compact_messages)(result)
    }
    pub fn truncate_head_preserving_checkpoint(
        &self,
        messages: &[JsValue],
        ratio: f64,
    ) -> Vec<JsValue> {
        (self.truncate_head_preserving_checkpoint)(messages, ratio).messages
    }
    pub fn try_auto_compact(
        &self,
        input: ContextInput,
        default_max_context_tokens: f64,
    ) -> HostFuture<JsValue> {
        if let Some(runner) = &self.auto_compact {
            return runner.auto_compact(input);
        }
        use crate::runtime::context::compaction::orchestrator::CompactionOrchestrator;
        CompactionOrchestrator::new(self.clone(), default_max_context_tokens).auto_compact(input)
    }
}

/// Shared with the transport serializer that publishes budgetProjection.
pub use pilotdeck_protocol::compaction::project_compaction_budget;
