//! Compaction capability contract and a single manual summary pass.
//! Post-summary pressure handling, including for manual force, and automatic
//! policy/stage orchestration are supplied by pr8b through `auto_compact`.
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
    fn evaluate(
        &self,
        messages: &[JsValue],
        max_context_tokens: f64,
        reserved: Option<f64>,
    ) -> Result<JsValue, HostError>;
    fn estimate_messages(&self, messages: &[JsValue]) -> Result<f64, HostError>;
}
impl CompactionBudgetPort for TokenBudgetManager {
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
pub type CheckpointTruncator = Arc<dyn Fn(&[JsValue], f64) -> Vec<JsValue> + Send + Sync>;

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
            truncate_head_preserving_checkpoint: Arc::new(truncate_head_preserving_checkpoint),
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
        (self.truncate_head_preserving_checkpoint)(messages, ratio)
    }
    pub fn try_auto_compact(
        &self,
        input: ContextInput,
        default_max_context_tokens: f64,
    ) -> HostFuture<JsValue> {
        if let Some(runner) = &self.auto_compact {
            return runner.auto_compact(input);
        }
        let port = self.clone();
        Box::pin(async move { port.manual(input, default_max_context_tokens).await })
    }
    async fn manual(
        &self,
        input: ContextInput,
        default_context: f64,
    ) -> Result<JsValue, HostError> {
        let fields = JsValue::Object(input.fields);
        let context = field(&fields, "maxContextTokens")
            .as_number()
            .unwrap_or(default_context);
        let manual = string_field(&fields, "trigger").is_some_and(|s| s.equals_str("manual"))
            && field(&fields, "manualForce") == &JsValue::Bool(true);
        let Some(budget) = self.budget.as_ref().filter(|_| manual) else {
            return Ok(skipped(empty_snapshot(context)));
        };
        let messages = match field(&fields, "messages") {
            JsValue::Array(messages) => messages,
            _ => return Err(failure("Compaction input messages must be an array.")),
        };
        let reserved = field(&fields, "reservedOutputTokens").as_number();
        let evaluate = |messages: Vec<JsValue>| {
            let evaluator = input.budget_evaluator.clone();
            async move {
                if let Some(evaluator) = evaluator {
                    evaluator.evaluate(JsValue::Array(messages)).await
                } else {
                    budget.evaluate(&messages, context, reserved)
                }
            }
        };
        let initial = evaluate(messages.clone()).await?;
        let Some(summary) = &self.summary else {
            return Ok(skipped(initial));
        };
        let effective = js_max(
            1.0,
            field(&initial, "effectiveContextTokens")
                .as_number()
                .or_else(|| field(&initial, "maxContextTokens").as_number())
                .unwrap_or(context)
                .floor(),
        );
        let mut summary_input = CompactionInput::new("manual", messages.clone());
        summary_input.effective_context_tokens = Some(effective);
        summary_input.target_post_tokens = Some(js_max(1.0, (effective * 0.60).floor()));
        summary_input.session_id = Some(
            string_field(&fields, "sessionId")
                .cloned()
                .unwrap_or_default(),
        );
        summary_input.turn_id = Some(string_field(&fields, "turnId").cloned().unwrap_or_default());
        summary_input.signal = input
            .abort_signal
            .as_ref()
            .and_then(|signal| signal.downcast_ref::<Cancellation>())
            .cloned();
        let result = summary.run(summary_input).await?;
        if result.error.is_some() || result.summary_message.is_none() {
            // No fabricated boundary or head truncation on a failed manual
            // summary. The original transcript remains the active surface.
            let _ = evaluate(messages.clone()).await?;
            return Ok(skipped(initial));
        }
        let messages = ensure_trailing_user_message(self.build_post_compact_messages(&result));
        let snapshot = evaluate(messages.clone()).await?;
        Ok(object([
            ("type", text("compacted")),
            ("messages", JsValue::Array(messages)),
            ("tier", text("full")),
            ("snapshot", snapshot),
            ("result", result.to_value()),
        ]))
    }
}
fn skipped(snapshot: JsValue) -> JsValue {
    object([("type", text("skipped")), ("snapshot", snapshot)])
}
fn empty_snapshot(context: f64) -> JsValue {
    object([
        ("tokens", JsValue::Number(0.0)),
        ("maxContextTokens", JsValue::Number(context)),
        ("warningRatio", JsValue::Number(0.0)),
        ("blockingRatio", JsValue::Number(0.0)),
        ("state", text("ok")),
        ("ratio", JsValue::Number(0.0)),
    ])
}

/// Serializable budget intent; preserve explicitly supplied false/zero.
pub fn project_compaction_budget(input: &JsValue) -> JsValue {
    let stage = if !matches!(
        field(input, "budgetStage"),
        JsValue::Undefined | JsValue::Null
    ) {
        field(input, "budgetStage").clone()
    } else if field(input, "allowFallbackOnFailure") == &JsValue::Bool(true) {
        text("recovery")
    } else if field(input, "maxContextTokens").is_undefined() {
        text("pre_route")
    } else {
        text("routed")
    };
    let mut projection = object([
        ("stage", stage),
        (
            "trigger",
            if matches!(field(input, "trigger"), JsValue::Undefined | JsValue::Null) {
                text("auto")
            } else {
                field(input, "trigger").clone()
            },
        ),
    ]);
    for name in [
        "maxContextTokens",
        "reservedOutputTokens",
        "manualForce",
        "allowFallbackOnFailure",
    ] {
        if !field(input, name).is_undefined() {
            set(&mut projection, name, field(input, name).clone());
        }
    }
    for (from, to) in [
        ("budgetRequest", "request"),
        ("budgetPreparation", "preparation"),
        ("budgetCalibration", "calibration"),
    ] {
        if js_truthy(field(input, from)) {
            set(&mut projection, to, field(input, from).clone());
        }
    }
    projection
}
