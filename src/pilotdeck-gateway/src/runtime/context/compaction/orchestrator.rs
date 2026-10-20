//! Native stage composition from `CompactionOrchestrator.ts`.
//! Returns a candidate; durable replacement remains owned by the recorder.
use super::{
    engine::{CompactionInput, CompactionResult, ProtectedTools, port::*},
    *,
};
use crate::host::{
    Cancellation, HostError, HostFuture, ModuleError, modules::context::ContextInput,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct CompactionOrchestrator {
    port: CompactionPort,
    max_context_tokens: f64,
    on_surface_changed: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl CompactionOrchestrator {
    pub fn new(port: CompactionPort, max_context_tokens: f64) -> Self {
        Self {
            port,
            max_context_tokens,
            on_surface_changed: None,
        }
    }
    pub fn with_surface_changed(mut self, callback: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_surface_changed = Some(callback);
        self
    }
    fn changed(&self) {
        if let Some(callback) = &self.on_surface_changed {
            callback();
        }
    }
    pub async fn run(&self, input: ContextInput) -> Result<JsValue, HostError> {
        let fields = JsValue::Object(input.fields.clone());
        let context = field(&fields, "maxContextTokens")
            .as_number()
            .unwrap_or(self.max_context_tokens);
        let manual = string_field(&fields, "trigger").is_some_and(|s| s.equals_str("manual"))
            && field(&fields, "manualForce") == &JsValue::Bool(true);
        let port = &self.port;
        let Some(budget) = port
            .budget
            .as_ref()
            .filter(|_| manual || port.policy.is_some())
        else {
            return Ok(skipped(empty_snapshot(context)));
        };
        let JsValue::Array(messages) = field(&fields, "messages") else {
            return Err(ModuleError::new("Compaction input messages must be an array.").into());
        };
        let mut messages = messages.clone();
        let evaluate = |messages: Vec<JsValue>| {
            let evaluator = input.budget_evaluator.clone();
            let reserved = field(&fields, "reservedOutputTokens").as_number();
            async move {
                if let Some(evaluator) = evaluator {
                    evaluator.evaluate(JsValue::Array(messages)).await
                } else {
                    budget.evaluate(&messages, context, reserved)
                }
            }
        };
        let mut current = evaluate(messages.clone()).await?;
        if manual {
            if port.summary.is_none() {
                return Ok(skipped(current));
            }
        } else {
            let decision = port
                .policy
                .as_ref()
                .unwrap()
                .evaluate_snapshot(current.clone())?;
            if !string_field(&decision, "type").is_some_and(|s| s.equals_str("trigger")) {
                return Ok(skipped(field(&decision, "snapshot").clone()));
            }
            if let Some(micro) = &port.micro {
                let mut micro_input = MicroCompactionInput::new(&messages);
                micro_input.trim_to_tokens = Some(768.0);
                let result = micro.apply(&micro_input)?;
                messages = result.messages;
                current = evaluate(messages.clone()).await?;
                if ratio(&current) < 0.90 {
                    if result.rewritten == 0 {
                        return Ok(skipped(current));
                    }
                    self.changed();
                    return Ok(object([
                        ("type", "compacted".into()),
                        (
                            "messages",
                            JsValue::Array(ensure_trailing_user_message(messages)),
                        ),
                        ("tier", "micro".into()),
                        ("snapshot", current),
                    ]));
                }
            }
            if ratio(&current) < 0.90 {
                return Ok(skipped(current));
            }
        }
        let Some(summary) = &port.summary else {
            return Ok(skipped(current));
        };
        let effective = js_max(
            1.0,
            field(&current, "effectiveContextTokens")
                .as_number()
                .or_else(|| field(&current, "maxContextTokens").as_number())
                .unwrap_or(f64::NAN)
                .floor(),
        );
        let target = js_max(1.0, (effective * 0.60).floor());
        let summary_input = |trigger: &str, messages: Vec<JsValue>| {
            let mut value = CompactionInput::new(trigger, messages);
            value.effective_context_tokens = Some(effective);
            value.target_post_tokens = Some(target);
            value.session_id = Some(
                string_field(&fields, "sessionId")
                    .cloned()
                    .unwrap_or_default(),
            );
            value.turn_id = Some(string_field(&fields, "turnId").cloned().unwrap_or_default());
            value.signal = input
                .abort_signal
                .as_ref()
                .and_then(|s| s.downcast_ref::<Cancellation>())
                .cloned();
            value
        };
        let result = summary
            .run(summary_input(
                if manual { "manual" } else { "auto" },
                messages.clone(),
            ))
            .await?;
        let summary_succeeded = succeeded(&result);
        let mut post = if summary_succeeded {
            ensure_trailing_user_message(port.build_post_compact_messages(&result))
        } else {
            messages
        };
        let mut final_result = summary_succeeded.then_some(result);
        let mut snapshot = evaluate(post.clone()).await?;
        let mut snip_applied = false;
        if summary_succeeded
            && ratio(&snapshot) > 0.60
            && let Some(snip) = &port.snip
        {
            let message_tokens = budget.estimate_messages(&post)?;
            let non_message_tokens = js_max(0.0, n(&snapshot, "tokens") - message_tokens);
            let message_target = js_max(1.0, target - non_message_tokens);
            let result = snip.snip(
                &post,
                SnipCallOptions {
                    target_total_tokens: Some(js_min(message_tokens, message_target)),
                    ..Default::default()
                },
            )?;
            post = result.messages;
            snapshot = evaluate(post.clone()).await?;
            snip_applied = result.applied;
        }

        let mut emergency_applied = false;
        if ratio(&snapshot) >= 0.90 {
            // The baseline deliberately evaluates again on emergency entry.
            snapshot = evaluate(post.clone()).await?;
            if !below_emergency(&snapshot) {
                let mut emergency_input = summary_input("reactive", post.clone());
                emergency_input.keep_tail_ratio = Some(0.05);
                emergency_input.protected_tool_names = ProtectedTools::Disabled;
                emergency_input.max_output_tokens = Some(1536.0);
                emergency_input.cache_reset = Some(true);
                let emergency = summary.run(emergency_input).await?;
                if succeeded(&emergency) {
                    let mut replacement = emergency.clone();
                    replacement.cache_reset = true;
                    replacement.stable_prefix.clear();
                    post = ensure_trailing_user_message(
                        port.build_post_compact_messages(&replacement),
                    );
                    emergency_applied = true;
                    snapshot = evaluate(post.clone()).await?;
                    final_result = Some(emergency);
                }
                if !below_emergency(&snapshot) {
                    if let Some(micro) = &port.micro {
                        let mut micro_input = MicroCompactionInput::new(&post);
                        micro_input.trim_to_tokens = Some(256.0);
                        micro_input.keep_latest = Some(1.0);
                        micro_input.protected_tool_names = Some(None);
                        let projected = micro.apply(&micro_input)?;
                        post = projected.messages;
                        emergency_applied |= projected.rewritten > 0;
                        snapshot = evaluate(post.clone()).await?;
                    }
                    if !below_emergency(&snapshot) {
                        let truncated = (port.truncate_head_preserving_checkpoint)(&post, 0.10);
                        emergency_applied |= truncated.changed;
                        post = truncated.messages;
                        snapshot = evaluate(post.clone()).await?;
                        if let Some(result) = &mut final_result {
                            result.diagnostics.push(diagnostic(
                                "context_hard_truncate", "error",
                                format!("Emergency head truncation kept approximately 10% of the live history (tokens={}, max={}). Earlier live turns may be unavailable outside the durable transcript.",
                                    number(n(&snapshot, "tokens")), number(n(&snapshot, "maxContextTokens"))),
                            ));
                        }
                    }
                }
            }
        }
        let overflow = ratio(&snapshot) >= 1.0;
        if !summary_succeeded && !snip_applied && !emergency_applied && !overflow {
            return Ok(skipped(current));
        }
        if ratio(&snapshot) > 0.60
            && let Some(result) = &mut final_result
        {
            result.diagnostics.push(diagnostic("compaction_target_not_reached", "warning",
                format!("Compaction remained above the 60% target (tokens={}, target={}, ratio={}). Protected checkpoints, tool turns, or the required recent tail may account for the remainder.",
                    number(n(&snapshot, "tokens")), number(target), fixed(ratio(&snapshot)))));
        }
        if summary_succeeded || snip_applied || emergency_applied {
            self.changed();
        }
        if overflow && let Some(result) = &mut final_result {
            result.diagnostics.push(diagnostic("context_overflow_after_emergency_compaction", "error",
                format!("Context remains over the effective input budget after emergency compaction (tokens={}, max={}, ratio={}). The stable checkpoint, current request, tool protocol, and required tail are the remaining sources.",
                    number(n(&snapshot, "tokens")), number(n(&snapshot, "maxContextTokens")), fixed(ratio(&snapshot)))));
        }
        let mut output = JsObject::new();
        output.insert("type".into(), "compacted".into());
        output.insert("messages".into(), JsValue::Array(post));
        output.insert(
            "tier".into(),
            if ratio(&snapshot) >= 0.90 {
                "emergency"
            } else {
                "full"
            }
            .into(),
        );
        output.insert("snapshot".into(), snapshot);
        if let Some(result) = final_result {
            output.insert("result".into(), result.to_value());
        }
        if overflow {
            output.insert(
                "error".into(),
                "context_overflow_after_emergency_compaction".into(),
            );
        }
        Ok(JsValue::Object(output))
    }
}
impl CompactionRunner for CompactionOrchestrator {
    fn auto_compact(&self, input: ContextInput) -> HostFuture<JsValue> {
        let runner = self.clone();
        Box::pin(async move { runner.run(input).await })
    }
}
fn n(value: &JsValue, key: &str) -> f64 {
    field(value, key).as_number().unwrap_or(f64::NAN)
}
fn ratio(snapshot: &JsValue) -> f64 {
    n(snapshot, "ratio")
}
fn below_emergency(snapshot: &JsValue) -> bool {
    ratio(snapshot) < 0.90
}
fn number(value: f64) -> String {
    crate::js_value::number_string(value)
}
fn fixed(value: f64) -> String {
    pilotdeck_protocol::number::to_fixed_3(value)
}
fn succeeded(result: &CompactionResult) -> bool {
    result.error.is_none() && result.summary_message.is_some()
}
fn diagnostic(code: &str, severity: &str, message: String) -> JsValue {
    object([
        ("code", code.into()),
        ("severity", severity.into()),
        ("message", JsValue::String(message.into())),
    ])
}
pub(crate) fn skipped(snapshot: JsValue) -> JsValue {
    object([("type", "skipped".into()), ("snapshot", snapshot)])
}
pub(crate) fn empty_snapshot(context: f64) -> JsValue {
    object([
        ("tokens", JsValue::Number(0.0)),
        ("maxContextTokens", JsValue::Number(context)),
        ("warningRatio", JsValue::Number(0.0)),
        ("blockingRatio", JsValue::Number(0.0)),
        ("state", "ok".into()),
        ("ratio", JsValue::Number(0.0)),
    ])
}
